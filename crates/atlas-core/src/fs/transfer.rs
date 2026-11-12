//! Unified transfer engine: the single code path behind paste, drag-and-drop
//! (internal and external), and menu copy/move.
//!
//! Pipeline: [`validate`] → [`detect_conflicts`] → (conflict dialog in the UI)
//! → [`build_pairs`] → [`execute`]. Every failure surfaces as a per-item
//! [`OpError`] in the final [`TransferReport`] — nothing is silently skipped.

use atlas_model::app_state::BgResult;
use atlas_model::ops::{
    ConflictItem, ConflictPolicy, OpError, OpErrorKind, Resolution, TransferKind, TransferOrigin,
    TransferReport, TransferRequest,
};
use atlas_model::pane::TabId;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const CHUNK_SIZE: usize = 1024 * 1024;
/// Small files use one native blocking copy. This avoids dispatching multiple
/// blocking-pool read/write operations per file while keeping large copies on
/// the cancellable, byte-progress path.
const NATIVE_SMALL_FILE_MAX: u64 = 8 * 1024 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);
/// Abort after this many consecutive identical errors (dead destination,
/// unplugged network share…) instead of spamming one toast per file.
const MAX_REPEATED_ERRORS: usize = 3;

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

/// Case-insensitive path equality on Windows (NTFS), exact elsewhere.
pub fn paths_equal_ci(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        false
    }
}

fn starts_with_ci(child: &Path, ancestor: &Path) -> bool {
    if child.starts_with(ancestor) {
        return true;
    }
    if cfg!(windows) {
        let c = child.to_string_lossy().to_lowercase();
        let a = ancestor.to_string_lossy().to_lowercase();
        return Path::new(&c).starts_with(Path::new(&a));
    }
    false
}

fn same_volume(a: &Path, b: &Path) -> bool {
    super::paths::same_volume(a, b)
}

/// Add the `\\?\` extended-length prefix on Windows when a path is long enough
/// to hit `MAX_PATH` limits. Applied only at syscall sites; reported paths stay
/// in their normal form.
fn ext_path(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.as_os_str();
        if s.len() > 240 && p.is_absolute() {
            let disp = p.to_string_lossy();
            if !disp.starts_with(r"\\?\") {
                if let Some(unc) = disp.strip_prefix(r"\\") {
                    return PathBuf::from(format!(r"\\?\UNC\{unc}"));
                }
                return PathBuf::from(format!(r"\\?\{disp}"));
            }
        }
    }
    p.to_path_buf()
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

/// Map an `io::Error` to a typed cause the UI can translate.
pub fn io_error_kind(io: &std::io::Error) -> OpErrorKind {
    if let Some(code) = io.raw_os_error() {
        #[cfg(windows)]
        match code {
            5 => return OpErrorKind::PermissionDenied,
            // ERROR_SHARING_VIOLATION / ERROR_LOCK_VIOLATION
            32 | 33 => return OpErrorKind::InUse,
            // ERROR_DISK_FULL / ERROR_HANDLE_DISK_FULL
            112 | 39 => return OpErrorKind::DiskFull,
            // ERROR_FILENAME_EXCED_RANGE / ERROR_BUFFER_OVERFLOW
            206 | 111 => return OpErrorKind::PathTooLong,
            // ERROR_BAD_NETPATH / ERROR_UNEXP_NET_ERR / ERROR_NETNAME_DELETED /
            // ERROR_BAD_NET_NAME / ERROR_NETWORK_UNREACHABLE
            53 | 59 | 64 | 67 | 1231 => return OpErrorKind::NetworkUnreachable,
            _ => {}
        }

        #[cfg(unix)]
        match code {
            // EIO: I/O error (not permission denied)
            5 => return OpErrorKind::Msg(io.to_string()),
            // EACCES: Permission denied
            13 => return OpErrorKind::PermissionDenied,
            // EEXIST: File exists
            17 => return OpErrorKind::AlreadyExists,
            // ENOSPC: No space left on device
            28 => return OpErrorKind::DiskFull,
            // EBUSY / ETXTBSY: In use / busy
            16 | 26 => return OpErrorKind::InUse,
            // ENAMETOOLONG: File name too long
            36 => return OpErrorKind::PathTooLong,
            // Network errors: ENETUNREACH / ENETDOWN / EHOSTUNREACH
            101 | 100 | 113 => return OpErrorKind::NetworkUnreachable,
            _ => {}
        }
    }
    match io.kind() {
        std::io::ErrorKind::PermissionDenied => OpErrorKind::PermissionDenied,
        std::io::ErrorKind::NotFound => OpErrorKind::Gone,
        std::io::ErrorKind::AlreadyExists => OpErrorKind::AlreadyExists,
        _ => OpErrorKind::Msg(io.to_string()),
    }
}

fn op_error(path: &Path, io: &std::io::Error) -> OpError {
    OpError {
        path: path.to_path_buf(),
        kind: io_error_kind(io),
    }
}

// ---------------------------------------------------------------------------
// Filename validation (shared by rename inline, create folder, batch rename)
// ---------------------------------------------------------------------------

/// Typed filename-validation failure; the UI translates each variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilenameError {
    Empty,
    InvalidChar(char),
    TrailingDotOrSpace,
    Reserved(String),
}

/// Validate a bare file/folder name for Windows.
#[cfg(windows)]
pub fn validate_filename(name: &str) -> Result<(), FilenameError> {
    if matches!(name, "." | "..") {
        return Err(FilenameError::Reserved(name.to_string()));
    }
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if name.trim().is_empty() {
        return Err(FilenameError::Empty);
    }
    if let Some(bad) = name.chars().find(|c| {
        matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (*c as u32) < 32
    }) {
        return Err(FilenameError::InvalidChar(bad));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(FilenameError::TrailingDotOrSpace);
    }
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    if RESERVED.contains(&base.as_str()) {
        return Err(FilenameError::Reserved(base));
    }
    Ok(())
}

