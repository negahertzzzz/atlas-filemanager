#![allow(dead_code)] // reason: retained API surface not exercised by every GPUI flow yet
use crate::file_entry::{FileEntry, FileKind};
pub use crate::grouping::{GroupBy, GroupRow};
use crate::location::Location;
use std::collections::HashSet;
use std::sync::Arc;

/// One directory column in the Miller (columns) view.
#[derive(Clone)]
pub struct MillerColumn {
    pub location: Location,
    pub entries: std::sync::Arc<[FileEntry]>,
    pub load_state: LoadState,
    pub op_id: u64,
    pub has_excluded_items: bool,
    /// Multi-selection state for this column (single-select for navigation,
    /// ctrl/shift for multi, rubber band populates `selected` set).
    pub selection: SelectionState,
    visibility: Option<ColumnVisibility>,
}

#[derive(Clone)]
struct ColumnVisibility {
    entries: Arc<[FileEntry]>,
    filter: String,
    files: bool,
    folders: bool,
    indices: Arc<Vec<usize>>,
    counts: VisibleCounts,
}

impl MillerColumn {
    pub fn new(location: Location) -> Self {
        Self {
            location,
            entries: std::sync::Arc::new([]),
            load_state: LoadState::NotLoaded,
            op_id: 0,
            has_excluded_items: false,
            selection: SelectionState::default(),
            visibility: None,
        }
    }

    /// Preserve the selected paths when a watcher/rename refresh reorders a column.
    pub fn set_entries(&mut self, entries: Arc<[FileEntry]>) {
        let selected: HashSet<_> = self
            .selection
            .selected
            .iter()
            .filter_map(|&idx| self.entries.get(idx).map(|entry| entry.path().clone()))
            .collect();
        let cursor = self
            .selection
            .keyboard_cursor
            .and_then(|idx| self.entries.get(idx))
            .map(|entry| entry.path().clone());
        self.entries = entries;
        self.visibility = None;
        self.selection.selected = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| selected.contains(&entry.path()))
            .map(|(idx, _)| idx)
            .collect();
        self.selection.keyboard_cursor =
            cursor.and_then(|path| self.entries.iter().position(|entry| entry.path() == path));
        self.selection.selection_anchor = self.selection.keyboard_cursor;
        self.selection.last_clicked_index = self.selection.keyboard_cursor;
    }

    /// The primary selected index (first selected item), used for column-open logic.
    pub fn primary_selected(&self) -> Option<usize> {
        self.selection
            .keyboard_cursor
            .or_else(|| self.selection.selected.iter().copied().next())
    }

    fn cached_visibility(
        &self,
        filter: &str,
        files: bool,
        folders: bool,
    ) -> Option<&ColumnVisibility> {
        self.visibility.as_ref().filter(|cache| {
            Arc::ptr_eq(&cache.entries, &self.entries)
                && cache.filter == filter
                && cache.files == files
                && cache.folders == folders
        })
    }

    pub fn visible_indices(&self, filter: &str, files: bool, folders: bool) -> Arc<Vec<usize>> {
        self.cached_visibility(filter, files, folders).map_or_else(
            || Arc::new(filter_visible_entries(&self.entries, filter, files, folders).0),
            |cache| cache.indices.clone(),
        )
    }

    pub fn visible_counts(&self, filter: &str, files: bool, folders: bool) -> VisibleCounts {
        self.cached_visibility(filter, files, folders).map_or_else(
            || filter_visible_entries(&self.entries, filter, files, folders).1,
            |cache| cache.counts,
        )
    }

    /// Keep original entry indices in selection/operations. Only display
    /// positions are compressed, and only a changed projection drops selection.
    pub fn refresh_visibility(&mut self, filter: &str, files: bool, folders: bool) {
        if self.cached_visibility(filter, files, folders).is_some() {
            return;
        }
        let (indices, counts) = filter_visible_entries(&self.entries, filter, files, folders);
        self.selection.retain_visible(&indices);
        self.visibility = Some(ColumnVisibility {
            entries: self.entries.clone(),
            filter: filter.into(),
            files,
            folders,
            indices: Arc::new(indices),
            counts,
        });
    }
}

/// Cascading column navigation state for the Miller view.
#[derive(Clone, Default)]
pub struct MillerState {
    pub columns: Vec<MillerColumn>,
    pub focused: usize,
}

#[derive(Clone, Default)]
pub struct TreeState {
    pub expanded: std::collections::HashSet<std::path::PathBuf>,
    pub children: std::collections::HashMap<std::path::PathBuf, std::sync::Arc<[FileEntry]>>,
}

