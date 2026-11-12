use atlas_model::app_state::BgResult;
use atlas_model::ops::{OpError, OpErrorKind};
use atlas_model::pane::TabId;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_reports_only_successful_paths_and_preserves_cancelled_sources() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("atlas-delete-drop-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        struct TempTree(PathBuf);
        impl Drop for TempTree {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = TempTree(root.clone());
        let removed = root.join("removed.txt");
        let preserved = root.join("preserved.txt");
        std::fs::write(&removed, b"delete").unwrap();
        std::fs::write(&preserved, b"cancel").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, rx) = flume::unbounded();
        runtime.block_on(delete_files(
            vec![removed.clone(), root.join("missing.txt")],
            false,
            1,
            tx,
            Arc::new(AtomicBool::new(false)),
            vec![],
        ));
        let events = rx.try_iter().collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(event, BgResult::PathsDeleted(paths) if paths == &vec![removed.clone()])));
        assert!(events.iter().any(|event| matches!(event, BgResult::OpComplete { result: Err(errors), .. } if errors.len() == 1)));
        assert!(!removed.exists());
        let (tx, rx) = flume::unbounded();
        runtime.block_on(delete_files(
            vec![preserved.clone()],
            false,
            2,
            tx,
            Arc::new(AtomicBool::new(true)),
            vec![],
        ));
        assert!(!rx
            .try_iter()
            .any(|event| matches!(event, BgResult::PathsDeleted(_))));
        assert!(preserved.exists());
    }
}

// Copy/move live in the unified transfer engine (`super::transfer`); this
// module keeps the simpler one-shot operations (delete, rename, create dir).

pub async fn delete_files(
    paths: Vec<PathBuf>,
    to_trash: bool,
    op_id: u64,
    tx: flume::Sender<BgResult>,
    cancel: Arc<AtomicBool>,
    affected_tabs: Vec<TabId>,
) {
    let total = paths.len() as u64;
    let mut errors: Vec<OpError> = Vec::new();
    let mut completed = Vec::new();
    let mut cancelled = false;
    for (i, path) in paths.iter().enumerate() {
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
            current_file: path.clone(),
        });
        if !path.exists() {
            errors.push(OpError {
                path: path.clone(),
                kind: OpErrorKind::Gone,
            });
            continue;
        }
        if to_trash && crate::fs::paths::wsl_distro_name(path).is_some() {
            errors.push(OpError {
                path: path.clone(),
                kind: OpErrorKind::TrashUnsupported,
            });
            continue;
        }
        let result = if to_trash {
            let p = path.clone();
            tokio::task::spawn_blocking(move || {
                trash::delete(&p).map_err(|e| OpErrorKind::Msg(e.to_string()))
            })
            .await
            .unwrap_or_else(|e| Err(OpErrorKind::Msg(e.to_string())))
        } else {
            remove_path(path).await.map_err(|e| {
                e.downcast_ref::<std::io::Error>().map_or_else(
                    || OpErrorKind::Msg(e.to_string()),
                    super::transfer::io_error_kind,
                )
            })
        };
        if let Err(kind) = result {
            errors.push(OpError {
                path: path.clone(),
                kind,
            });
        } else {
            completed.push(path.clone());
        }
    }
    if cancelled {
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
    if !completed.is_empty() {
        let _ = tx.send(BgResult::PathsDeleted(completed));
    }
    let _ = tx.send(BgResult::OpComplete {
        id: op_id,
        result,
        affected_tabs,
    });
}

pub async fn rename_file(
    from: PathBuf,
    to: PathBuf,
    op_id: u64,
    tx: flume::Sender<BgResult>,
    affected_tabs: Vec<TabId>,
) {
    let result = tokio::fs::rename(&from, &to).await.map_err(|e| {
        vec![OpError {
            path: from.clone(),
            kind: super::transfer::io_error_kind(&e),
        }]
    });
    let _ = tx.send(BgResult::OpComplete {
        id: op_id,
        result,
        affected_tabs,
    });
}

pub async fn create_dir(
    path: PathBuf,
    op_id: u64,
    tx: flume::Sender<BgResult>,
    affected_tabs: Vec<TabId>,
) {
    let result = tokio::fs::create_dir(&path).await.map_err(|e| {
        vec![OpError {
            path: path.clone(),
            kind: super::transfer::io_error_kind(&e),
        }]
    });
    let _ = tx.send(BgResult::OpComplete {
        id: op_id,
        result,
        affected_tabs,
    });
}

pub async fn create_file(
    path: PathBuf,
    op_id: u64,
    tx: flume::Sender<BgResult>,
    affected_tabs: Vec<TabId>,
) {
    let result = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .await
        .map(|_| ())
        .map_err(|e| {
            vec![OpError {
                path: path.clone(),
                kind: super::transfer::io_error_kind(&e),
            }]
        });
    let _ = tx.send(BgResult::OpComplete {
        id: op_id,
        result,
        affected_tabs,
    });
}

async fn remove_path(path: &Path) -> anyhow::Result<()> {
    if path.is_dir() {
        tokio::fs::remove_dir_all(path).await?;
    } else {
        tokio::fs::remove_file(path).await?;
    }
    Ok(())
}
