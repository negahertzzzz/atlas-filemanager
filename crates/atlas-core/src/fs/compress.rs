//! Zip creation: compress a selection of files/folders into a new archive.
//! Follows the `ops.rs` contract — progress via `OpProgress`, per-item failures
//! as typed [`OpError`]s in the final `OpComplete`.

use atlas_model::app_state::BgResult;
use atlas_model::ops::{OpError, OpErrorKind};
use atlas_model::pane::TabId;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Compress `sources` (files and/or directories, recursively) into `dest_zip`.
/// Entry names are relative to each source's parent, so extracting reproduces
/// the selected items side by side.
pub async fn compress_to_zip(
    sources: Vec<PathBuf>,
    dest_zip: PathBuf,
    op_id: u64,
    tx: flume::Sender<BgResult>,
    cancel: Arc<AtomicBool>,
    affected_tabs: Vec<TabId>,
) {
    let result = tokio::task::spawn_blocking(move || {
        compress_sync(&sources, &dest_zip, op_id, &tx, &cancel, affected_tabs);
    })
    .await;
    if result.is_err() {
        tracing::error!("compress worker panicked");
    }
}

fn compress_sync(
    sources: &[PathBuf],
    dest_zip: &Path,
    op_id: u64,
    tx: &flume::Sender<BgResult>,
    cancel: &AtomicBool,
    affected_tabs: Vec<TabId>,
) {
    let mut errors: Vec<OpError> = Vec::new();

    // Collect the full file list first so progress has a real total.
    let mut files: Vec<(PathBuf, PathBuf)> = Vec::new(); // (abs path, zip entry name)
    let mut dirs: Vec<PathBuf> = Vec::new(); // zip dir entries (keep empty dirs)
    for src in sources {
        let base = src.parent().unwrap_or(Path::new(""));
        collect(src, base, &mut files, &mut dirs, &mut errors);
    }
    let total = files.len() as u64;

    let out = match std::fs::File::create(dest_zip) {
        Ok(f) => f,
        Err(e) => {
            let _ = tx.send(BgResult::OpComplete {
                id: op_id,
                result: Err(vec![OpError {
                    path: dest_zip.to_path_buf(),
                    kind: super::transfer::io_error_kind(&e),
                }]),
                affected_tabs,
            });
            return;
        }
    };
    let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(out));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);

    for dir in dirs {
        let name = zip_entry_name(&dir);
        let _ = zip.add_directory(name, options);
    }

    let mut cancelled = false;
    let mut buf = vec![0u8; 1 << 20];
    for (i, (abs, rel)) in files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let _ = tx.send(BgResult::OpProgress {
            id: op_id,
            done: i as u64,
            total,
            bytes_done: 0,
            bytes_total: 0,
            current_file: abs.clone(),
        });
        let mut reader = match std::fs::File::open(abs) {
            Ok(f) => f,
            Err(e) => {
                errors.push(OpError {
                    path: abs.clone(),
                    kind: super::transfer::io_error_kind(&e),
                });
                continue;
            }
        };
        if let Err(e) = zip.start_file(zip_entry_name(rel), options) {
            errors.push(OpError {
                path: abs.clone(),
                kind: OpErrorKind::Msg(e.to_string()),
            });
            continue;
        }
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Err(e) = zip.write_all(&buf[..n]) {
                        errors.push(OpError {
                            path: abs.clone(),
                            kind: OpErrorKind::Msg(e.to_string()),
                        });
                        break;
                    }
                }
                Err(e) => {
                    errors.push(OpError {
                        path: abs.clone(),
                        kind: super::transfer::io_error_kind(&e),
                    });
                    break;
                }
            }
        }
    }

    if let Err(e) = zip.finish() {
        errors.push(OpError {
            path: dest_zip.to_path_buf(),
            kind: OpErrorKind::Msg(e.to_string()),
        });
    }
    if cancelled {
        // A partial archive is misleading — remove it.
        let _ = std::fs::remove_file(dest_zip);
        errors.push(OpError {
            path: PathBuf::new(),
            kind: OpErrorKind::Cancelled,
        });
    }
    let result = if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    };
    let _ = tx.send(BgResult::OpComplete {
        id: op_id,
        result,
        affected_tabs,
    });
}

/// Recursively gather files (and empty dirs) under `src`, with entry names
/// relative to `base`.
fn collect(
    src: &Path,
    base: &Path,
    files: &mut Vec<(PathBuf, PathBuf)>,
    dirs: &mut Vec<PathBuf>,
    errors: &mut Vec<OpError>,
) {
    let rel = src.strip_prefix(base).unwrap_or(src).to_path_buf();
    if src.is_dir() {
        let rd = match std::fs::read_dir(src) {
            Ok(rd) => rd,
            Err(e) => {
                errors.push(OpError {
                    path: src.to_path_buf(),
                    kind: super::transfer::io_error_kind(&e),
                });
                return;
            }
        };
        let mut any = false;
        for entry in rd.flatten() {
            any = true;
            collect(&entry.path(), base, files, dirs, errors);
        }
        if !any {
            dirs.push(rel);
        }
    } else {
        files.push((src.to_path_buf(), rel));
    }
}

/// Zip entry names always use forward slashes.
fn zip_entry_name(rel: &Path) -> String {
    rel.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compresses_files_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("inner.txt"), "inner").unwrap();
        std::fs::write(tmp.path().join("a.txt"), "hello").unwrap();
        let dest = tmp.path().join("out.zip");

        let (tx, rx) = flume::unbounded();
        compress_to_zip(
            vec![tmp.path().join("a.txt"), dir.clone()],
            dest.clone(),
            1,
            tx,
            Arc::new(AtomicBool::new(false)),
            Vec::new(),
        )
        .await;

        let complete = rx
            .drain()
            .find_map(|m| match m {
                BgResult::OpComplete { result, .. } => Some(result),
                _ => None,
            })
            .expect("OpComplete sent");
        assert!(complete.is_ok(), "{complete:?}");

        let file = std::fs::File::open(&dest).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len())
            .map(|i| archive.by_index(i).unwrap().name().unwrap().into_owned())
            .collect();
        assert!(names.contains(&"a.txt".to_string()), "{names:?}");
        assert!(names.contains(&"d/inner.txt".to_string()), "{names:?}");
    }
}