impl TreeState {
    pub fn toggle_expand(&mut self, path: &std::path::PathBuf) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.clone());
        }
    }

    pub fn is_expanded(&self, path: &std::path::PathBuf) -> bool {
        self.expanded.contains(path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState {
    NotLoaded,
    Loading,
    Loaded,
    Error(String),
}

#[derive(Clone, Default)]
pub struct SelectionState<T = usize> {
    pub selected: HashSet<T>,
    pub last_clicked_index: Option<T>,
    pub keyboard_cursor: Option<T>,
    pub selection_anchor: Option<T>,
    pub scroll_to_selected: bool,
    pub last_pointer_click: Option<(T, std::time::Instant)>,
}

impl SelectionState<usize> {
    /// Re-attach an index selection to file identity after its index space
    /// changed (`old[i]` was the item at index `i`, `new[j]` is the item at `j`).
    ///
    /// Selected items that vanished hand their selection to their nearest
    /// surviving ancestor, so collapsing a folder selects that folder (Finder).
    /// In a flat listing a deleted file has no listed ancestor and simply drops.
    pub fn remap_by_path(&self, old: &[std::path::PathBuf], new: &[std::path::PathBuf]) -> Self {
        let position: std::collections::HashMap<&std::path::Path, usize> = new
            .iter()
            .enumerate()
            .map(|(i, path)| (path.as_path(), i))
            .collect();
        let locate = |idx: usize| -> Option<usize> {
            let path = old.get(idx)?;
            path.ancestors()
                .find_map(|candidate| position.get(candidate).copied())
        };
        let exact = |idx: usize| -> Option<usize> {
            old.get(idx)
                .and_then(|path| position.get(path.as_path()).copied())
        };
        let cursor = self.keyboard_cursor.and_then(locate);
        let moved =
            self.keyboard_cursor.is_some() && self.keyboard_cursor.and_then(exact).is_none();
        Self {
            selected: self
                .selected
                .iter()
                .filter_map(|&idx| locate(idx))
                .collect(),
            keyboard_cursor: cursor,
            selection_anchor: self.selection_anchor.and_then(exact).or(cursor),
            last_clicked_index: self.last_clicked_index.and_then(exact).or(cursor),
            // A cursor handed to an ancestor must be revealed; otherwise keep the
            // pending request as it was.
            scroll_to_selected: self.scroll_to_selected || (moved && cursor.is_some()),
            last_pointer_click: None,
        }
    }
}

impl<T: Clone + Eq + std::hash::Hash> SelectionState<T> {
    /// A slow second click is distinct from the platform's double-click open.
    pub fn slow_rename_click(&mut self, item: T, now: std::time::Instant, eligible: bool) -> bool {
        let rename = eligible
            && self
                .last_pointer_click
                .as_ref()
                .is_some_and(|(previous, at)| {
                    let elapsed = now.saturating_duration_since(*at);
                    previous == &item
                        && (std::time::Duration::from_millis(500)
                            ..=std::time::Duration::from_millis(1200))
                            .contains(&elapsed)
                });
        self.last_pointer_click = if eligible { Some((item, now)) } else { None };
        rename
    }

    pub fn all_visible_selected(&self, visible: &[T]) -> bool {
        !visible.is_empty() && visible.iter().all(|item| self.selected.contains(item))
    }

    /// Select exactly the displayed set, or clear it when the set is fully selected.
    pub fn toggle_all_visible(&mut self, visible: &[T]) {
        if visible.is_empty() {
            return;
        }
        if self.all_visible_selected(visible) {
            self.clear();
        } else {
            self.selected = visible.iter().cloned().collect();
            self.keyboard_cursor = visible.first().cloned();
            self.selection_anchor = self.keyboard_cursor.clone();
            self.last_clicked_index = self.keyboard_cursor.clone();
        }
    }

    /// Apply pointer selection in display order, shared by panes, Miller and the shelf.
    pub fn select(&mut self, item: T, visible: &[T], shift: bool, ctrl: bool) {
        self.retain_visible(visible);
        if !visible.contains(&item) {
            return;
        }
        if shift {
            let anchor = self
                .selection_anchor
                .as_ref()
                .or(self.keyboard_cursor.as_ref())
                .unwrap_or(&item)
                .clone();
            if let (Some(a), Some(b)) = (
                visible.iter().position(|value| value == &anchor),
                visible.iter().position(|value| value == &item),
            ) {
                if !ctrl {
                    self.selected.clear();
                }
                self.selected
                    .extend(visible[a.min(b)..=a.max(b)].iter().cloned());
            }
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(anchor);
            }
        } else {
            if ctrl {
                if !self.selected.remove(&item) {
                    self.selected.insert(item.clone());
                }
            } else {
                self.selected.clear();
                self.selected.insert(item.clone());
            }
            self.selection_anchor = Some(item.clone());
        }
        self.last_clicked_index = Some(item.clone());
        self.keyboard_cursor = Some(item);
    }

    /// Move the keyboard cursor with the same anchor and modifier rules in every view.
    /// A column-major List supplies its row count so Shift selects a visual rectangle.
    /// Ctrl alone only moves focus; Ctrl+Shift adds the range to the current selection.
    pub fn navigate(
        &mut self,
        item: T,
        visible: &[T],
        extend: bool,
        ctrl: bool,
        column_rows: Option<usize>,
    ) {
        self.retain_visible(visible);
        let Some(next) = visible.iter().position(|value| value == &item) else {
            return;
        };
        let anchor = self
            .selection_anchor
            .as_ref()
            .or(self.keyboard_cursor.as_ref())
            .unwrap_or(&item)
            .clone();
        if extend {
            let start = visible
                .iter()
                .position(|value| value == &anchor)
                .unwrap_or(next);
            if !ctrl {
                self.selected.clear();
            }
            if let Some(rows) = column_rows.filter(|rows| *rows > 0) {
                let (top, bottom) = (
                    (start % rows).min(next % rows),
                    (start % rows).max(next % rows),
                );
                let (left, right) = (
                    (start / rows).min(next / rows),
                    (start / rows).max(next / rows),
                );
                for column in left..=right {
                    let lo = column * rows + top;
                    let hi = (column * rows + bottom + 1).min(visible.len());
                    if lo < hi {
                        self.selected.extend(visible[lo..hi].iter().cloned());
                    }
                }
            } else {
                self.selected
                    .extend(visible[start.min(next)..=start.max(next)].iter().cloned());
            }
            self.selection_anchor = Some(anchor);
        } else if !ctrl {
            self.selected.clear();
            self.selected.insert(item.clone());
            self.selection_anchor = Some(item.clone());
        }
        self.keyboard_cursor = Some(item.clone());
        self.last_clicked_index = Some(item);
        self.last_pointer_click = None;
        self.scroll_to_selected = true;
    }

    pub fn retain_visible(&mut self, visible: &[T]) {
        let visible: HashSet<_> = visible.iter().collect();
        self.selected.retain(|item| visible.contains(item));
        for cursor in [
            &mut self.last_clicked_index,
            &mut self.keyboard_cursor,
            &mut self.selection_anchor,
        ] {
            if cursor.as_ref().is_some_and(|item| !visible.contains(item)) {
                *cursor = None;
            }
        }
    }

    pub fn clear(&mut self) {
        self.last_pointer_click = None;
        self.selected.clear();
        self.last_clicked_index = None;
        self.keyboard_cursor = None;
        self.selection_anchor = None;
        self.scroll_to_selected = false;
    }
}

#[derive(Clone, Default)]
pub struct PathEditState {
    pub editing: bool,
    pub text: String,
    pub focus: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchStatus {
    #[default]
    Idle,
    Searching,
    Ready,
    Error,
}

#[derive(Clone, Default)]
pub struct SearchInPlaceState {
    pub active: bool,
    pub query: String,
    pub recursive: bool,
    pub focus: bool,
    pub status: SearchStatus,
    pub generation: u64,
    pub original_entries: Option<std::sync::Arc<[FileEntry]>>,
    pub error: Option<String>,
}

impl SearchInPlaceState {
    pub fn reset(&mut self) {
        let recursive = self.recursive;
        let generation = self.generation.wrapping_add(1);
        *self = Self {
            recursive,
            generation,
            ..Self::default()
        };
    }

    pub fn is_filtering(&self) -> bool {
        self.active && !self.query.trim().is_empty()
    }
}

/// Live `shown/total` counts per kind for the bottom bar, cached alongside the
/// `visible` rebuild so reading them every frame is free.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VisibleCounts {
    pub folders_shown: usize,
    pub folders_total: usize,
    pub files_shown: usize,
    pub files_total: usize,
    pub bytes_shown: u128,
}

#[derive(Clone)]
pub struct TabState {
    pub listing_elapsed: Option<std::time::Duration>,
    pub group_by: GroupBy,
    pub collapsed_groups: std::collections::BTreeSet<String>,
    grouped_rows: Arc<Vec<GroupRow>>,
    grouped_visible: Arc<Vec<usize>>,
    group_cache: Option<(u64, String, bool, bool, GroupBy, chrono::NaiveDate, u64)>,
    group_revision: u64,
    grouped_cells: Option<(
        usize,
        crate::grouping::GroupFlow,
        Arc<crate::grouping::GroupCells>,
    )>,
    /// Created/accessed stamps for the listing with this `entries_version`.
    entry_times: Option<(u64, Arc<Vec<crate::grouping::EntryTimes>>)>,
    entry_times_requested: Option<u64>,
    pub entries: std::sync::Arc<[FileEntry]>,
    pub filter: String,
    /// Per-pane visibility toggles, mirrored from `TabData` so the cached
    /// filtering below doesn't need access to the pane tree.
    pub show_files: bool,
    pub show_folders: bool,
    /// A metadata-only listing found entries omitted by hidden/system settings.
    pub has_excluded_items: bool,
    pub load_state: LoadState,
    pub last_nav_op_id: u64,
    pub grid_columns: usize,
    pub selection: SelectionState,
    pub path_edit: PathEditState,
    pub search: SearchInPlaceState,
    pub miller: MillerState,
    pub tree: TreeState,
    /// Width of the pane as of the last rendered frame, used to detect active
    /// resize and suppress layout thrashing in the grid view.
    pub last_pane_width: f32,
    /// Height of the pane as of the last rendered frame, used by canvas-backed
    /// views to avoid shaping rows outside the visible viewport.
    pub last_pane_height: f32,
    /// Bumped whenever `entries` is replaced; used to invalidate `visible` cache.
    /// Mutate `entries` only through [`TabState::set_entries`]/[`TabState::begin_navigation`]
    /// so this stays in sync.
    pub entries_version: u64,
    /// Cached result of filtering `entries` by `filter`, rebuilt only when the
    /// filter text or the entry set changes (see [`TabState::visible_indices`]).
    pub visible: Arc<Vec<usize>>,
    pub visible_filter: String,
    pub visible_version: u64,
    pub visible_valid: bool,
    visible_show_files: bool,
    visible_show_folders: bool,
    counts: VisibleCounts,
    /// Cached set of unique category labels (e.g. "Carpeta", "Imagen") derived from
    /// `entries`, rebuilt only when `entries_version` advances.
    pub type_labels: Vec<String>,
    type_labels_version: u64,
    /// Bumped once each time a *fresh* listing (tab create / navigation) finishes
    /// loading — NOT on incremental fs-watch refreshes. Views key their item
    /// appear animation on this so rows fade/slide in only when content is new.
    pub appear_seq: u64,
    /// Set by create/navigate paths; consumed on the next completed load to bump
    /// `appear_seq`. Keeps watch refreshes from re-triggering the appear animation.
    pub pending_appear: bool,
    pub is_branch_view: bool,
}

impl Default for TabState {
    fn default() -> Self {
        Self {
            listing_elapsed: None,
            group_by: GroupBy::None,
            collapsed_groups: std::collections::BTreeSet::new(),
            grouped_rows: Arc::default(),
            grouped_visible: Arc::default(),
            group_cache: None,
            group_revision: 0,
            grouped_cells: None,
            entry_times: None,
            entry_times_requested: None,
            entries: std::sync::Arc::new([]),
            filter: String::new(),
            show_files: true,
            show_folders: true,
            has_excluded_items: false,
            load_state: LoadState::NotLoaded,
            last_nav_op_id: 0,
            grid_columns: 1,
            selection: SelectionState::default(),
            path_edit: PathEditState::default(),
            search: SearchInPlaceState::default(),
            miller: MillerState::default(),
            tree: TreeState::default(),
            last_pane_width: 0.0,
            last_pane_height: 0.0,
            entries_version: 0,
            visible: Arc::new(Vec::new()),
            visible_filter: String::new(),
            visible_version: 0,
            visible_valid: false,
            visible_show_files: true,
            visible_show_folders: true,
            counts: VisibleCounts::default(),
            type_labels: Vec::new(),
            type_labels_version: u64::MAX, // force rebuild on first call
            appear_seq: 0,
            pending_appear: false,
            is_branch_view: false,
        }
    }
}

impl TabState {
    /// Paths of the index space `self.selection` lives in: the visible tree rows
    /// in Tree view, the flat listing everywhere else (Miller columns own their
    /// own selection and remap themselves in `set_entries`).
    pub fn selection_paths(&self, tree: bool) -> Vec<std::path::PathBuf> {
        if tree {
            self.visible_tree_rows_snapshot()
                .into_iter()
                .map(|row| row.0.path())
                .collect()
        } else {
            self.entries.iter().map(|entry| entry.path()).collect()
        }
    }

    /// Path of one index of the selection's index space (see `selection_paths`).
    pub fn selection_path(&self, tree: bool, idx: usize) -> Option<std::path::PathBuf> {
        if tree {
            self.visible_tree_rows_snapshot()
                .get(idx)
                .map(|row| row.0.path())
        } else {
            self.entries.get(idx).map(|entry| entry.path())
        }
    }

    /// The one entry point for structural changes (new listing, re-sort, tree
    /// expand/collapse, children arriving, watcher deltas). The selection stays
    /// on the same files by construction, so no caller can forget to remap.
    pub fn mutate_preserving_selection(&mut self, tree: bool, mutate: impl FnOnce(&mut Self)) {
        self.mutate_preserving_selection_with(tree, |path| path, mutate);
    }

    /// Like [`Self::mutate_preserving_selection`], with a rename map applied to
    /// the old identities (watcher renames keep their item selected).
    pub fn mutate_preserving_selection_with(
        &mut self,
        tree: bool,
        rename: impl Fn(std::path::PathBuf) -> std::path::PathBuf,
        mutate: impl FnOnce(&mut Self),
    ) {
        // A fresh navigation has no identity to restore. Building two complete
        // path vectors and a million-entry lookup here stalled the UI thread.
        if self.selection.selected.is_empty()
            && self.selection.keyboard_cursor.is_none()
            && self.selection.selection_anchor.is_none()
            && self.selection.last_clicked_index.is_none()
        {
            mutate(self);
            self.selection.last_pointer_click = None;
            return;
        }
        let before: Vec<_> = self.selection_paths(tree).into_iter().map(rename).collect();
        let selection = self.selection.clone();
        mutate(self);
        let after = self.selection_paths(tree);
        self.selection = selection.remap_by_path(&before, &after);
    }

    pub fn refresh_miller_visibility(&mut self) {
        for column in &mut self.miller.columns {
            column.refresh_visibility(&self.filter, self.show_files, self.show_folders);
        }
    }

    pub fn miller_visible_indices(&self, column: usize) -> Arc<Vec<usize>> {
        self.miller.columns.get(column).map_or_else(
            || Arc::new(Vec::new()),
            |column| column.visible_indices(&self.filter, self.show_files, self.show_folders),
        )
    }

    pub fn selected_entries(&self, mode: &crate::settings::ViewMode) -> Vec<&FileEntry> {
        use crate::settings::ViewMode;
        let (selection, entries) = if *mode == ViewMode::Miller {
            let Some(column) = self.miller.columns.get(self.miller.focused) else {
                return Vec::new();
            };
            (&column.selection, &column.entries)
        } else {
            (&self.selection, &self.entries)
        };
        let mut selected: Vec<_> = selection.selected.iter().copied().collect();
        selected.sort_unstable();
        if *mode == ViewMode::Tree {
            let rows = self.visible_tree_rows_snapshot();
            selected
                .into_iter()
                .filter_map(|i| rows.get(i).map(|row| row.0))
                .collect()
        } else if *mode == ViewMode::Miller {
            let visible = self.miller_visible_indices(self.miller.focused);
            selected
                .into_iter()
                .filter(|i| visible.binary_search(i).is_ok())
                .filter_map(|i| entries.get(i))
                .collect()
        } else {
            selected
                .into_iter()
                .filter_map(|i| entries.get(i))
                .collect()
        }
    }

    /// Type-to-select uses the actual index space of the displayed view. It
    /// leaves the filter and listing untouched, including expanded tree rows.
    pub fn select_typed_prefix(
        &mut self,
        mode: &crate::settings::ViewMode,
        prefix: &str,
        cycle: bool,
    ) -> bool {
        self.select_typed_prefix_with_indices(mode, prefix, cycle, None)
    }

    pub fn select_typed_prefix_with_indices(
        &mut self,
        mode: &crate::settings::ViewMode,
        prefix: &str,
        cycle: bool,
        visible_override: Option<&[usize]>,
    ) -> bool {
        use crate::settings::ViewMode;
        let prefix = prefix.to_lowercase();
        let find = |entries: &mut dyn Iterator<Item = (usize, &FileEntry)>,
                    cursor: Option<usize>| {
            let cursor = cycle.then_some(cursor).flatten();
            let mut after_cursor = cursor.is_none();
            let mut first = None;
            for (index, entry) in entries {
                if entry.name_lower().starts_with(&prefix) {
                    if after_cursor {
                        return Some(index);
                    }
                    first.get_or_insert(index);
                }
                if cursor == Some(index) {
                    after_cursor = true;
                }
            }
            first
        };
        let cursor = self.selection.keyboard_cursor;
        let (selection, found) = match mode {
            ViewMode::Miller => {
                let Some(column) = self.miller.columns.get_mut(self.miller.focused) else {
                    return false;
                };
                let visible =
                    column.visible_indices(&self.filter, self.show_files, self.show_folders);
                let found = find(
                    &mut visible.iter().map(|i| (*i, &column.entries[*i])),
                    column.selection.keyboard_cursor,
                );
                (&mut column.selection, found)
            }
            ViewMode::Tree => {
                let rows = self.visible_tree_rows();
                let found = find(
                    &mut rows.iter().enumerate().map(|(i, row)| (i, row.0)),
                    cursor,
                );
                (&mut self.selection, found)
            }
            _ => {
                let visible = self.visible_indices_shared();
                let visible = visible_override.unwrap_or(&visible);
                let found = find(&mut visible.iter().map(|i| (*i, &self.entries[*i])), cursor);
                (&mut self.selection, found)
            }
        };
        if let Some(index) = found {
            selection.select(index, &[index], false, false);
            selection.scroll_to_selected = true;
            true
        } else {
            false
        }
    }

    /// Translate an entry selection position to the sectioned virtual-list row.
    pub fn grouped_row_for_entry(&mut self, entry: usize) -> Option<usize> {
        self.visible_indices();
        self.grouped_rows
            .iter()
            .position(|row| *row == GroupRow::Entry(entry))
    }
    pub fn set_grouping(&mut self, mode: GroupBy, collapsed: std::collections::BTreeSet<String>) {
        if self.group_by != mode || self.collapsed_groups != collapsed {
            self.group_by = mode;
            self.collapsed_groups = collapsed;
            self.group_cache = None;
            self.group_revision = self.group_revision.wrapping_add(1);
        }
    }

    /// Headers and entries as one position space for the DG views. Cached per
    /// group snapshot and flow; the entry-only order stays in `visible_indices`.
    pub fn grouped_cells_shared(
        &mut self,
        flow: crate::grouping::GroupFlow,
    ) -> Arc<crate::grouping::GroupCells> {
        self.visible_indices();
        let key = Arc::as_ptr(&self.grouped_rows) as usize;
        if let Some((cached, cached_flow, cells)) = &self.grouped_cells {
            if *cached == key && *cached_flow == flow {
                return cells.clone();
            }
        }
        let cells = Arc::new(crate::grouping::flow_cells(&self.grouped_rows, flow));
        self.grouped_cells = Some((key, flow, cells.clone()));
        cells
    }

    /// Listing version whose created/accessed stamps still have to be read for
    /// the active grouping; `None` when nothing is missing or already requested.
    pub fn entry_times_needed(&self) -> Option<u64> {
        let wants = matches!(self.group_by, GroupBy::Created | GroupBy::Accessed);
        let current = self
            .entry_times
            .as_ref()
            .is_some_and(|(version, _)| *version == self.entries_version);
        (wants && !current && self.entry_times_requested != Some(self.entries_version))
            .then_some(self.entries_version)
    }

    pub fn mark_entry_times_requested(&mut self, version: u64) {
        self.entry_times_requested = Some(version);
    }

    /// Install stamps read in the background; stale listings are dropped.
    pub fn set_entry_times(&mut self, version: u64, times: Arc<Vec<crate::grouping::EntryTimes>>) {
        if version == self.entries_version {
            self.entry_times = Some((version, times));
            self.group_cache = None;
            self.group_revision = self.group_revision.wrapping_add(1);
        }
    }

    pub fn grouped_rows_shared(&mut self) -> Arc<Vec<GroupRow>> {
        self.visible_indices();
        self.grouped_rows.clone()
    }

    /// Replace the directory listing, invalidating the cached visible set.
    pub fn set_entries(&mut self, entries: std::sync::Arc<[FileEntry]>) {
        self.entries = entries;
        self.entries_version = self.entries_version.wrapping_add(1);
        self.visible_valid = false;
    }

    /// Returns a deduplicated sorted list of category labels for all entries.
    ///
    /// Rebuilt only when `entries_version` advances (i.e., when the directory listing
    /// changes). The `label_fn` is called only during a rebuild, not on every render.
    pub fn type_labels_cached(&mut self, label_fn: impl Fn(&FileEntry) -> String) -> &[String] {
        if self.type_labels_version != self.entries_version {
            let mut set = std::collections::BTreeSet::new();
            for entry in self.entries.iter() {
                set.insert(label_fn(entry));
            }
            self.type_labels = set.into_iter().collect();
            self.type_labels_version = self.entries_version;
        }
        &self.type_labels
    }

    pub fn install_type_labels(&mut self, labels: Vec<String>) {
        self.type_labels = labels;
        self.type_labels_version = self.entries_version;
    }

    pub fn ready_type_labels(&self) -> &[String] {
        if self.type_labels_version == self.entries_version {
            &self.type_labels
        } else {
            &[]
        }
    }

    pub fn begin_navigation(&mut self) {
        self.begin_navigation_preserving_filter(false);
    }

    pub fn begin_navigation_preserving_filter(&mut self, sticky: bool) {
        let query = (sticky && self.search.active).then(|| self.search.query.clone());
        self.listing_elapsed = None;
        // A real navigation (not a watch refresh) — the next completed load should
        // play the item appear animation.
        self.pending_appear = true;
        self.entries = std::sync::Arc::new([]);
        self.entries_version = self.entries_version.wrapping_add(1);
        self.visible_valid = false;
        self.selection.clear();
        self.filter.clear();
        self.show_files = true;
        self.show_folders = true;
        self.has_excluded_items = false;
        self.search.reset();
        self.path_edit.editing = false;
        self.path_edit.error = None;
        // The Miller view lazily re-initialises its first column from the pane path.
        self.miller = MillerState::default();
        self.tree = TreeState::default();
        if let Some(query) = query {
            self.search.active = true;
            self.search.query.clone_from(&query);
            self.filter = query;
        }
    }

    /// Flip the per-kind visibility toggles, invalidating the cached visible set.
    pub fn set_visibility(&mut self, show_files: bool, show_folders: bool) {
        if self.show_files != show_files || self.show_folders != show_folders {
            self.show_files = show_files;
            self.show_folders = show_folders;
            self.visible_valid = false;
        }
    }

    /// `shown/total` per kind as of the last `visible_indices` rebuild.
    pub fn counts(&mut self) -> VisibleCounts {
        self.visible_indices();
        self.counts
    }

    /// Indices into `entries` that pass the current `filter`, as a cached slice.
    ///
    /// The expensive part (lowercasing every entry name) only runs when the filter
    /// text, the visibility toggles or the entry set actually change; otherwise this
    /// returns the slice built on a previous frame.
    pub fn visible_indices(&mut self) -> &[usize] {
        if !self.visible_valid
            || self.visible_version != self.entries_version
            || self.visible_filter != self.filter
            || self.visible_show_files != self.show_files
            || self.visible_show_folders != self.show_folders
        {
            let needle = self.filter.to_lowercase();
            let (show_files, show_folders) = (self.show_files, self.show_folders);
            let entries = &self.entries;
            let visible = Arc::make_mut(&mut self.visible);
            visible.clear();
            let mut counts = VisibleCounts::default();
            for entry in entries.iter() {
                if matches!(entry.kind(), FileKind::Dir) {
                    counts.folders_total += 1;
                } else {
                    counts.files_total += 1;
                }
            }
            let kind_visible = |entry: &FileEntry| {
                if matches!(entry.kind(), FileKind::Dir) {
                    show_folders
                } else {
                    show_files
                }
            };
            if needle.is_empty() {
                visible.extend(
                    entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| kind_visible(e))
                        .map(|(idx, _)| idx),
                );
            } else {
                visible.extend(
                    entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| kind_visible(e))
                        .filter_map(|(idx, entry)| {
                            fuzzy_score(&needle, &entry.name_lower()).map(|_| idx)
                        }),
                );
            }
            for &idx in visible.iter() {
                counts.bytes_shown += u128::from(entries[idx].size().unwrap_or(0));
                if matches!(entries[idx].kind(), FileKind::Dir) {
                    counts.folders_shown += 1;
                } else {
                    counts.files_shown += 1;
                }
            }
            self.counts = counts;
            self.visible_filter.clear();
            self.visible_filter.push_str(&self.filter);
            self.visible_version = self.entries_version;
            self.visible_show_files = self.show_files;
            self.visible_show_folders = self.show_folders;
            self.visible_valid = true;
            self.group_cache = None;
        }
        if self.group_by == GroupBy::None {
            return &self.visible;
        }
        let today = chrono::Local::now().date_naive();
        let valid = self.group_cache.as_ref().is_some_and(
            |(version, filter, files, folders, mode, day, revision)| {
                *version == self.entries_version
                    && filter == &self.filter
                    && *files == self.show_files
                    && *folders == self.show_folders
                    && *mode == self.group_by
                    && *day == today
                    && *revision == self.group_revision
            },
        );
        if !valid {
            let times = self
                .entry_times
                .as_ref()
                .filter(|(version, _)| *version == self.entries_version)
                .map(|(_, times)| times.clone());
            let (rows, order) = crate::grouping::build_groups(
                &self.entries,
                &self.visible,
                self.group_by,
                &self.collapsed_groups,
                today,
                times.as_deref().map(Vec::as_slice),
            );
            self.grouped_rows = Arc::new(rows);
            self.grouped_visible = Arc::new(order);
            self.group_cache = Some((
                self.entries_version,
                self.filter.clone(),
                self.show_files,
                self.show_folders,
                self.group_by,
                today,
                self.group_revision,
            ));
        }
        &self.grouped_visible
    }

    /// Cheap render snapshot of the cached visible indices. Keeping this behind an
    /// `Arc` avoids copying the full directory index every frame in virtualized views.
    pub fn visible_indices_shared(&mut self) -> Arc<Vec<usize>> {
        self.visible_indices();
        if self.group_by == GroupBy::None {
            self.visible.clone()
        } else {
            self.grouped_visible.clone()
        }
    }

    /// Publish a completed off-thread folder filter without making the next UI
    /// render repeat the scan/sort that produced it.
    pub fn set_visible_filter_result(
        &mut self,
        filter: String,
        visible: Vec<usize>,
        counts: VisibleCounts,
    ) {
        self.visible = Arc::new(visible);
        self.visible_filter = filter;
        self.visible_version = self.entries_version;
        self.visible_show_files = self.show_files;
        self.visible_show_folders = self.show_folders;
        self.counts = counts;
        self.visible_valid = true;
        self.group_cache = None;
    }

    /// Flattened list of visible nodes `(entry: &FileEntry, depth: usize, is_expanded: bool, has_children: bool)`
    /// respecting expanded folders and search filters, ideal for `uniform_list`.
    pub fn visible_tree_rows(&mut self) -> Vec<(&FileEntry, usize, bool, bool)> {
        self.visible_indices();
        self.visible_tree_rows_snapshot()
    }

    /// Read-only projection for selection/preview queries between render passes.
    /// Recompute only when the cached root filter no longer describes this state.
    pub fn visible_tree_rows_snapshot(&self) -> Vec<(&FileEntry, usize, bool, bool)> {
        let fresh;
        let indices = if self.visible_valid
            && self.visible_version == self.entries_version
            && self.visible_filter == self.filter
            && self.visible_show_files == self.show_files
            && self.visible_show_folders == self.show_folders
        {
            &*self.visible
        } else {
            fresh = filter_visible_entries(
                &self.entries,
                &self.filter,
                self.show_files,
                self.show_folders,
            )
            .0;
            &fresh
        };
        let mut rows = Vec::new();
        let mut visited = HashSet::new();

        for &idx in indices {
            let entry = &self.entries[idx];
            let is_dir = matches!(entry.kind(), FileKind::Dir);
            let is_expanded = is_dir && self.tree.is_expanded(&entry.path());
            let has_children = if is_dir {
                match self.tree.children.get(&entry.path()) {
                    Some(children) => !children.is_empty(),
                    None => true,
                }
            } else {
                false
            };
            rows.push((entry, 0, is_expanded, has_children));

            if is_expanded && visited.insert(entry.path().clone()) {
                self.push_visible_tree_children(&entry.path(), 1, &mut visited, &mut rows);
                visited.remove(&entry.path());
            }
        }

        rows
    }

    fn push_visible_tree_children<'a>(
        &'a self,
        parent_path: &std::path::Path,
        depth: usize,
        visited: &mut HashSet<std::path::PathBuf>,
        rows: &mut Vec<(&'a FileEntry, usize, bool, bool)>,
    ) {
        let Some(children) = self.tree.children.get(parent_path) else {
            return;
        };
        let needle = self.filter.to_lowercase();
        let (show_files, show_folders) = (self.show_files, self.show_folders);
        let kind_visible = |entry: &FileEntry| {
            if matches!(entry.kind(), FileKind::Dir) {
                show_folders
            } else {
                show_files
            }
        };

        for child in children.iter() {
            if !kind_visible(child) {
                continue;
            }
            if !needle.is_empty() && fuzzy_score(&needle, &child.name_lower()).is_none() {
                continue;
            }
            let is_dir = matches!(child.kind(), FileKind::Dir);
            let is_expanded = is_dir && self.tree.is_expanded(&child.path());
            let has_children = if is_dir {
                match self.tree.children.get(&child.path()) {
                    Some(c) => !c.is_empty(),
                    None => true,
                }
            } else {
                false
            };
            rows.push((child, depth, is_expanded, has_children));

            if is_expanded && visited.insert(child.path().clone()) {
                self.push_visible_tree_children(&child.path(), depth + 1, visited, rows);
                visited.remove(&child.path());
            }
        }
    }
}

