//! Backend-neutral catalog search contracts and the unprivileged filesystem fallback.
//!
//! The Windows service and user-session crawlers implement the same trait. Queries carry a
//! generation so callers can cheaply discard work produced for an obsolete input value.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub type SearchGeneration = u64;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum SearchScope {
    #[default]
    Everything,
    Volume(PathBuf),
    Folder(PathBuf),
    Location(PathBuf),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SearchCategory {
    File,
    Folder,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchFilters {
    pub categories: Vec<SearchCategory>,
    pub extensions: Vec<String>,
    pub min_size: Option<u64>,
    pub max_size: Option<u64>,
    pub modified_after: Option<u64>,
    pub modified_before: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum SearchSortField {
    #[default]
    Relevance,
    Name,
    Path,
    Size,
    Modified,
    Type,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchSort {
    pub field: SearchSortField,
    pub descending: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub generation: SearchGeneration,
    pub text: String,
    /// Interactive pickers may return a fast provisional first page for very
    /// short queries. Full Search views leave this false for exhaustive paging.
    #[serde(default)]
    pub interactive: bool,
    pub scope: SearchScope,
    pub filters: SearchFilters,
    pub sort: SearchSort,
    pub offset: usize,
    pub limit: usize,
}

impl SearchQuery {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            limit: 500,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum SearchOrigin {
    WindowsService,
    LocalFilesystem,
    Network,
    Other(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    /// Seconds since the Unix epoch, if supplied by the filesystem.
    pub modified: Option<u64>,
    pub origin: SearchOrigin,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<u64>,
    pub origin: SearchOrigin,
    pub score: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SearchBackendStatus {
    Loading,
    Ready,
    Disconnected,
    Fallback,
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchProgress {
    pub source: String,
    pub status: SearchBackendStatus,
    pub indexed: u64,
    pub total: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchBatch {
    pub generation: SearchGeneration,
    pub results: Vec<SearchResult>,
    pub complete: bool,
}

/// A source of metadata-only filename results. Implementations must not open result files.
pub trait SearchBackend: Send + Sync {
    fn id(&self) -> &str;
    fn status(&self) -> SearchProgress;
    fn search(&self, query: &SearchQuery) -> SearchBatch;
}

/// In-memory index used for FAT/ReFS/exFAT, UNC opt-ins and service fallback.
pub struct LocalSearchBackend {
    id: String,
    entries: RwLock<Vec<CatalogEntry>>,
    indexed: AtomicU64,
    status: RwLock<SearchBackendStatus>,
}

impl LocalSearchBackend {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            entries: RwLock::new(Vec::new()),
            indexed: AtomicU64::new(0),
            status: RwLock::new(SearchBackendStatus::Loading),
        }
    }

    /// Replaces the catalog progressively; every call immediately becomes searchable.
    pub fn extend(&self, entries: impl IntoIterator<Item = CatalogEntry>) {
        let mut catalog = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let before = catalog.len();
        catalog.extend(entries);
        self.indexed
            .fetch_add((catalog.len() - before) as u64, Ordering::Relaxed);
    }

    pub fn finish(&self) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = SearchBackendStatus::Ready;
    }

    pub fn set_status(&self, status: SearchBackendStatus) {
        *self.status.write().unwrap_or_else(|e| e.into_inner()) = status;
    }

    pub fn crawl(&self, root: &Path, max_entries: usize) {
        let entries = walkdir::WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                !crate::fs::scan_policy::is_scan_excluded(entry.path())
                    && !crate::fs::preview::is_cloud_placeholder(entry.path())
            })
            .filter_map(Result::ok)
            .skip(1)
            .take(max_entries)
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                Some(CatalogEntry {
                    path: entry.into_path(),
                    is_dir: metadata.is_dir(),
                    size: if metadata.is_file() {
                        metadata.len()
                    } else {
                        0
                    },
                    modified: metadata.modified().ok().and_then(epoch_seconds),
                    origin: SearchOrigin::LocalFilesystem,
                })
            })
            .collect::<Vec<_>>();
        self.extend(entries);
        self.finish();
    }

    /// Schema-versioned JSON snapshot written using replace-by-rename semantics.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        #[derive(Serialize)]
        struct Snapshot<'a> {
            schema: u32,
            entries: &'a [CatalogEntry],
        }
        let entries = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let bytes = serde_json::to_vec(&Snapshot {
            schema: 1,
            entries: &entries,
        })?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)?;
        Ok(())
    }

    pub fn load(id: impl Into<String>, path: &Path) -> anyhow::Result<Self> {
        #[derive(Deserialize)]
        struct Snapshot {
            schema: u32,
            entries: Vec<CatalogEntry>,
        }
        let snapshot: Snapshot = serde_json::from_slice(&fs::read(path)?)?;
        anyhow::ensure!(snapshot.schema == 1, "incompatible search index schema");
        let count = snapshot.entries.len() as u64;
        Ok(Self {
            id: id.into(),
            entries: RwLock::new(snapshot.entries),
            indexed: AtomicU64::new(count),
            status: RwLock::new(SearchBackendStatus::Ready),
        })
    }
}

