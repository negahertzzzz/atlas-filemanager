//! Byte-budgeted LRU. Reads promote entries, including reads from canvas paint.
use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::thumbs::ThumbState;

pub(crate) struct ThumbnailCache {
    inner: RefCell<Cache<ThumbState>>,
}

struct Cache<V> {
    entries: lru::LruCache<PathBuf, (V, usize)>,
    bytes: usize,
    budget: usize,
}

impl<V> Cache<V> {
    fn new(budget: usize) -> Self {
        Self {
            entries: lru::LruCache::unbounded(),
            bytes: 0,
            budget,
        }
    }

    fn insert(&mut self, key: PathBuf, value: V, bytes: usize) {
        if let Some((_, previous)) = self.entries.pop(&key) {
            self.bytes = self.bytes.saturating_sub(previous);
        }
        // An image larger than the entire budget must not flush the hot cache.
        if bytes > self.budget {
            return;
        }
        while self.bytes.saturating_add(bytes) > self.budget {
            let Some((_, (_, evicted))) = self.entries.pop_lru() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(evicted);
        }
        self.bytes += bytes;
        self.entries.put(key, (value, bytes));
    }
}

impl ThumbnailCache {
    pub(crate) fn new(megabytes: usize) -> Self {
        Self {
            inner: RefCell::new(Cache::new(megabytes.clamp(1, 4096) * 1024 * 1024)),
        }
    }

    pub(crate) fn contains_key(&self, path: &Path) -> bool {
        self.inner.borrow_mut().entries.get(path).is_some()
    }

    pub(crate) fn set_budget_mb(&self, megabytes: usize) {
        let mut cache = self.inner.borrow_mut();
        cache.budget = megabytes.clamp(1, 4096) * 1024 * 1024;
        while cache.bytes > cache.budget {
            let Some((_, (_, bytes))) = cache.entries.pop_lru() else {
                break;
            };
            cache.bytes = cache.bytes.saturating_sub(bytes);
        }
    }

    pub(crate) fn get(&self, path: &Path) -> Option<ThumbState> {
        self.inner
            .borrow_mut()
            .entries
            .get(path)
            .map(|(value, _)| value.clone())
    }

    pub(crate) fn insert(&self, path: PathBuf, state: ThumbState) {
        let bytes = match &state {
            ThumbState::Ready(image) => {
                let size = image.size(0);
                (u32::from(size.width) as usize)
                    .saturating_mul(u32::from(size.height) as usize)
                    .saturating_mul(4)
            }
            _ => 0,
        };
        // Charge keys and failed/loading sentinels too, to bound non-image entries.
        let cost = bytes
            .saturating_add(path.as_os_str().len().saturating_mul(2))
            .saturating_add(128);
        self.inner.borrow_mut().insert(path, state, cost);
    }

    pub(crate) fn remove(&self, path: &Path) {
        let mut cache = self.inner.borrow_mut();
        if let Some((_, bytes)) = cache.entries.pop(path) {
            cache.bytes = cache.bytes.saturating_sub(bytes);
        }
    }

    pub(crate) fn clear(&self) {
        let mut cache = self.inner.borrow_mut();
        cache.entries.clear();
        cache.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::Cache;

    #[test]
    fn evicts_cold_entries_by_bytes_and_preserves_promoted_entries() {
        let mut cache = Cache::new(10);
        cache.insert("a".into(), 1, 4);
        cache.insert("b".into(), 2, 4);
        cache.entries.get(std::path::Path::new("a"));
        cache.insert("c".into(), 3, 5);
        assert!(cache.entries.contains(std::path::Path::new("a")));
        assert!(!cache.entries.contains(std::path::Path::new("b")));
        assert_eq!(cache.bytes, 9);
        cache.insert("oversized".into(), 4, 11);
        assert_eq!(cache.entries.len(), 2);
        cache.insert("a".into(), 5, 2);
        assert_eq!(cache.bytes, 7);
    }
}
