//! Duplicate finder over the analysed tree. Read-only: it never deletes.
//!
//! Stages, each only on what the previous one could not tell apart:
//! 1. same size (from the in-memory scan, no I/O);
//! 2. same first and last 64 KiB;
//! 3. same SHA-256 of the whole file.
use atlas_model::cleanup::{DuplicateGroup, DuplicateReport};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

const SAMPLE: u64 = 64 * 1024;
const MAX_GROUPS: usize = 2_000;
/// Smallest file worth reporting; tiny copies free nothing meaningful.
pub const DEFAULT_MIN_BYTES: u64 = 1024 * 1024;

pub struct DuplicateJob {
    pub receiver: flume::Receiver<DuplicateReport>,
    cancel: Arc<AtomicBool>,
}

impl DuplicateJob {
    /// Search the files the last scan indexed below `root`.
    pub fn start(root: PathBuf, generation: u64, min_bytes: u64) -> Self {
        // Collecting millions of indexed files is too slow for the UI thread.
        Self::start_with(
            move || super::tree::cached_files(&root).unwrap_or_default(),
            generation,
            min_bytes,
        )
    }

    fn start_with(
        files: impl FnOnce() -> Vec<(PathBuf, u64)> + Send + 'static,
        generation: u64,
        min_bytes: u64,
    ) -> Self {
        let (sender, receiver) = flume::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let drain = receiver.clone();
        std::thread::Builder::new()
            .name("atlas-duplicates".into())
            .spawn(move || {
                let publish = |report: &DuplicateReport| {
                    let mut value = report.clone();
                    loop {
                        match sender.try_send(value) {
                            Ok(()) | Err(flume::TrySendError::Disconnected(_)) => break,
                            Err(flume::TrySendError::Full(back)) => {
                                value = back;
                                let _ = drain.try_recv();
                            }
                        }
                    }
                };
                find(files(), generation, min_bytes, &flag, publish);
            })
            .ok();
        Self { receiver, cancel }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Drop for DuplicateJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn sample(path: &Path, bytes: u64) -> std::io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    if bytes <= SAMPLE * 2 {
        // Small enough to read whole: this hash is final.
        let mut all = Vec::with_capacity(bytes as usize);
        file.read_to_end(&mut all)?;
        hasher.update(&all);
    } else {
        let mut buffer = vec![0u8; SAMPLE as usize];
        file.read_exact(&mut buffer)?;
        hasher.update(&buffer);
        file.seek(SeekFrom::End(-(SAMPLE as i64)))?;
        file.read_exact(&mut buffer)?;
        hasher.update(&buffer);
    }
    Ok(hasher.finalize().into())
}

fn full(path: &Path, cancel: &AtomicBool, mut read: impl FnMut(u64)) -> std::io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "cancelled",
            ));
        }
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        read(n as u64);
    }
    Ok(hasher.finalize().into())
}

