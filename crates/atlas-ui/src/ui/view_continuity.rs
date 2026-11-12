//! Preserve file identity when switching between flat, tree and column indices.
use crate::app::App;
use atlas_model::{location::Location, pane::TabId, settings::ViewMode, tab::SelectionState};
use std::path::PathBuf;

/// Long keyboard jumps reveal only the final 400 logical pixels of travel.
fn bounded_scroll_start(current: f32, target: f32) -> f32 {
    target - (target - current).clamp(-400.0, 400.0)
}

fn file_scroll_limit(content_h: f32, viewport_h: f32, margin_ratio: f32) -> f32 {
    let margin = if content_h > 0.0 {
        viewport_h * margin_ratio
    } else {
        0.0
    };
    (content_h + margin - viewport_h).max(0.0)
}

#[derive(Clone)]
pub(crate) struct ViewSelection {
    location: Location,
    paths: Vec<PathBuf>,
    selection: SelectionState,
    scroll: Option<PathBuf>,
}

pub(crate) fn remap(
    selection: &SelectionState,
    old: &[PathBuf],
    new: &[PathBuf],
) -> SelectionState {
    let index = |idx: usize| {
        old.get(idx)
            .and_then(|path| new.iter().position(|candidate| candidate == path))
    };
    SelectionState {
        selected: selection
            .selected
            .iter()
            .filter_map(|idx| index(*idx))
            .collect(),
        keyboard_cursor: selection.keyboard_cursor.and_then(index),
        selection_anchor: selection.selection_anchor.and_then(index),
        last_clicked_index: selection.last_clicked_index.and_then(index),
        scroll_to_selected: false,
        last_pointer_click: None,
    }
}

impl App {
    /// Whether `pane`'s selection indexes visible tree rows (Tree view).
    pub(crate) fn selection_is_tree(&self, pane: TabId) -> bool {
        self.tab_data_for(pane)
            .is_some_and(|tab| tab.view_mode == ViewMode::Tree)
    }

    /// The single entry point for structural tab changes (listing, re-sort, tree
    /// expand/collapse, children arriving, watcher deltas):
    /// - the selection stays on the same files (model invariant), and
    /// - in row views the first visible row stays put, so content never jumps
    ///   under the reader. At the very top nothing is pinned, so new items
    ///   appear in place (Finder/Explorer).
    pub(crate) fn mutate_tab(
        &mut self,
        pane: TabId,
        mutate: impl FnOnce(&mut atlas_model::tab::TabState),
    ) {
        self.mutate_tab_with(pane, |path| path, mutate);
    }

    /// [`Self::mutate_tab`] with a rename map for the old identities.
    pub(crate) fn mutate_tab_with(
        &mut self,
        pane: TabId,
        rename: impl Fn(PathBuf) -> PathBuf,
        mutate: impl FnOnce(&mut atlas_model::tab::TabState),
    ) {
        let tree = self.selection_is_tree(pane);
        let anchor = self.row_scroll_anchor(pane);
        if let Some(ts) = self.state.tab_states.get_mut(&pane) {
            ts.mutate_preserving_selection_with(tree, rename, mutate);
        }
        if let Some(anchor) = anchor {
            self.restore_row_scroll_anchor(pane, anchor);
        }
    }

    /// Item paths in display order for one-item-per-row views (Tree, ungrouped
    /// Details). Derived on demand; nothing is retained.
    fn display_row_paths(&mut self, pane: TabId) -> Option<Vec<PathBuf>> {
        let (mode, group_by) = {
            let tab = self.tab_data_for(pane)?;
            (tab.view_mode.clone(), tab.group_by)
        };
        match mode {
            ViewMode::Tree => Some(self.state.tab_states.get(&pane)?.selection_paths(true)),
            ViewMode::Details if group_by == atlas_model::tab::GroupBy::None => {
                let visible = self.flat_visible_indices(pane);
                let ts = self.state.tab_states.get(&pane)?;
                Some(
                    visible
                        .iter()
                        .filter_map(|i| ts.entries.get(*i))
                        .map(|e| e.path())
                        .collect(),
                )
            }
            _ => None,
        }
    }