/// CPU-only folder filtering used by the UI worker. Keeping it in the model
/// makes the background and synchronous paths preserve the same folder order.
pub fn filter_visible_entries(
    entries: &[FileEntry],
    filter: &str,
    show_files: bool,
    show_folders: bool,
) -> (Vec<usize>, VisibleCounts) {
    let needle = filter.to_lowercase();
    let mut counts = VisibleCounts::default();
    let kind_visible = |entry: &FileEntry| {
        if matches!(entry.kind(), FileKind::Dir) {
            show_folders
        } else {
            show_files
        }
    };
    for entry in entries {
        if matches!(entry.kind(), FileKind::Dir) {
            counts.folders_total += 1;
        } else {
            counts.files_total += 1;
        }
    }

    let visible: Vec<usize> = if needle.is_empty() {
        entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| kind_visible(entry))
            .map(|(idx, _)| idx)
            .collect()
    } else {
        entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| kind_visible(entry))
            .filter_map(|(idx, entry)| fuzzy_score(&needle, &entry.name_lower()).map(|_| idx))
            .collect()
    };

    for &idx in &visible {
        counts.bytes_shown += u128::from(entries[idx].size().unwrap_or(0));
        if matches!(entries[idx].kind(), FileKind::Dir) {
            counts.folders_shown += 1;
        } else {
            counts.files_shown += 1;
        }
    }
    (visible, counts)
}

