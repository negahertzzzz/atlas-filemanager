//! Read-only virtual file system over archive files (zip, tar.*).
//!
//! # Supported formats
//! - `.zip`, `.jar`, `.war`, `.ear` — via the `zip` crate
//! - `.tar` — plain tar
//! - `.tar.gz` / `.tgz` — gzip-compressed tar
//! - `.tar.bz2` / `.tbz2` — bzip2-compressed tar
//! - `.tar.xz` / `.txz` — xz-compressed tar
//! - `.tar.zst` / `.tzst` — zstd-compressed tar (unsupported, graceful error)

pub mod bomb_guard;
pub mod cache;
pub(crate) mod common;
#[cfg(feature = "rar")]
pub mod rar_backend;
pub mod sanitize;
#[cfg(feature = "sevenz")]
pub mod sevenz_backend;
pub mod tar_backend;
pub mod zip_backend;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use atlas_model::file_entry::{FileEntry, FileKind};

// ─── Capabilities ─────────────────────────────────────────────────────────────

/// What operations a given archive backend supports.
#[derive(Debug, Clone, Copy)]
pub struct ArchiveCaps {
    pub write: bool,
    pub passwords: bool,
    pub preserves_compression_on_rewrite: bool,
}

// ─── Error type ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ArchiveError {
    PasswordRequired,
    WrongPassword,
    Corrupt {
        detail: String,
        partial: Option<Vec<FileEntry>>,
    },
    UnsupportedOp(&'static str),
    Io(std::io::Error),
    Cancelled,
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PasswordRequired => write!(f, "Password required"),
            Self::WrongPassword => write!(f, "Wrong password"),
            Self::Corrupt { detail, .. } => write!(f, "Corrupt archive: {detail}"),
            Self::UnsupportedOp(op) => write!(f, "Unsupported operation: {op}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Cancelled => write!(f, "Operation cancelled"),
        }
    }
}

impl std::error::Error for ArchiveError {}

// ─── Operation control (pause / cancel) ───────────────────────────────────────

const OP_RUNNING: u8 = 0;
const OP_PAUSED: u8 = 1;
const OP_CANCELLED: u8 = 2;

/// Shared control handle for archive operations.
///
/// Pass a clone to the background worker; keep the original to pause/cancel.
#[derive(Clone)]
pub struct OpCtrl(Arc<AtomicU8>);

impl OpCtrl {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU8::new(OP_RUNNING)))
    }

    pub fn pause(&self) {
        let _ = self
            .0
            .compare_exchange(OP_RUNNING, OP_PAUSED, Ordering::SeqCst, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        let _ = self
            .0
            .compare_exchange(OP_PAUSED, OP_RUNNING, Ordering::SeqCst, Ordering::Relaxed);
    }

    pub fn cancel(&self) {
        self.0.store(OP_CANCELLED, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::Relaxed) == OP_PAUSED
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed) == OP_CANCELLED
    }

    /// Call at each entry boundary (and every ~1 MB during extraction).
    /// Blocks while paused; returns `Err(Cancelled)` if cancelled.
    pub fn checkpoint(&self) -> Result<(), ArchiveError> {
        loop {
            match self.0.load(Ordering::Relaxed) {
                OP_RUNNING => return Ok(()),
                OP_PAUSED => std::thread::sleep(Duration::from_millis(100)),
                _ => return Err(ArchiveError::Cancelled),
            }
        }
    }
}

