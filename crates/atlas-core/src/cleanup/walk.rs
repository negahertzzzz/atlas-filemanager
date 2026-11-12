//! Parallel directory walker. Every folder is one task on a work-stealing pool;
//! a folder is enumerated once, in bulk, and its entries are registered in the
//! shared tree under a single short lock.
use super::{
    policy,
    tree::{SpaceTree, TreeBuilder, FILE_KEEP},
};
use atlas_model::cleanup::ScanBackend;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

const MAX_DEPTH: usize = 128;
const UNSAFE_ATTRIBUTES: u32 = 0x400 | 0x1000 | 0x40000 | 0x400000;

pub(super) struct Control {
    pub cancel: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    pub files: AtomicU64,
    pub bytes: AtomicU64,
}

struct Shared<'a> {
    builder: Mutex<TreeBuilder>,
    control: &'a Control,
}

impl Shared<'_> {
    fn proceed(&self) -> bool {
        while self.control.paused.load(Ordering::Acquire)
            && !self.control.cancel.load(Ordering::Acquire)
        {
            std::thread::sleep(Duration::from_millis(30));
        }
        !self.control.cancel.load(Ordering::Acquire)
    }
}

#[cfg(windows)]
fn read_dir(path: &Path, mut emit: impl FnMut(&str, bool, u32, u64)) -> std::io::Result<()> {
    atlas_win_space::directory::enumerate_while(path, |e| {
        emit(
            &e.name.to_string_lossy(),
            e.attributes & 0x10 != 0,
            e.attributes,
            e.size,
        );
        true
    })
}
#[cfg(not(windows))]
fn read_dir(path: &Path, mut emit: impl FnMut(&str, bool, u32, u64)) -> std::io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };
        let attrs = u32::from(meta.file_type().is_symlink()) * 0x400;
        emit(
            &entry.file_name().to_string_lossy(),
            meta.is_dir(),
            attrs,
            meta.len(),
        );
    }
    Ok(())
}

fn visit<'s>(
    shared: &'s Shared<'s>,
    scope: &rayon::Scope<'s>,
    path: PathBuf,
    id: u32,
    depth: usize,
) {
    if !shared.proceed() {
        return;
    }
    if depth >= MAX_DEPTH || path.as_os_str().len() > 16_384 {
        shared
            .builder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .limited = true;
        return;
    }
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut kept: Vec<(String, u64)> = Vec::new();
    let (mut loose_bytes, mut loose_files, mut skipped) = (0u64, 0u64, 0u64);
    let (mut files, mut bytes) = (0u64, 0u64);
    let result = read_dir(&path, |name, directory, attributes, size| {
        if attributes & UNSAFE_ATTRIBUTES != 0 {
            skipped += 1;
        } else if directory {
            let child = path.join(name);
            if policy::protected(&child)
                || crate::vault::is_private_path(&child)
                || crate::vault::live::is_private(&child)
            {
                skipped += 1;
            } else {
                dirs.push((name.to_owned(), child));
            }
        } else {
            files += 1;
            bytes = bytes.saturating_add(size);
            if size >= FILE_KEEP {
                kept.push((name.to_owned(), size));
            } else {
                loose_bytes = loose_bytes.saturating_add(size);
                loose_files += 1;
            }
        }
    });
    shared.control.files.fetch_add(files, Ordering::Relaxed);
    shared.control.bytes.fetch_add(bytes, Ordering::Relaxed);
    let children: Vec<u32> = {
        let mut builder = shared
            .builder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        builder.skipped += skipped;
        if result.is_err() {
            builder.errors += 1;
        }
        builder.fold(id, loose_bytes, loose_files);
        for (name, size) in &kept {
            builder.add_file(id, name, *size);
        }
        dirs.iter()
            .map(|(name, _)| builder.add_dir(id, name))
            .collect()
    };
    for ((_, child), cid) in dirs.into_iter().zip(children) {
        scope.spawn(move |s| visit(shared, s, child, cid, depth + 1));
    }
}

fn thread_count() -> usize {
    let cpus = std::thread::available_parallelism().map_or(8, |n| n.get());
    // Enumeration blocks on the filesystem, so oversubscribe the cores.
    crate::resources::scaled((cpus * 2).clamp(8, 32))
}

/// Walks `root` and returns its tree. `poll` runs about every 100 ms on the
/// calling thread so the caller can publish progress.
pub(super) fn walk(
    root: &Path,
    control: &Control,
    mut poll: impl FnMut(u64, u64),
) -> std::io::Result<SpaceTree> {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(thread_count())
        .thread_name(|i| format!("atlas-walk-{i}"))
        .start_handler(|_| crate::resources::lower_current_thread())
        .build()
        .map_err(std::io::Error::other)?;
    let shared = Shared {
        builder: Mutex::new(TreeBuilder::new(FILE_KEEP)),
        control,
    };
    std::thread::scope(|s| {
        let job = s.spawn(|| {
            pool.install(|| rayon::scope(|scope| visit(&shared, scope, root.to_owned(), 0, 0)))
        });
        while !job.is_finished() {
            std::thread::sleep(Duration::from_millis(100));
            poll(
                control.files.load(Ordering::Relaxed),
                control.bytes.load(Ordering::Relaxed),
            );
        }
        let _ = job.join();
    });
    let builder = shared.builder.into_inner().unwrap();
    Ok(builder.finish(root.to_owned(), ScanBackend::NativeDirectory))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parallel_walk_totals_match_and_skip_protected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..20 {
            std::fs::create_dir_all(root.join(format!("d{i}/sub"))).unwrap();
            std::fs::write(root.join(format!("d{i}/sub/f")), vec![0u8; 1000]).unwrap();
            std::fs::write(root.join(format!("d{i}/big")), vec![0u8; 300_000]).unwrap();
        }
        std::fs::create_dir(root.join(".ssh")).unwrap();
        std::fs::write(root.join(".ssh/id"), [0u8; 50]).unwrap();
        let control = Control {
            cancel: Arc::default(),
            paused: Arc::default(),
            files: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        };
        let tree = walk(root, &control, |_, _| {}).unwrap();
        let s = tree.snapshot(root, 1).unwrap();
        assert_eq!(s.bytes, 20 * 301_000);
        assert_eq!(s.entry_count, 40);
        assert_eq!(s.skipped, 1);
        assert_eq!(s.entries.len(), 20);
    }
}
