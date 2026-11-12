#![allow(dead_code)] // reason: retained API surface not exercised by every GPUI flow yet
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use jwalk::{Parallelism, WalkDir};

const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(80);
const MAX_SCAN_THREADS: usize = 4;

#[derive(Clone, Debug)]
pub struct SpaceItem {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[derive(Clone, Debug)]
struct ScanSnapshot {
    scan_id: u64,
    items: Vec<SpaceItem>,
    scanned_bytes: u64,
    scanned_entries: u64,
    error_count: u64,
    finished: bool,
    stats: Option<crate::fs::dir_stats::DirtyDirStats>,
}

#[derive(Clone)]
pub struct SpaceLensState {
    watcher: Option<Arc<crate::fs::watcher::WatcherHandle>>,
    watch_rx: flume::Receiver<atlas_model::app_state::BgResult>,
    pub dir_stats: crate::fs::dir_stats::DirtyDirStats,
    pending_changes: Vec<atlas_model::app_state::FsChange>,
    pub active_path: Option<PathBuf>,
    pub items: Vec<SpaceItem>,
    pub selected_for_deletion: HashSet<PathBuf>,
    pub loading: bool,
    pub physics_nodes: HashMap<PathBuf, PhysicsNode>,
    pub bubble_pointer_down: bool,
    pub scanned_bytes: u64,
    pub scanned_entries: u64,
    pub error_count: u64,
    active_scan_id: u64,
    scan_generation: Arc<AtomicU64>,
    rx: flume::Receiver<ScanSnapshot>,
    tx: flume::Sender<ScanSnapshot>,
}

/// Physics-bubble render scratch for the (not-yet-ported) disk-usage analyzer.
/// Framework-neutral: `pos`/`vel` are `[x, y]` and `color` is `[r, g, b, a]`; the UI
/// layer maps these to its own geometry/color types when the analyzer is ported.
#[derive(Clone, Debug)]
pub struct PhysicsNode {
    pub pos: [f32; 2],
    pub vel: [f32; 2],
    pub radius: f32,
    pub target_radius: f32,
    pub color: [u8; 4],
}

impl Default for SpaceLensState {
    fn default() -> Self {
        let (tx, rx) = flume::unbounded();
        let (_, watch_rx) = flume::unbounded();
        Self {
            watcher: None,
            watch_rx,
            dir_stats: Default::default(),
            pending_changes: Vec::new(),
            active_path: None,
            items: Vec::new(),
            selected_for_deletion: HashSet::new(),
            loading: false,
            physics_nodes: HashMap::new(),
            bubble_pointer_down: false,
            scanned_bytes: 0,
            scanned_entries: 0,
            error_count: 0,
            active_scan_id: 0,
            scan_generation: Arc::new(AtomicU64::new(0)),
            rx,
            tx,
        }
    }
}

impl SpaceLensState {
    pub fn update(&mut self) {
        while let Ok(result) = self.watch_rx.try_recv() {
            match result {
                atlas_model::app_state::BgResult::FsChanges { changes, .. } => {
                    self.apply_fs_changes(&changes)
                }
                atlas_model::app_state::BgResult::FsEvent(path) => {
                    self.dir_stats.dirty.insert(path);
                }
                _ => {}
            }
        }
        while let Ok(snapshot) = self.rx.try_recv() {
            if snapshot.scan_id != self.active_scan_id {
                continue;
            }
            self.items = snapshot.items;
            self.scanned_bytes = snapshot.scanned_bytes;
            self.scanned_entries = snapshot.scanned_entries;
            self.error_count = snapshot.error_count;
            self.loading = !snapshot.finished;
            if let Some(stats) = snapshot.stats {
                self.dir_stats = stats;
                let changes = std::mem::take(&mut self.pending_changes);
                self.apply_fs_changes(&changes);
                self.refresh_incremental_totals();
            }
            self.physics_nodes
                .retain(|path, _| self.items.iter().any(|item| item.path == *path));
        }
    }

    pub fn open_for_path(&mut self, path: PathBuf) {
        self.watcher = None;
        let (watch_tx, watch_rx) = flume::unbounded();
        self.watch_rx = watch_rx;
        self.watcher = crate::fs::watcher::watch_path_recursive(&path, watch_tx)
            .ok()
            .map(Arc::new);
        self.dir_stats = crate::fs::dir_stats::DirtyDirStats::new(path.clone());
        self.pending_changes.clear();
        let scan_id = self.scan_generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.active_scan_id = scan_id;
        self.active_path = Some(path.clone());
        self.items.clear();
        self.selected_for_deletion.clear();
        self.physics_nodes.clear();
        self.bubble_pointer_down = false;
        self.loading = true;
        self.scanned_bytes = 0;
        self.scanned_entries = 0;
        self.error_count = 0;

        let tx = self.tx.clone();
        let scan_generation = self.scan_generation.clone();
        std::thread::spawn(move || scan_path(path, scan_id, scan_generation, tx));
    }

