//! Framework-neutral contracts for the disk analyzer and reviewed cleanup.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanStatus {
    #[default]
    Scanning,
    Paused,
    Complete,
    Partial,
    Cancelled,
    Failed,
}

impl ScanStatus {
    pub const fn terminal(self) -> bool {
        !matches!(self, Self::Scanning | Self::Paused)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskEntry {
    pub path: PathBuf,
    pub bytes: u64,
    pub directory: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub volume: u64,
    pub id: [u8; 16],
    pub bytes: u64,
    pub modified: u64,
    pub links: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AncestorIdentity {
    pub path: PathBuf,
    pub volume: u64,
    pub id: [u8; 16],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanupCandidate {
    pub path: PathBuf,
    pub identity: FileIdentity,
    pub ancestors: Vec<AncestorIdentity>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanupPlan {
    pub version: u32,
    pub id: String,
    pub created_unix_seconds: u64,
    pub volume_root: PathBuf,
    pub temp_root: PathBuf,
    pub candidates: Vec<CleanupCandidate>,
}

#[derive(Clone, Debug, Default)]
pub struct DiskSnapshot {
    pub generation: u64,
    pub root: PathBuf,
    pub status: ScanStatus,
    pub entries: Vec<DiskEntry>,
    pub large_files: Vec<DiskEntry>,
    pub candidates: Vec<CleanupCandidate>,
    /// Old files a category scan found. They are only inspected (opened) when the
    /// user confirms, so discovery never triggers on-access antivirus scans.
    pub reclaimable: Vec<DiskEntry>,
    pub bytes: u64,
    pub other_bytes: u64,
    pub entry_count: u64,
    pub skipped: u64,
    pub errors: u64,
    pub limited: bool,
    pub message: Option<String>,
    pub capacity: Option<(u64, u64)>,
    pub backend: ScanBackend,
    pub progress: Option<f32>,
    pub fast_scan_available: bool,
    pub backend_note: Option<String>,
}

/// One rectangle of the nested treemap: a folder (with children) or a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreemapNode {
    pub path: PathBuf,
    pub name: String,
    pub bytes: u64,
    pub directory: bool,
    /// Largest children first. Their bytes plus `other` equal `bytes`.
    pub children: Vec<TreemapNode>,
    /// Bytes of small files and of children too small to draw.
    pub other: u64,
}

/// Space used by one file type (extension) below the analysed folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeStat {
    /// Lower-case extension without the dot; empty for files without one.
    pub extension: String,
    pub bytes: u64,
    pub files: u64,
    /// True for the bucket of files too small to be indexed one by one.
    pub small_files: bool,
}

/// Files with identical content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateGroup {
    /// Size of each copy.
    pub bytes: u64,
    /// Every copy, oldest first: the first one is the natural one to keep.
    pub paths: Vec<PathBuf>,
}

impl DuplicateGroup {
    /// Space freed by keeping a single copy.
    pub const fn wasted(&self) -> u64 {
        self.bytes.saturating_mul(self.paths.len().saturating_sub(1) as u64)
    }
}

/// Progress and result of a duplicate search.
#[derive(Clone, Debug, Default)]
pub struct DuplicateReport {
    pub generation: u64,
    pub groups: Vec<DuplicateGroup>,
    /// Bytes read for hashing so far, and the most it may read.
    pub hashed_bytes: u64,
    pub total_bytes: u64,
    pub candidates: usize,
    pub finished: bool,
    pub cancelled: bool,
    pub errors: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScanBackend {
    #[default]
    NativeDirectory,
    NtfsMft,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupResult {
    pub path: PathBuf,
    pub deleted_bytes: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanupReport {
    pub plan_id: String,
    pub results: Vec<CleanupResult>,
    pub cancelled: bool,
    pub free_before: Option<u64>,
    pub free_after: Option<u64>,
    pub error: Option<String>,
}
