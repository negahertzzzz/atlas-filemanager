use super::policy;
use atlas_model::cleanup::{DiskEntry, DiskSnapshot, ScanStatus};
use std::{
    cmp::{Ordering as Cmp, Reverse},
    collections::BinaryHeap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant, SystemTime},
};

const MAX_ROWS: usize = 512;
const MAX_FILES: usize = 256;
const MAX_CANDIDATES: usize = 20_000;
const MAX_DEPTH: usize = 128;
const MAX_PATH_BYTES: usize = 16 * 1024 * 1024;
const MIN_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanMode {
    Folder,
    FastFolder,
    Temporary,
    /// Like `Temporary`, but the given root is itself a registered category root.
    Category,
}

pub struct ScanJob {
    pub receiver: flume::Receiver<DiskSnapshot>,
    cancel: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}

impl ScanJob {
    pub fn start(root: PathBuf, generation: u64, mode: ScanMode) -> Self {
        let (sender, receiver) = flume::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let job = Self {
            receiver,
            cancel,
            paused,
        };
        let worker = Worker {
            sender,
            drain: job.receiver.clone(),
            cancel: job.cancel.clone(),
            paused: job.paused.clone(),
            snapshot: DiskSnapshot {
                generation,
                root: root.clone(),
                ..Default::default()
            },
            rows: BinaryHeap::new(),
            files: BinaryHeap::new(),
            last_publish: Instant::now(),
            stored_path_bytes: 0,
            mode,
        };
        let (queue, threads, depth) = match mode {
            ScanMode::Folder | ScanMode::FastFolder => (&SCAN_QUEUE, 1, 8),
            ScanMode::Temporary => (&TEMP_QUEUE, 1, 8),
            // Category roots are many, small and independent.
            ScanMode::Category => (&CATEGORY_QUEUE, 4, 128),
        };
        let queue = queue.get_or_init(|| {
            let (sender, receiver) = flume::bounded::<Worker>(depth);
            // Shared scanners prevent rapid navigation from creating an
            // unbounded number of blocked filesystem threads.
            for _ in 0..threads {
                let receiver = receiver.clone();
                std::thread::spawn(move || {
                    while let Ok(mut worker) = receiver.recv() {
                        let root = worker.snapshot.root.clone();
                        worker.run(root);
                    }
                });
            }
            sender
        });
        if let Err(error) = queue.try_send(worker) {
            let mut worker = error.into_inner();
            worker.snapshot.status = ScanStatus::Failed;
            worker.snapshot.message = Some("cleanup_busy".into());
            worker.publish(None);
        }
        job
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
    }
}
impl Drop for ScanJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Clone)]
struct Ranked(DiskEntry);
impl PartialEq for Ranked {
    fn eq(&self, rhs: &Self) -> bool {
        self.cmp(rhs) == Cmp::Equal
    }
}
impl Eq for Ranked {}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, rhs: &Self) -> Option<Cmp> {
        Some(self.cmp(rhs))
    }
}
impl Ord for Ranked {
    fn cmp(&self, rhs: &Self) -> Cmp {
        self.0
            .bytes
            .cmp(&rhs.0.bytes)
            .then_with(|| rhs.0.path.cmp(&self.0.path))
    }
}

fn retain(heap: &mut BinaryHeap<Reverse<Ranked>>, entry: DiskEntry, limit: usize) {
    heap.push(Reverse(Ranked(entry)));
    if heap.len() > limit {
        heap.pop();
    }
}
fn sorted(heap: &BinaryHeap<Reverse<Ranked>>) -> Vec<DiskEntry> {
    let mut entries: Vec<_> = heap.iter().map(|r| r.0 .0.clone()).collect();
    entries.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    entries
}

struct Worker {
    sender: flume::Sender<DiskSnapshot>,
    drain: flume::Receiver<DiskSnapshot>,
    cancel: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    snapshot: DiskSnapshot,
    rows: BinaryHeap<Reverse<Ranked>>,
    files: BinaryHeap<Reverse<Ranked>>,
    last_publish: Instant,
    stored_path_bytes: usize,
    mode: ScanMode,
}

static SCAN_QUEUE: OnceLock<flume::Sender<Worker>> = OnceLock::new();
// A user asking to clean Temp never waits behind a whole-volume analysis.
static TEMP_QUEUE: OnceLock<flume::Sender<Worker>> = OnceLock::new();
static CATEGORY_QUEUE: OnceLock<flume::Sender<Worker>> = OnceLock::new();