    pub fn close(&mut self) {
        self.dir_stats = Default::default();
        self.pending_changes.clear();
        self.active_scan_id = self.scan_generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.watcher = None;
        self.active_path = None;
        self.items.clear();
        self.selected_for_deletion.clear();
        self.physics_nodes.clear();
        self.bubble_pointer_down = false;
        self.loading = false;
        self.scanned_bytes = 0;
        self.scanned_entries = 0;
        self.error_count = 0;
    }

    pub fn apply_fs_changes(&mut self, changes: &[atlas_model::app_state::FsChange]) {
        if self.active_path.is_none() {
            return;
        }
        if self.loading {
            self.pending_changes.extend_from_slice(changes);
            return;
        }
        self.dir_stats.apply(changes);
        let Some(root) = &self.active_path else {
            return;
        };
        for change in changes {
            let entry = match change {
                atlas_model::app_state::FsChange::Remove(path) => {
                    self.items.retain(|item| !item.path.starts_with(path));
                    continue;
                }
                atlas_model::app_state::FsChange::Rename { from, entry } => {
                    self.items.retain(|item| !item.path.starts_with(from));
                    entry
                }
                atlas_model::app_state::FsChange::Upsert(entry) => entry,
            };
            let path = entry.path();
            let Some(child) = top_level_child(root, &path) else {
                continue;
            };
            if !self.items.iter().any(|item| item.path == child) {
                self.items.push(SpaceItem {
                    name: child
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    is_dir: child != path
                        || matches!(entry.kind(), atlas_model::file_entry::FileKind::Dir),
                    size: 0,
                    path: child,
                });
            }
        }
        self.refresh_incremental_totals();
    }

    fn refresh_incremental_totals(&mut self) {
        let Some(root) = &self.active_path else {
            return;
        };
        for item in &mut self.items {
            item.size = self.dir_stats.total(&item.path).min(u128::from(u64::MAX)) as u64;
        }
        self.scanned_bytes = self.dir_stats.total(root).min(u128::from(u64::MAX)) as u64;
        self.scanned_entries = self.dir_stats.file_count() as u64;
        self.items
            .sort_by(|a, b| b.size.cmp(&a.size).then(a.path.cmp(&b.path)));
    }
}

fn scan_path(
    root: PathBuf,
    scan_id: u64,
    scan_generation: Arc<AtomicU64>,
    tx: flume::Sender<ScanSnapshot>,
) {
    let stats = std::cell::RefCell::new(crate::fs::dir_stats::DirtyDirStats::new(root.clone()));
    #[cfg(target_os = "windows")]
    {
        let result = atlas_win_space::scan_ntfs_with_files(
            &root,
            || scan_generation.load(Ordering::Relaxed) != scan_id,
            |items, scanned_bytes, scanned_entries, error_count, finished| {
                let keyed = items
                    .into_iter()
                    .map(|item| {
                        let item = SpaceItem {
                            path: item.path,
                            name: item.name,
                            is_dir: item.is_dir,
                            size: item.size,
                        };
                        (item.path.clone(), item)
                    })
                    .collect();
                send_snapshot(
                    &tx,
                    scan_id,
                    &keyed,
                    scanned_bytes,
                    scanned_entries,
                    error_count,
                    finished,
                    finished.then(|| stats.borrow().clone()),
                )
            },
            |path| !crate::fs::scan_policy::is_scan_excluded(path),
            |path, size| stats.borrow_mut().record_file(path, size),
        );
        match result {
            Ok(()) => return,
            Err(atlas_win_space::ScanError::Cancelled) => return,
            // Reading a volume's USN journal can require privileges that normal
            // folder browsing does not. Keep the portable walker available on
            // NTFS too; a failed optimization must not turn readable folders empty.
            Err(_) => {
                *stats.borrow_mut() = crate::fs::dir_stats::DirtyDirStats::new(root.clone());
            }
        }
    }

    let threads = crate::resources::scaled(
        std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(MAX_SCAN_THREADS),
    );
    #[cfg(windows)]
    let parallelism = if atlas_win_space::storage::incurs_seek_penalty(&root) == Some(false) {
        Parallelism::RayonNewPool(threads)
    } else {
        // Rotating and unrecognized drives stay serial to avoid head thrashing.
        Parallelism::Serial
    };
    #[cfg(not(windows))]
    let parallelism = Parallelism::RayonNewPool(threads);
    let filter_root = root.clone();
    let walker = WalkDir::new(&root)
        .min_depth(1)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir(move |_, _, (), children| {
            for entry in children.iter_mut().filter_map(|entry| entry.as_mut().ok()) {
                if should_skip_descendants(&filter_root, &entry.path())
                    || crate::fs::scan_policy::is_scan_excluded(&entry.path())
                    || crate::fs::preview::is_cloud_placeholder(&entry.path())
                {
                    entry.read_children_path = None;
                }
            }
        })
        .parallelism(parallelism);

    let mut items = HashMap::<PathBuf, SpaceItem>::new();
    let mut scanned_bytes = 0_u64;
    let mut scanned_entries = 0_u64;
    let mut error_count = 0_u64;
    let mut last_snapshot = Instant::now();

    for result in walker {
        if scan_generation.load(Ordering::Relaxed) != scan_id {
            return;
        }

        let Ok(entry) = result else {
            error_count += 1;
            continue;
        };
        let path = entry.path();
        if crate::fs::scan_policy::is_scan_excluded(&path)
            || crate::fs::preview::is_cloud_placeholder(&path)
        {
            continue;
        }
        let Some(child_path) = top_level_child(&root, &path) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            error_count += 1;
            continue;
        };
        if metadata.is_symlink() {
            continue;
        }
        let size = if metadata.is_file() {
            metadata.len()
        } else {
            0
        };
        if metadata.is_file() {
            stats.borrow_mut().record_file(path.clone(), size);
        }

        scanned_bytes = scanned_bytes.saturating_add(size);
        scanned_entries += 1;
        let item = items
            .entry(child_path.clone())
            .or_insert_with(|| SpaceItem {
                name: child_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path: child_path,
                is_dir: entry.depth() == 1 && entry.file_type().is_dir(),
                size: 0,
            });
        item.size = item.size.saturating_add(size);

        if last_snapshot.elapsed() >= SNAPSHOT_INTERVAL {
            if !send_snapshot(
                &tx,
                scan_id,
                &items,
                scanned_bytes,
                scanned_entries,
                error_count,
                false,
                None,
            ) {
                return;
            }
            last_snapshot = Instant::now();
        }
    }

