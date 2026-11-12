use std::path::PathBuf;

use atlas_model::app_state::BgResult;
use atlas_model::pane::TabId;
use atlas_model::preview_cache::{
    is_audio_ext, is_image_ext, DirPreviewEntry, PreviewResult, DIR_PREVIEW_LIMIT,
};

use super::markdown::{is_markdown_ext, parse_markdown};
use super::text::highlight_text;
use super::MAX_TEXT_BYTES;

pub async fn load_archive_preview(
    tab: TabId,
    archive: PathBuf,
    inner: PathBuf,
    tx: flume::Sender<BgResult>,
) {
    let ext = inner
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let result = {
        let inner2 = inner.clone();
        tokio::task::spawn_blocking(move || -> PreviewResult {
            if super::is_cloud_placeholder(&archive) {
                return super::offline_preview();
            }
            let provider = match crate::fs::archive::open_with(&archive, None) {
                Some(p) => p,
                None => return PreviewResult::Error("Formato de archivo no soportado".into()),
            };
            let mut reader = match provider.read_bytes(&inner2) {
                Ok(r) => r,
                Err(e) => return PreviewResult::Error(e.to_string()),
            };
            let cap = if matches!(ext.as_str(), "csv" | "tsv") {
                2 * 1024 * 1024
            } else if ext == "docx" {
                32 * 1024 * 1024
            } else if is_image_ext(&ext) {
                16 * 1024 * 1024
            } else {
                MAX_TEXT_BYTES
            };
            let mut buf = Vec::with_capacity(cap.min(1024 * 1024));
            let mut tmp = [0u8; 65536];
            loop {
                use std::io::Read as _;
                match reader.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.len() >= cap {
                            break;
                        }
                    }
                    Err(e) => return PreviewResult::Error(e.to_string()),
                }
            }
            bytes_to_preview(buf, &ext)
        })
        .await
        .unwrap_or_else(|e| PreviewResult::Error(e.to_string()))
    };

    let _ = tx.send(BgResult::PreviewReady {
        tab,
        path: inner,
        result,
    });
}

fn bytes_to_preview(bytes: Vec<u8>, ext: &str) -> PreviewResult {
    if ext == "docx" {
        super::document::preview_docx_reader(std::io::Cursor::new(bytes), "")
    } else if matches!(ext, "csv" | "tsv") {
        crate::ext::runner::delimited_bytes(
            &bytes,
            if ext == "tsv" { b'\t' } else { b',' },
            bytes.len() >= 2 * 1024 * 1024,
        )
    } else if is_image_ext(ext) {
        match image::load_from_memory(&bytes) {
            Ok(img) => {
                let original_width = img.width() as usize;
                let original_height = img.height() as usize;
                let img =
                    img.thumbnail(super::MAX_IMAGE_PREVIEW_EDGE, super::MAX_IMAGE_PREVIEW_EDGE);
                let rgba = img.to_rgba8();
                let (width, height) = rgba.dimensions();
                PreviewResult::Image {
                    rgba: rgba.into_raw(),
                    width: width as usize,
                    height: height as usize,
                    original_width,
                    original_height,
                    exif: Vec::new(),
                }
            }
            Err(e) => PreviewResult::Error(e.to_string()),
        }
    } else if is_markdown_ext(ext) {
        if bytes.len() > MAX_TEXT_BYTES {
            return PreviewResult::Binary {
                size: bytes.len() as u64,
            };
        }
        match String::from_utf8(bytes) {
            Ok(source) => {
                let blocks = parse_markdown(&source);
                PreviewResult::Markdown {
                    blocks,
                    source,
                    base_uri: String::new(),
                }
            }
            Err(e) => PreviewResult::Binary {
                size: e.into_bytes().len() as u64,
            },
        }
    } else if ext == "pdf" || is_audio_ext(ext) {
        PreviewResult::Binary {
            size: bytes.len() as u64,
        }
    } else {
        if bytes.len() > MAX_TEXT_BYTES {
            return PreviewResult::Binary {
                size: bytes.len() as u64,
            };
        }
        match String::from_utf8(bytes) {
            Ok(text) => {
                let lines = highlight_text(&text, ext);
                PreviewResult::Text { lines, text }
            }
            Err(e) => PreviewResult::Binary {
                size: e.into_bytes().len() as u64,
            },
        }
    }
}

/// Listing preview for an archive file: root entries (dirs first) plus totals
/// over the whole tree, walked breadth-first with an entry budget.
pub(crate) fn preview_archive_listing(archive: &std::path::Path) -> PreviewResult {
    const WALK_BUDGET: usize = 20_000;
    if super::is_cloud_placeholder(archive) {
        return super::offline_preview();
    }
    let Some(provider) = crate::fs::archive::open(archive) else {
        return PreviewResult::Error("Unsupported archive format".into());
    };
    let root = match provider.list(std::path::Path::new("")) {
        Ok(entries) => entries,
        Err(e) => return PreviewResult::Error(e.to_string()),
    };
    let (mut total_count, mut total_size) = (0usize, 0u64);
    let mut queue: std::collections::VecDeque<PathBuf> = std::collections::VecDeque::new();
    for entry in &root {
        total_count += 1;
        total_size += entry.size().unwrap_or(0);
        if entry.kind() == atlas_model::file_entry::FileKind::Dir {
            queue.push_back(entry.path());
        }
    }
    while let Some(dir) = queue.pop_front() {
        if total_count >= WALK_BUDGET {
            break;
        }
        for entry in provider.list(&dir).unwrap_or_default() {
            total_count += 1;
            total_size += entry.size().unwrap_or(0);
            if entry.kind() == atlas_model::file_entry::FileKind::Dir {
                queue.push_back(entry.path());
            }
        }
    }
    let mut entries: Vec<DirPreviewEntry> = root
        .iter()
        .map(|entry| DirPreviewEntry {
            name: entry.name.to_string(),
            path: archive.join(entry.path()),
            is_dir: entry.kind() == atlas_model::file_entry::FileKind::Dir,
            size: entry.size(),
            extension: entry.extension().map(|e| e.to_lowercase().to_string()),
        })
        .collect();
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    entries.truncate(DIR_PREVIEW_LIMIT);
    PreviewResult::Dir {
        entries,
        total_count,
        total_size,
    }
}