impl Default for OpCtrl {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Public trait ────────────────────────────────────────────────────────────

/// Read-only access to archive contents.
pub trait ArchiveProvider: Send + Sync {
    /// What this backend supports.
    fn caps(&self) -> ArchiveCaps {
        ArchiveCaps {
            write: false,
            passwords: false,
            preserves_compression_on_rewrite: false,
        }
    }
    /// List all entries directly inside `inner_dir`. Pass `Path::new("")` for the root.
    fn list(&self, inner_dir: &Path) -> anyhow::Result<Vec<FileEntry>>;
    /// Stream the raw bytes of `inner_path` (must be a file, not a directory).
    fn read_bytes(&self, inner_path: &Path) -> anyhow::Result<Box<dyn Read + Send>>;
    /// Extract `inner_path` into `dest` (creates/overwrites the destination file).
    fn extract_to(&self, inner_path: &Path, dest: &Path) -> anyhow::Result<()>;
    /// The archive file on disk.
    fn archive_path(&self) -> &Path;
}

/// Largest `Zone.Identifier` stream copied from an archive. Real ones are a
/// few hundred bytes; anything bigger is not a Mark-of-the-Web.
#[cfg(windows)]
const MAX_ZONE_IDENTIFIER: u64 = 64 * 1024;

/// Copy the archive's Mark-of-the-Web (`Zone.Identifier` stream) onto a file
/// that was extracted from it, as Explorer does. Without it an executable or
/// Office document downloaded inside a zip would run without SmartScreen or
/// Protected View once Atlas extracts it. Best effort; a no-op off Windows and
/// for archives that carry no mark.
pub fn propagate_mark_of_the_web(archive: &Path, extracted: &Path) {
    #[cfg(windows)]
    {
        use std::io::Read as _;
        fn zone_stream(path: &Path) -> PathBuf {
            let mut stream = path.as_os_str().to_os_string();
            stream.push(":Zone.Identifier");
            PathBuf::from(stream)
        }
        let Ok(file) = std::fs::File::open(zone_stream(archive)) else {
            return;
        };
        let mut zone = Vec::new();
        if file
            .take(MAX_ZONE_IDENTIFIER + 1)
            .read_to_end(&mut zone)
            .is_err()
            || zone.is_empty()
            || zone.len() as u64 > MAX_ZONE_IDENTIFIER
        {
            return;
        }
        #[allow(clippy::disallowed_methods)] // reason: extraction runs on blocking workers
        if let Err(error) = std::fs::write(zone_stream(extracted), &zone) {
            tracing::debug!(%error, ?extracted, "could not propagate Mark-of-the-Web");
        }
    }
    #[cfg(not(windows))]
    let _ = (archive, extracted);
}

/// [`ArchiveProvider::extract_to`] for files that leave Atlas (opened with
/// their default app, dragged out, copied out): keeps the archive's
/// Mark-of-the-Web on the extracted copy.
pub fn extract_for_export(
    provider: &dyn ArchiveProvider,
    inner_path: &Path,
    dest: &Path,
) -> anyhow::Result<()> {
    provider.extract_to(inner_path, dest)?;
    propagate_mark_of_the_web(provider.archive_path(), dest);
    Ok(())
}

// ─── Extension detection ─────────────────────────────────────────────────────

/// Detected archive format (used by `archive_format` and `open`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar(tar_backend::Compression),
    Rar,
    SevenZ,
}

/// Detect the archive format from `path`, handling double extensions
/// (`.tar.gz`, `.tar.bz2`, `.tar.xz`, `.tar.zst`) before the final extension.
pub fn archive_format(path: &Path) -> Option<ArchiveFormat> {
    let fname = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    // Double extensions first.
    if fname.ends_with(".tar.gz") {
        return Some(ArchiveFormat::Tar(tar_backend::Compression::Gzip));
    }
    if fname.ends_with(".tar.bz2") {
        return Some(ArchiveFormat::Tar(tar_backend::Compression::Bzip2));
    }
    if fname.ends_with(".tar.xz") {
        return Some(ArchiveFormat::Tar(tar_backend::Compression::Xz));
    }
    if fname.ends_with(".tar.zst") {
        return Some(ArchiveFormat::Tar(tar_backend::Compression::Zst));
    }
    // Single extension.
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "zip" | "jar" | "war" | "ear" => Some(ArchiveFormat::Zip),
        "rar" | "cbr" => Some(ArchiveFormat::Rar),
        "7z" | "cb7" => Some(ArchiveFormat::SevenZ),
        "tar" => Some(ArchiveFormat::Tar(tar_backend::Compression::None)),
        "tgz" => Some(ArchiveFormat::Tar(tar_backend::Compression::Gzip)),
        "tbz2" => Some(ArchiveFormat::Tar(tar_backend::Compression::Bzip2)),
        "txz" => Some(ArchiveFormat::Tar(tar_backend::Compression::Xz)),
        "tzst" => Some(ArchiveFormat::Tar(tar_backend::Compression::Zst)),
        _ => None,
    }
}

