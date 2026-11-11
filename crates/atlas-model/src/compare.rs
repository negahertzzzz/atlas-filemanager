//! Folder comparison results (two panes side by side).
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CompareStatus {
    /// Same size and modification time (within the 2 s FAT/SMB granularity).
    Same,
    /// Present on both sides with a different size or time.
    Changed,
    LeftOnly,
    RightOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareEntry {
    /// Size of a file; `None` for folders.
    pub bytes: Option<u64>,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareRow {
    /// Path relative to both compared roots.
    pub relative: PathBuf,
    pub directory: bool,
    pub status: CompareStatus,
    pub left: Option<CompareEntry>,
    pub right: Option<CompareEntry>,
    /// Side with the more recent copy of a changed file.
    pub newer: Option<CompareSide>,
}

#[derive(Clone, Debug, Default)]
pub struct CompareReport {
    pub left: PathBuf,
    pub right: PathBuf,
    /// Every difference, plus identical files (status `Same`).
    pub rows: Vec<CompareRow>,
    /// Stopped at the row limit; the listing is incomplete.
    pub truncated: bool,
    pub errors: u64,
    pub finished: bool,
    pub cancelled: bool,
}

impl CompareReport {
    pub fn count(&self, status: CompareStatus) -> usize {
        self.rows.iter().filter(|row| row.status == status).count()
    }
}