#[cfg(test)]
mod tests {
    #[test]
    fn list_keyboard_shift_selects_a_rectangle_and_contracts_from_fixed_anchor() {
        let visible = [10, 11, 12, 20, 21, 22, 30, 31];
        let mut selection = super::SelectionState::default();
        selection.select(11, &visible, false, false);
        selection.navigate(21, &visible, true, false, Some(3));
        assert_eq!(selection.selected, [11, 21].into());
        selection.navigate(22, &visible, true, false, Some(3));
        assert_eq!(selection.selected, [11, 12, 21, 22].into());
        selection.navigate(31, &visible, true, false, Some(3));
        assert_eq!(selection.selected, [11, 21, 31].into());
        selection.navigate(10, &visible, true, false, Some(3));
        assert_eq!(selection.selected, [10, 11].into());
        assert_eq!(selection.selection_anchor, Some(11));
        selection.select(12, &visible, false, false);
        selection.navigate(31, &visible, true, false, Some(3));
        assert_eq!(selection.selected, [11, 12, 21, 22, 31].into());
    }

    #[test]
    fn keyboard_ctrl_moves_only_cursor_and_ctrl_shift_adds_range() {
        let visible = [0, 1, 2, 3, 4];
        let mut selection = super::SelectionState::default();
        selection.select(0, &visible, false, false);
        selection.navigate(2, &visible, false, true, None);
        assert_eq!(selection.selected, [0].into());
        assert_eq!(selection.keyboard_cursor, Some(2));
        selection.select(2, &visible, false, true);
        selection.navigate(4, &visible, true, true, None);
        assert_eq!(selection.selected, [0, 2, 3, 4].into());
        selection.navigate(3, &visible, true, false, None);
        assert_eq!(selection.selected, [2, 3].into());
    }