impl Worker {
    fn run(&mut self, root: PathBuf) {
        if self.cancel.load(Ordering::Acquire) {
            self.finish();
            return;
        }
        #[cfg(windows)]
        {
            let network = crate::fs::network::is_network_path(&root);
            // Network shares can be analysed (plain walk), never cleaned:
            // their deletions bypass the Recycle Bin.
            let analysis = matches!(self.mode, ScanMode::Folder | ScanMode::FastFolder);
            let local = root
                .ancestors()
                .last()
                .is_some_and(atlas_win_space::cleanup::local_drive);
            if !(local || (network && analysis)) {
                self.snapshot.status = ScanStatus::Failed;
                self.snapshot.message = Some("cleanup_unavailable".into());
                self.finish();
                return;
            }
            self.snapshot.capacity = atlas_win_space::cleanup::capacity(&root);
            self.snapshot.fast_scan_available = !network
                && root
                    .ancestors()
                    .last()
                    .is_some_and(atlas_win_space::mft_space::supported);
        }
        if self.mode == ScanMode::Temporary || self.mode == ScanMode::Category {
            let temp = if self.mode == ScanMode::Category {
                Some(root.clone()).filter(|r| policy::is_cleanup_root(r))
            } else {
                policy::user_temp_root()
            };
            let Some(temp) = temp else {
                self.snapshot.status = ScanStatus::Failed;
                self.snapshot.message = Some("cleanup_unavailable".into());
                self.finish();
                return;
            };
            if !temp.starts_with(&root) {
                self.snapshot.message = Some("cleanup_no_temp_volume".into());
                self.finish();
                return;
            }
            self.snapshot.root = temp.clone();
            self.read_root(&temp);
        } else {
            if policy::protected(&root)
                || crate::vault::is_private_path(&root)
                || crate::vault::live::is_private(&root)
            {
                self.snapshot.status = ScanStatus::Failed;
                self.snapshot.message = Some("cleanup_unavailable".into());
                self.finish();
                return;
            }
            if let Some(snapshot) = super::tree::cached_snapshot(&root, self.snapshot.generation) {
                self.send_final(snapshot);
                return;
            }
            #[cfg(windows)]
            if self.snapshot.fast_scan_available
                && !crate::fs::network::is_network_path(&root)
                && self.try_mft(&root)
            {
                return;
            }
            self.walk_tree(&root);
            return;
        }
        self.finish();
    }