/// Returns `true` for file extensions Atlas can navigate into as archives.
pub fn is_archive_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "zip"
            | "jar"
            | "war"
            | "ear"
            | "rar"
            | "cbr"
            | "7z"
            | "cb7"
            | "tar"
            | "tgz"
            | "tbz2"
            | "txz"
            | "tzst"
    )
}

/// Returns `true` when `path` looks like a supported archive (by name/extension).
pub fn is_archive_path(path: &Path) -> bool {
    archive_format(path).is_some()
}

// ─── Factory ─────────────────────────────────────────────────────────────────

/// Open the archive at `path` with an optional password, using the provider cache.
///
/// Returns `None` if the format is unsupported or the file cannot be opened.
/// The `password` parameter is accepted but only used by backends that support
/// encryption (zip AES, 7z, rar — wired up in F5).
pub fn open_with(
    path: &Path,
    #[allow(unused_variables)] password: Option<&str>,
) -> Option<Arc<dyn ArchiveProvider>> {
    let fmt = archive_format(path);
    tracing::debug!(?path, ?fmt, "archive::open_with");
    cache::get_or_open(path, |p| {
        let result = match archive_format(p)? {
            ArchiveFormat::Zip => zip_backend::ZipProvider::new(p)
                .map_err(|e| {
                    tracing::warn!(%e, ?p, "ZIP open failed");
                    e
                })
                .ok()
                .map(|b| Arc::new(b) as Arc<dyn ArchiveProvider>),
            ArchiveFormat::Tar(compression) => tar_backend::TarProvider::new(p, compression)
                .map_err(|e| {
                    tracing::warn!(%e, ?p, "TAR open failed");
                    e
                })
                .ok()
                .map(|b| Arc::new(b) as Arc<dyn ArchiveProvider>),
            #[cfg(feature = "rar")]
            ArchiveFormat::Rar => rar_backend::RarProvider::new(p)
                .map_err(|e| {
                    tracing::warn!(%e, ?p, "RAR open failed");
                    e
                })
                .ok()
                .map(|b| Arc::new(b) as Arc<dyn ArchiveProvider>),
            #[cfg(not(feature = "rar"))]
            ArchiveFormat::Rar => {
                tracing::warn!(?p, "RAR support not compiled (feature 'rar' disabled)");
                None
            }
            #[cfg(feature = "sevenz")]
            ArchiveFormat::SevenZ => sevenz_backend::SevenzProvider::new(p, password)
                .map_err(|e| {
                    tracing::warn!(%e, ?p, "7z open failed");
                    e
                })
                .ok()
                .map(|b| Arc::new(b) as Arc<dyn ArchiveProvider>),
            #[cfg(not(feature = "sevenz"))]
            ArchiveFormat::SevenZ => {
                tracing::warn!(?p, "7z support not compiled (feature 'sevenz' disabled)");
                None
            }
        };
        if result.is_none() {
            tracing::warn!(?p, "archive::open_with returned None");
        }
        result
    })
}

/// Open the archive at `path`. Wrapper around `open_with(path, None)`.
///
/// Returns `None` if the format is unsupported or the file cannot be opened.
pub fn open(path: &Path) -> Option<Arc<dyn ArchiveProvider>> {
    open_with(path, None)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Build a `FileEntry` representing a virtual item inside an archive.
pub(crate) fn make_entry(
    archive: &Path,
    inner_path: PathBuf,
    name: String,
    is_dir: bool,
    size: Option<u64>,
) -> FileEntry {
    FileEntry::from(atlas_model::file_entry::FileEntryData {
        name: name.into(),
        path: inner_path,
        kind: if is_dir {
            FileKind::Dir
        } else {
            FileKind::File
        },
        size,
        modified: None,
        _is_hidden: false,
        _is_symlink: false,
        archive_root: Some(archive.to_path_buf()),
        recycle_info: None,
    })
}

/// Format a breadcrumb-style display path: `archive.zip › inner › dir`.
pub fn display_path(archive: &Path, inner: &Path) -> String {
    let archive_name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let inner_str = inner.to_string_lossy();
    if inner_str.is_empty() {
        archive_name
    } else {
        format!("{archive_name} › {}", inner_str.replace(['/', '\\'], " › "))
    }
}