    #[test]
    fn clear_resets_every_focus_and_rename_state_before_next_shift() {
        let visible = [0, 1, 2];
        let mut selection = super::SelectionState::default();
        selection.select(0, &visible, false, false);
        selection.navigate(2, &visible, true, false, None);
        selection.last_pointer_click = Some((2, std::time::Instant::now()));
        selection.clear();
        assert!(selection.selected.is_empty());
        assert!(selection.keyboard_cursor.is_none());
        assert!(selection.selection_anchor.is_none());
        assert!(selection.last_clicked_index.is_none());
        assert!(selection.last_pointer_click.is_none());
        assert!(!selection.scroll_to_selected);
        selection.navigate(1, &visible, true, false, None);
        assert_eq!(selection.selected, [1].into());
        selection.navigate(2, &visible, true, false, None);
        assert_eq!(selection.selected, [1, 2].into());
    }

    #[test]
    fn shift_uses_keyboard_cursor_when_anchor_is_missing_and_ignores_stale_hits() {
        let visible = [0, 1, 2];
        let mut selection = super::SelectionState::default();
        selection.keyboard_cursor = Some(0);
        selection.select(2, &visible, true, false);
        assert_eq!(selection.selected, [0, 1, 2].into());
        assert_eq!(selection.selection_anchor, Some(0));
        selection.select(9, &visible, false, false);
        assert_eq!(selection.selected, [0, 1, 2].into());
        assert_eq!(selection.keyboard_cursor, Some(2));
    }