    #[cfg(windows)]
    fn try_mft(&mut self, root: &Path) -> bool {
        use std::cell::RefCell;
        let elevated = self.mode == ScanMode::FastFolder;
        if elevated {
            self.snapshot.backend = atlas_model::cleanup::ScanBackend::NtfsMft;
            self.snapshot.message = Some("cleanup_mft_permission".into());
            self.publish(None);
        }
        let worker = RefCell::new(&mut *self);
        let result = super::mft::scan(
            root,
            elevated,
            || worker.borrow_mut().check_control(),
            |done, total| {
                let mut worker = worker.borrow_mut();
                worker.snapshot.backend = atlas_model::cleanup::ScanBackend::NtfsMft;
                worker.snapshot.progress = Some(done as f32 / total.max(1) as f32);
                worker.snapshot.message = Some("cleanup_reading_mft".into());
                worker.maybe_publish();
            },
        );
        match result {
            Ok(tree) => {
                self.store_tree(tree, root);
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                self.finish();
                true
            }
            Err(error) => {
                self.snapshot.backend = atlas_model::cleanup::ScanBackend::NativeDirectory;
                self.snapshot.backend_note = Some(error.to_string());
                self.snapshot.message = elevated.then(|| "cleanup_fast_fallback".into());
                self.snapshot.progress = None;
                false
            }
        }
    }
    fn store_tree(&mut self, tree: super::tree::SpaceTree, root: &Path) {
        let tree = Arc::new(tree);
        match tree.snapshot(root, self.snapshot.generation) {
            Some(snapshot) => {
                super::tree::store(tree);
                self.send_final(snapshot);
            }
            None => {
                self.snapshot.status = ScanStatus::Failed;
                self.finish();
            }
        }
    }
    fn send_final(&mut self, snapshot: DiskSnapshot) {
        self.snapshot = snapshot;
        let mut value = self.snapshot.clone();
        loop {
            match self.sender.try_send(value) {
                Ok(()) | Err(flume::TrySendError::Disconnected(_)) => break,
                Err(flume::TrySendError::Full(returned)) => {
                    value = returned;
                    let _ = self.drain.try_recv();
                }
            }
        }
    }
    fn walk_tree(&mut self, root: &Path) {
        use super::walk::{walk, Control};
        use std::sync::atomic::AtomicU64;
        let control = Control {
            cancel: self.cancel.clone(),
            paused: self.paused.clone(),
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        };
        self.publish(None);
        let result = walk(root, &control, |files, bytes| {
            if self.sender.is_disconnected() {
                self.cancel.store(true, Ordering::Release);
            }
            self.snapshot.entry_count = files;
            self.snapshot.bytes = bytes;
            self.snapshot.status = if self.paused.load(Ordering::Acquire) {
                ScanStatus::Paused
            } else {
                ScanStatus::Scanning
            };
            self.publish(None);
        });
        match result {
            Ok(tree) if !self.cancel.load(Ordering::Acquire) => self.store_tree(tree, root),
            Ok(_) => self.finish(),
            Err(_) => {
                self.snapshot.errors += 1;
                self.snapshot.status = ScanStatus::Failed;
                self.finish();
            }
        }
    }
    fn finish(&mut self) {
        self.snapshot.status = if self.cancel.load(Ordering::Acquire) {
            ScanStatus::Cancelled
        } else if self.snapshot.status == ScanStatus::Failed {
            ScanStatus::Failed
        } else if self.snapshot.errors > 0 || self.snapshot.skipped > 0 || self.snapshot.limited {
            ScanStatus::Partial
        } else {
            ScanStatus::Complete
        };
        self.publish(None);
    }
    fn read_root(&mut self, root: &Path) {
        #[cfg(windows)]
        let _lease = match atlas_win_space::cleanup::pin_directory(root, true) {
            Ok(lease) => lease,
            Err(_) => {
                self.snapshot.status = ScanStatus::Failed;
                self.snapshot.errors += 1;
                self.snapshot.message = Some("cleanup_unavailable".into());
                return;
            }
        };
        let valid = fs::symlink_metadata(root).is_ok_and(|m| m.is_dir() && !unsafe_entry(&m));
        if !valid
            || policy::protected(root)
            || crate::vault::is_private_path(root)
            || crate::vault::live::is_private(root)
        {
            self.snapshot.status = ScanStatus::Failed;
            self.snapshot.message = Some("cleanup_unavailable".into());
            return;
        }
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(_) => {
                self.snapshot.status = ScanStatus::Failed;
                self.snapshot.errors += 1;
                return;
            }
        };
        self.publish(None);
        for entry in entries {
            if !self.check_control() {
                break;
            }
            let Ok(entry) = entry else {
                self.snapshot.errors += 1;
                continue;
            };
            let path = entry.path();
            let Some(metadata) = self.metadata(&path) else {
                continue;
            };
            let mut row = DiskEntry {
                path,
                bytes: 0,
                directory: metadata.is_dir(),
            };
            if metadata.is_dir() {
                row.bytes = self.directory_size(&row.path, 0);
            } else if metadata.is_file() {
                row.bytes = self.file(&row.path, &metadata);
            }
            if self.rows.len() == MAX_ROWS {
                self.snapshot.limited = true;
            }
            retain(&mut self.rows, row, MAX_ROWS);
            if self.last_publish.elapsed() > Duration::from_millis(250) {
                self.publish(None);
            }
        }
    }

    fn maybe_publish(&mut self) {
        if self.last_publish.elapsed() >= Duration::from_millis(250) {
            self.publish(None);
        }
    }
    fn metadata(&mut self, path: &Path) -> Option<fs::Metadata> {
        if policy::protected(path)
            || crate::vault::is_private_path(path)
            || crate::vault::live::is_private(path)
        {
            self.snapshot.skipped += 1;
            return None;
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) if !unsafe_entry(&metadata) => Some(metadata),
            Ok(_) => {
                self.snapshot.skipped += 1;
                None
            }
            Err(_) => {
                self.snapshot.errors += 1;
                None
            }
        }
    }
    fn directory_size(&mut self, path: &Path, depth: usize) -> u64 {
        if depth >= MAX_DEPTH || path.as_os_str().len() > 16_384 {
            self.snapshot.limited = true;
            return 0;
        }
        #[cfg(windows)]
        let _lease = match atlas_win_space::cleanup::pin_directory(path, false) {
            Ok(lease) => lease,
            Err(_) => {
                self.snapshot.skipped += 1;
                return 0;
            }
        };
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(_) => {
                self.snapshot.errors += 1;
                return 0;
            }
        };
        let mut bytes = 0u64;
        for entry in entries {
            if !self.check_control() {
                break;
            }
            let Ok(entry) = entry else {
                self.snapshot.errors += 1;
                continue;
            };
            let path = entry.path();
            let Some(metadata) = self.metadata(&path) else {
                continue;
            };
            let size = if metadata.is_dir() {
                self.directory_size(&path, depth + 1)
            } else if metadata.is_file() {
                self.file(&path, &metadata)
            } else {
                0
            };
            bytes = bytes.saturating_add(size);
            if self.last_publish.elapsed() > Duration::from_millis(250) {
                self.publish(None);
            }
        }
        bytes
    }
    fn file(&mut self, path: &Path, metadata: &fs::Metadata) -> u64 {
        let bytes = metadata.len();
        self.snapshot.entry_count += 1;
        self.snapshot.bytes = self.snapshot.bytes.saturating_add(bytes);
        retain(
            &mut self.files,
            DiskEntry {
                path: path.to_owned(),
                bytes,
                directory: false,
            },
            MAX_FILES,
        );
        if self.mode != ScanMode::Folder
            && self.mode != ScanMode::FastFolder
            && bytes > 0
            && metadata
                .modified()
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age >= MIN_AGE)
        {
            if self.mode == ScanMode::Category {
                if self.snapshot.reclaimable.len() >= MAX_CANDIDATES {
                    self.snapshot.limited = true;
                } else {
                    self.snapshot.reclaimable.push(DiskEntry {
                        path: path.to_owned(),
                        bytes,
                        directory: false,
                    });
                }
            } else if self.snapshot.candidates.len() >= MAX_CANDIDATES
                || self.stored_path_bytes >= MAX_PATH_BYTES
            {
                self.snapshot.limited = true;
            } else {
                #[cfg(windows)]
                match atlas_win_space::cleanup::inspect(path) {
                    Ok(candidate) => {
                        let path_bytes = candidate
                            .ancestors
                            .iter()
                            .map(|p| p.path.as_os_str().len() * 2)
                            .sum::<usize>()
                            + path.as_os_str().len() * 2;
                        if self.stored_path_bytes.saturating_add(path_bytes) > MAX_PATH_BYTES {
                            self.snapshot.limited = true;
                        } else {
                            self.stored_path_bytes += path_bytes;
                            self.snapshot.candidates.push(candidate);
                        }
                    }
                    Err(_) => self.snapshot.skipped += 1,
                }
            }
        }
        bytes
    }
    fn check_control(&mut self) -> bool {
        while self.paused.load(Ordering::Acquire) && !self.cancel.load(Ordering::Acquire) {
            if self.snapshot.status != ScanStatus::Paused {
                self.snapshot.status = ScanStatus::Paused;
                self.publish(None);
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        if self.snapshot.status == ScanStatus::Paused {
            self.snapshot.status = ScanStatus::Scanning;
        }
        // A full category listing needs no further walking.
        if self.mode == ScanMode::Category && self.snapshot.reclaimable.len() >= MAX_CANDIDATES {
            self.snapshot.limited = true;
            return false;
        }
        !self.cancel.load(Ordering::Acquire) && !self.sender.is_disconnected()
    }
    fn publish(&mut self, _current: Option<DiskEntry>) {
        self.snapshot.entries = sorted(&self.rows);
        self.snapshot.large_files = sorted(&self.files);
        self.snapshot.other_bytes = self.snapshot.bytes.saturating_sub(
            self.snapshot
                .entries
                .iter()
                .fold(0u64, |total, e| total.saturating_add(e.bytes)),
        );
        let mut value = self.snapshot.clone();
        loop {
            match self.sender.try_send(value) {
                Ok(()) | Err(flume::TrySendError::Disconnected(_)) => break,
                Err(flume::TrySendError::Full(returned)) => {
                    value = returned;
                    let _ = self.drain.try_recv();
                }
            }
        }
        self.last_publish = Instant::now();
    }
}

fn unsafe_entry(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Reparse points and offline/recall-on-open/data-access files are not traversed.
        metadata.file_attributes() & (0x400 | 0x1000 | 0x40000 | 0x400000) != 0
    }
    #[cfg(not(windows))]
    {
        metadata.is_symlink()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_rows_preserve_totals_and_largest_entries() {
        let mut heap = BinaryHeap::new();
        for i in 0..2_000 {
            retain(
                &mut heap,
                DiskEntry {
                    path: PathBuf::from(i.to_string()),
                    bytes: i,
                    directory: false,
                },
                64,
            );
        }
        let rows = sorted(&heap);
        assert_eq!(rows.len(), 64);
        assert_eq!(rows[0].bytes, 1_999);
        assert_eq!(rows[63].bytes, 1_936);
    }
    #[test]
    fn scanner_reports_bytes_and_preserves_terminal_snapshot() {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join("dir")).unwrap();
        fs::write(fixture.path().join("dir/a"), [0; 19]).unwrap();
        fs::write(fixture.path().join("b"), [0; 7]).unwrap();
        let job = ScanJob::start(fixture.path().to_owned(), 12, ScanMode::Folder);
        loop {
            let snapshot = job.receiver.recv_timeout(Duration::from_secs(10)).unwrap();
            if snapshot.status.terminal() {
                assert_eq!(snapshot.generation, 12);
                assert_eq!(snapshot.bytes, 26);
                assert_eq!(snapshot.entry_count, 2);
                assert_eq!(snapshot.entries.len(), 1);
                break;
            }
        }
    }
    #[test]
    fn protected_content_is_never_enumerated() {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join(".ssh")).unwrap();
        fs::write(fixture.path().join(".ssh/id"), [0; 40]).unwrap();
        let job = ScanJob::start(fixture.path().to_owned(), 1, ScanMode::Folder);
        loop {
            let s = job.receiver.recv_timeout(Duration::from_secs(10)).unwrap();
            if s.status.terminal() {
                assert_eq!(s.bytes, 0);
                assert_eq!(s.skipped, 1);
                assert_eq!(s.status, ScanStatus::Partial);
                break;
            }
        }
        // A protected folder must also be refused when supplied as the root,
        // rather than only when encountered below a permitted root.
        let job = ScanJob::start(fixture.path().join(".ssh"), 2, ScanMode::Folder);
        loop {
            let s = job.receiver.recv_timeout(Duration::from_secs(10)).unwrap();
            if s.status.terminal() {
                assert_eq!(s.status, ScanStatus::Failed);
                assert_eq!(s.bytes, 0);
                assert!(s.entries.is_empty());
                break;
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn temporary_discovery_only_offers_old_unlocked_unprotected_files() {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join(".ssh")).unwrap();
        for name in ["old.tmp", "fresh.tmp", "locked.tmp", ".ssh/secret"] {
            let path = fixture.path().join(name);
            fs::write(&path, b"data").unwrap();
            if name != "fresh.tmp" {
                fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_modified(SystemTime::now() - Duration::from_secs(8 * 24 * 3600))
                    .unwrap();
            }
        }
        let _writer = fs::File::options()
            .write(true)
            .open(fixture.path().join("locked.tmp"))
            .unwrap();
        let (sender, drain) = flume::bounded(1);
        let mut worker = Worker {
            sender,
            drain,
            cancel: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            snapshot: DiskSnapshot::default(),
            rows: BinaryHeap::new(),
            files: BinaryHeap::new(),
            last_publish: Instant::now(),
            stored_path_bytes: 0,
            mode: ScanMode::Temporary,
        };
        worker.read_root(fixture.path());
        worker.finish();
        assert_eq!(worker.snapshot.candidates.len(), 1);
        assert_eq!(
            worker.snapshot.candidates[0].path,
            fixture.path().join("old.tmp")
        );
        assert_eq!(worker.snapshot.bytes, 12);
        assert_eq!(worker.snapshot.skipped, 2);
        assert_eq!(worker.snapshot.status, ScanStatus::Partial);
    }
}
