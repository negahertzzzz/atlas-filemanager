//! Content search for the goto bar's "search in content" mode.
//!
//! nucleo (the folder index) only fuzzy-matches *names*; to match inside file bodies
//! we use ripgrep's library crates: `ignore::WalkBuilder` (fast parallel walk that
//! respects `.gitignore`, skips hidden dirs) + `grep-searcher`/`grep-regex` (with
//! built-in binary detection). Kept model-agnostic: results are delivered through an
//! `on_hit` callback so the UI layer owns the channel/generation bookkeeping.

use grep_regex::RegexMatcherBuilder;
use grep_searcher::{Searcher, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Skip files larger than this (keeps the search snappy and avoids huge blobs).
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Stop once this many files have matched (caps work and result list size).
const MAX_RESULTS: usize = 500;

/// A sink that records whether the file matched, stopping at the first hit.
struct FirstMatch {
    hit: bool,
}

impl Sink for FirstMatch {
    type Error = std::io::Error;
    fn matched(&mut self, _searcher: &Searcher, _mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        self.hit = true;
        Ok(false) // false = stop searching this file
    }
}

/// Recursively search `root` for files whose contents contain `query` (literal,
/// smart-case). Runs on the rayon pool; calls `on_hit(path)` for each match. Bails out
/// when `cancel` is set (e.g. the query changed) or once `MAX_RESULTS` is reached.
pub fn search_content(
    root: PathBuf,
    query: String,
    cancel: Arc<AtomicBool>,
    on_hit: impl Fn(PathBuf) + Send + Sync + 'static,
) {
    rayon::spawn(move || {
        let _background = crate::resources::Background::enter();
        if query.is_empty() {
            return;
        }
        // Treat the query as a literal substring (escaped), smart-case like nucleo.
        let Ok(matcher) = RegexMatcherBuilder::new()
            .case_smart(true)
            .build(&regex::escape(&query))
        else {
            return;
        };

        let on_hit = Arc::new(on_hit);
        let count = Arc::new(AtomicUsize::new(0));

        WalkBuilder::new(&root)
            .hidden(true)
            .git_ignore(true)
            .filter_entry(|entry| {
                !crate::fs::scan_policy::is_scan_excluded(entry.path())
                    && !crate::fs::preview::is_cloud_placeholder(entry.path())
            })
            .build_parallel()
            .run(|| {
                let matcher = matcher.clone();
                let on_hit = Arc::clone(&on_hit);
                let cancel = Arc::clone(&cancel);
                let count = Arc::clone(&count);
                Box::new(move |entry| {
                    if cancel.load(Ordering::Relaxed) {
                        return WalkState::Quit;
                    }
                    let Ok(entry) = entry else {
                        return WalkState::Continue;
                    };
                    if crate::fs::scan_policy::is_scan_excluded(entry.path())
                        || crate::fs::preview::is_cloud_placeholder(entry.path())
                    {
                        return WalkState::Skip;
                    }
                    if !entry.file_type().is_some_and(|t| t.is_file()) {
                        return WalkState::Continue;
                    }
                    if entry.metadata().map_or(0, |m| m.len()) > MAX_FILE_BYTES {
                        return WalkState::Continue;
                    }
                    let mut sink = FirstMatch { hit: false };
                    if Searcher::new()
                        .search_path(&matcher, entry.path(), &mut sink)
                        .is_ok()
                        && sink.hit
                    {
                        on_hit(entry.path().to_path_buf());
                        if count.fetch_add(1, Ordering::Relaxed) + 1 >= MAX_RESULTS {
                            cancel.store(true, Ordering::Relaxed);
                            return WalkState::Quit;
                        }
                    }
                    WalkState::Continue
                })
            });
    });
}