    #[test]
    fn grouping_collapsed_buckets_are_excluded_from_keyboard_and_shift_selection() {
        let mut tab = super::TabState::default();
        tab.set_entries(
            vec![
                entry("zeta", FileKind::File),
                entry("alpha", FileKind::File),
                entry("aardvark", FileKind::File),
            ]
            .into(),
        );
        tab.set_grouping(super::GroupBy::Name, Default::default());
        assert_eq!(tab.visible_indices(), &[1, 2, 0]);
        assert_eq!(tab.grouped_row_for_entry(0), Some(4));
        let original = tab.grouped_rows_shared();
        assert!(std::sync::Arc::ptr_eq(
            &original,
            &tab.grouped_rows_shared()
        ));
        tab.set_grouping(super::GroupBy::Name, ["name:A-H".to_owned()].into());
        assert_eq!(tab.visible_indices(), &[0]);
        let visible = tab.visible_indices_shared();
        tab.selection.select(0, &visible, true, false);
        assert_eq!(tab.selection.selected, [0].into());
        assert_eq!(tab.grouped_row_for_entry(1), None);
        assert_eq!(tab.grouped_rows_shared().len(), 3);
        tab.filter = "alpha".into();
        assert!(tab.visible_indices().is_empty());
        assert_eq!(tab.grouped_rows_shared().len(), 1);
        tab.set_grouping(super::GroupBy::None, Default::default());
        assert_eq!(tab.visible_indices(), &[1]);
    }
    #[test]
    fn slow_click_obeys_timing_identity_and_modifier_boundaries() {
        use std::time::{Duration, Instant};
        for (delay, expected) in [
            (499, false),
            (500, true),
            (750, true),
            (1200, true),
            (1201, false),
        ] {
            let mut selection = super::SelectionState::<usize>::default();
            let start = Instant::now();
            assert!(!selection.slow_rename_click(7, start, true));
            assert_eq!(
                selection.slow_rename_click(7, start + Duration::from_millis(delay), true),
                expected
            );
        }
        let mut selection = super::SelectionState::<usize>::default();
        let start = Instant::now();
        selection.slow_rename_click(7, start, true);
        assert!(!selection.slow_rename_click(8, start + Duration::from_millis(750), true));
        assert!(!selection.slow_rename_click(8, start + Duration::from_millis(1500), false));
        assert!(selection.last_pointer_click.is_none());
    }
    #[test]
    fn header_select_all_uses_only_filtered_visible_items_and_toggles_off() {
        let mut selection = super::SelectionState::<usize>::default();
        selection.selected.extend([1, 9]);
        let visible = [7, 1, 4];
        assert!(!selection.all_visible_selected(&visible));
        selection.toggle_all_visible(&visible);
        assert_eq!(
            selection.selected,
            std::collections::HashSet::from([7, 1, 4])
        );
        assert_eq!(selection.keyboard_cursor, Some(7));
        assert!(selection.all_visible_selected(&visible));
        selection.toggle_all_visible(&visible);
        assert!(selection.selected.is_empty());
        assert!(!selection.all_visible_selected(&[]));
        selection.selected.insert(9);
        selection.toggle_all_visible(&[]);
        assert!(selection.selected.contains(&9));
    }
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn selection_follows_identity_and_collapsing_hands_it_to_the_folder() {
        let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        let old = paths(&["a", "dir", "dir/x", "dir/y", "z"]);
        let mut selection = SelectionState::default();
        selection.selected = [2, 4].into_iter().collect();
        selection.keyboard_cursor = Some(2);
        selection.selection_anchor = Some(4);

        // Re-sort: same files, new positions.
        let resorted = paths(&["z", "dir", "dir/y", "dir/x", "a"]);
        let next = selection.remap_by_path(&old, &resorted);
        assert_eq!(next.selected, [3, 0].into_iter().collect());
        assert_eq!(next.keyboard_cursor, Some(3));
        assert_eq!(next.selection_anchor, Some(0));
        assert!(!next.scroll_to_selected);

        // Collapse `dir`: its selected child hands the selection to `dir`.
        let collapsed = paths(&["a", "dir", "z"]);
        let next = selection.remap_by_path(&old, &collapsed);
        assert_eq!(next.selected, [1, 2].into_iter().collect());
        assert_eq!(next.keyboard_cursor, Some(1));
        assert!(next.scroll_to_selected);

        // Flat delete: no listed ancestor, the item just drops.
        let deleted = paths(&["a", "dir", "z"]);
        let flat_old = paths(&["a", "dir", "gone", "z"]);
        let mut flat = SelectionState::default();
        flat.selected = [2].into_iter().collect();
        let next = flat.remap_by_path(&flat_old, &deleted);
        assert!(next.selected.is_empty());
        assert_eq!(next.keyboard_cursor, None);
    }

    #[test]
    fn selection_shift_keeps_anchor_and_follows_display_order() {
        let visible = [8, 2, 5, 1, 7];
        let mut selection = SelectionState::default();
        selection.select(2, &visible, false, false);
        selection.select(7, &visible, true, false);
        assert_eq!(selection.selected, HashSet::from([2, 5, 1, 7]));
        selection.select(5, &visible, true, false);
        assert_eq!(selection.selected, HashSet::from([2, 5]));
        assert_eq!(selection.selection_anchor, Some(2));
    }

    #[test]
    fn selection_ctrl_shift_adds_range_and_ctrl_toggles() {
        let visible = [0, 1, 2, 3, 4];
        let mut selection = SelectionState::default();
        selection.select(0, &visible, false, false);
        selection.select(2, &visible, false, true);
        selection.select(4, &visible, true, true);
        assert_eq!(selection.selected, HashSet::from([0, 2, 3, 4]));
        selection.select(3, &visible, false, true);
        assert_eq!(selection.selected, HashSet::from([0, 2, 4]));
    }

