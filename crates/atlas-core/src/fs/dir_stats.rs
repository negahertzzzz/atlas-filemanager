//! In-memory file-size ledger. Watcher deltas adjust every cached ancestor;
//! ordinary writes, removals and renames never trigger recursive rescans.
use atlas_model::app_state::FsChange;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default)]
pub struct DirtyDirStats {
    root: PathBuf,
    files: HashMap<PathBuf, u64>,
    totals: HashMap<PathBuf, u128>,
    pub dirty: HashSet<PathBuf>,
}

impl DirtyDirStats {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            ..Self::default()
        }
    }
    pub fn total(&self, path: &Path) -> u128 {
        self.totals
            .get(path)
            .copied()
            .unwrap_or_else(|| u128::from(self.files.get(path).copied().unwrap_or(0)))
    }
    pub fn file_count(&self) -> usize {
        self.files.len()
    }
    pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.keys()
    }
    pub fn record_file(&mut self, path: PathBuf, size: u64) {
        if !path.starts_with(&self.root) {
            return;
        }
        let old = self.files.insert(path.clone(), size).unwrap_or(0);
        for ancestor in path
            .ancestors()
            .skip(1)
            .take_while(|p| p.starts_with(&self.root))
        {
            let total = self.totals.entry(ancestor.to_path_buf()).or_default();
            *total = total.saturating_sub(u128::from(old)) + u128::from(size);
        }
    }
    pub fn remove(&mut self, path: &Path) {
        if !path.starts_with(&self.root) {
            return;
        }
        if let Some(size) = self.files.remove(path) {
            for ancestor in path
                .ancestors()
                .skip(1)
                .take_while(|p| p.starts_with(&self.root))
            {
                if let Some(total) = self.totals.get_mut(ancestor) {
                    *total = total.saturating_sub(u128::from(size));
                }
            }
            return;
        }
        let removed: Vec<_> = self
            .files
            .keys()
            .filter(|file| file.starts_with(path))
            .cloned()
            .collect();
        for file in removed {
            let size = self.files.remove(&file).unwrap_or(0);
            for ancestor in file
                .ancestors()
                .skip(1)
                .take_while(|p| p.starts_with(&self.root))
            {
                if let Some(total) = self.totals.get_mut(ancestor) {
                    *total = total.saturating_sub(u128::from(size));
                }
            }
        }
        self.totals.retain(|dir, _| !dir.starts_with(path));
        self.dirty.retain(|dir| !dir.starts_with(path));
    }
    pub fn apply(&mut self, changes: &[FsChange]) {
        for change in changes {
            match change {
                FsChange::Remove(path) => self.remove(path),
                FsChange::Upsert(entry) => {
                    if entry.is_cloud_placeholder() || entry.is_symlink() {
                        self.remove(&entry.path());
                    } else if matches!(entry.kind(), atlas_model::file_entry::FileKind::Dir) {
                        if entry.path().starts_with(&self.root)
                            && !self.totals.contains_key(&entry.path())
                        {
                            self.dirty.insert(entry.path());
                        }
                    } else if let Some(size) = entry.size() {
                        self.record_file(entry.path(), size);
                    }
                }
                FsChange::Rename { from, entry } => {
                    let new = entry.path();
                    if entry.is_cloud_placeholder() || entry.is_symlink() {
                        self.remove(from);
                        self.remove(&new);
                        continue;
                    }
                    if !matches!(entry.kind(), atlas_model::file_entry::FileKind::Dir) {
                        self.remove(from);
                        if let Some(size) = entry.size() {
                            self.record_file(new, size);
                        }
                        continue;
                    }
                    let moved: Vec<_> = self
                        .files
                        .iter()
                        .filter_map(|(path, size)| {
                            path.strip_prefix(from)
                                .ok()
                                .map(|suffix| (new.join(suffix), *size))
                        })
                        .collect();
                    self.remove(from);
                    for (path, size) in moved {
                        self.record_file(path, size);
                    }
                    if new.starts_with(&self.root) && !self.totals.contains_key(&new) {
                        self.dirty.insert(new.clone());
                    }
                    if !matches!(entry.kind(), atlas_model::file_entry::FileKind::Dir)
                        && !entry.is_cloud_placeholder()
                        && !entry.is_symlink()
                    {
                        if let Some(size) = entry.size() {
                            self.record_file(new, size);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_model::file_entry::{FileEntry, FileEntryData, FileKind};
    fn file(path: &str, size: u64) -> FileEntry {
        FileEntry::from(FileEntryData {
            name: "file".into(),
            path: path.into(),
            kind: FileKind::File,
            size: Some(size),
            modified: None,
            _is_hidden: false,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        })
    }
    #[test]
    fn nested_write_remove_and_rename_adjust_ancestors_without_io() {
        let mut stats = DirtyDirStats::new(PathBuf::from("/a"));
        stats.record_file("/a/b/file".into(), 10);
        stats.record_file("/a/c/keep".into(), 7);
        stats.apply(&[FsChange::Upsert(file("/a/b/file", 30))]);
        assert_eq!(stats.total(Path::new("/a/b")), 30);
        assert_eq!(stats.total(Path::new("/a")), 37);
        stats.apply(&[FsChange::Rename {
            from: "/a/b/file".into(),
            entry: file("/a/c/file", 31),
        }]);
        assert_eq!(stats.total(Path::new("/a/b")), 0);
        assert_eq!(stats.total(Path::new("/a/c")), 38);
        stats.apply(&[FsChange::Remove("/a/c".into())]);
        assert_eq!(stats.total(Path::new("/a")), 0);
        assert_eq!(stats.file_count(), 0);
    }
    #[test]
    fn totals_preserve_full_u64_values_and_ignore_changes_outside_root() {
        let mut stats = DirtyDirStats::new(PathBuf::from("/a"));
        stats.record_file("/a/one".into(), u64::MAX);
        stats.record_file("/a/two".into(), u64::MAX);
        stats.record_file("/other/three".into(), 9);
        assert_eq!(stats.total(Path::new("/a")), u128::from(u64::MAX) * 2);
        stats.apply(&[FsChange::Remove("/a/missing".into())]);
        assert_eq!(stats.file_count(), 2);
    }
}
