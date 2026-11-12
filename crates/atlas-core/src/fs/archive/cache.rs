//! LRU provider cache — re-uses parsed archive indexes across navigations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

use super::ArchiveProvider;

const CAP: usize = 8;

struct Entry {
    provider: Arc<dyn ArchiveProvider>,
    mtime: SystemTime,
    len: u64,
    lru_seq: u64,
}

struct Cache {
    map: HashMap<PathBuf, Entry>,
    seq: u64,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| {
    Mutex::new(Cache {
        map: HashMap::new(),
        seq: 0,
    })
});

fn fingerprint(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// Get a cached provider for `path`, or open a new one via `factory`.
///
/// The cache validates freshness by mtime + file size. Evicts the least
/// recently used entry when at capacity.
pub fn get_or_open(
    path: &Path,
    factory: impl FnOnce(&Path) -> Option<Arc<dyn ArchiveProvider>>,
) -> Option<Arc<dyn ArchiveProvider>> {
    let fp = fingerprint(path)?;

    // Fast path — hit in cache.
    {
        let mut c = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hit = c.map.get(path).and_then(|e| {
            if e.mtime == fp.0 && e.len == fp.1 {
                Some(e.provider.clone())
            } else {
                None
            }
        });
        match hit {
            Some(provider) => {
                c.seq += 1;
                let seq = c.seq;
                if let Some(e) = c.map.get_mut(path) {
                    e.lru_seq = seq;
                }
                return Some(provider);
            }
            None => {
                // Stale or missing — evict before re-opening.
                c.map.remove(path);
            }
        }
    }

    // Slow path — open the archive (lock released so IO doesn't block cache).
    let provider = factory(path)?;

    {
        let mut c = CACHE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Evict LRU if at capacity.
        if c.map.len() >= CAP {
            if let Some(lru) = c
                .map
                .iter()
                .min_by_key(|(_, e)| e.lru_seq)
                .map(|(k, _)| k.clone())
            {
                c.map.remove(&lru);
            }
        }
        let new_seq = c.seq + 1;
        c.seq = new_seq;
        c.map.insert(
            path.to_path_buf(),
            Entry {
                provider: provider.clone(),
                mtime: fp.0,
                len: fp.1,
                lru_seq: new_seq,
            },
        );
    }

    Some(provider)
}

/// Remove the cache entry for `path` (call after writing to an archive).
pub fn invalidate(path: &Path) {
    if let Some(mut c) = CACHE.lock().or_else(atlas_model::sync::recover).ok() {
        c.map.remove(path);
    }
}