    #[test]
    fn selection_dropzone_paths_use_the_same_range_rules() {
        let visible: Vec<_> = ["z.txt", "a.txt", "m.txt", "b.txt"]
            .map(PathBuf::from)
            .into();
        let mut selection = SelectionState::default();
        selection.select(visible[0].clone(), &visible, false, false);
        selection.select(visible[3].clone(), &visible, true, false);
        assert_eq!(selection.selected.len(), 4);
        selection.select(visible[1].clone(), &visible, true, false);
        assert_eq!(selection.selected, visible[..2].iter().cloned().collect());
        selection.select(visible[0].clone(), &visible, false, true);
        assert_eq!(selection.selected, HashSet::from([visible[1].clone()]));
    }

    #[test]
    fn selection_removed_anchor_does_not_leave_a_broken_shift_range() {
        let mut selection = SelectionState::default();
        selection.select(0, &[0, 1, 2], false, false);
        selection.select(2, &[1, 2], true, false);
        assert_eq!(selection.selected, HashSet::from([2]));
        selection.clear();
        assert!(selection.selection_anchor.is_none());
        assert!(selection.keyboard_cursor.is_none());
    }

    fn entry(name: &str, kind: FileKind) -> FileEntry {
        FileEntry::from(crate::file_entry::FileEntryData {
            name: name.into(),
            path: std::path::PathBuf::from(name),
            kind,
            size: None,
            modified: None,
            _is_hidden: false,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        })
    }

    #[test]
    fn miller_refresh_preserves_selected_paths_and_drops_removed_entries() {
        let mut col = MillerColumn::new(Location::Directory(std::path::PathBuf::from(".")));
        col.set_entries(vec![entry("a", FileKind::File), entry("b", FileKind::File)].into());
        col.selection.selected.extend([0, 1]);
        col.selection.keyboard_cursor = Some(1);
        col.set_entries(
            vec![
                entry("0", FileKind::File),
                entry("b", FileKind::File),
                entry("a", FileKind::File),
            ]
            .into(),
        );
        assert_eq!(col.selection.selected, HashSet::from([1, 2]));
        assert_eq!(col.selection.keyboard_cursor, Some(1));
        col.set_entries(vec![entry("a", FileKind::File)].into());
        assert_eq!(col.selection.selected, HashSet::from([0]));
        assert_eq!(col.selection.keyboard_cursor, None);
        assert_eq!(col.selection.selection_anchor, None);
    }

    #[test]
    fn miller_filter_keeps_storage_order_indices_counts_and_cached_frames_in_sync() {
        let mut column = MillerColumn::new(Location::Directory("/".into()));
        column.set_entries(
            vec![
                entry("zphoto.txt", FileKind::File),
                entry("photo-dir", FileKind::Dir),
                entry("photo.txt", FileKind::File),
                entry("unmatched.txt", FileKind::File),
            ]
            .into(),
        );
        column.selection.selected = HashSet::from([0, 1, 2, 3]);
        column.selection.keyboard_cursor = Some(1);
        column.refresh_visibility("photo", true, false);
        let first = column.visible_indices("photo", true, false);
        assert_eq!(*first, vec![0, 2]);
        assert_eq!(column.selection.selected, HashSet::from([0, 2]));
        assert_eq!(column.selection.keyboard_cursor, None);
        let counts = column.visible_counts("photo", true, false);
        assert_eq!(
            (
                counts.files_shown,
                counts.files_total,
                counts.folders_shown,
                counts.folders_total
            ),
            (2, 3, 0, 1)
        );
        for _ in 0..100 {
            column.refresh_visibility("photo", true, false);
            assert!(Arc::ptr_eq(
                &first,
                &column.visible_indices("photo", true, false)
            ));
        }
        column.set_entries(
            vec![
                entry("photo.txt", FileKind::File),
                entry("photo-dir", FileKind::Dir),
                entry("zphoto.txt", FileKind::File),
            ]
            .into(),
        );
        column.refresh_visibility("photo", true, false);
        assert_eq!(*column.visible_indices("photo", true, false), vec![0, 2]);
        assert!(!Arc::ptr_eq(
            &first,
            &column.visible_indices("photo", true, false)
        ));
        column.refresh_visibility("photo", false, true);
        assert_eq!(*column.visible_indices("photo", false, true), vec![1]);
        assert!(column.selection.selected.is_empty());
    }

    #[test]
    fn miller_operations_and_prefix_selection_never_use_filtered_out_rows() {
        use crate::settings::ViewMode;
        let mut state = TabState::default();
        let mut column = MillerColumn::new(Location::Directory("/".into()));
        column.set_entries(
            vec![
                entry("alpha-dir", FileKind::Dir),
                entry("alpha.txt", FileKind::File),
                entry("another.txt", FileKind::File),
            ]
            .into(),
        );
        column.selection.selected = HashSet::from([0, 1, 2]);
        state.miller.columns.push(column);
        state.filter = "alpha".into();
        state.show_folders = false;
        // Even before a render refresh, operation queries must use the latest
        // filter instead of an earlier frame's selection.
        assert_eq!(
            state
                .selected_entries(&ViewMode::Miller)
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha.txt"]
        );
        assert!(state.select_typed_prefix(&ViewMode::Miller, "a", false));
        assert_eq!(state.miller.columns[0].selection.keyboard_cursor, Some(1));
        state.refresh_miller_visibility();
        state.filter = "no match".into();
        state.refresh_miller_visibility();
        assert!(state.miller_visible_indices(0).is_empty());
        assert!(state.selected_entries(&ViewMode::Miller).is_empty());
        assert!(!state.select_typed_prefix(&ViewMode::Miller, "a", false));
    }

    #[test]
    fn typed_selection_respects_the_same_metadata_projection_as_the_details_view() {
        use crate::settings::ViewMode;
        let mut state = TabState::default();
        state.set_entries(
            vec![
                entry("alpha-hidden.txt", FileKind::File),
                entry("alpha-visible.txt", FileKind::File),
            ]
            .into(),
        );
        assert!(state.select_typed_prefix_with_indices(&ViewMode::Details, "a", false, Some(&[1])));
        assert_eq!(state.selection.keyboard_cursor, Some(1));
        assert!(!state.select_typed_prefix_with_indices(&ViewMode::Details, "a", false, Some(&[])));
    }

    #[test]
    fn visibility_toggles_filter_by_kind_and_feed_counts() {
        let mut ts = TabState::default();
        ts.set_entries(
            vec![
                entry("docs", FileKind::Dir),
                entry("a.txt", FileKind::File),
                entry("b.txt", FileKind::File),
            ]
            .into(),
        );

        assert_eq!(ts.visible_indices(), &[0, 1, 2]);
        let c = ts.counts();
        assert_eq!((c.folders_shown, c.folders_total), (1, 1));
        assert_eq!((c.files_shown, c.files_total), (2, 2));

        ts.set_visibility(true, false);
        assert_eq!(ts.visible_indices(), &[1, 2]);
        let c = ts.counts();
        assert_eq!((c.folders_shown, c.folders_total), (0, 1));

        ts.set_visibility(false, true);
        assert_eq!(ts.visible_indices(), &[0]);
        let c = ts.counts();
        assert_eq!((c.files_shown, c.files_total), (0, 2));
    }

    #[test]
    fn visibility_combines_with_fuzzy_filter() {
        let mut ts = TabState::default();
        ts.set_entries(
            vec![
                entry("textures", FileKind::Dir),
                entry("notes.txt", FileKind::File),
                entry("image.png", FileKind::File),
            ]
            .into(),
        );
        ts.filter = "txt".to_string();
        // "txt" fuzzy-matches both "textures" (t-x-t) and "notes.txt".
        assert_eq!(ts.visible_indices().len(), 2);

        ts.set_visibility(true, false);
        assert_eq!(ts.visible_indices(), &[1]);
        let c = ts.counts();
        assert_eq!(c.files_shown, 1);
        assert_eq!(c.folders_shown, 0);
    }