/// Validate a bare file/folder name for Unix/Linux: only `/` and `\0` are invalid.
#[cfg(not(windows))]
pub fn validate_filename(name: &str) -> Result<(), FilenameError> {
    if matches!(name, "." | "..") {
        return Err(FilenameError::Reserved(name.to_string()));
    }
    if name.is_empty() {
        return Err(FilenameError::Empty);
    }
    if let Some(bad) = name.chars().find(|&c| c == '/' || c == '\0') {
        return Err(FilenameError::InvalidChar(bad));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

pub struct ValidationOutcome {
    /// Sources that passed every check, in original order.
    pub valid: Vec<PathBuf>,
    /// Per-item failures to surface as toasts; the rest of the transfer continues.
    pub errors: Vec<OpError>,
    /// A request-level failure: the destination folder no longer exists;
    /// nothing can run. Holds the missing destination for the UI to name.
    pub fatal: Option<PathBuf>,
}

fn target_name<'a>(src: &'a Path, names: Option<&'a [OsString]>, index: usize) -> OsString {
    names
        .and_then(|names| names.get(index))
        .filter(|name| !name.is_empty())
        .cloned()
        .unwrap_or_else(|| src.file_name().unwrap_or_default().to_os_string())
}

/// Pre-flight checks shared by every entry point (paste, drag-and-drop, menu, undo).
pub fn validate(req: &TransferRequest) -> ValidationOutcome {
    let mut out = ValidationOutcome {
        valid: Vec::new(),
        errors: Vec::new(),
        fatal: None,
    };
    if !req.dest.is_dir() {
        out.fatal = Some(req.dest.clone());
        return out;
    }
    for src in &req.sources {
        if !src.exists() {
            out.errors.push(OpError {
                path: src.clone(),
                kind: OpErrorKind::Gone,
            });
            continue;
        }
        if paths_equal_ci(src, &req.dest) || starts_with_ci(&req.dest, src) {
            out.errors.push(OpError {
                path: src.clone(),
                kind: OpErrorKind::SelfNested,
            });
            continue;
        }
        // Moving into the directory it already lives in is a no-op, not an error.
        if req.kind == TransferKind::Move
            && src
                .parent()
                .is_some_and(|parent| paths_equal_ci(parent, &req.dest))
        {
            continue;
        }
        // Skip sources nested under another selected source: copying the parent
        // already brings them along.
        if req
            .sources
            .iter()
            .any(|other| other != src && starts_with_ci(src, other))
        {
            continue;
        }
        out.valid.push(src.clone());
    }
    out
}

// ---------------------------------------------------------------------------
// Conflict detection & pair building
// ---------------------------------------------------------------------------

/// Split sources into those whose target name already exists at `dest`
/// (conflicts, with metadata for the dialog) and the rest (clean).
pub fn detect_conflicts(
    sources: &[PathBuf],
    target_names: Option<&[OsString]>,
    dest: &Path,
) -> (Vec<ConflictItem>, Vec<(PathBuf, OsString)>) {
    let mut conflicts = Vec::new();
    let mut clean = Vec::new();
    for (index, src) in sources.iter().enumerate() {
        let target_name = target_name(src, target_names, index);
        let target = dest.join(&target_name);
        if target.exists() {
            let src_meta = src.metadata().ok();
            let dst_meta = target.metadata().ok();
            conflicts.push(ConflictItem {
                source: src.clone(),
                target_name,
                is_dir: target.is_dir(),
                src_size: src_meta
                    .as_ref()
                    .filter(|m| m.is_file())
                    .map(std::fs::Metadata::len),
                src_modified: src_meta.and_then(|m| m.modified().ok()),
                dst_size: dst_meta
                    .as_ref()
                    .filter(|m| m.is_file())
                    .map(std::fs::Metadata::len),
                dst_modified: dst_meta.and_then(|m| m.modified().ok()),
                existing: target,
            });
        } else {
            clean.push((src.clone(), target_name));
        }
    }
    (conflicts, clean)
}

/// Produce a destination path under `dest` whose name doesn't collide, by
/// inserting " (N)" before the extension: "file.txt" → "file (1).txt".
pub fn unique_target(dest: &Path, name: &OsStr) -> PathBuf {
    let candidate = dest.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let name = Path::new(name);
    let stem = name
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = name.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 1.. {
        let new_name = match &ext {
            Some(ext) => format!("{stem} ({n}).{ext}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = dest.join(new_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// One planned source→destination transfer, with the resolution to re-apply if
/// the destination (re)appears between planning and execution.
#[derive(Debug, Clone)]
pub struct TransferPair {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub on_conflict: Resolution,
}

pub struct BuildOutcome {
    pub pairs: Vec<TransferPair>,
    pub skipped: Vec<PathBuf>,
}

/// Turn clean + resolved-conflict sources into executable pairs. `policy` must
/// already be resolved (not `Ask`); `Ask` degrades to skipping the conflicts.
pub fn build_pairs(
    clean: &[(PathBuf, OsString)],
    conflicts: &[ConflictItem],
    policy: &ConflictPolicy,
    dest: &Path,
) -> BuildOutcome {
    let mut out = BuildOutcome {
        pairs: Vec::new(),
        skipped: Vec::new(),
    };
    for (src, target_name) in clean {
        out.pairs.push(TransferPair {
            dst: dest.join(target_name),
            src: src.clone(),
            // No conflict was visible when planning; if one appears in the
            // race window, renaming is the only choice that can't lose data.
            on_conflict: Resolution::Rename,
        });
    }
    for (i, item) in conflicts.iter().enumerate() {
        let resolution = match policy {
            ConflictPolicy::OverwriteAll => Resolution::Overwrite,
            ConflictPolicy::RenameAll => Resolution::Rename,
            ConflictPolicy::PerItem(choices) => choices.get(i).copied().unwrap_or(Resolution::Skip),
            ConflictPolicy::SkipAll => Resolution::Skip,
            ConflictPolicy::Ask => {
                tracing::warn!("build_pairs called with unresolved Ask policy; skipping");
                Resolution::Skip
            }
        };
        match resolution {
            Resolution::Skip => out.skipped.push(item.source.clone()),
            Resolution::Rename => out.pairs.push(TransferPair {
                dst: unique_target(dest, &item.target_name),
                src: item.source.clone(),
                on_conflict: Resolution::Rename,
            }),
            Resolution::Overwrite => out.pairs.push(TransferPair {
                dst: dest.join(&item.target_name),
                src: item.source.clone(),
                on_conflict: Resolution::Overwrite,
            }),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

struct TransferCtx {
    tx: flume::Sender<BgResult>,
    op_id: u64,
    files_done: u64,
    files_total: u64,
    bytes_done: u64,
    bytes_total: u64,
    current_file: PathBuf,
    last_progress: Instant,
    cancel: Arc<AtomicBool>,
    errors: Vec<OpError>,
    started_at: Instant,
}

impl TransferCtx {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn send_progress(&mut self, force: bool) {
        if !force && self.last_progress.elapsed() < PROGRESS_INTERVAL {
            return;
        }
        self.last_progress = Instant::now();
        let _ = self.tx.send(BgResult::OpProgress {
            id: self.op_id,
            done: self.files_done,
            total: self.files_total,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            current_file: self.current_file.clone(),
        });
    }
}

/// File count and total size under `paths` (blocking walk). Keeping both in the
/// same pass makes directory progress accurate without another metadata walk.
fn scan_stats(paths: &[PathBuf]) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    for p in paths {
        if p.is_dir() {
            for entry in walkdir::WalkDir::new(p).into_iter().flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_file() {
                        files += 1;
                        bytes += meta.len();
                    }
                }
            }
        } else if let Ok(meta) = p.metadata() {
            if meta.is_file() {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    (files, bytes)
}

/// Free space on the volume containing `dest`, if it can be determined.
fn available_space(dest: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        return atlas_win_space::available_space(dest);
    }
    #[cfg(not(windows))]
    let disks = sysinfo::Disks::new_with_refreshed_list();
    #[cfg(not(windows))]
    disks
        .iter()
        .filter(|d| starts_with_ci(dest, d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(sysinfo::Disk::available_space)
}

async fn trash_path(p: &Path) -> Result<(), String> {
    let p = p.to_path_buf();
    tokio::task::spawn_blocking(move || trash::delete(&p).map_err(|e| e.to_string()))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

async fn remove_path(path: &Path) -> std::io::Result<()> {
    let p = ext_path(path);
    if path.is_dir() {
        tokio::fs::remove_dir_all(&p).await
    } else {
        tokio::fs::remove_file(&p).await
    }
}

enum FileOutcome {
    Done,
    Cancelled,
}

struct ParallelTreeResult {
    errors: Vec<(PathBuf, std::io::Error)>,
    cancelled: bool,
}

/// Copy a new tree of small files with conservative bounded parallelism.
/// Existing destinations and mixed large-file trees stay on the sequential
/// path, where merge/conflict semantics and chunk-level cancellation apply.
async fn try_copy_small_tree_parallel(
    src: &Path,
    dst: &Path,
    ctx: &mut TransferCtx,
) -> Option<FileOutcome> {
    if !src.is_dir() || dst.exists() {
        return None;
    }
    let source = src.to_path_buf();
    let destination = dst.to_path_buf();
    let cancel = ctx.cancel.clone();
    let worker_cancel = cancel.clone();
    let worker_count = if source.to_string_lossy().starts_with(r"\\")
        || destination.to_string_lossy().starts_with(r"\\")
    {
        2
    } else {
        4
    };
    let (progress_tx, progress_rx) = flume::bounded(64);
    let mut worker = tokio::task::spawn_blocking(move || -> Option<ParallelTreeResult> {
        use rayon::prelude::*;
        use std::sync::atomic::AtomicU64;

        let mut directories = Vec::new();
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(&source).min_depth(1) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => return None,
            };
            let relative = entry.path().strip_prefix(&source).ok()?.to_path_buf();
            let target = destination.join(relative);
            if entry.file_type().is_dir() {
                directories.push(target);
            } else if entry.file_type().is_file() {
                let size = entry.metadata().ok()?.len();
                if size > NATIVE_SMALL_FILE_MAX {
                    return None;
                }
                files.push((entry.path().to_path_buf(), target, size));
            }
        }
        if files.len() < 32 {
            return None;
        }
        if std::fs::create_dir(&destination).is_err() {
            return None;
        }
        directories.sort_unstable_by_key(|path| path.components().count());
        for directory in directories {
            if let Err(error) = std::fs::create_dir(&directory) {
                return Some(ParallelTreeResult {
                    errors: vec![(directory, error)],
                    cancelled: false,
                });
            }
        }

        let bytes_done = AtomicU64::new(0);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(crate::resources::scaled(worker_count))
            .thread_name(|index| format!("atlas-copy-{index}"))
            .start_handler(|_| crate::resources::lower_current_thread())
            .build()
            .ok()?;
        let outcomes = pool.install(|| {
            files
                .par_iter()
                .filter_map(|(source, target, size)| {
                    if worker_cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    match std::fs::copy(source, target) {
                        Ok(_) => {
                            let done = bytes_done.fetch_add(*size, Ordering::Relaxed) + *size;
                            let _ = progress_tx.try_send((done, source.clone()));
                            None
                        }
                        Err(error) => Some((source.clone(), error)),
                    }
                })
                .collect()
        });
        Some(ParallelTreeResult {
            errors: outcomes,
            cancelled: worker_cancel.load(Ordering::Relaxed),
        })
    });

    let base_bytes = ctx.bytes_done;
    let result = loop {
        tokio::select! {
            result = &mut worker => break result.ok().flatten(),
            update = progress_rx.recv_async() => {
                if let Ok((done, path)) = update {
                    ctx.bytes_done = base_bytes.saturating_add(done);
                    ctx.current_file = path;
                    ctx.send_progress(false);
                }
            }
        }
    }?;
    for (path, error) in result.errors {
        ctx.errors.push(op_error(&path, &error));
    }
    if result.cancelled || cancel.load(Ordering::Relaxed) {
        Some(FileOutcome::Cancelled)
    } else {
        Some(FileOutcome::Done)
    }
}

/// Chunked single-file copy: byte-level progress + cancellation. A cancelled
/// copy removes its partial destination.
async fn copy_file_chunked(
    src: &Path,
    dst: &Path,
    ctx: &mut TransferCtx,
) -> std::io::Result<FileOutcome> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let meta = tokio::fs::metadata(ext_path(src)).await?;
    let source_len = meta.len();
    #[cfg(unix)]
    let permissions = meta.permissions();
    if source_len <= NATIVE_SMALL_FILE_MAX {
        if ctx.cancelled() {
            return Ok(FileOutcome::Cancelled);
        }
        let source = ext_path(src);
        let target = ext_path(dst);
        let result = tokio::task::spawn_blocking(move || std::fs::copy(source, target))
            .await
            .map_err(std::io::Error::other)?;
        match result {
            Ok(copied) => {
                #[cfg(unix)]
                let _ = tokio::fs::set_permissions(dst, permissions.clone()).await;
                ctx.bytes_done += copied;
                ctx.send_progress(false);
                return Ok(FileOutcome::Done);
            }
            Err(error) => {
                let _ = tokio::fs::remove_file(ext_path(dst)).await;
                return Err(error);
            }
        }
    }
    #[cfg(windows)]
    {
        let source = src.to_path_buf();
        let target = dst.to_path_buf();
        let cancel = ctx.cancel.clone();
        let worker_cancel = cancel.clone();
        let (progress_tx, progress_rx) = flume::bounded(8);
        let mut native = tokio::task::spawn_blocking(move || {
            if !worker_cancel.load(Ordering::Relaxed)
                && atlas_win_space::try_block_clone(&source, &target).unwrap_or(false)
            {
                let _ = progress_tx.try_send((source_len, source_len));
                return Ok(source_len);
            }
            atlas_win_space::copy_file2(&source, &target, &worker_cancel, |done, total| {
                let _ = progress_tx.try_send((done, total));
            })
        });
        let base_bytes = ctx.bytes_done;
        let native_result = loop {
            tokio::select! {
                result = &mut native => break result.map_err(std::io::Error::other)?,
                update = progress_rx.recv_async() => {
                    if let Ok((done, _total)) = update {
                        ctx.bytes_done = base_bytes.saturating_add(done.min(source_len));
                        ctx.send_progress(false);
                    }
                }
            }
        };
        match native_result {
            Ok(copied) => {
                ctx.bytes_done = base_bytes.saturating_add(copied);
                ctx.send_progress(false);
                return Ok(FileOutcome::Done);
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::Interrupted
                    || cancel.load(Ordering::Relaxed) =>
            {
                let _ = tokio::fs::remove_file(ext_path(dst)).await;
                return Ok(FileOutcome::Cancelled);
            }
            Err(error) => {
                if !crate::vault::live::is_private(src) && !crate::vault::live::is_private(dst) {
                    tracing::debug!(?error, path = %src.display(), "CopyFile2 unavailable; using chunked fallback");
                }
                ctx.bytes_done = base_bytes;
                let _ = tokio::fs::remove_file(ext_path(dst)).await;
            }
        }
    }
    let mut reader = tokio::fs::File::open(ext_path(src)).await?;
    let mut writer = tokio::fs::File::create(ext_path(dst)).await?;
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        if ctx.cancelled() {
            drop(writer);
            let _ = tokio::fs::remove_file(ext_path(dst)).await;
            return Ok(FileOutcome::Cancelled);
        }
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        if let Err(e) = writer.write_all(&buf[..n]).await {
            // Don't leave a truncated file behind on write failure either.
            drop(writer);
            let _ = tokio::fs::remove_file(ext_path(dst)).await;
            return Err(e);
        }
        ctx.bytes_done += n as u64;
        ctx.send_progress(false);
    }
    writer.flush().await?;
    #[cfg(unix)]
    let _ = tokio::fs::set_permissions(dst, permissions).await;
    Ok(FileOutcome::Done)
}

/// Recursive copy that accumulates per-file errors instead of aborting the
/// subtree. Files that already exist at the destination (mid-merge conflicts)
/// are sent to the trash before being replaced.
async fn copy_recursive(src: &Path, dst: &Path, ctx: &mut TransferCtx) -> FileOutcome {
    if ctx.cancelled() {
        return FileOutcome::Cancelled;
    }
    if let Some(outcome) = try_copy_small_tree_parallel(src, dst, ctx).await {
        return outcome;
    }
    if src.is_dir() {
        if dst.exists() && !dst.is_dir() {
            // Directory replacing a file: recycle the file first.
            if let Err(e) = trash_path(dst).await {
                ctx.errors.push(OpError {
                    path: dst.to_path_buf(),
                    kind: OpErrorKind::ReplaceFailed(e.to_string()),
                });
                return FileOutcome::Done;
            }
        }
        if let Err(e) = tokio::fs::create_dir_all(ext_path(dst)).await {
            ctx.errors.push(op_error(dst, &e));
            return FileOutcome::Done;
        }
        let mut rd = match tokio::fs::read_dir(ext_path(src)).await {
            Ok(rd) => rd,
            Err(e) => {
                ctx.errors.push(op_error(src, &e));
                return FileOutcome::Done;
            }
        };
        loop {
            match rd.next_entry().await {
                Ok(Some(entry)) => {
                    let child_src = src.join(entry.file_name());
                    let child_dst = dst.join(entry.file_name());
                    let outcome = Box::pin(copy_recursive(&child_src, &child_dst, &mut *ctx)).await;
                    if matches!(outcome, FileOutcome::Cancelled) {
                        return FileOutcome::Cancelled;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    ctx.errors.push(op_error(src, &e));
                    break;
                }
            }
        }
        FileOutcome::Done
    } else {
        if dst.exists() {
            if paths_equal_ci(src, dst) {
                ctx.errors.push(OpError {
                    path: src.to_path_buf(),
                    kind: OpErrorKind::SelfOverwrite,
                });
                return FileOutcome::Done;
            }
            // Overwrite = recycle the old file first, so the user can recover it.
            if let Err(e) = trash_path(dst).await {
                ctx.errors.push(OpError {
                    path: dst.to_path_buf(),
                    kind: OpErrorKind::ReplaceFailed(e.to_string()),
                });
                return FileOutcome::Done;
            }
        }
        ctx.current_file = src.to_path_buf();
        match copy_file_chunked(src, dst, ctx).await {
            Ok(outcome) => outcome,
            Err(e) => {
                ctx.errors.push(op_error(src, &e));
                FileOutcome::Done
            }
        }
    }
}

/// Execute planned pairs and report a [`TransferReport`] via
/// [`BgResult::TransferComplete`]. Never panics, never swallows an error.
#[allow(clippy::too_many_arguments)] // reason: one self-contained op descriptor per arg
pub async fn execute(
    pairs: Vec<TransferPair>,
    kind: TransferKind,
    origin: TransferOrigin,
    op_id: u64,
    tx: flume::Sender<BgResult>,
    cancel: Arc<AtomicBool>,
    affected_tabs: Vec<TabId>,
) {
    let mut report = TransferReport::default();

    // Byte totals drive both progress and the disk-space pre-check. Only
    // sources that physically copy consume space: everything for Copy, only
    // cross-volume sources for Move.
    let scan_sources: Vec<PathBuf> = pairs
        .iter()
        .filter(|p| kind == TransferKind::Copy || !same_volume(&p.src, &p.dst))
        .map(|p| p.src.clone())
        .collect();
    let (files_total, needed) = {
        let scan = scan_sources.clone();
        tokio::task::spawn_blocking(move || scan_stats(&scan))
            .await
            .unwrap_or((pairs.len() as u64, 0))
    };
    if needed > 0 {
        if let Some(free) = pairs
            .first()
            .map(|p| &p.dst)
            .and_then(|d| available_space(d))
        {
            if needed > free {
                report.errors.push(OpError {
                    path: pairs.first().map(|p| p.dst.clone()).unwrap_or_default(),
                    kind: OpErrorKind::NotEnoughSpace {
                        needed: crate::util::display::format_size(needed),
                        free: crate::util::display::format_size(free),
                    },
                });
                let _ = tx.send(BgResult::TransferComplete {
                    id: op_id,
                    kind,
                    origin,
                    report,
                    affected_tabs,
                });
                return;
            }
        }
    }

    let mut ctx = TransferCtx {
        tx: tx.clone(),
        op_id,
        files_done: 0,
        files_total: files_total.max(pairs.len() as u64),
        bytes_done: 0,
        bytes_total: needed,
        current_file: PathBuf::new(),
        // First sends are forced (`send_progress(true)`), so starting "now" is fine.
        last_progress: Instant::now(),
        cancel,
        errors: Vec::new(),
        started_at: Instant::now(),
    };

    let mut repeated: (OpErrorKind, usize) = (OpErrorKind::Msg(String::new()), 0);

    'pairs: for pair in &pairs {
        if ctx.cancelled() {
            report.cancelled = true;
            break;
        }
        let src = &pair.src;
        let mut dst = pair.dst.clone();

        if !src.exists() {
            report.errors.push(OpError {
                path: src.clone(),
                kind: OpErrorKind::Gone,
            });
            continue;
        }

        // Re-resolve a conflict that (re)appeared between planning and now.
        let mut merge_dirs = false;
        if dst.exists() && !paths_equal_ci(src, &dst) {
            match pair.on_conflict {
                Resolution::Skip => {
                    report.skipped.push(src.clone());
                    continue;
                }
                Resolution::Rename => {
                    if let Some(parent) = dst.parent() {
                        dst = unique_target(parent, dst.file_name().unwrap_or_default());
                    }
                }
                Resolution::Overwrite => {
                    if src.is_dir() && dst.is_dir() {
                        merge_dirs = true; // combine folders; per-file conflicts recycle inside
                    } else if let Err(e) = trash_path(&dst).await {
                        report.errors.push(OpError {
                            path: dst.clone(),
                            kind: OpErrorKind::ReplaceFailed(e.to_string()),
                        });
                        continue;
                    }
                }
            }
        } else if dst.exists() && paths_equal_ci(src, &dst) {
            // Same physical file: a copy auto-renames; a move is a no-op.
            match kind {
                TransferKind::Copy => {
                    if let Some(parent) = dst.parent() {
                        dst = unique_target(parent, dst.file_name().unwrap_or_default());
                    }
                }
                TransferKind::Move => continue,
            }
        }

        ctx.current_file = src.clone();
        ctx.send_progress(true);
        let errors_before = ctx.errors.len();

        match kind {
            TransferKind::Move if !merge_dirs => {
                // Atomic rename first; fall back to copy+delete (cross-volume).
                match tokio::fs::rename(ext_path(src), ext_path(&dst)).await {
                    Ok(()) => report.completed.push((src.clone(), dst.clone())),
                    Err(rename_err) => {
                        if same_volume(src, &dst) && !src.is_dir() {
                            // Same volume + plain file: the rename itself failed
                            // (locked, permissions…) — copying would just hide it.
                            ctx.errors.push(op_error(src, &rename_err));
                        } else {
                            let outcome = copy_recursive(src, &dst, &mut ctx).await;
                            if matches!(outcome, FileOutcome::Cancelled) {
                                report.cancelled = true;
                                break 'pairs;
                            }
                            if ctx.errors.len() == errors_before {
                                // Source is only removed after a *complete* copy;
                                // a partial move must never lose the original.
                                match remove_path(src).await {
                                    Ok(()) => report.completed.push((src.clone(), dst.clone())),
                                    Err(e) => ctx.errors.push(OpError {
                                        path: src.clone(),
                                        kind: OpErrorKind::SourceNotRemoved(e.to_string()),
                                    }),
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                // Copy, or a folder-merge move (copy then delete source).
                let outcome = copy_recursive(src, &dst, &mut ctx).await;
                if matches!(outcome, FileOutcome::Cancelled) {
                    report.cancelled = true;
                    break 'pairs;
                }
                if ctx.errors.len() == errors_before {
                    if kind == TransferKind::Move {
                        match remove_path(src).await {
                            Ok(()) => report.completed.push((src.clone(), dst.clone())),
                            Err(e) => ctx.errors.push(OpError {
                                path: src.clone(),
                                kind: OpErrorKind::SourceNotRemoved(e.to_string()),
                            }),
                        }
                    } else {
                        report.completed.push((src.clone(), dst.clone()));
                    }
                }
            }
        }

        // Anti-spam: a dead destination fails every remaining item the same way.
        if let Some(last) = ctx.errors.last() {
            if ctx.errors.len() > errors_before {
                if last.kind == repeated.0 {
                    repeated.1 += 1;
                    if repeated.1 >= MAX_REPEATED_ERRORS {
                        report.errors.append(&mut ctx.errors);
                        report.errors.push(OpError {
                            path: dst.clone(),
                            kind: OpErrorKind::RepeatedAbort,
                        });
                        break 'pairs;
                    }
                } else {
                    repeated = (last.kind.clone(), 1);
                }
            }
        }

        ctx.files_done += 1;
        ctx.send_progress(true);
    }

    report.errors.append(&mut ctx.errors);
    let elapsed = ctx.started_at.elapsed();
    let bytes_per_second = if elapsed.is_zero() {
        0
    } else {
        ctx.bytes_done
            .saturating_mul(1_000)
            .checked_div(elapsed.as_millis().max(1) as u64)
            .unwrap_or(0)
    };
    tracing::info!(
        op_id,
        kind = ?kind,
        files_done = ctx.files_done,
        files_total = ctx.files_total,
        bytes_done = ctx.bytes_done,
        bytes_total = ctx.bytes_total,
        elapsed_ms = elapsed.as_millis(),
        bytes_per_second,
        errors = report.errors.len(),
        cancelled = report.cancelled,
        "transfer benchmark"
    );
    let _ = tx.send(BgResult::TransferComplete {
        id: op_id,
        kind,
        origin,
        report,
        affected_tabs,
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_model::ops::TransferOrigin;
    use std::fs;

    fn req(sources: Vec<PathBuf>, dest: &Path, kind: TransferKind) -> TransferRequest {
        TransferRequest {
            sources,
            target_names: None,
            dest: dest.to_path_buf(),
            kind,
            policy: ConflictPolicy::Ask,
            origin: TransferOrigin::Menu,
        }
    }

    fn write(p: &Path, content: &str) {
        fs::write(p, content).unwrap();
    }

    async fn run(
        pairs: Vec<TransferPair>,
        kind: TransferKind,
        cancel: Arc<AtomicBool>,
    ) -> TransferReport {
        let (tx, rx) = flume::unbounded();
        execute(pairs, kind, TransferOrigin::Menu, 1, tx, cancel, Vec::new()).await;
        loop {
            match rx.recv().expect("channel closed without TransferComplete") {
                BgResult::TransferComplete { report, .. } => return report,
                _ => continue,
            }
        }
    }

    fn not_cancelled() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn validate_reports_stale_source() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("gone.txt");
        let out = validate(&req(vec![gone.clone()], tmp.path(), TransferKind::Copy));
        assert!(out.fatal.is_none());
        assert!(out.valid.is_empty());
        assert_eq!(out.errors.len(), 1);
        assert_eq!(out.errors[0].path, gone);
        assert_eq!(out.errors[0].kind, OpErrorKind::Gone);
    }

    #[test]
    fn validate_rejects_missing_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.txt");
        write(&src, "x");
        let out = validate(&req(
            vec![src],
            &tmp.path().join("nope"),
            TransferKind::Copy,
        ));
        assert!(out.fatal.is_some());
    }

    #[test]
    fn validate_rejects_dest_inside_src() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("dir");
        let inner = dir.join("inner");
        fs::create_dir_all(&inner).unwrap();
        let out = validate(&req(vec![dir.clone()], &inner, TransferKind::Move));
        assert!(out.valid.is_empty());
        assert_eq!(out.errors.len(), 1);
        assert_eq!(out.errors[0].kind, OpErrorKind::SelfNested);
    }

    #[test]
    fn validate_move_same_parent_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.txt");
        write(&src, "x");
        let out = validate(&req(vec![src], tmp.path(), TransferKind::Move));
        assert!(out.valid.is_empty());
        assert!(out.errors.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn validate_move_same_parent_case_insensitive() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.txt");
        write(&src, "x");
        let upper: PathBuf = PathBuf::from(tmp.path().to_string_lossy().to_uppercase());
        let out = validate(&req(vec![src], &upper, TransferKind::Move));
        assert!(
            out.valid.is_empty(),
            "case-only dest change must be a no-op"
        );
        assert!(out.errors.is_empty());
    }

    #[test]
    fn validate_drops_nested_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("dir");
        fs::create_dir(&dir).unwrap();
        let child = dir.join("c.txt");
        write(&child, "x");
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let out = validate(&req(vec![dir.clone(), child], &dest, TransferKind::Copy));
        assert_eq!(out.valid, vec![dir]);
    }

    #[test]
    fn detect_conflicts_splits() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let a = tmp.path().join("a.txt");
        let b = tmp.path().join("b.txt");
        write(&a, "1");
        write(&b, "2");
        write(&dest.join("a.txt"), "old");
        let (conflicts, clean) = detect_conflicts(&[a.clone(), b.clone()], None, &dest);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].source, a);
        assert_eq!(conflicts[0].existing, dest.join("a.txt"));
        assert_eq!(clean, vec![(b, OsString::from("b.txt"))]);
    }

    #[test]
    fn detect_conflicts_uses_target_names() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let src = tmp.path().join("$Rabc123");
        write(&src, "1");
        let names = vec![OsString::from("foto.png")];
        let (conflicts, clean) = detect_conflicts(&[src.clone()], Some(&names), &dest);
        assert!(conflicts.is_empty());
        let out = build_pairs(&clean, &conflicts, &ConflictPolicy::Ask, &dest);
        assert_eq!(out.pairs[0].src, src);
        assert_eq!(out.pairs[0].dst, dest.join("foto.png"));
    }

    #[test]
    fn unique_target_appends_counter() {
        let tmp = tempfile::tempdir().unwrap();
        write(&tmp.path().join("f.txt"), "0");
        write(&tmp.path().join("f (1).txt"), "1");
        let t = unique_target(tmp.path(), OsStr::new("f.txt"));
        assert_eq!(t, tmp.path().join("f (2).txt"));
    }

    #[test]
    fn build_pairs_per_item() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        for n in ["x.txt", "y.txt"] {
            write(&tmp.path().join(n), "src");
            write(&dest.join(n), "old");
        }
        let (conflicts, clean) = detect_conflicts(
            &[tmp.path().join("x.txt"), tmp.path().join("y.txt")],
            None,
            &dest,
        );
        assert!(clean.is_empty());
        let policy = ConflictPolicy::PerItem(vec![Resolution::Skip, Resolution::Rename]);
        let out = build_pairs(&clean, &conflicts, &policy, &dest);
        assert_eq!(out.skipped, vec![tmp.path().join("x.txt")]);
        assert_eq!(out.pairs.len(), 1);
        assert_eq!(out.pairs[0].dst, dest.join("y (1).txt"));
    }

    #[tokio::test]
    async fn copy_clean_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let src = tmp.path().join("a.txt");
        write(&src, "hello");
        let pairs = vec![TransferPair {
            src: src.clone(),
            dst: dest.join("a.txt"),
            on_conflict: Resolution::Rename,
        }];
        let report = run(pairs, TransferKind::Copy, not_cancelled()).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.completed, vec![(src, dest.join("a.txt"))]);
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "hello");
    }

    #[tokio::test]
    async fn native_small_file_path_copies_exact_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("small.bin");
        let dst = tmp.path().join("small-copy.bin");
        let bytes = vec![0x5A; 64 * 1024];
        fs::write(&src, &bytes).unwrap();

        let report = run(
            vec![TransferPair {
                src: src.clone(),
                dst: dst.clone(),
                on_conflict: Resolution::Rename,
            }],
            TransferKind::Copy,
            not_cancelled(),
        )
        .await;

        assert!(report.errors.is_empty());
        assert_eq!(fs::read(dst).unwrap(), bytes);
    }

    #[tokio::test]
    async fn parallel_small_tree_copies_every_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("parallel-source");
        let dst = tmp.path().join("parallel-copy");
        fs::create_dir(&src).unwrap();
        for index in 0..64 {
            let directory = src.join(format!("group-{}", index % 4));
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join(format!("{index:03}.bin")),
                vec![index as u8; 4096],
            )
            .unwrap();
        }

        let report = run(
            vec![TransferPair {
                src: src.clone(),
                dst: dst.clone(),
                on_conflict: Resolution::Rename,
            }],
            TransferKind::Copy,
            not_cancelled(),
        )
        .await;

        assert!(report.errors.is_empty());
        for index in 0..64 {
            let copied = dst
                .join(format!("group-{}", index % 4))
                .join(format!("{index:03}.bin"));
            assert_eq!(fs::read(copied).unwrap(), vec![index as u8; 4096]);
        }
    }

    #[tokio::test]
    async fn copy_race_conflict_renames_instead_of_overwriting() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let src = tmp.path().join("a.txt");
        write(&src, "new");
        // Conflict appears *after* planning (pair built as clean).
        write(&dest.join("a.txt"), "old");
        let pairs = vec![TransferPair {
            src: src.clone(),
            dst: dest.join("a.txt"),
            on_conflict: Resolution::Rename,
        }];
        let report = run(pairs, TransferKind::Copy, not_cancelled()).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "old");
        assert_eq!(fs::read_to_string(dest.join("a (1).txt")).unwrap(), "new");
    }

    #[tokio::test]
    async fn move_renames_and_reports_real_pairs() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let src = tmp.path().join("a.txt");
        write(&src, "new");
        write(&dest.join("a.txt"), "old");
        let (conflicts, clean) = detect_conflicts(&[src.clone()], None, &dest);
        let out = build_pairs(&clean, &conflicts, &ConflictPolicy::RenameAll, &dest);
        let report = run(out.pairs, TransferKind::Move, not_cancelled()).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            report.completed,
            vec![(src.clone(), dest.join("a (1).txt"))]
        );
        assert!(!src.exists(), "move must remove the source");
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "old");
    }

    #[tokio::test]
    async fn move_overwrite_recycles_old_dest() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let src = tmp.path().join("a.txt");
        write(&src, "new");
        write(&dest.join("a.txt"), "old");
        let (conflicts, clean) = detect_conflicts(&[src.clone()], None, &dest);
        let out = build_pairs(&clean, &conflicts, &ConflictPolicy::OverwriteAll, &dest);
        let report = run(out.pairs, TransferKind::Move, not_cancelled()).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(fs::read_to_string(dest.join("a.txt")).unwrap(), "new");
        assert!(!src.exists());
    }

    #[tokio::test]
    async fn merge_directories_on_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        let src_dir = tmp.path().join("dir");
        fs::create_dir_all(dest.join("dir")).unwrap();
        fs::create_dir(&src_dir).unwrap();
        write(&src_dir.join("new.txt"), "n");
        write(&dest.join("dir").join("keep.txt"), "k");
        let (conflicts, clean) = detect_conflicts(&[src_dir.clone()], None, &dest);
        let out = build_pairs(&clean, &conflicts, &ConflictPolicy::OverwriteAll, &dest);
        let report = run(out.pairs, TransferKind::Copy, not_cancelled()).await;
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(
            fs::read_to_string(dest.join("dir").join("keep.txt")).unwrap(),
            "k",
            "merge must keep unrelated existing files"
        );
        assert_eq!(
            fs::read_to_string(dest.join("dir").join("new.txt")).unwrap(),
            "n"
        );
    }

    #[tokio::test]
    async fn stale_source_at_execute_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let gone = tmp.path().join("gone.txt");
        let pairs = vec![TransferPair {
            src: gone.clone(),
            dst: dest.join("gone.txt"),
            on_conflict: Resolution::Rename,
        }];
        let report = run(pairs, TransferKind::Copy, not_cancelled()).await;
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].path, gone);
    }

    #[tokio::test]
    async fn cancel_before_start_copies_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let src = tmp.path().join("a.txt");
        write(&src, "x");
        let pairs = vec![TransferPair {
            src,
            dst: dest.join("a.txt"),
            on_conflict: Resolution::Rename,
        }];
        let report = run(pairs, TransferKind::Copy, Arc::new(AtomicBool::new(true))).await;
        assert!(report.cancelled);
        assert!(report.completed.is_empty());
        assert!(!dest.join("a.txt").exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn locked_source_reports_in_use() {
        use std::os::windows::fs::OpenOptionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let src = tmp.path().join("locked.txt");
        write(&src, "x");
        // Exclusive handle: share_mode 0 denies all other access.
        let _lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&src)
            .unwrap();
        let pairs = vec![TransferPair {
            src: src.clone(),
            dst: dest.join("locked.txt"),
            on_conflict: Resolution::Rename,
        }];
        let report = run(pairs, TransferKind::Copy, not_cancelled()).await;
        assert_eq!(report.errors.len(), 1);
        assert!(
            matches!(report.errors[0].kind, OpErrorKind::InUse),
            "got: {:?}",
            report.errors[0].kind
        );
    }

    #[tokio::test]
    async fn move_partial_copy_keeps_source() {
        // A directory move where one child fails to copy must NOT delete the
        // source directory.
        let tmp = tempfile::tempdir().unwrap();
        let dest = tmp.path().join("dest");
        fs::create_dir(&dest).unwrap();
        let src_dir = tmp.path().join("dir");
        fs::create_dir(&src_dir).unwrap();
        write(&src_dir.join("ok.txt"), "ok");
        #[cfg(windows)]
        let _lock = {
            use std::os::windows::fs::OpenOptionsExt;
            write(&src_dir.join("locked.txt"), "x");
            fs::OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(src_dir.join("locked.txt"))
                .unwrap()
        };
        #[cfg(not(windows))]
        {
            // No portable way to force a per-file copy failure; skip.
            return;
        }
        #[cfg(windows)]
        {
            let pairs = vec![TransferPair {
                src: src_dir.clone(),
                dst: dest.join("dir"),
                on_conflict: Resolution::Rename,
            }];
            let report = run(pairs, TransferKind::Move, not_cancelled()).await;
            assert!(!report.errors.is_empty());
            assert!(src_dir.exists(), "partial move must keep the source");
            assert!(report.completed.is_empty());
        }
    }

    #[test]
    #[cfg(windows)]
    fn filename_validation() {
        assert!(validate_filename(".").is_err());
        assert!(validate_filename("..").is_err());
        assert!(validate_filename("normal.txt").is_ok());
        assert!(validate_filename("").is_err());
        assert!(validate_filename("   ").is_err());
        assert!(validate_filename("a:b.txt").is_err());
        assert!(validate_filename("que?.txt").is_err());
        assert!(validate_filename("CON").is_err());
        assert!(validate_filename("con.txt").is_err());
        assert!(validate_filename("lpt3.log").is_err());
        assert!(validate_filename("dot.").is_err());
        assert!(validate_filename("space ").is_err());
        assert!(validate_filename("ok (1).txt").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn filename_validation_unix() {
        assert!(validate_filename(".").is_err());
        assert!(validate_filename("..").is_err());
        assert!(validate_filename("literal\\name.txt").is_ok());
        assert!(validate_filename("normal.txt").is_ok());
        assert!(validate_filename("").is_err());
        assert!(validate_filename("a:b.txt").is_ok());
        assert!(validate_filename("que?.txt").is_ok());
        assert!(validate_filename("*star*").is_ok());
        assert!(validate_filename("CON").is_ok());
        assert!(validate_filename("con.txt").is_ok());
        assert!(validate_filename("lpt3.log").is_ok());
        assert!(validate_filename("dot.").is_ok());
        assert!(validate_filename("space ").is_ok());
        assert!(validate_filename("ok (1).txt").is_ok());
        assert!(validate_filename("bad/name").is_err());
        assert!(validate_filename("bad\0name").is_err());
    }

    #[test]
    #[cfg(unix)]
    fn unix_io_error_kind_errno_mappings() {
        assert_eq!(
            io_error_kind(&std::io::Error::from_raw_os_error(5)),
            OpErrorKind::Msg(std::io::Error::from_raw_os_error(5).to_string())
        );
        assert_eq!(
            io_error_kind(&std::io::Error::from_raw_os_error(13)),
            OpErrorKind::PermissionDenied
        );
        assert_eq!(
            io_error_kind(&std::io::Error::from_raw_os_error(17)),
            OpErrorKind::AlreadyExists
        );
        assert_eq!(
            io_error_kind(&std::io::Error::from_raw_os_error(28)),
            OpErrorKind::DiskFull
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn copy_preserves_executable_permissions_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("script.sh");
        let dst = tmp.path().join("script_copy.sh");
        write(&src, "#!/bin/sh\necho hello\n");
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&src, perms).unwrap();

        let pairs = vec![TransferPair {
            src: src.clone(),
            dst: dst.clone(),
            on_conflict: Resolution::Rename,
        }];
        let report = run(pairs, TransferKind::Copy, not_cancelled()).await;
        assert!(
            report.errors.is_empty(),
            "copy had errors: {:?}",
            report.errors
        );
        assert!(dst.exists());
        let dst_mode = std::fs::metadata(&dst).unwrap().permissions().mode();
        assert_eq!(
            dst_mode & 0o111,
            0o111,
            "execute bits preserved: {dst_mode:o}"
        );
    }
}