    fn row_scroll_anchor(&mut self, pane: TabId) -> Option<(PathBuf, f32)> {
        let base = self
            .scroll_handles
            .get(&pane)?
            .0
            .borrow()
            .base_handle
            .clone();
        let row_h = f32::from(base.line_scroll_height()?);
        let offset = -f32::from(base.offset().y);
        if row_h <= 0.0 || offset < 0.5 {
            return None;
        }
        let rows = self.display_row_paths(pane)?;
        let top = (offset / row_h).floor() as usize;
        let path = rows.get(top)?.clone();
        Some((path, offset - top as f32 * row_h))
    }

    fn restore_row_scroll_anchor(&mut self, pane: TabId, (path, intra): (PathBuf, f32)) {
        let Some(rows) = self.display_row_paths(pane) else {
            return;
        };
        // The anchor row may have collapsed away: pin its nearest visible ancestor.
        let (index, intra) = match rows.iter().position(|row| *row == path) {
            Some(index) => (index, intra),
            None => {
                let Some(index) = path
                    .ancestors()
                    .skip(1)
                    .find_map(|ancestor| rows.iter().position(|row| row.as_path() == ancestor))
                else {
                    return;
                };
                (index, 0.0)
            }
        };
        let Some(handle) = self.scroll_handles.get(&pane) else {
            return;
        };
        let base = handle.0.borrow().base_handle.clone();
        let Some(row_h) = base.line_scroll_height().map(f32::from) else {
            return;
        };
        let offset = base.offset();
        let target = index as f32 * row_h + intra;
        if (target + f32::from(offset.y)).abs() > 0.5 {
            base.set_offset(gpui::point(offset.x, gpui::px(-target)));
        }
    }