impl SearchBackend for LocalSearchBackend {
    fn id(&self) -> &str {
        &self.id
    }
    fn status(&self) -> SearchProgress {
        SearchProgress {
            source: self.id.clone(),
            status: self
                .status
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            indexed: self.indexed.load(Ordering::Relaxed),
            total: None,
        }
    }
    fn search(&self, query: &SearchQuery) -> SearchBatch {
        let catalog = self.entries.read().unwrap_or_else(|e| e.into_inner());
        SearchBatch {
            generation: query.generation,
            results: search_entries(&catalog, query),
            complete: true,
        }
    }
}

/// Queries all sources, removes path duplicates and applies deterministic pagination.
pub fn search_all(backends: &[Arc<dyn SearchBackend>], query: &SearchQuery) -> SearchBatch {
    let mut unique = HashMap::<PathBuf, SearchResult>::new();
    for backend in backends {
        let batch = backend.search(query);
        if batch.generation != query.generation {
            continue;
        }
        for result in batch.results {
            let key = normalized_path(&result.path);
            unique
                .entry(key)
                .and_modify(|old| {
                    if result.score > old.score {
                        *old = result.clone();
                    }
                })
                .or_insert(result);
        }
    }
    let mut results: Vec<_> = unique.into_values().collect();
    sort_results(&mut results, query.sort);
    let results = results
        .into_iter()
        .skip(query.offset)
        .take(query.limit.max(1))
        .collect();
    SearchBatch {
        generation: query.generation,
        results,
        complete: true,
    }
}

fn search_entries(entries: &[CatalogEntry], query: &SearchQuery) -> Vec<SearchResult> {
    entries
        .iter()
        .filter(|entry| {
            !crate::vault::is_private_path(&entry.path)
                && !crate::vault::live::is_private(&entry.path)
                && in_scope(&entry.path, &query.scope)
                && matches_filters(entry, &query.filters)
        })
        .filter_map(|entry| {
            score_entry(entry, &query.text).map(|score| SearchResult {
                path: entry.path.clone(),
                is_dir: entry.is_dir,
                size: entry.size,
                modified: entry.modified,
                origin: entry.origin.clone(),
                score,
            })
        })
        .collect()
}

fn score_entry(entry: &CatalogEntry, query: &str) -> Option<i64> {
    if query.trim().is_empty() {
        return Some(0);
    }
    let name = entry.path.file_name()?.to_string_lossy();
    let path = entry.path.to_string_lossy();
    let sensitive = query.chars().any(char::is_uppercase);
    let (needle, name, path) = if sensitive {
        (query.to_owned(), name.into_owned(), path.into_owned())
    } else {
        (
            query.to_lowercase(),
            name.to_lowercase(),
            path.to_lowercase(),
        )
    };
    if name == needle {
        Some(4_000)
    } else if name.starts_with(&needle) {
        Some(3_000 - name.len() as i64)
    } else if name.contains(&needle) {
        Some(2_000 - name.len() as i64)
    } else if path.contains(&needle) {
        Some(1_000 - path.len() as i64)
    } else {
        fuzzy_subsequence(&needle, &name).map(|score| 500 + score)
    }
}