    #[test]
    fn quick_filter_preserves_folder_sort_order_in_foreground_and_worker() {
        let mut ts = TabState::default();
        ts.set_entries(
            vec![
                entry("holiday-photo.jpg", FileKind::File),
                entry("notes.txt", FileKind::File),
                entry("photo.jpg", FileKind::File),
            ]
            .into(),
        );
        ts.filter = "photo".into();
        // A prefix scores higher, but filtering must not move it above the
        // earlier file in the directory's existing date/size/name sort order.
        assert_eq!(ts.visible_indices(), &[0, 2]);
        let (visible, _) = super::filter_visible_entries(&ts.entries, "photo", true, true);
        assert_eq!(visible, [0, 2]);
    }

    #[test]
    fn navigation_resets_kind_visibility() {
        let mut ts = TabState::default();
        ts.set_visibility(true, false);

        ts.begin_navigation();

        ts.set_entries(vec![entry("docs", FileKind::Dir), entry("a.txt", FileKind::File)].into());
        assert_eq!(ts.visible_indices(), &[0, 1]);
    }

    #[test]
    fn sticky_filter_applies_to_the_new_directory_and_invalidates_old_search_results() {
        let mut ts = TabState::default();
        ts.search.active = true;
        ts.search.query = ".rs".into();
        ts.search.recursive = true;
        ts.search.generation = 17;
        ts.filter = ".rs".into();
        ts.begin_navigation_preserving_filter(true);
        ts.set_entries(
            vec![
                entry("main.rs", FileKind::File),
                entry("README.md", FileKind::File),
            ]
            .into(),
        );
        assert_eq!(ts.search.query, ".rs");
        assert_eq!(ts.visible_indices(), &[0]);
        assert!(ts.search.active && ts.search.recursive);
        assert_ne!(ts.search.generation, 17);
        let previous = ts.search.generation;
        ts.begin_navigation();
        assert!(ts.filter.is_empty() && ts.search.query.is_empty() && !ts.search.active);
        assert_ne!(ts.search.generation, previous);
    }

    #[test]
    fn typed_selection_cycles_visible_items_without_filtering_the_listing() {
        use crate::settings::ViewMode;
        let mut ts = TabState::default();
        ts.set_entries(
            vec![
                entry("alpha", FileKind::File),
                entry("beta", FileKind::File),
                entry("alpine", FileKind::File),
            ]
            .into(),
        );
        assert!(ts.select_typed_prefix(&ViewMode::Details, "AL", false));
        assert_eq!(ts.selection.keyboard_cursor, Some(0));
        assert!(ts.select_typed_prefix(&ViewMode::Details, "al", true));
        assert_eq!(ts.selection.keyboard_cursor, Some(2));
        assert!(ts.select_typed_prefix(&ViewMode::Details, "al", true));
        assert_eq!(ts.selection.keyboard_cursor, Some(0));
        assert!(!ts.select_typed_prefix(&ViewMode::Details, "missing", false));
        assert_eq!(ts.visible_indices(), &[0, 1, 2]);
        assert!(ts.filter.is_empty() && !ts.search.active);
    }

    #[test]
    fn typed_selection_uses_expanded_tree_rows_and_the_focused_miller_column() {
        use crate::settings::ViewMode;
        let mut ts = TabState::default();
        let folder = entry("folder", FileKind::Dir);
        ts.set_entries(vec![folder.clone()].into());
        ts.tree.expanded.insert(folder.path());
        ts.tree.children.insert(
            folder.path(),
            vec![entry("needle.txt", FileKind::File)].into(),
        );
        assert!(ts.select_typed_prefix(&ViewMode::Tree, "needle", false));
        assert_eq!(ts.selection.keyboard_cursor, Some(1));
        let mut column =
            super::MillerColumn::new(crate::location::Location::Directory("/other".into()));
        column.entries = vec![
            entry("other", FileKind::Dir),
            entry("needle.txt", FileKind::File),
        ]
        .into();
        ts.miller.columns.push(column);
        assert!(ts.select_typed_prefix(&ViewMode::Miller, "needle", false));
        assert_eq!(ts.miller.columns[0].selection.keyboard_cursor, Some(1));
        assert_eq!(ts.miller.columns[0].selection.selected, [1].into());
    }

    #[test]
    fn tree_state_expand_toggle() {
        let mut tree = TreeState::default();
        let path = std::path::PathBuf::from("/home/user/docs");
        assert!(!tree.is_expanded(&path));
        tree.toggle_expand(&path);
        assert!(tree.is_expanded(&path));
        tree.toggle_expand(&path);
        assert!(!tree.is_expanded(&path));
    }

    #[test]
    fn visible_tree_rows_flattens_hierarchy() {
        let mut ts = TabState::default();
        let docs = entry("docs", FileKind::Dir);
        let file1 = entry("file1.txt", FileKind::File);
        let docs_path = docs.path().clone();

        let child1 = FileEntry::from(crate::file_entry::FileEntryData {
            name: "subfolder".into(),
            path: docs_path.join("subfolder"),
            kind: FileKind::Dir,
            size: None,
            modified: None,
            _is_hidden: false,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        });
        let child2 = FileEntry::from(crate::file_entry::FileEntryData {
            name: "inner.md".into(),
            path: docs_path.join("inner.md"),
            kind: FileKind::File,
            size: Some(10),
            modified: None,
            _is_hidden: false,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        });

        ts.set_entries(vec![docs.clone(), file1.clone()].into());
        ts.tree.children.insert(
            docs_path.clone(),
            vec![child1.clone(), child2.clone()].into(),
        );

        // Collapsed docs folder:
        let rows = ts.visible_tree_rows();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0.name, "docs");
        assert_eq!(rows[0].1, 0); // depth
        assert_eq!(rows[0].2, false); // is_expanded
        assert_eq!(rows[0].3, true); // has_children
        assert_eq!(rows[1].0.name, "file1.txt");
        assert_eq!(rows[1].1, 0);
        assert_eq!(rows[1].2, false);
        assert_eq!(rows[1].3, false);

        // Expand docs folder:
        ts.tree.toggle_expand(&docs_path);
        let rows = ts.visible_tree_rows();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].0.name, "docs");
        assert_eq!(rows[0].1, 0);
        assert_eq!(rows[0].2, true);
        assert_eq!(rows[1].0.name, "subfolder");
        assert_eq!(rows[1].1, 1); // depth 1
        assert_eq!(rows[1].2, false);
        assert_eq!(rows[2].0.name, "inner.md");
        assert_eq!(rows[2].1, 1);
        assert_eq!(rows[3].0.name, "file1.txt");
        assert_eq!(rows[3].1, 0);
    }
}

fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }

    let mut score = 0_i64;
    let mut last_match: Option<usize> = None;
    let mut search_from = 0;

    for q in query.chars().filter(|c| !c.is_whitespace()) {
        let hay = &candidate[search_from..];
        let offset = hay.find(q)?;
        let pos = search_from + offset;
        if pos == 0 {
            score += 24;
        }
        if candidate[..pos].ends_with(['-', '_', '.', ' ', '/', '\\']) {
            score += 12;
        }
        if let Some(prev) = last_match {
            let gap = pos.saturating_sub(prev + 1);
            score += 20_i64.saturating_sub(i64::try_from(gap).unwrap_or(20));
        }
        score += 4;
        last_match = Some(pos);
        search_from = pos + q.len_utf8();
    }

    if candidate.starts_with(query) {
        score += 60;
    } else if candidate.contains(query) {
        score += 30;
    }

    score -= i64::try_from(candidate.len()).unwrap_or(i64::MAX / 2) / 8;
    Some(score)
}
