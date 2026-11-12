use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo};
use std::path::PathBuf;
use std::sync::Arc;

/// Fuzzy-matching the folder index doesn't need a worker per core. Passing `None`
/// to `Nucleo::new` would spawn `available_parallelism()` threads (one per core),
/// all kept alive for the whole session even while the goto bar is closed.
const NUCLEO_THREADS: usize = 2;

/// One indexed filesystem entry. `is_dir` comes from the directory walk (no
/// extra metadata call at render time).
#[derive(Clone)]
pub struct IndexEntry {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// `match_paths` tunes the scoring bonuses for `/`-separated haystacks
/// (filename and path-prefix bonuses), which is exactly what we index.
const fn config() -> Config {
    Config::DEFAULT.match_paths()
}

pub struct NucleoPool {
    pub nucleo: Nucleo<IndexEntry>,
    query: Option<String>,
    complete: bool,
}

impl NucleoPool {
    pub fn new() -> Self {
        Self {
            nucleo: Nucleo::new(config(), Arc::new(|| {}), Some(NUCLEO_THREADS), 1),
            query: None,
            complete: false,
        }
    }

    pub fn injector(&self) -> nucleo::Injector<IndexEntry> {
        self.nucleo.injector()
    }

    pub fn set_pattern(&mut self, query: &str) {
        if self.query.as_deref() == Some(query) {
            return;
        }
        self.query = Some(query.to_owned());
        self.complete = false;
        self.nucleo
            .pattern
            .reparse(0, query, CaseMatching::Smart, Normalization::Smart, false);
    }

    /// Returns whether the snapshot or index completion needs repainting.
    pub fn tick(&mut self) -> bool {
        let status = self.nucleo.tick(10);
        let complete = !status.running && !self.is_pending() && self.nucleo.active_injectors() == 0;
        let changed = status.changed || complete != self.complete;
        self.complete = complete;
        changed
    }

    pub fn results(&self, limit: usize) -> Vec<IndexEntry> {
        let snapshot = self.nucleo.snapshot();
        // The worker's previous snapshot can outlive a query change. Never
        // present those old matches under the new search text.
        if self.is_pending() {
            return Vec::new();
        }
        let count = (snapshot.matched_item_count() as usize).min(limit);
        (0..count)
            .filter_map(|i| snapshot.get_matched_item(i as u32))
            .map(|item| item.data.clone())
            .collect()
    }

    pub fn is_pending(&self) -> bool {
        self.nucleo.snapshot().pattern().column_pattern(0).atoms
            != self.nucleo.pattern.column_pattern(0).atoms
    }

    /// Both the directory producers and the matcher have finished. In
    /// particular, an empty snapshot while a walker is active is not final.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn reset(&mut self) {
        self.nucleo = Nucleo::new(config(), Arc::new(|| {}), Some(NUCLEO_THREADS), 1);
        self.query = None;
        self.complete = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn empty_scopes_finish_only_after_the_directory_producer_exits() {
        let mut pool = NucleoPool::new();
        let injector = pool.injector();
        pool.set_pattern("");
        pool.tick();
        assert!(
            !pool.is_complete(),
            "a cold scoped index must not freeze an empty page"
        );
        drop(injector);
        assert!(
            pool.tick(),
            "finishing an empty directory must repaint the loading state"
        );
        assert!(pool.is_complete());
        assert!(pool.results(10).is_empty());
        assert!(!pool.tick(), "a completed empty scope must stay idle");
        pool.set_pattern("downloads");
        assert!(!pool.is_complete());
        pool.reset();
        assert!(!pool.is_complete());
    }

    #[test]
    fn streamed_matches_report_a_change_without_another_keystroke() {
        let mut pool = NucleoPool::new();
        pool.set_pattern("egui");
        pool.tick();
        let path = PathBuf::from("developer/egui-app");
        pool.injector().push(
            IndexEntry {
                path: path.clone(),
                is_dir: true,
            },
            |_, cols| {
                cols[0] = "developer/egui-app".into();
            },
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let changed = pool.tick();
            if !pool.results(1).is_empty() {
                assert!(changed, "new results must trigger a finder repaint");
                assert_eq!(pool.results(1)[0].path, path);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "streamed folder was never published"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn identical_render_queries_stay_idle_and_changed_queries_hide_old_matches() {
        let mut pool = NucleoPool::new();
        pool.injector().push(
            IndexEntry {
                path: "developer/egui-app".into(),
                is_dir: true,
            },
            |_, cols| {
                cols[0] = "developer/egui-app".into();
            },
        );
        pool.set_pattern("egui");
        let deadline = Instant::now() + Duration::from_secs(5);
        while pool.results(1).is_empty() {
            pool.tick();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        for _ in 0..10 {
            pool.set_pattern("egui");
            assert!(
                !pool.tick(),
                "an unchanged finder render must not restart matching"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        pool.set_pattern("pexer");
        assert!(
            pool.results(1).is_empty(),
            "old egui matches must not appear under pexer"
        );
        pool.reset();
        pool.set_pattern("pexer");
        assert_eq!(pool.query.as_deref(), Some("pexer"));
        assert!(!pool.nucleo.pattern.column_pattern(0).atoms.is_empty());
    }
}
