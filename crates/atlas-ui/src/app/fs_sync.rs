use crate::app::App;
use atlas_core::fs::list::{
    entries_for_paths, list_branch_dir_with_preview, list_dir, list_dir_with_preview,
};
use atlas_model::app_state::BgResult;
use atlas_model::layout_tree::{Tile, TileId};
use atlas_model::location::Location;
use atlas_model::pane::FolderOrder;
use atlas_model::tab::LoadState;
use atlas_model::tag_cache::TagId;
use std::path::PathBuf;

pub(super) async fn has_excluded_directory_items(
    path: &std::path::Path,
    show_hidden: bool,
    empty: bool,
) -> bool {
    if !empty || show_hidden {
        return false;
    }
    match tokio::fs::read_dir(path).await {
        Ok(mut directory) => directory.next_entry().await.ok().flatten().is_some(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod exclusion_tests {
    use super::*;
    #[tokio::test]
    async fn empty_listing_distinguishes_a_truly_empty_directory_from_excluded_items() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("atlas-empty-view-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        assert!(!has_excluded_directory_items(&root, false, true).await);
        let hidden = root.join(".hidden.txt");
        std::fs::write(&hidden, []).unwrap();
        assert!(has_excluded_directory_items(&root, false, true).await);
        assert!(!has_excluded_directory_items(&root, true, true).await);
        assert!(!has_excluded_directory_items(&root, false, false).await);
        std::fs::remove_file(hidden).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}

impl App {
    /// Read created/accessed stamps for a pane grouped by those dates. The
    /// compact entry only keeps its modification time, and a stat per file must
    /// never run on the UI thread.
    pub(crate) fn request_group_times(&mut self, pane: TileId) {
        let Some(ts) = self.state.tab_states.get_mut(&pane) else {
            return;
        };
        let Some(version) = ts.entry_times_needed() else {
            return;
        };
        ts.mark_entry_times_requested(version);
        let paths: Vec<Option<PathBuf>> = ts
            .entries
            .iter()
            .map(|entry| {
                (entry.archive_root().is_none() && entry.recycle_info().is_none())
                    .then(|| entry.path())
            })
            .collect();
        let tx = self.tx.clone();
        self.rt.spawn_blocking(move || {
            #[allow(clippy::disallowed_methods)]
            // reason: runs on the blocking pool, off the UI thread
            let times = paths
                .into_iter()
                .map(|path| {
                    let meta = path.and_then(|path| std::fs::metadata(path).ok());
                    (
                        meta.as_ref().and_then(|meta| meta.created().ok()),
                        meta.as_ref().and_then(|meta| meta.accessed().ok()),
                    )
                })
                .collect();
            let _ = tx.send(BgResult::GroupTimes {
                tab: pane,
                version,
                times,
            });
        });
    }

    pub(crate) fn spawn_list_dir(
        &mut self,
        tab_id: TileId,
        path: PathBuf,
        sort_col: atlas_model::file_entry::SortColumn,
        sort_dir: atlas_model::file_entry::SortDir,
    ) {
        if atlas_core::vault::live::is_private(&path) && !self.owns_native_vault_path(&path) {
            return;
        }
        let folder_order = self
            .tab_data_for(tab_id)
            .map_or_else(atlas_model::pane::FolderOrder::default, |td| {
                td.folder_order
            });
        let is_branch_view = self
            .tab_data_for(tab_id)
            .is_some_and(|td| td.is_branch_view);
        let op_id = self.state.next_op_id();
        let ts = self.state.tab_states.entry(tab_id).or_default();
        ts.load_state = LoadState::Loading;
        ts.last_nav_op_id = op_id;

        let tx = self.tx.clone();
        let show_hidden = self.state.settings.show_hidden;
        self.rt.spawn(async move {
            let started = std::time::Instant::now();
            let preview_tx = tx.clone();
            let res = if is_branch_view {
                list_branch_dir_with_preview(
                    &path,
                    show_hidden,
                    sort_col,
                    sort_dir,
                    move |entries| {
                        let _ = preview_tx.send(BgResult::DirListPreview {
                            tab: tab_id,
                            op_id,
                            entries,
                        });
                    },
                )
                .await
            } else {
                list_dir_with_preview(
                    &path,
                    show_hidden,
                    sort_col,
                    sort_dir,
                    folder_order,
                    move |entries| {
                        let _ = preview_tx.send(BgResult::DirListPreview {
                            tab: tab_id,
                            op_id,
                            entries,
                        });
                    },
                )
                .await
            };
            match res {
                Ok(entries) => {
                    let _ = tx.send(BgResult::DirListTiming {
                        tab: tab_id,
                        op_id,
                        elapsed: started.elapsed(),
                    });
                    // Only an empty filtered listing needs this extra query. It
                    // reads a directory record, never the contents of a file.
                    let has_excluded_items =
                        has_excluded_directory_items(&path, show_hidden, entries.is_empty()).await;
                    let _ = tx.send(BgResult::DirListExclusions {
                        tab: tab_id,
                        op_id,
                        has_excluded_items,
                    });
                    let _ = tx.send(BgResult::DirListed {
                        tab: tab_id,
                        op_id,
                        entries,
                    });
                }
                Err(e) => {
                    let _ = tx.send(BgResult::DirListError {
                        tab: tab_id,
                        op_id,
                        error: e.to_string(),
                    });
                }
            }
        });
    }

    /// Re-sort the already-loaded listing of `tab_id` in place (no re-listing),
    /// remapping the selection by path. Used by the folders-first toggle so the
    /// pane reorders instantly.
    pub(crate) fn resort_entries_in_place(
        &mut self,
        tab_id: TileId,
        sort_col: &atlas_model::file_entry::SortColumn,
        sort_dir: &atlas_model::file_entry::SortDir,
        folder_order: FolderOrder,
    ) {
        // Selection continuity (flat or tree index space) is the model invariant.
        self.mutate_tab(tab_id, |ts| {
            let mut entries = std::mem::take(&mut ts.entries).to_vec();
            atlas_core::fs::list::sort_entries(&mut entries, sort_col, sort_dir, folder_order);
            ts.set_entries(entries.into());
            for column in &mut ts.miller.columns {
                let mut entries = column.entries.to_vec();
                atlas_core::fs::list::sort_entries(&mut entries, sort_col, sort_dir, folder_order);
                column.set_entries(entries.into());
            }
            for children in ts.tree.children.values_mut() {
                atlas_core::fs::list::sort_entries(
                    std::sync::Arc::make_mut(children),
                    sort_col,
                    sort_dir,
                    folder_order,
                );
            }
        });
    }

    /// Ensure the tag cache has association data for every currently-selected path, so
    /// the inspector can show their tags. Queries are sent only for paths we don't yet
    /// know about; an empty placeholder is inserted immediately to avoid re-querying
    /// the same paths every frame while the worker's answer is in flight.
    pub(crate) fn sync_selection_tags(&mut self) {
        let unknown: Vec<PathBuf> = self
            .state
            .selected_paths()
            .into_iter()
            .filter(|p| !self.state.tag_cache.knows(p))
            .collect();
        if unknown.is_empty() {
            return;
        }
        for p in &unknown {
            self.state.tag_cache.for_path.entry(p.clone()).or_default();
        }
        let _ = self
            .tag_db_tx
            .send(atlas_core::fs::tag_db::TagDbCmd::QueryTagsForPaths(unknown));
    }

    /// Rebuild the listing of every tab showing results for `tag` from the paths the
    /// tag-db worker just returned. Mirrors `spawn_list_dir` but sources entries from an
    /// explicit path set instead of a `read_dir`, and reuses `BgResult::DirListed` so the
    /// views update through the exact same path as a normal directory load.
    pub(crate) fn populate_tag_tabs(&mut self, tag: TagId, paths: Vec<PathBuf>) {
        let targets: Vec<(
            TileId,
            atlas_model::file_entry::SortColumn,
            atlas_model::file_entry::SortDir,
            FolderOrder,
        )> = self
            .state
            .pane_tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| match tile {
                Tile::Pane(p) if p.location == Location::Tag(tag) => {
                    Some((*id, p.sort_col.clone(), p.sort_dir.clone(), p.folder_order))
                }
                _ => None,
            })
            .collect();

        for (tab_id, sort_col, sort_dir, folder_order) in targets {
            let op_id = self.state.next_op_id();
            let ts = self.state.tab_states.entry(tab_id).or_default();
            ts.load_state = LoadState::Loading;
            ts.last_nav_op_id = op_id;

            let tx = self.tx.clone();
            let show_hidden = self.state.settings.show_hidden;
            let paths = paths.clone();
            self.rt.spawn(async move {
                let entries =
                    entries_for_paths(&paths, show_hidden, sort_col, sort_dir, folder_order).await;

                let _ = tx.send(BgResult::DirListed {
                    tab: tab_id,
                    op_id,
                    entries,
                });
            });
        }
    }

    /// Load one Miller column's listing in the background.
    pub(crate) fn spawn_miller_list(&mut self, tile: TileId, location: Location) {
        let op_id = self.state.next_op_id();
        let show_hidden = self.state.settings.show_hidden;
        let (col, dir, folder_order) = self
            .tab_data_for(tile)
            .map(|td| (td.sort_col.clone(), td.sort_dir.clone(), td.folder_order))
            .unwrap_or((
                Default::default(),
                Default::default(),
                FolderOrder::default(),
            ));
        if let Some(ts) = self.state.tab_states.get_mut(&tile) {
            if let Some(c) = ts
                .miller
                .columns
                .iter_mut()
                .find(|c| c.location == location)
            {
                c.op_id = op_id;
                c.load_state = LoadState::Loading;
            }
        }
        let tx = self.tx.clone();
        let loc = location.clone();
        self.rt.spawn(async move {
            let entries = match loc.clone() {
                Location::Directory(path) => list_dir(&path, show_hidden, col, dir, folder_order)
                    .await
                    .unwrap_or_default(),
                Location::Archive { archive, inner } => tokio::task::spawn_blocking(move || {
                    atlas_core::fs::archive::open(&archive)
                        .and_then(|p| p.list(&inner).ok())
                        .unwrap_or_default()
                })
                .await
                .unwrap_or_default(),
                _ => Vec::new(),
            };
            let has_excluded_items = match &location {
                Location::Directory(path) => {
                    has_excluded_directory_items(path, show_hidden, entries.is_empty()).await
                }
                _ => false,
            };
            let _ = tx.send(BgResult::MillerListed {
                tab: tile,
                location,
                op_id,
                entries,
                has_excluded_items,
            });
        });
    }

    pub(crate) fn spawn_list_tree(&mut self, tab_id: TileId, path: PathBuf) {
        if !self.tree_loading.insert((tab_id, path.clone())) {
            return;
        }
        let show_hidden = self.state.settings.show_hidden;
        let (col, dir, folder_order) = self
            .tab_data_for(tab_id)
            .map(|td| (td.sort_col.clone(), td.sort_dir.clone(), td.folder_order))
            .unwrap_or((
                atlas_model::file_entry::SortColumn::Name,
                atlas_model::file_entry::SortDir::Asc,
                FolderOrder::default(),
            ));
        let tx = self.tx.clone();
        let p = path.clone();
        let archive = self
            .tab_data_for(tab_id)
            .and_then(|tab| match &tab.location {
                Location::Archive { archive, .. } => Some(archive.clone()),
                _ => None,
            });
        self.rt.spawn(async move {
            let entries = if let Some(archive) = archive {
                let inner = p.clone();
                tokio::task::spawn_blocking(move || {
                    atlas_core::fs::archive::open(&archive)
                        .and_then(|provider| provider.list(&inner).ok())
                        .unwrap_or_default()
                })
                .await
                .unwrap_or_default()
            } else {
                list_dir(&p, show_hidden, col, dir, folder_order)
                    .await
                    .unwrap_or_default()
            };

            let _ = tx.send(BgResult::TreeListed {
                tab: tab_id,
                path: p,
                entries,
            });
        });
    }

    /// Select an item in a Miller column: focus that column, truncate deeper
    /// columns, and (for a folder) open a new child column.
    pub(crate) fn miller_select(
        &mut self,
        tile: TileId,
        column: usize,
        idx: usize,
        shift: bool,
        ctrl: bool,
    ) {
        self.activate_tab(tile);
        let entry = self
            .state
            .tab_states
            .get(&tile)
            .and_then(|ts| ts.miller.columns.get(column))
            .and_then(|c| c.entries.get(idx))
            .map(|e| (e.path(), e.kind().clone(), e.archive_root().clone()));
        let Some((path, kind, archive_root)) = entry else {
            return;
        };
        let child_to_load = {
            let Some(ts) = self.state.tab_states.get_mut(&tile) else {
                return;
            };
            if column >= ts.miller.columns.len() {
                return;
            }
            ts.miller.focused = column;
            let col = &mut ts.miller.columns[column];
            col.refresh_visibility(&ts.filter, ts.show_files, ts.show_folders);
            let visible = col.visible_indices(&ts.filter, ts.show_files, ts.show_folders);
            if !visible.contains(&idx) {
                return;
            }
            col.selection.select(idx, &visible, shift, ctrl);
            // Modifier selection must not navigate or discard deeper columns.
            if shift || ctrl {
                return;
            }
            ts.miller.columns.truncate(column + 1);
            ts.selection.scroll_to_selected = true;
            if kind == atlas_model::file_entry::FileKind::Dir {
                let child_loc = if let Some(archive) = archive_root {
                    // Dir inside an archive — navigate as virtual path.
                    Location::Archive {
                        archive,
                        inner: path.clone(),
                    }
                } else {
                    Location::Directory(path.clone())
                };
                ts.miller
                    .columns
                    .push(atlas_model::tab::MillerColumn::new(child_loc.clone()));
                Some(child_loc)
            } else if atlas_core::fs::archive::is_archive_path(&path) {
                let child_loc = Location::Archive {
                    archive: path.clone(),
                    inner: std::path::PathBuf::new(),
                };
                ts.miller
                    .columns
                    .push(atlas_model::tab::MillerColumn::new(child_loc.clone()));
                Some(child_loc)
            } else {
                None
            }
        };
        self.miller_scroll_handles
            .retain(|(pane, col), _| *pane != tile || *col <= column);
        if let Some(child_loc) = child_to_load {
            self.spawn_miller_list(tile, child_loc);
        }
    }

    /// When the pane is in Miller view, the live selection is the focused
    /// column's — not `ts.selection` (which tracks the hidden flat listing).
    /// Returns `None` for non-Miller panes so callers fall through to the flat path.
    pub(crate) fn miller_selection_paths_for_pane(&self, pane: TileId) -> Option<Vec<PathBuf>> {
        let td = self.tab_data_for(pane)?;
        if !matches!(td.view_mode, atlas_model::settings::ViewMode::Miller) {
            return None;
        }
        let ts = self.state.tab_states.get(&pane)?;
        Some(
            ts.selected_entries(&atlas_model::settings::ViewMode::Miller)
                .into_iter()
                .map(|entry| entry.path())
                .collect(),
        )
    }

    /// When the focused pane is in Miller view, the live selection is the focused
    /// column's — not `ts.selection` (which tracks the hidden flat listing).
    /// Returns `None` for non-Miller panes so callers fall through to the flat path.
    pub(crate) fn miller_focused_selection_paths(&self) -> Option<Vec<PathBuf>> {
        self.miller_selection_paths_for_pane(self.state.focused_tile)
    }

    /// Keyboard cursor movement inside the focused Miller column. Returns `true`
    /// when the focused pane is in Miller view (event consumed), else `false` so
    /// the flat-list `nav_apply` runs. `target` maps `(cursor, len) → new cursor`.
    pub(crate) fn miller_nav_apply(
        &mut self,
        target: &dyn Fn(usize, usize) -> usize,
        extend: bool,
        ctrl: bool,
    ) -> bool {
        let tile = self.state.focused_tile;
        let is_miller = self
            .tab_data_for(tile)
            .is_some_and(|td| matches!(td.view_mode, atlas_model::settings::ViewMode::Miller));
        if !is_miller {
            return false;
        }
        let Some((column, cur, visible)) = self.state.tab_states.get(&tile).and_then(|ts| {
            let ci = ts.miller.focused;
            let col = ts.miller.columns.get(ci)?;
            let visible = ts.miller_visible_indices(ci);
            Some((
                ci,
                col.selection
                    .keyboard_cursor
                    .and_then(|i| visible.iter().position(|row| *row == i))
                    .unwrap_or(0),
                visible,
            ))
        }) else {
            return true;
        };
        if visible.is_empty() {
            return true;
        }
        let next = visible[target(cur, visible.len()).min(visible.len() - 1)];
        if let Some(col) = self
            .state
            .tab_states
            .get_mut(&tile)
            .and_then(|ts| ts.miller.columns.get_mut(column))
        {
            col.selection.navigate(next, &visible, extend, ctrl, None);
        }
        if !extend && !ctrl {
            self.miller_select(tile, column, next, false, false);
        }
        if let Some(ts) = self.state.tab_states.get_mut(&tile) {
            ts.selection.scroll_to_selected = true;
        }
        true
    }

    /// Left in Miller view: move focus to the parent column (column - 1).
    pub(crate) fn miller_nav_left(&mut self, extend: bool, ctrl: bool) -> bool {
        let tile = self.state.focused_tile;
        let is_miller = self
            .tab_data_for(tile)
            .is_some_and(|td| matches!(td.view_mode, atlas_model::settings::ViewMode::Miller));
        if !is_miller {
            return false;
        }
        let (ci, cur) = {
            let Some(ts) = self.state.tab_states.get_mut(&tile) else {
                return true;
            };
            if ts.miller.focused > 0 {
                ts.miller.focused -= 1;
                let ci = ts.miller.focused;
                let cur = ts
                    .miller
                    .columns
                    .get(ci)
                    .and_then(|col| col.selection.keyboard_cursor);
                (Some(ci), cur)
            } else {
                (None, None)
            }
        };
        if let (Some(ci), Some(cur)) = (ci, cur) {
            if !extend && !ctrl {
                self.miller_select(tile, ci, cur, false, false);
            }
            if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                ts.selection.scroll_to_selected = true;
            }
        }
        true
    }

    /// Right in Miller view: move focus to the child column (column + 1), or
    /// expand the directory under cursor if not already open.
    pub(crate) fn miller_nav_right(&mut self, extend: bool, ctrl: bool) -> bool {
        let tile = self.state.focused_tile;
        let is_miller = self
            .tab_data_for(tile)
            .is_some_and(|td| matches!(td.view_mode, atlas_model::settings::ViewMode::Miller));
        if !is_miller {
            return false;
        }
        let (has_next_col, has_children) = {
            let Some(ts) = self.state.tab_states.get(&tile) else {
                return true;
            };
            let ci = ts.miller.focused;
            let next_col_exists = ci + 1 < ts.miller.columns.len();
            let is_dir = ts
                .miller
                .columns
                .get(ci)
                .and_then(|c| {
                    let cur = c.selection.keyboard_cursor?;
                    c.entries
                        .get(cur)
                        .map(|e| e.kind() == atlas_model::file_entry::FileKind::Dir)
                })
                .unwrap_or(false);
            (next_col_exists, is_dir)
        };

        if has_next_col {
            let ci = if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                ts.miller.focused += 1;
                ts.miller.focused
            } else {
                return true;
            };
            let cursor = self.state.tab_states.get(&tile).and_then(|ts| {
                let visible = ts.miller_visible_indices(ci);
                ts.miller
                    .columns
                    .get(ci)?
                    .selection
                    .keyboard_cursor
                    .filter(|i| visible.contains(i))
                    .or_else(|| visible.first().copied())
            });
            if let Some(cursor) = cursor {
                let visible = self
                    .state
                    .tab_states
                    .get(&tile)
                    .map(|ts| ts.miller_visible_indices(ci))
                    .unwrap_or_default();
                if let Some(col) = self
                    .state
                    .tab_states
                    .get_mut(&tile)
                    .and_then(|ts| ts.miller.columns.get_mut(ci))
                {
                    col.selection.navigate(cursor, &visible, extend, ctrl, None);
                }
                if !extend && !ctrl {
                    self.miller_select(tile, ci, cursor, false, false);
                }
                if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                    ts.selection.scroll_to_selected = true;
                }
            }
        } else if has_children {
            self.miller_open_cursor();
        }
        true
    }

    /// Enter in Miller view: activate the focused column's cursor entry (files
    /// open; dirs re-cascade). Returns `false` for non-Miller panes.
    pub(crate) fn miller_open_cursor(&mut self) -> bool {
        let tile = self.state.focused_tile;
        let is_miller = self
            .tab_data_for(tile)
            .is_some_and(|td| matches!(td.view_mode, atlas_model::settings::ViewMode::Miller));
        if !is_miller {
            return false;
        }
        let target = self.state.tab_states.get(&tile).and_then(|ts| {
            let ci = ts.miller.focused;
            let col = ts.miller.columns.get(ci)?;
            let idx = col.selection.keyboard_cursor?;
            Some((ci, idx))
        });
        if let Some((column, idx)) = target {
            self.process_actions(vec![crate::input::action::Action::MillerActivate {
                tile,
                column,
                idx,
            }]);
        }
        true
    }

    /// Keyboard cursor movement in Tree view. Returns `true` when focused pane is Tree view.
    pub(crate) fn tree_nav_apply(
        &mut self,
        target: &dyn Fn(usize, usize) -> usize,
        extend: bool,
        cursor_only: bool,
    ) -> bool {
        let tile = self.state.focused_tile;
        let is_tree = self
            .tab_data_for(tile)
            .is_some_and(|td| matches!(td.view_mode, atlas_model::settings::ViewMode::Tree));
        if !is_tree {
            return false;
        }
        let Some(ts) = self.state.tab_states.get_mut(&tile) else {
            return true;
        };
        let len = ts.visible_tree_rows().len();
        if len == 0 {
            return true;
        }
        let cur = ts.selection.keyboard_cursor.unwrap_or(0);
        let np = target(cur, len).min(len - 1);
        let visible: Vec<_> = (0..len).collect();
        ts.selection
            .navigate(np, &visible, extend, cursor_only, None);
        true
    }

    /// Left in Tree view: collapse folder if expanded; or move to parent folder.
    pub(crate) fn tree_nav_left(&mut self, extend: bool, ctrl: bool) -> bool {
        let tile = self.state.focused_tile;
        let is_tree = self
            .tab_data_for(tile)
            .is_some_and(|td| matches!(td.view_mode, atlas_model::settings::ViewMode::Tree));
        if !is_tree {
            return false;
        }
        let (path_to_collapse, parent_idx) = {
            let Some(ts) = self.state.tab_states.get_mut(&tile) else {
                return true;
            };
            let cur = ts.selection.keyboard_cursor.unwrap_or(0);
            let rows = ts.visible_tree_rows();
            if let Some((entry, depth, is_expanded, _)) = rows.get(cur).cloned() {
                if entry.kind() == atlas_model::file_entry::FileKind::Dir && is_expanded {
                    (Some(entry.path()), None)
                } else if depth > 0 {
                    let mut p_idx = None;
                    for i in (0..cur).rev() {
                        if rows[i].1 == depth - 1 {
                            p_idx = Some(i);
                            break;
                        }
                    }
                    (None, p_idx)
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        };

        if let Some(path) = path_to_collapse {
            // Rows below shift: keep selection on its files and the view still.
            self.mutate_tab(tile, |ts| ts.tree.toggle_expand(&path));
            return true;
        }
        let Some(ts) = self.state.tab_states.get_mut(&tile) else {
            return true;
        };
        if let Some(i) = parent_idx {
            let visible = (0..ts.visible_tree_rows().len()).collect::<Vec<_>>();
            ts.selection.navigate(i, &visible, extend, ctrl, None);
            // Same minimal keyboard reveal as up/down; never recenter.
            ts.selection.scroll_to_selected = true;
        }
        true
    }

    /// Right in Tree view: expand folder if collapsed; or move to first child.
    pub(crate) fn tree_nav_right(&mut self, extend: bool, ctrl: bool) -> bool {
        let tile = self.state.focused_tile;
        let is_tree = self
            .tab_data_for(tile)
            .is_some_and(|td| matches!(td.view_mode, atlas_model::settings::ViewMode::Tree));
        if !is_tree {
            return false;
        }
        enum NavRight {
            Expand(std::path::PathBuf),
            Select(usize),
            None,
        }
        let nav_action = {
            let Some(ts) = self.state.tab_states.get_mut(&tile) else {
                return true;
            };
            let cur = ts.selection.keyboard_cursor.unwrap_or(0);
            let rows = ts.visible_tree_rows();
            if let Some((entry, depth, is_expanded, has_children)) = rows.get(cur).cloned() {
                if entry.kind() == atlas_model::file_entry::FileKind::Dir {
                    if !is_expanded {
                        NavRight::Expand(entry.path())
                    } else if has_children && cur + 1 < rows.len() && rows[cur + 1].1 == depth + 1 {
                        NavRight::Select(cur + 1)
                    } else {
                        NavRight::None
                    }
                } else {
                    NavRight::None
                }
            } else {
                NavRight::None
            }
        };

        match nav_action {
            NavRight::Expand(path) => {
                self.mutate_tab(tile, |ts| ts.tree.toggle_expand(&path));
                let needs_fetch = self
                    .state
                    .tab_states
                    .get(&tile)
                    .is_some_and(|ts| !ts.tree.children.contains_key(&path));
                if needs_fetch {
                    self.spawn_list_tree(tile, path);
                }
            }
            NavRight::Select(next) => {
                if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                    let visible = (0..ts.visible_tree_rows().len()).collect::<Vec<_>>();
                    ts.selection.navigate(next, &visible, extend, ctrl, None);
                    // Same minimal keyboard reveal as up/down; never recenter.
                    ts.selection.scroll_to_selected = true;
                }
            }
            NavRight::None => {}
        }
        true
    }

    /// Enter in Tree view: open file or toggle folder expand.
    pub(crate) fn tree_open_cursor(&mut self) -> bool {
        let tile = self.state.focused_tile;
        let is_tree = self
            .tab_data_for(tile)
            .is_some_and(|td| matches!(td.view_mode, atlas_model::settings::ViewMode::Tree));
        if !is_tree {
            return false;
        }
        enum OpenCursor {
            Toggle(std::path::PathBuf),
            OpenFile(std::path::PathBuf, Option<std::path::PathBuf>),
            None,
        }
        let action = {
            let Some(ts) = self.state.tab_states.get_mut(&tile) else {
                return true;
            };
            let cur = ts.selection.keyboard_cursor.unwrap_or(0);
            let rows = ts.visible_tree_rows();
            if let Some((entry, _, _, _)) = rows.get(cur) {
                if entry.kind() == atlas_model::file_entry::FileKind::Dir {
                    OpenCursor::Toggle(entry.path())
                } else {
                    OpenCursor::OpenFile(entry.path(), entry.archive_root())
                }
            } else {
                OpenCursor::None
            }
        };

        match action {
            OpenCursor::Toggle(path) => {
                self.mutate_tab(tile, |ts| ts.tree.toggle_expand(&path));
                let needs_fetch = self.state.tab_states.get(&tile).is_some_and(|ts| {
                    !ts.tree.children.contains_key(&path) && ts.tree.is_expanded(&path)
                });
                if needs_fetch {
                    self.spawn_list_tree(tile, path);
                }
            }
            OpenCursor::OpenFile(path, archive_root) => {
                self.activate_file_path(tile, path, archive_root);
            }
            OpenCursor::None => {}
        }
        true
    }

    /// Re-list every Miller column of `tile` (post file-op refresh). No-op for
    /// panes that never opened Miller view.
    pub(crate) fn refresh_miller_columns(&mut self, tile: TileId) {
        let locations: Vec<Location> = self
            .state
            .tab_states
            .get(&tile)
            .map(|ts| {
                ts.miller
                    .columns
                    .iter()
                    .map(|c| c.location.clone())
                    .collect()
            })
            .unwrap_or_default();
        for loc in locations {
            self.spawn_miller_list(tile, loc);
        }
    }

    /// List a directory inside an archive file in the background.
    pub(crate) fn spawn_list_archive(
        &mut self,
        tab_id: TileId,
        archive: PathBuf,
        inner: PathBuf,
        sort_col: atlas_model::file_entry::SortColumn,
        sort_dir: atlas_model::file_entry::SortDir,
    ) {
        let folder_order = self
            .tab_data_for(tab_id)
            .map_or_else(atlas_model::pane::FolderOrder::default, |td| {
                td.folder_order
            });
        let op_id = self.state.next_op_id();
        let ts = self.state.tab_states.entry(tab_id).or_default();
        ts.load_state = LoadState::Loading;
        ts.last_nav_op_id = op_id;

        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = tokio::task::spawn_blocking({
                let archive = archive.clone();
                let inner = inner.clone();
                move || -> Result<Vec<atlas_model::file_entry::FileEntry>, String> {
                    let provider = atlas_core::fs::archive::open(&archive)
                        .ok_or_else(|| rust_i18n::t!("app_archive_unsupported").to_string())?;
                    let mut entries = provider.list(&inner).map_err(|e| e.to_string())?;
                    atlas_core::fs::list::sort_entries(
                        &mut entries,
                        &sort_col,
                        &sort_dir,
                        folder_order,
                    );
                    Ok(entries)
                }
            })
            .await;

            let send_result = match result {
                Ok(Ok(entries)) => atlas_model::app_state::BgResult::ArchiveListed {
                    tab: tab_id,
                    op_id,
                    archive,
                    inner,
                    entries,
                },
                Ok(Err(e)) => atlas_model::app_state::BgResult::ArchiveListError {
                    tab: tab_id,
                    op_id,
                    error: e,
                },
                Err(e) => atlas_model::app_state::BgResult::ArchiveListError {
                    tab: tab_id,
                    op_id,
                    error: e.to_string(),
                },
            };
            let _ = tx.send(send_result);
        });
    }

    /// Unified listing dispatcher used by `navigate_to`. Gets sort from pane data.
    pub(crate) fn spawn_list(&mut self, tab_id: TileId, mut loc: Location) {
        if let Some(Tile::Pane(tab)) = self.state.pane_tree.tiles.get_mut(tab_id) {
            if tab.location == loc {
                tab.reconcile_volume(&self.mounted_volumes);
                loc = tab.location.clone();
            }
        }
        let (col, dir) = self
            .tab_data_for(tab_id)
            .map(|td| (td.sort_col.clone(), td.sort_dir.clone()))
            .unwrap_or_default();
        self.dispatch_list(tab_id, loc, col, dir);
    }

    pub(crate) fn spawn_list_recycle_bin(
        &mut self,
        tab_id: TileId,
        sort_col: atlas_model::file_entry::SortColumn,
        sort_dir: atlas_model::file_entry::SortDir,
    ) {
        let op_id = self.state.next_op_id();
        let ts = self.state.tab_states.entry(tab_id).or_default();
        ts.load_state = LoadState::Loading;
        ts.last_nav_op_id = op_id;
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let entries = atlas_core::fs::recycle_bin::list_recycle_bin(sort_col, sort_dir).await;

            let _ = tx.send(BgResult::DirListed {
                tab: tab_id,
                op_id,
                entries,
            });
        });
    }

    pub(crate) fn spawn_open_files(&self, paths: Vec<PathBuf>) {
        self.rt.spawn(async move {
            let _ = tokio::task::spawn_blocking(move || {
                for path in paths {
                    if atlas_core::vault::is_private_path(&path) {
                        continue;
                    }
                    let _ = open::that(path);
                }
            })
            .await;
        });
    }

    /// Extract selected virtual archive entries to `dest`.
    /// `items` = (`archive_path`, `inner_path`) pairs.
    ///
    /// Runs as a tracked operation: per-item progress, cancellation, and every
    /// failure reported via `OpComplete`. Dirs are extracted recursively.
    /// Existing names at `dest` are auto-renamed ("name (1)") — extraction
    /// never overwrites silently.
    pub(crate) fn extract_archive_items(
        &mut self,
        _tab_id: TileId,
        items: Vec<(PathBuf, PathBuf)>,
        dest: PathBuf,
    ) {
        let affected = self.affected_tabs_for_path(&dest);
        let (op_id, cancel) = self
            .state
            .ops
            .start_op_labeled(items.len(), rust_i18n::t!("fs_sync_extracting").to_string());
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let _ = tokio::task::spawn_blocking(move || {
                let total = items.len() as u64;
                let mut errors: Vec<String> = Vec::new();
                let mut bytes_done = 0u64;
                // Group by archive to open each file only once.
                let mut by_archive: std::collections::HashMap<PathBuf, Vec<PathBuf>> =
                    std::collections::HashMap::new();
                for (archive, inner) in items {
                    by_archive.entry(archive).or_default().push(inner);
                }
                let mut done = 0u64;
                'outer: for (archive_path, inners) in by_archive {
                    let Some(provider) = atlas_core::fs::archive::open(&archive_path) else {
                        errors.push(
                            rust_i18n::t!(
                                "fs_sync_open_err",
                                path = archive_path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                            )
                            .to_string(),
                        );
                        continue;
                    };
                    for inner in inners {
                        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            errors.push(
                                rust_i18n::t!(
                                    "fs_sync_extract_cancelled",
                                    done = done,
                                    total = total
                                )
                                .to_string(),
                            );
                            break 'outer;
                        }
                        let name = inner
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| "extracted".to_string());
                        let _ = tx.send(BgResult::OpProgress {
                            id: op_id,
                            done,
                            total,
                            bytes_done,
                            bytes_total: 0,
                            current_file: PathBuf::from(&name),
                        });
                        let out_base = atlas_core::fs::transfer::unique_target(
                            &dest,
                            std::ffi::OsStr::new(&name),
                        );
                        let written = extract_entry_recursive(
                            provider.as_ref(),
                            &inner,
                            &out_base,
                            &cancel,
                            &mut errors,
                        );
                        bytes_done += written;
                        done += 1;
                    }
                }
                let result = if errors.is_empty() {
                    Ok(())
                } else {
                    // These strings are already localized (built with `t!` above).
                    Err(errors
                        .into_iter()
                        .map(|msg| atlas_model::ops::OpError {
                            path: std::path::PathBuf::new(),
                            kind: atlas_model::ops::OpErrorKind::Msg(msg),
                        })
                        .collect())
                };
                let _ = tx.send(BgResult::OpComplete {
                    id: op_id,
                    result,
                    affected_tabs: affected,
                });
            })
            .await;
        });
    }
}