fn modified(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// Group `files` by identical content. `publish` receives throttled progress
/// and the final report.
pub fn find(
    files: Vec<(PathBuf, u64)>,
    generation: u64,
    min_bytes: u64,
    cancel: &AtomicBool,
    publish: impl Fn(&DuplicateReport),
) -> DuplicateReport {
    let mut report = DuplicateReport {
        generation,
        ..Default::default()
    };
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for (path, bytes) in files {
        if bytes >= min_bytes.max(1) {
            by_size.entry(bytes).or_default().push(path);
        }
    }
    by_size.retain(|_, paths| paths.len() > 1);
    report.candidates = by_size.values().map(Vec::len).sum();
    report.total_bytes = by_size
        .iter()
        .map(|(bytes, paths)| bytes.saturating_mul(paths.len() as u64))
        .fold(0, u64::saturating_add);
    publish(&report);
    let mut last = Instant::now();
    // Largest sizes first: they free the most space and show up earliest.
    let mut sizes: Vec<u64> = by_size.keys().copied().collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    for size in sizes {
        if cancel.load(Ordering::Acquire) {
            report.cancelled = true;
            break;
        }
        let paths = by_size.remove(&size).unwrap_or_default();
        if size <= SAMPLE * 2 {
            report.hashed_bytes = report
                .hashed_bytes
                .saturating_add(size.saturating_mul(paths.len() as u64));
        }
        let mut by_sample: HashMap<[u8; 32], Vec<PathBuf>> = HashMap::new();
        for path in paths {
            match sample(&path, size) {
                Ok(hash) => by_sample.entry(hash).or_default().push(path),
                Err(_) => report.errors += 1,
            }
        }
        for (_, paths) in by_sample.into_iter().filter(|(_, p)| p.len() > 1) {
            // Files of at most two samples were read completely already.
            let groups: Vec<Vec<PathBuf>> = if size <= SAMPLE * 2 {
                vec![paths]
            } else {
                let mut by_full: HashMap<[u8; 32], Vec<PathBuf>> = HashMap::new();
                for path in paths {
                    let result = full(&path, cancel, |n| {
                        report.hashed_bytes = report.hashed_bytes.saturating_add(n);
                        if last.elapsed() > Duration::from_millis(200) {
                            publish(&report);
                            last = Instant::now();
                        }
                    });
                    match result {
                        Ok(hash) => by_full.entry(hash).or_default().push(path),
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                            report.cancelled = true;
                            break;
                        }
                        Err(_) => report.errors += 1,
                    }
                }
                by_full.into_values().filter(|p| p.len() > 1).collect()
            };
            for mut paths in groups {
                paths.sort_by_key(|p| modified(p));
                report.groups.push(DuplicateGroup { bytes: size, paths });
            }
            if report.cancelled {
                break;
            }
        }
        if report.groups.len() >= MAX_GROUPS {
            break;
        }
        if last.elapsed() > Duration::from_millis(200) {
            report.groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()));
            publish(&report);
            last = Instant::now();
        }
    }
    report.groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()));
    report.finished = true;
    publish(&report);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_content_is_grouped_and_lookalikes_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let big = |byte: u8, tail: u8| {
            let mut data = vec![byte; 300_000];
            data[150_000] = tail; // differs only in the middle: needs the full hash
            data
        };
        let files = [
            ("a.bin", big(1, 9)),
            ("b.bin", big(1, 9)),
            ("c.bin", big(1, 8)),
            ("d.bin", vec![7u8; 1000]),
            ("e.bin", vec![7u8; 1000]),
            ("lonely.bin", vec![1u8; 4321]),
        ];
        let mut listed = Vec::new();
        for (name, data) in &files {
            let path = dir.path().join(name);
            std::fs::write(&path, data).unwrap();
            listed.push((path, data.len() as u64));
        }
        let cancel = AtomicBool::new(false);
        let report = find(listed, 3, 1, &cancel, |_| {});
        assert!(report.finished && !report.cancelled);
        assert_eq!(report.generation, 3);
        assert_eq!(report.groups.len(), 2);
        let large = &report.groups[0];
        assert_eq!(large.bytes, 300_000);
        assert_eq!(large.wasted(), 300_000);
        let names: Vec<_> = large
            .paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.bin".to_owned()) && names.contains(&"b.bin".to_owned()));
        assert!(!names.contains(&"c.bin".to_owned()));
        assert_eq!(report.groups[1].paths.len(), 2);
    }

    #[test]
    fn minimum_size_and_cancellation_are_respected() {
        let dir = tempfile::tempdir().unwrap();
        let mut listed = Vec::new();
        for name in ["x", "y"] {
            let path = dir.path().join(name);
            std::fs::write(&path, [5u8; 100]).unwrap();
            listed.push((path, 100));
        }
        let cancel = AtomicBool::new(false);
        assert!(find(listed.clone(), 1, 1_000, &cancel, |_| {})
            .groups
            .is_empty());
        cancel.store(true, Ordering::Release);
        let report = find(listed, 1, 1, &cancel, |_| {});
        assert!(report.cancelled && report.groups.is_empty());
    }
}