    pub(crate) fn file_scroll_max(&self, content_h: f32, viewport_h: f32) -> f32 {
        file_scroll_limit(
            content_h,
            viewport_h,
            self.state.settings.scroll_overscroll_fraction(),
        )
    }
    pub(crate) fn set_keyboard_scroll_offset(
        &self,
        base: &gpui::ScrollHandle,
        target: gpui::Point<gpui::Pixels>,
    ) {
        if self.state.settings.disable_animations
            || self.state.settings.scroll_smoothness <= 0.0
            || self.pending_rename.is_some()
        {
            base.set_offset(target);
            return;
        }
        let offset = base.offset();
        let start = gpui::point(
            gpui::px(bounded_scroll_start(offset.x.into(), target.x.into())),
            gpui::px(bounded_scroll_start(offset.y.into(), target.y.into())),
        );
        if start != offset {
            base.set_offset(start);
        }
        base.set_offset_smooth(target);
    }
    /// Keyboard reveal for the focused cursor. Scrolls only when the cursor would
    /// leave the viewport (one item of context past the edge); never recenters.
    pub(crate) fn scroll_keyboard_selection(&mut self, pane: TabId) -> bool {
        use crate::ui::dg::scroll::keyboard_target;
        let mode = self.tab_data_for(pane).map(|tab| tab.view_mode.clone());
        if matches!(mode, Some(ViewMode::Tree | ViewMode::Miller)) {
            let Some(state) = self.state.tab_states.get(&pane) else {
                return false;
            };
            let (row, count, handle) = if mode == Some(ViewMode::Miller) {
                let index = state.miller.focused;
                let Some(column) = state.miller.columns.get(index) else {
                    return false;
                };
                let visible = state.miller_visible_indices(index);
                let cursor = column
                    .selection
                    .keyboard_cursor
                    .or_else(|| column.primary_selected());
                (
                    cursor.and_then(|i| visible.iter().position(|row| *row == i)),
                    visible.len(),
                    self.miller_scroll_handles.get(&(pane, index)),
                )
            } else {
                (
                    state.selection.keyboard_cursor,
                    state.visible_tree_rows_snapshot().len(),
                    self.scroll_handles.get(&pane),
                )
            };
            let (Some(row), Some(handle)) = (row, handle) else {
                return false;
            };
            let base = handle.0.borrow().base_handle.clone();
            let Some(row_h) = base.line_scroll_height().map(f32::from) else {
                return false;
            };
            let height = f32::from(base.bounds().size.height);
            if height <= 1.0 || row >= count {
                return false;
            }
            let offset = base.offset();
            let current = (-f32::from(offset.y)).max(0.0);
            let target = keyboard_target(
                row as f32 * row_h,
                row_h,
                height,
                current,
                self.file_scroll_max(count as f32 * row_h, height),
            );
            if (target - current).abs() > 0.5 {
                self.set_keyboard_scroll_offset(&base, gpui::point(offset.x, gpui::px(-target)));
            }
            return true;
        }
        if !matches!(
            mode,
            Some(ViewMode::Details | ViewMode::List | ViewMode::Grid)
        ) || self.pending_rename.is_some()
        {
            let strategy = if self.pending_rename.is_some() {
                gpui::ScrollStrategy::Center
            } else {
                gpui::ScrollStrategy::Nearest
            };
            return self.scroll_selected_into_view(pane, strategy);
        }
        let visible = self.view_cells(pane);
        let Some(tab) = self.state.tab_states.get_mut(&pane) else {
            return false;
        };
        let Some(entry) = tab
            .selection
            .keyboard_cursor
            .or_else(|| tab.selection.selected.iter().min().copied())
        else {
            return false;
        };
        // (item start, item extent, pitch-free content extent, horizontal axis)
        let (start, len, content, horizontal) = {
            let Some(position) = visible.iter().position(|index| *index == entry) else {
                return false;
            };
            match mode {
                Some(ViewMode::Details) => {
                    let Some(hit) = self.details_hit.get(&pane) else {
                        return false;
                    };
                    (
                        position as f32 * hit.row_h,
                        hit.row_h,
                        visible.len() as f32 * hit.row_h,
                        false,
                    )
                }
                Some(ViewMode::Grid) => {
                    let Some(hit) = self.grid_hit.get(&pane) else {
                        return self.scroll_selected_into_view(pane, gpui::ScrollStrategy::Nearest);
                    };
                    let cols = hit.cols.max(1);
                    let rows = visible.len().div_ceil(cols);
                    let gap = (hit.pitch_y - hit.cell_h).max(0.0);
                    (
                        hit.pad_y + hit.bands.row_top(position / cols, hit.pitch_y),
                        hit.cell_h,
                        hit.pad_y * 2.0 + rows as f32 * hit.pitch_y - gap + hit.bands.total(),
                        false,
                    )
                }
                _ => {
                    let Some(hit) = self.list_hit.get(&pane) else {
                        return false;
                    };
                    // Column-major List scrolls horizontally, one column at a time.
                    let rows = hit.major_rows.max(1);
                    let columns = visible.len().div_ceil(rows);
                    (
                        (position / rows) as f32 * hit.pitch_x,
                        hit.cell_w,
                        columns as f32 * hit.pitch_x,
                        true,
                    )
                }
            }
        };
        let Some(handle) = self.scroll_handles.get(&pane) else {
            return false;
        };
        let base = handle.0.borrow().base_handle.clone();
        let bounds = base.bounds();
        let viewport = f32::from(if horizontal {
            bounds.size.width
        } else {
            bounds.size.height
        });
        if viewport <= 1.0 || len <= 0.0 {
            return false;
        }
        let offset = base.offset();
        let current = (-f32::from(if horizontal { offset.x } else { offset.y })).max(0.0);
        let max = if horizontal {
            (content - viewport).max(0.0)
        } else {
            self.file_scroll_max(content, viewport)
        };
        let target = keyboard_target(start, len, viewport, current, max);
        if (target - current).abs() > 0.5 {
            let target = if horizontal {
                gpui::point(gpui::px(-target), offset.y)
            } else {
                gpui::point(offset.x, gpui::px(-target))
            };
            self.set_keyboard_scroll_offset(&base, target);
        }
        true
    }