fn fuzzy_subsequence(needle: &str, haystack: &str) -> Option<i64> {
    let mut chars = haystack.chars().enumerate();
    let mut score = 0;
    let mut previous = None;
    for wanted in needle.chars().filter(|c| !c.is_whitespace()) {
        let (position, _) = chars.find(|(_, found)| *found == wanted)?;
        score += 10 - previous.map_or(0, |p| position.saturating_sub(p + 1).min(10) as i64);
        previous = Some(position);
    }
    Some(score)
}

fn in_scope(path: &Path, scope: &SearchScope) -> bool {
    match scope {
        SearchScope::Everything => true,
        SearchScope::Volume(root) | SearchScope::Folder(root) | SearchScope::Location(root) => {
            path.starts_with(root)
        }
    }
}

fn matches_filters(entry: &CatalogEntry, filters: &SearchFilters) -> bool {
    let category = if entry.is_dir {
        SearchCategory::Folder
    } else {
        SearchCategory::File
    };
    if !filters.categories.is_empty() && !filters.categories.contains(&category) {
        return false;
    }
    if !entry.is_dir && !filters.extensions.is_empty() {
        let extension = entry
            .path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase());
        if !filters.extensions.iter().any(|e| {
            extension.as_deref() == Some(e.trim_start_matches('.').to_lowercase().as_str())
        }) {
            return false;
        }
    }
    if filters.min_size.is_some_and(|min| entry.size < min)
        || filters.max_size.is_some_and(|max| entry.size > max)
    {
        return false;
    }
    if filters
        .modified_after
        .is_some_and(|min| entry.modified.is_none_or(|v| v < min))
        || filters
            .modified_before
            .is_some_and(|max| entry.modified.is_none_or(|v| v > max))
    {
        return false;
    }
    true
}

fn sort_results(results: &mut [SearchResult], sort: SearchSort) {
    results.sort_by(|a, b| {
        let order = match sort.field {
            SearchSortField::Relevance => b.score.cmp(&a.score),
            SearchSortField::Name => a.path.file_name().cmp(&b.path.file_name()),
            SearchSortField::Path => a.path.cmp(&b.path),
            SearchSortField::Size => a.size.cmp(&b.size),
            SearchSortField::Modified => a.modified.cmp(&b.modified),
            SearchSortField::Type => a
                .is_dir
                .cmp(&b.is_dir)
                .then_with(|| a.path.extension().cmp(&b.path.extension())),
        };
        let order = if sort.descending {
            order.reverse()
        } else {
            order
        };
        order.then_with(|| a.path.cmp(&b.path))
    });
}

fn normalized_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

fn epoch_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(path: &str, size: u64) -> CatalogEntry {
        CatalogEntry {
            path: PathBuf::from(path),
            is_dir: false,
            size,
            modified: Some(100),
            origin: SearchOrigin::LocalFilesystem,
        }
    }

    #[test]
    fn exact_name_beats_prefix_and_path() {
        let backend = LocalSearchBackend::new("test");
        backend.extend([
            entry("root/report.txt", 1),
            entry("root/report-old.txt", 2),
            entry("report/archive.txt", 3),
        ]);
        let results = backend.search(&SearchQuery::text("report")).results;
        let mut results = results;
        sort_results(&mut results, SearchSort::default());
        assert_eq!(results[0].path, PathBuf::from("root/report.txt"));
    }

    #[test]
    fn combined_filters_apply() {
        let backend = LocalSearchBackend::new("test");
        backend.extend([entry("root/a.rs", 10), entry("root/b.txt", 30)]);
        let mut query = SearchQuery::text("");
        query.filters.extensions = vec!["rs".into()];
        query.filters.max_size = Some(20);
        assert_eq!(backend.search(&query).results.len(), 1);
    }

    #[test]
    fn smart_case_is_sensitive_only_with_uppercase() {
        let backend = LocalSearchBackend::new("test");
        backend.extend([entry("ReadMe.txt", 1)]);
        assert_eq!(
            backend.search(&SearchQuery::text("readme")).results.len(),
            1
        );
        assert!(backend
            .search(&SearchQuery::text("README"))
            .results
            .is_empty());
    }
}