/// Recursively extract `inner` from `provider` into `dest_path`.
/// If `inner` is a directory, creates `dest_path/` and extracts all children.
/// Returns total bytes written.
pub(crate) fn extract_entry_recursive(
    provider: &dyn atlas_core::fs::archive::ArchiveProvider,
    inner: &std::path::Path,
    dest_path: &std::path::Path,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    errors: &mut Vec<String>,
) -> u64 {
    use atlas_core::fs::archive::bomb_guard::BombGuard;
    use atlas_core::fs::archive::sanitize::sanitize_inner_path;
    use std::io::Read as _;

    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return 0;
    }

    // Check if this is a directory by listing it.
    match provider.list(inner) {
        Ok(children) if !children.is_empty() => {
            // It's a non-empty directory — create it and recurse.
            #[allow(clippy::disallowed_methods)]
            let _ = std::fs::create_dir_all(dest_path);
            let mut total = 0u64;
            for child in children {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                let child_name = match sanitize_inner_path(std::path::Path::new(&child.name))
                    .and_then(|p| p.file_name().map(|n| n.to_os_string()))
                {
                    Some(name) => name,
                    None => {
                        errors.push(format!(
                            "Ruta de archivo no segura descartada en extracción: '{}'",
                            child.name
                        ));
                        continue;
                    }
                };
                let child_dest = dest_path.join(&child_name);
                total +=
                    extract_entry_recursive(provider, &child.path(), &child_dest, cancel, errors);
            }
            total
        }
        _ => {
            // Treat as a file (or empty dir — attempt extract, create dir if that fails).
            let mut reader = match provider.read_bytes(inner) {
                Ok(r) => r,
                Err(_) => {
                    // Likely an empty directory entry — create it and move on.
                    #[allow(clippy::disallowed_methods)]
                    let _ = std::fs::create_dir_all(dest_path);
                    return 0;
                }
            };
            if let Some(parent) = dest_path.parent() {
                #[allow(clippy::disallowed_methods)]
                let _ = std::fs::create_dir_all(parent);
            }
            let mut out = match std::fs::File::create(dest_path) {
                Ok(f) => f,
                Err(e) => {
                    errors.push(
                        rust_i18n::t!(
                            "fs_sync_create_err",
                            path = dest_path.file_name().unwrap_or_default().to_string_lossy(),
                            e = e
                        )
                        .to_string(),
                    );
                    return 0;
                }
            };
            let mut guard = BombGuard::default();
            let mut buf = [0u8; 65536];
            let mut total = 0u64;
            loop {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    drop(out);
                    #[allow(clippy::disallowed_methods)]
                    let _ = std::fs::remove_file(dest_path);
                    return total;
                }
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if guard.check_chunk(n as u64).is_err() {
                            errors.push(
                                rust_i18n::t!(
                                    "fs_sync_zip_bomb",
                                    path =
                                        dest_path.file_name().unwrap_or_default().to_string_lossy()
                                )
                                .to_string(),
                            );
                            drop(out);
                            #[allow(clippy::disallowed_methods)]
                            let _ = std::fs::remove_file(dest_path);
                            return total;
                        }
                        use std::io::Write as _;
                        if let Err(e) = out.write_all(&buf[..n]) {
                            errors.push(
                                rust_i18n::t!(
                                    "fs_sync_write_err",
                                    path =
                                        dest_path.file_name().unwrap_or_default().to_string_lossy(),
                                    e = e
                                )
                                .to_string(),
                            );
                            return total;
                        }
                        total += n as u64;
                    }
                    Err(e) => {
                        errors.push(
                            rust_i18n::t!(
                                "fs_sync_read_err",
                                path = dest_path.file_name().unwrap_or_default().to_string_lossy(),
                                e = e
                            )
                            .to_string(),
                        );
                        return total;
                    }
                }
            }
            drop(out);
            atlas_core::fs::archive::propagate_mark_of_the_web(provider.archive_path(), dest_path);
            total
        }
    }
}