    pub(crate) fn capture_view_selection(&mut self, pane: TabId) {
        // A rapid second view change keeps the original snapshot until data is ready.
        if self.view_selection_pending.contains_key(&pane) {
            return;
        }
        let Some(tab) = self.tab_data_for(pane) else {
            return;
        };
        let (location, mode) = (tab.location.clone(), tab.view_mode.clone());
        let Some(ts) = self.state.tab_states.get_mut(&pane) else {
            return;
        };
        let (paths, selection): (Vec<_>, _) = match mode {
            ViewMode::Miller => {
                let Some(col) = ts.miller.columns.get(ts.miller.focused) else {
                    return;
                };
                (
                    col.entries.iter().map(|entry| entry.path()).collect(),
                    col.selection.clone(),
                )
            }
            ViewMode::Tree => (
                ts.visible_tree_rows()
                    .into_iter()
                    .map(|(entry, ..)| entry.path())
                    .collect(),
                ts.selection.clone(),
            ),
            _ => (
                ts.entries.iter().map(|entry| entry.path()).collect(),
                ts.selection.clone(),
            ),
        };
        let top_index = match mode {
            ViewMode::Grid => self
                .grid_hit
                .get(&pane)
                .and_then(|hit| hit.visible.get(hit.start_idx))
                .copied(),
            ViewMode::List => self
                .list_hit
                .get(&pane)
                .and_then(|hit| hit.visible.get(hit.start_idx))
                .copied(),
            ViewMode::Details => self
                .details_hit
                .get(&pane)
                .and_then(|hit| hit.visible.get(hit.start_idx))
                .copied(),
            _ => None,
        };
        let scroll = selection
            .keyboard_cursor
            .or_else(|| selection.selected.iter().min().copied())
            .or(top_index)
            .and_then(|idx| paths.get(idx).cloned());
        self.view_selection_pending.insert(
            pane,
            ViewSelection {
                location,
                paths,
                selection,
                scroll,
            },
        );
        self.grid_hit.remove(&pane);
        self.list_hit.remove(&pane);
        self.details_hit.remove(&pane);
    }

    pub(crate) fn restore_view_selection(&mut self, pane: TabId) {
        let Some(snapshot) = self.view_selection_pending.get(&pane).cloned() else {
            return;
        };
        let Some(tab) = self.tab_data_for(pane) else {
            self.view_selection_pending.remove(&pane);
            return;
        };
        let mode = tab.view_mode.clone();
        if tab.location != snapshot.location {
            self.view_selection_pending.remove(&pane);
            return;
        }
        let Some(ts) = self.state.tab_states.get_mut(&pane) else {
            return;
        };
        match mode {
            ViewMode::Miller => {
                let Some(col) = ts
                    .miller
                    .columns
                    .iter_mut()
                    .find(|col| col.location == snapshot.location)
                else {
                    return;
                };
                if !matches!(col.load_state, atlas_model::tab::LoadState::Loaded) {
                    return;
                }
                let paths: Vec<_> = col.entries.iter().map(|entry| entry.path()).collect();
                col.selection = remap(&snapshot.selection, &snapshot.paths, &paths);
            }
            ViewMode::Tree => {
                let paths: Vec<_> = ts
                    .visible_tree_rows()
                    .into_iter()
                    .map(|(entry, ..)| entry.path())
                    .collect();
                ts.selection = remap(&snapshot.selection, &snapshot.paths, &paths);
            }
            _ => {
                if !matches!(ts.load_state, atlas_model::tab::LoadState::Loaded) {
                    return;
                }
                let paths: Vec<_> = ts.entries.iter().map(|entry| entry.path()).collect();
                ts.selection = remap(&snapshot.selection, &snapshot.paths, &paths);
            }
        }
        self.view_selection_pending.remove(&pane);
        if let Some(path) = snapshot.scroll {
            self.view_scroll_pending
                .insert(pane, (snapshot.location, path));
        }
    }