    if scan_generation.load(Ordering::Relaxed) == scan_id {
        let _ = send_snapshot(
            &tx,
            scan_id,
            &items,
            scanned_bytes,
            scanned_entries,
            error_count,
            true,
            Some(stats.into_inner()),
        );
    }
}

#[cfg(target_os = "windows")]
fn is_windows_volume_root(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    let bytes = s.as_bytes();
    bytes.len() == 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn send_snapshot(
    tx: &flume::Sender<ScanSnapshot>,
    scan_id: u64,
    items: &HashMap<PathBuf, SpaceItem>,
    scanned_bytes: u64,
    scanned_entries: u64,
    error_count: u64,
    finished: bool,
    stats: Option<crate::fs::dir_stats::DirtyDirStats>,
) -> bool {
    let mut items: Vec<_> = items.values().cloned().collect();
    items.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    tx.send(ScanSnapshot {
        scan_id,
        items,
        scanned_bytes,
        scanned_entries,
        error_count,
        finished,
        stats,
    })
    .is_ok()
}

fn top_level_child(root: &Path, path: &Path) -> Option<PathBuf> {
    path.strip_prefix(root)
        .ok()?
        .components()
        .next()
        .map(|component| root.join(component.as_os_str()))
}

fn should_skip_descendants(root: &Path, path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        root == Path::new("/")
            && path.parent() == Some(root)
            && matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("dev" | "proc" | "run" | "sys")
            )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (root, path);
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn recursive_watcher_updates_nested_totals_and_close_rejects_stale_snapshots() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("file.bin");
        std::fs::write(&file, [0u8; 10]).unwrap();
        let mut lens = super::SpaceLensState::default();
        lens.open_for_path(root.path().to_path_buf());
        assert!(
            lens.watcher.is_some(),
            "recursive watcher must start for the fixture"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while lens.loading && std::time::Instant::now() < deadline {
            lens.update();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!lens.loading);
        assert_eq!(lens.dir_stats.total(root.path()), 10);
        std::fs::write(&file, [0u8; 37]).unwrap();
        while lens.dir_stats.total(root.path()) != 37 && std::time::Instant::now() < deadline {
            lens.update();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(lens.dir_stats.total(&nested), 37);
        assert_eq!(lens.dir_stats.total(root.path()), 37);
        std::fs::remove_file(&file).unwrap();
        while lens.dir_stats.total(root.path()) != 0 && std::time::Instant::now() < deadline {
            lens.update();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(lens.dir_stats.total(root.path()), 0);
        let old_id = lens.active_scan_id;
        lens.close();
        lens.tx
            .send(super::ScanSnapshot {
                scan_id: old_id,
                items: vec![],
                scanned_bytes: 99,
                scanned_entries: 1,
                error_count: 0,
                finished: true,
                stats: None,
            })
            .unwrap();
        lens.update();
        assert_eq!(lens.scanned_bytes, 0);
        assert!(lens.active_path.is_none());
    }
    #[cfg(target_os = "linux")]
    use super::should_skip_descendants;
    use super::top_level_child;
    use std::path::{Path, PathBuf};

    #[test]
    fn finds_immediate_child_for_nested_paths() {
        assert_eq!(
            top_level_child(
                Path::new("/home/user"),
                Path::new("/home/user/projects/app/src")
            ),
            Some(PathBuf::from("/home/user/projects"))
        );
    }

    #[test]
    fn rejects_paths_outside_root() {
        assert_eq!(
            top_level_child(Path::new("/home/user"), Path::new("/tmp/file")),
            None
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn skips_linux_virtual_mounts_only_when_scanning_root() {
        assert!(should_skip_descendants(Path::new("/"), Path::new("/proc")));
        assert!(!should_skip_descendants(Path::new("/"), Path::new("/home")));
        assert!(!should_skip_descendants(
            Path::new("/proc"),
            Path::new("/proc/self")
        ));
    }
}