    pub(crate) fn apply_view_scroll(&mut self, cx: &mut gpui::Context<Self>) {
        for (pane, (location, path)) in self.view_scroll_pending.clone() {
            let Some(tab) = self.tab_data_for(pane) else {
                self.view_scroll_pending.remove(&pane);
                continue;
            };
            if tab.location != location {
                self.view_scroll_pending.remove(&pane);
                continue;
            }
            let mode = tab.view_mode.clone();
            let Some(ts) = self.state.tab_states.get_mut(&pane) else {
                continue;
            };
            let position = match mode {
                ViewMode::Miller => ts.miller.columns.iter().enumerate().find_map(|(ci, col)| {
                    col.entries
                        .iter()
                        .position(|entry| entry.path() == path)
                        .and_then(|idx| {
                            ts.miller_visible_indices(ci).iter().position(|i| *i == idx)
                        })
                        .map(|position| (ci, position))
                }),
                ViewMode::Tree => ts
                    .visible_tree_rows()
                    .iter()
                    .position(|(entry, ..)| entry.path() == path)
                    .map(|idx| (0, idx)),
                _ => ts
                    .entries
                    .iter()
                    .position(|entry| entry.path() == path)
                    .and_then(|idx| ts.visible_indices().iter().position(|entry| *entry == idx))
                    .map(|pos| (0, pos)),
            };
            let Some((ci, pos)) = position else {
                self.view_scroll_pending.remove(&pane);
                continue;
            };
            let applied = match mode {
                ViewMode::Grid => self
                    .grid_hit
                    .get(&pane)
                    .map(|hit| hit.cols.max(1))
                    .is_some_and(|cols| {
                        let cell = self.cell_position(pane, pos);
                        self.scroll_grid_row_to(pane, cell / cols, gpui::ScrollStrategy::Nearest)
                    }),
                ViewMode::Details => {
                    self.details_hit.contains_key(&pane)
                        && self.scroll_details_row_to(pane, pos, gpui::ScrollStrategy::Nearest)
                }
                ViewMode::List => {
                    let cell = self.cell_position(pane, pos);
                    if let (Some(_), Some(handle)) =
                        (self.list_hit.get(&pane), self.scroll_handles.get(&pane))
                    {
                        // List consumes the cell position (column-major, horizontal).
                        handle.scroll_to_item(cell, gpui::ScrollStrategy::Nearest);
                        true
                    } else {
                        false
                    }
                }
                ViewMode::Tree => {
                    if let Some(handle) = self.scroll_handles.get(&pane) {
                        handle.scroll_to_item(pos, gpui::ScrollStrategy::Nearest);
                        true
                    } else {
                        false
                    }
                }
                ViewMode::Miller => {
                    if let Some(handle) = self.miller_scroll_handles.get(&(pane, ci)) {
                        handle.scroll_to_item(pos, gpui::ScrollStrategy::Nearest);
                        true
                    } else {
                        false
                    }
                }
            };
            if applied {
                self.view_scroll_pending.remove(&pane);
                self.notify_pane(pane, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_keyboard_jumps_teleport_to_the_last_four_hundred_pixels_in_both_directions() {
        assert_eq!(bounded_scroll_start(0.0, -50_000.0), -49_600.0);
        assert_eq!(bounded_scroll_start(-50_000.0, 0.0), -400.0);
        assert_eq!(bounded_scroll_start(-100.0, -300.0), -100.0);
        assert_eq!(bounded_scroll_start(-100.0, -100.0), -100.0);
    }
    #[test]
    fn trailing_space_matches_the_viewport_and_can_be_disabled() {
        assert_eq!(file_scroll_limit(1000.0, 500.0, 0.4), 700.0);
        assert_eq!(file_scroll_limit(1000.0, 500.0, 0.0), 500.0);
        assert_eq!(file_scroll_limit(0.0, 500.0, 0.4), 0.0);
        assert_eq!(file_scroll_limit(100.0, 500.0, 0.4), 0.0);
        assert_eq!(file_scroll_limit(1000.0, 1000.0, 0.4), 400.0);
        // Even at the maximum scroll, the last file stays in the middle third.
        let relative = 990.0 - file_scroll_limit(1000.0, 500.0, 0.4);
        assert!((500.0 / 3.0..=500.0 * 2.0 / 3.0).contains(&relative));
    }

    #[test]
    fn native_scroll_handle_keeps_precise_units_and_the_actual_row_height() {
        let handle = gpui::ScrollHandle::new();
        handle.set_bottom_scroll_margin(0.4);
        handle.set_line_scroll_height(gpui::px(26.0));
        assert_eq!(handle.bottom_scroll_margin(), 0.4);
        let height = handle.line_scroll_height().unwrap();
        let notch = gpui::ScrollDelta::Lines(gpui::point(0.0, 3.0));
        assert_eq!(f32::from(notch.pixel_delta(height).y), 78.0);
        let precise = gpui::ScrollDelta::PreciseLines(gpui::point(0.0, 0.25));
        assert!(precise.precise());
        assert_eq!(f32::from(precise.pixel_delta(height).y), 6.5);
        let pixels = gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.0), gpui::px(2.25)));
        assert_eq!(f32::from(pixels.pixel_delta(height).y), 2.25);
    }

    #[test]
    fn canvas_scrollers_share_overscroll_easing_and_precise_input_cancellation() {
        use gpui::{point, px, size, Bounds, ScrollDelta, ScrollHandle};
        let handle = ScrollHandle::new();
        handle.set_smooth(0.32, 1.0);
        handle.set_bottom_scroll_margin(0.4);
        handle.set_line_scroll_height(px(26.0));
        let now = std::time::Instant::now();
        let viewport = Bounds::new(point(px(0.0), px(0.0)), size(px(300.0), px(500.0)));
        let content = size(px(300.0), px(1000.0));
        assert!(!handle.update_scroll_geometry(viewport, content, now));
        assert_eq!(handle.max_offset().y, px(700.0));
        handle.apply_scroll_delta(ScrollDelta::Lines(point(0.0, -3.0)), now);
        assert_eq!(handle.offset().y, px(0.0));
        assert!(handle.update_scroll_geometry(
            viewport,
            content,
            now + std::time::Duration::from_millis(16)
        ));
        let eased = handle.offset().y;
        assert!(eased < px(0.0) && eased > px(-78.0));
        handle.apply_scroll_delta(ScrollDelta::PreciseLines(point(0.0, -0.25)), now);
        assert_eq!(handle.offset().y, eased - px(6.5));
        assert!(!handle.update_scroll_geometry(
            viewport,
            content,
            now + std::time::Duration::from_secs(1)
        ));
        assert_eq!(handle.offset().y, eased - px(6.5));
        handle.update_scroll_geometry(viewport, size(px(300.0), px(0.0)), now);
        assert_eq!(handle.offset().y, px(0.0));
        assert_eq!(handle.max_offset().y, px(0.0));
    }
    #[test]
    fn remaps_tree_and_column_indices_without_selecting_a_different_file() {
        let old = ["parent", "child", "sibling"].map(PathBuf::from);
        let new = ["sibling", "parent"].map(PathBuf::from);
        let selection = SelectionState {
            selected: [1, 2].into_iter().collect(),
            keyboard_cursor: Some(2),
            selection_anchor: Some(1),
            last_clicked_index: Some(2),
            scroll_to_selected: true,
            last_pointer_click: None,
        };
        let result = remap(&selection, &old, &new);
        assert_eq!(result.selected, [0].into_iter().collect());
        assert_eq!(result.keyboard_cursor, Some(0));
        assert_eq!(result.selection_anchor, None);
        assert_eq!(result.last_clicked_index, Some(0));
    }
}
