//! Pane interactions: rubber-band selection, inline rename, type-to-filter, ctrl-scroll.
#![allow(clippy::too_many_lines)] // reason: cohesive UI block kept whole

use crate::app::{App, RubberTarget};
use crate::input::action::Action;
use crate::ui::edge_autoscroll;
use atlas_model::clamp::ClampLenient;
use atlas_model::layout_tree::Tile;
use atlas_model::pane::TabId;
use atlas_model::settings::ViewMode;
use gpui::prelude::*;
use gpui::{
    canvas, Context, DispatchPhase, Entity, Focusable, MouseMoveEvent, MouseUpEvent,
    ScrollWheelEvent, Window,
};
use gpui_component::input::{IndentInline, OutdentInline};
use gpui_component::{ActiveTheme, WindowExt};
use std::time::Instant;

mod selection_geometry;

impl App {
    pub(crate) fn suppress_navigation_double_click(
        &self,
        pane: TabId,
        position: gpui::Point<gpui::Pixels>,
        clicks: usize,
    ) -> bool {
        #[cfg(windows)]
        let interval = std::time::Duration::from_millis(u64::from(
            atlas_win_space::integration::double_click_interval_ms(),
        ));
        #[cfg(not(windows))]
        let interval = std::time::Duration::from_millis(500);
        self.recent_folder_navigation
            .as_ref()
            .is_some_and(|click| click.suppresses(pane, position, clicks, Instant::now(), interval))
    }
    pub(crate) fn queue_tree_folder_click(
        &mut self,
        pane: TabId,
        index: usize,
        path: std::path::PathBuf,
        origin: gpui::Point<gpui::Pixels>,
    ) {
        self.pending_folder_click =
            self.tab_data_for(pane)
                .map(|tab| crate::ui::click_navigation::FolderClick {
                    pane,
                    index,
                    tree: true,
                    source: tab.location.clone(),
                    path,
                    origin,
                });
        self.activate_tab(pane);
        if let Some(tab) = self.state.tab_states.get_mut(&pane) {
            tab.selection.select(index, &[index], false, false);
        }
    }
    pub(crate) fn navigate_clicked_folder(&mut self, pane: TabId, path: std::path::PathBuf) {
        self.navigate_entry(pane, path);
    }

    fn finish_folder_click(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(click) = self.pending_folder_click.take() else {
            return false;
        };
        let valid = self.state.settings.single_click_folders_only
            && self.tab_data_for(click.pane).is_some_and(|tab| {
                click.accepts_release(
                    position,
                    &tab.location,
                    self.rubber.as_ref().is_some_and(|rubber| rubber.started),
                )
            })
            && self.state.tab_states.get(&click.pane).is_some_and(|tab| {
                let rows;
                let entry = if click.tree {
                    rows = tab.visible_tree_rows_snapshot();
                    rows.get(click.index).map(|row| row.0)
                } else {
                    tab.entries.get(click.index)
                };
                entry.is_some_and(|entry| {
                    entry.kind() == atlas_model::file_entry::FileKind::Dir
                        && entry.path() == click.path
                })
            });
        if !valid {
            return false;
        }
        self.rubber = None;
        self.navigate_clicked_folder(click.pane, click.path);
        self.recent_folder_navigation = Some(crate::ui::click_navigation::NavigationClick {
            pane: click.pane,
            origin: position,
            at: Instant::now(),
        });
        self.notify_pane(click.pane, cx);
        cx.notify();
        true
    }

    pub(crate) fn pane_view_slider_value(&self, pane_id: TabId) -> f32 {
        self.tab_data_for(pane_id).map_or(0.0, |pane| {
            Self::slider_value_for_view(&pane.view_mode, pane.thumbnail_size)
        })
    }

    pub(crate) fn slider_value_for_view(mode: &ViewMode, thumbnail_size: f32) -> f32 {
        match mode {
            ViewMode::Details => 0.0,
            ViewMode::Tree => 1.0,
            ViewMode::List => 2.0,
            ViewMode::Grid => 3.0 + ((thumbnail_size - 64.0) / 16.0).round().clamp(0.0, 20.0),
            ViewMode::Miller => 0.0,
        }
    }

    pub(crate) fn set_pane_view_from_slider_value(&mut self, pane_id: TabId, value: f32) -> bool {
        let value = value.round().clamp(0.0, 23.0);
        if value <= 0.0 {
            self.set_pane_view_mode(pane_id, ViewMode::Details)
        } else if value <= 1.0 {
            self.set_pane_view_mode(pane_id, ViewMode::Tree)
        } else if value <= 2.0 {
            self.set_pane_view_mode(pane_id, ViewMode::List)
        } else {
            let thumbnail_size = 64.0 + (value - 3.0) * 16.0;
            self.set_pane_grid_thumbnail_size(pane_id, thumbnail_size)
        }
    }

    pub(crate) fn set_pane_view_mode(&mut self, pane_id: TabId, mode: ViewMode) -> bool {
        #[cfg(feature = "provider-host")]
        if matches!(mode, ViewMode::Tree | ViewMode::Miller)
            && self.tab_data_for(pane_id).is_some_and(|pane| {
                matches!(
                    pane.location,
                    atlas_model::location::Location::Remote { .. }
                )
            })
        {
            self.toast_info(rust_i18n::t!("cloud_flat_views_only").into_owned());
            return false;
        }
        if self
            .tab_data_for(pane_id)
            .is_some_and(|pane| pane.view_mode != mode)
        {
            self.capture_view_selection(pane_id);
        }
        let mut folder_pref_write = None;
        let mut changed = false;
        if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(pane_id) {
            if pane.view_mode != mode {
                pane.view_mode = mode.clone();
                changed = true;
            }
            if let atlas_model::location::Location::Directory(path) = &pane.location {
                folder_pref_write = Some((
                    path.clone(),
                    atlas_model::profiles::FolderViewPref {
                        recursive: false,
                        columns: None,
                        view_mode: mode.clone(),
                        sort_col: pane.sort_col.clone(),
                        sort_dir: pane.sort_dir.clone(),
                    },
                ));
            }
        }

        if !changed {
            return false;
        }

        self.activate_tab(pane_id);
        self.dirty_panes.insert(pane_id);
        self.save_folder_view_pref(folder_pref_write);
        self.reload_flat_entries_if_needed(pane_id, &mode);
        true
    }

    pub(crate) fn set_pane_grid_thumbnail_size(
        &mut self,
        pane_id: TabId,
        thumbnail_size: f32,
    ) -> bool {
        if self
            .tab_data_for(pane_id)
            .is_some_and(|pane| pane.view_mode != ViewMode::Grid)
        {
            self.capture_view_selection(pane_id);
        }
        let thumbnail_size = thumbnail_size.clamp(64.0, 384.0);
        let mut folder_pref_write = None;
        let mut changed = false;
        if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(pane_id) {
            if pane.view_mode != ViewMode::Grid {
                pane.view_mode = ViewMode::Grid;
                changed = true;
            }
            if (pane.thumbnail_size - thumbnail_size).abs() > f32::EPSILON {
                pane.thumbnail_size = thumbnail_size;
                changed = true;
            }
            if let atlas_model::location::Location::Directory(path) = &pane.location {
                folder_pref_write = Some((
                    path.clone(),
                    atlas_model::profiles::FolderViewPref {
                        recursive: false,
                        columns: None,
                        view_mode: ViewMode::Grid,
                        sort_col: pane.sort_col.clone(),
                        sort_dir: pane.sort_dir.clone(),
                    },
                ));
            }
        }

        if !changed {
            return false;
        }

        self.activate_tab(pane_id);
        self.dirty_panes.insert(pane_id);
        self.save_folder_view_pref(folder_pref_write);
        self.reload_flat_entries_if_needed(pane_id, &ViewMode::Grid);
        true
    }

    /// Show `mode` in the focused pane without persisting a folder preference.
    pub(crate) fn preview_pane_view(&mut self, mode: ViewMode) {
        let pane_id = self.state.focused_tile;
        #[cfg(feature = "provider-host")]
        if matches!(mode, ViewMode::Tree | ViewMode::Miller)
            && self.tab_data_for(pane_id).is_some_and(|pane| {
                matches!(
                    pane.location,
                    atlas_model::location::Location::Remote { .. }
                )
            })
        {
            return;
        }
        let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(pane_id) else {
            return;
        };
        if self.view_preview_before.is_none() {
            self.view_preview_before = Some((pane_id, pane.view_mode.clone()));
        }
        if pane.view_mode != mode {
            pane.view_mode = mode.clone();
            self.dirty_panes.insert(pane_id);
            self.reload_flat_entries_if_needed(pane_id, &mode);
        }
    }

    pub(crate) fn restore_pane_view_preview(&mut self) {
        let Some((pane_id, mode)) = self.view_preview_before.take() else {
            return;
        };
        if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(pane_id) {
            if pane.view_mode != mode {
                pane.view_mode = mode.clone();
                self.dirty_panes.insert(pane_id);
                self.reload_flat_entries_if_needed(pane_id, &mode);
            }
        }
    }

    pub(crate) fn preview_icon_style(&mut self, style: atlas_model::settings::IconStyle) {
        if self.icon_style_preview_before.is_none() {
            self.icon_style_preview_before = Some(self.state.settings.icon_style);
        }
        if self.state.settings.icon_style != style {
            self.state.settings.icon_style = style;
            self.apply_profile_icons();
            self.relist_panes(|_| true);
        }
    }

    pub(crate) fn restore_icon_style_preview(&mut self) {
        let Some(style) = self.icon_style_preview_before.take() else {
            return;
        };
        if self.state.settings.icon_style != style {
            self.state.settings.icon_style = style;
            self.apply_profile_icons();
            self.relist_panes(|_| true);
        }
    }

    fn reload_flat_entries_if_needed(&mut self, pane_id: TabId, mode: &ViewMode) {
        if *mode == ViewMode::Miller {
            return;
        }
        let needs_load = self
            .state
            .tab_states
            .get(&pane_id)
            .is_some_and(|ts| ts.entries.is_empty());
        if needs_load {
            if let Some(td) = self.tab_data_for(pane_id) {
                let loc = td.location.clone();
                let col = td.sort_col.clone();
                let dir = td.sort_dir.clone();
                self.dispatch_list(pane_id, loc, col, dir);
            }
        }
    }

    fn rubber_target_bounds(&self, target: RubberTarget) -> Option<gpui::Bounds<gpui::Pixels>> {
        if let Some(rubber) = self.rubber.as_ref() {
            if rubber.target == target {
                return Some(rubber.target_bounds);
            }
        }
        match target {
            RubberTarget::Pane(pane) => self.rubber_pane_bounds(pane),
            RubberTarget::MillerColumn(pane, column) => {
                self.miller_column_bounds.get(&(pane, column)).copied()
            }
            RubberTarget::Dropzone => self.dropzone_bounds,
        }
    }

    fn rubber_pane_bounds(&self, pane: TabId) -> Option<gpui::Bounds<gpui::Pixels>> {
        // Details records the actual rows viewport. The generic pane bounds also
        // include breadcrumb/header chrome, which shifts the marquee's content
        // coordinates by that chrome height and selects the wrong rows.
        if let Some(tab) = self.tab_data_for(pane) {
            match tab.view_mode {
                ViewMode::Details => {
                    if let Some(model) = self.details_hit.get(&pane) {
                        return Some(model.bounds);
                    }
                }
                ViewMode::Grid => {
                    if let Some(model) = self.grid_hit.get(&pane) {
                        return Some(model.bounds);
                    }
                }
                ViewMode::List => {
                    if let Some(model) = self.list_hit.get(&pane) {
                        return Some(model.bounds);
                    }
                }
                _ => {}
            }
        }
        self.pane_view_bounds
            .get(&pane)
            .or_else(|| self.pane_layout_bounds.get(&pane))
            .copied()
    }

    pub(crate) fn clamp_rubber_target_point(
        &self,
        target: RubberTarget,
        pos: gpui::Point<gpui::Pixels>,
    ) -> gpui::Point<gpui::Pixels> {
        let Some(bounds) = self.rubber_target_bounds(target) else {
            return pos;
        };
        gpui::point(
            gpui::px(
                f32::from(pos.x).clamp_lenient(f32::from(bounds.left()), f32::from(bounds.right())),
            ),
            gpui::px(
                f32::from(pos.y).clamp_lenient(f32::from(bounds.top()), f32::from(bounds.bottom())),
            ),
        )
    }

    fn clamp_point_to_bounds(
        bounds: gpui::Bounds<gpui::Pixels>,
        pos: gpui::Point<gpui::Pixels>,
    ) -> gpui::Point<gpui::Pixels> {
        gpui::point(
            gpui::px(
                f32::from(pos.x).clamp_lenient(f32::from(bounds.left()), f32::from(bounds.right())),
            ),
            gpui::px(
                f32::from(pos.y).clamp_lenient(f32::from(bounds.top()), f32::from(bounds.bottom())),
            ),
        )
    }

    /// A deferred selected-item click is a possible file drag, not a marquee.
    /// Consume it before the source hands the existing group to GPUI/OLE.
    pub(crate) fn prepare_selection_file_drag(&mut self) -> bool {
        if self
            .rubber
            .as_ref()
            .is_some_and(|r| r.selected_click.is_some())
        {
            self.rubber = None;
        }
        self.rubber.is_none()
    }
    pub(crate) fn ensure_drag_selection(&mut self, pane: TabId, entry_idx: usize) {
        if let Some(ts) = self.state.tab_states.get_mut(&pane) {
            if !ts.selection.selected.contains(&entry_idx) {
                ts.selection.selected.clear();
                ts.selection.selected.insert(entry_idx);
                // The cursor follows: menu "Rename"/F2 and Shift ranges must act
                // on the item that was right-clicked or dragged, not the old one.
                ts.selection.keyboard_cursor = Some(entry_idx);
                ts.selection.selection_anchor = Some(entry_idx);
                ts.selection.last_clicked_index = Some(entry_idx);
            } else {
                // Inside a multi-selection: keep it, but focus the clicked item.
                ts.selection.keyboard_cursor = Some(entry_idx);
            }
        }
    }

    pub(crate) fn select_context_menu_paths(
        &mut self,
        pane: TabId,
        entry_idx: usize,
    ) -> Vec<std::path::PathBuf> {
        self.ensure_drag_selection(pane, entry_idx);
        self.selected_paths()
    }

    pub(crate) fn details_entry_at(
        &self,
        pane: TabId,
        pos: gpui::Point<gpui::Pixels>,
    ) -> Option<usize> {
        self.details_hit
            .get(&pane)
            .and_then(|model| model.hit_entry(pos))
    }

    pub(crate) fn details_contains_point(
        &self,
        pane: TabId,
        pos: gpui::Point<gpui::Pixels>,
    ) -> bool {
        self.details_hit
            .get(&pane)
            .is_some_and(|model| model.bounds.contains(&pos))
    }

    pub(crate) fn details_row_selected(&self, pane: TabId, entry_idx: usize) -> bool {
        self.state
            .tab_states
            .get(&pane)
            .is_some_and(|ts| ts.selection.selected.contains(&entry_idx))
    }

    pub(crate) fn details_hover_at(
        &mut self,
        pane: TabId,
        pos: gpui::Point<gpui::Pixels>,
    ) -> Option<(TabId, usize)> {
        self.details_entry_at(pane, pos).map(|idx| (pane, idx))
    }

    pub(crate) fn details_left_mouse_down(
        &mut self,
        pane: TabId,
        entry_idx: Option<usize>,
        pos: gpui::Point<gpui::Pixels>,
        shift: bool,
        ctrl: bool,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        self.selection_left_mouse_down(
            RubberTarget::Pane(pane),
            entry_idx,
            pos,
            shift,
            ctrl,
            click_count,
            cx,
        );
    }

    /// One pointer policy for all selectable surfaces: unselected/background
    /// starts a marquee; selected items preserve the group for a file drag.
    pub(crate) fn selection_left_mouse_down(
        &mut self,
        target: RubberTarget,
        entry_idx: Option<usize>,
        pos: gpui::Point<gpui::Pixels>,
        shift: bool,
        ctrl: bool,
        click_count: usize,
        cx: &mut Context<Self>,
    ) {
        self.pending_folder_click = None;
        if let RubberTarget::Pane(pane) | RubberTarget::MillerColumn(pane, _) = target {
            if self.suppress_navigation_double_click(pane, pos, click_count) {
                return;
            }
        }
        if self.rename_edit.is_some() {
            self.commit_rename(cx);
        }
        if self.ctx_menu.is_some() {
            return;
        }
        if entry_idx.is_none() && click_count >= 2 && !shift && !ctrl {
            if let RubberTarget::Pane(pane) | RubberTarget::MillerColumn(pane, _) = target {
                self.rubber = None;
                self.activate_tab(pane);
                let parent = match target {
                    RubberTarget::MillerColumn(_, index) => self
                        .state
                        .tab_states
                        .get(&pane)
                        .and_then(|state| state.miller.columns.get(index))
                        .and_then(|column| column.location.ascend()),
                    _ => self
                        .tab_data_for(pane)
                        .and_then(|tab| tab.location.ascend()),
                };
                if let Some(location) = parent {
                    self.process_actions([Action::NavigateLocation {
                        tab: pane,
                        location,
                    }]);
                }
                self.notify_pane(pane, cx);
                return;
            }
        }
        if let (RubberTarget::Pane(pane), Some(index)) = (target, entry_idx) {
            if let Some(entry) = self
                .state
                .tab_states
                .get(&pane)
                .and_then(|tab| tab.entries.get(index))
            {
                if self.state.settings.single_click_opens_folder(
                    entry.kind() == atlas_model::file_entry::FileKind::Dir,
                    click_count,
                    shift || ctrl,
                ) {
                    if let Some(tab) = self.tab_data_for(pane) {
                        self.pending_folder_click =
                            Some(crate::ui::click_navigation::FolderClick {
                                pane,
                                index,
                                tree: false,
                                source: tab.location.clone(),
                                path: entry.path(),
                                origin: pos,
                            });
                    }
                }
            }
        }
        let single_folder_click = self.pending_folder_click.is_some();
        let selected = entry_idx.is_some_and(|idx| match target {
            RubberTarget::Pane(pane) => self.details_row_selected(pane, idx),
            RubberTarget::MillerColumn(pane, column) => self
                .state
                .tab_states
                .get(&pane)
                .and_then(|ts| ts.miller.columns.get(column))
                .is_some_and(|col| col.selection.selected.contains(&idx)),
            RubberTarget::Dropzone => self
                .state
                .sidebar_drop_zone
                .get(idx)
                .is_some_and(|item| self.dropzone_selection.selected.contains(&item.path)),
        });
        if let RubberTarget::Pane(pane) = target {
            if let Some(ts) = self.state.tab_states.get_mut(&pane) {
                if let Some(idx) = entry_idx {
                    let eligible = click_count == 1 && !shift && !ctrl && !single_folder_click;
                    let rename =
                        ts.selection
                            .slow_rename_click(idx, std::time::Instant::now(), eligible);
                    if rename && selected && ts.selection.selected.len() == 1 {
                        // Tree selection indexes visible tree rows, not `entries`.
                        let is_tree = self.state.pane_tree.tiles.get(pane).is_some_and(|tile| {
                            matches!(tile, atlas_model::layout_tree::Tile::Pane(p) if p.view_mode == ViewMode::Tree)
                        });
                        if let Some(path) = ts.selection_path(is_tree, idx) {
                            self.pending_rename = Some((pane, path, false));
                        }
                    }
                } else {
                    ts.selection.last_pointer_click = None;
                }
            }
        }
        if let RubberTarget::MillerColumn(pane, column) = target {
            if let Some(col) = self
                .state
                .tab_states
                .get_mut(&pane)
                .and_then(|state| state.miller.columns.get_mut(column))
            {
                if let Some(index) = entry_idx {
                    let slow = col.selection.slow_rename_click(
                        index,
                        std::time::Instant::now(),
                        click_count == 1 && !shift && !ctrl,
                    );
                    if slow && selected && col.selection.selected.len() == 1 {
                        if let Some(entry) = col.entries.get(index) {
                            self.pending_rename = Some((pane, entry.path(), false));
                        }
                    }
                } else {
                    col.selection.last_pointer_click = None;
                }
            }
        }
        let selected_click = selected
            && !shift
            && !ctrl
            && click_count == 1
            && match target {
                RubberTarget::Pane(pane) => self
                    .state
                    .tab_states
                    .get(&pane)
                    .is_some_and(|ts| ts.selection.selected.len() > 1),
                RubberTarget::MillerColumn(pane, column) => self
                    .state
                    .tab_states
                    .get(&pane)
                    .and_then(|ts| ts.miller.columns.get(column))
                    .is_some_and(|col| col.selection.selected.len() > 1),
                RubberTarget::Dropzone => self.dropzone_selection.selected.len() > 1,
            };
        if click_count < 2
            && ((!selected && (entry_idx.is_none() || (!shift && !ctrl))) || selected_click)
        {
            match target {
                RubberTarget::Pane(pane) => self.begin_rubber(pane, pos, shift || ctrl),
                RubberTarget::MillerColumn(pane, column) => {
                    self.begin_miller_rubber(pane, column, pos, shift || ctrl);
                }
                RubberTarget::Dropzone => self.begin_dropzone_rubber(pos, shift || ctrl),
            }
            if selected_click {
                if let Some(r) = self.rubber.as_mut() {
                    r.selected_click = entry_idx;
                }
            }
        }
        // Preserve the pre-press snapshot for Escape, then clear background clicks
        // immediately so cached panes never display the old focus cursor.
        if entry_idx.is_none() && !shift && !ctrl {
            match target {
                RubberTarget::Pane(pane) => {
                    if let Some(ts) = self.state.tab_states.get_mut(&pane) {
                        ts.selection.clear();
                    }
                }
                RubberTarget::MillerColumn(pane, column) => {
                    if let Some(col) = self
                        .state
                        .tab_states
                        .get_mut(&pane)
                        .and_then(|ts| ts.miller.columns.get_mut(column))
                    {
                        col.selection.clear();
                    }
                }
                RubberTarget::Dropzone => self.dropzone_selection.clear(),
            }
        }
        match target {
            RubberTarget::Pane(pane) => {
                self.activate_tab(pane);
                if let Some(idx) = entry_idx {
                    if click_count >= 2 && !shift && !ctrl {
                        self.process_actions(vec![Action::Activate { tile: pane, idx }]);
                    } else if !selected || shift || ctrl {
                        self.process_actions(vec![Action::Select {
                            tile: pane,
                            idx,
                            shift,
                            ctrl,
                        }]);
                    }
                }
                self.notify_pane(pane, cx);
            }
            RubberTarget::MillerColumn(pane, column) => {
                self.activate_tab(pane);
                if let Some(ts) = self.state.tab_states.get_mut(&pane) {
                    ts.miller.focused = column;
                    if let (Some(col), Some(idx)) = (ts.miller.columns.get_mut(column), entry_idx) {
                        if !selected || shift || ctrl {
                            let visible =
                                col.visible_indices(&ts.filter, ts.show_files, ts.show_folders);
                            col.selection.select(idx, &visible, shift, ctrl);
                        }
                    }
                }
                if click_count >= 2 && !shift && !ctrl {
                    if let Some(idx) = entry_idx {
                        self.process_actions(vec![Action::MillerActivate {
                            tile: pane,
                            column,
                            idx,
                        }]);
                    }
                }
                self.notify_pane(pane, cx);
            }
            RubberTarget::Dropzone => {
                if let Some(idx) = entry_idx {
                    let visible: Vec<_> = self
                        .state
                        .sidebar_drop_zone
                        .iter()
                        .map(|item| item.path.clone())
                        .collect();
                    if let Some(path) = visible.get(idx) {
                        if !selected || shift || ctrl {
                            self.dropzone_selection
                                .select(path.clone(), &visible, shift, ctrl);
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn details_right_mouse_down(
        &mut self,
        pane: TabId,
        entry_idx: Option<usize>,
        pos: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(entry_idx) = entry_idx {
            self.activate_tab(pane);
            let paths = self.select_context_menu_paths(pane, entry_idx);
            self.open_ctx_menu(paths, pos, window, cx);
            cx.notify();
            return;
        }

        if let Some(path) = self
            .tab_data_for(pane)
            .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()))
        {
            self.open_background_ctx_menu(Some(path), pos, window, cx);
            cx.notify();
        }
    }

    /// Items per column of an ungrouped (column-major) List pane, plus the number
    /// of whole columns in its viewport. `None` for every other layout.
    pub(crate) fn list_column_step(&self, pane: TabId) -> Option<(usize, usize)> {
        let tab = self.tab_data_for(pane)?;
        if tab.view_mode != ViewMode::List
            || self.state.tab_states.get(&pane)?.group_by != atlas_model::tab::GroupBy::None
        {
            return None;
        }
        let model = self.list_hit.get(&pane)?;
        let rows = model.major_rows;
        let page_cols = if model.pitch_x > 0.0 {
            ((f32::from(model.bounds.size.width) / model.pitch_x).floor() as usize).max(1)
        } else {
            1
        };
        (rows > 0).then_some((rows, page_cols))
    }

    /// Compute (cols, row_count) for autoscroll in the given pane. Uses tab_state
    /// pixel width for Grid/List; returns (1, count) for Details; (1, 0) for Miller.
    pub(crate) fn rubber_scroll_dims(&self, pane: TabId) -> (usize, usize) {
        let Some(ts) = self.state.tab_states.get(&pane) else {
            return (1, 0);
        };
        let Some(td) = self.tab_data_for(pane) else {
            return (1, 0);
        };
        // Grouped Details/List scroll over headers + files, as published by the view.
        let count = match (
            &td.view_mode,
            ts.group_by != atlas_model::tab::GroupBy::None,
        ) {
            (ViewMode::Details, true) => self
                .details_hit
                .get(&pane)
                .map_or(ts.visible.len(), |model| model.total_count),
            (ViewMode::List, true) => self
                .list_hit
                .get(&pane)
                .map_or(ts.visible.len(), |model| model.total_count),
            (ViewMode::Grid, true) => self
                .grid_hit
                .get(&pane)
                .map_or(ts.visible.len(), |model| model.total_count),
            _ => ts.visible.len(),
        };
        match &td.view_mode {
            atlas_model::settings::ViewMode::Details => (1, count),
            atlas_model::settings::ViewMode::Grid => {
                if let Some(model) = self.grid_hit.get(&pane) {
                    let cols = model.cols.max(1);
                    return (cols, count.div_ceil(cols));
                }
                let thumb = td.thumbnail_size;
                let gap = self.state.settings.grid_spacing.clamp(4.0, 48.0);
                let pad = (gap * 0.75).round().max(4.0);
                let pane_w = ts.last_pane_width.max(thumb + 1.0);
                let inner = (pane_w - 2.0 * pad).max(thumb);
                let cols = (((inner + gap) / (thumb + gap)).floor() as usize).max(1);
                (cols, count.div_ceil(cols))
            }
            // Column-major (Explorer) List: the vertical step is always one item;
            // horizontal moves use `list_column_step`.
            atlas_model::settings::ViewMode::List => (1, count),
            atlas_model::settings::ViewMode::Miller => (1, 0),
            atlas_model::settings::ViewMode::Tree => (1, count),
        }
    }

    /// Phase C: recompute the focused rubber-band selection by intersecting its
    /// rectangle (window coords) against the recorded visible-item rects.
    ///
    /// Any positive-area AABB overlap counts, matching the
    /// Finder/Explorer/marquee convention: a rect that merely grazes the corner of
    /// an item selects it. The old center-point test made items "jump" into the
    /// selection only once the marquee crossed their midpoint, which read as laggy
    /// and "not marking objects in real time".
    pub(crate) fn apply_rubber(&mut self) -> bool {
        let Some(r) = self.rubber.as_ref() else {
            return false;
        };
        if !r.started {
            return false;
        }
        let target = r.target;
        // Clamp both endpoints to the pane so the hit-test region matches the
        // marquee rectangle that is actually drawn (which is also clamped in the
        // paint callback). Without this the visible box and the selection region
        // could disagree near pane edges → "misalignment".
        let origin = self.clamp_rubber_target_point(target, r.origin);
        let current = self.clamp_rubber_target_point(target, r.current);
        let (ox, oy, cx_, cy) = match target {
            RubberTarget::Pane(_) | RubberTarget::MillerColumn(_, _) => {
                let cur = self.rubber_content_point(target, current);
                (
                    f32::from(r.content_origin.x),
                    f32::from(r.content_origin.y),
                    f32::from(cur.x),
                    f32::from(cur.y),
                )
            }
            RubberTarget::Dropzone => (
                f32::from(origin.x),
                f32::from(origin.y),
                f32::from(current.x),
                f32::from(current.y),
            ),
        };
        let (rl, rr) = (ox.min(cx_), ox.max(cx_));
        let (rt, rb) = (oy.min(cy), oy.max(cy));
        match target {
            RubberTarget::Pane(pane) => {
                let hits = self.rubber_content_hits_for_pane(pane, rl, rr, rt, rb);
                let Some(r) = self.rubber.as_mut() else {
                    return false;
                };
                let mut sel: std::collections::HashSet<usize> = if r.additive {
                    r.base.selected.clone()
                } else {
                    std::collections::HashSet::new()
                };
                sel.extend(hits);
                if let Some(ts) = self.state.tab_states.get_mut(&pane) {
                    let changed = ts.selection.selected != sel;
                    ts.selection.keyboard_cursor = r
                        .base
                        .keyboard_cursor
                        .filter(|i| sel.contains(i))
                        .or_else(|| sel.iter().min().copied());
                    ts.selection.selected = sel;
                    return changed;
                }
                false
            }
            RubberTarget::MillerColumn(pane, column) => {
                let Some(r) = self.rubber.as_ref() else {
                    return false;
                };
                let cursor = r.base.keyboard_cursor;
                let mut sel = if r.additive {
                    r.base.selected.clone()
                } else {
                    std::collections::HashSet::new()
                };
                if rr > rl && rb > rt {
                    sel.extend(self.rubber_content_hits_for_miller(pane, column, rt, rb));
                }
                if let Some(col) = self
                    .state
                    .tab_states
                    .get_mut(&pane)
                    .and_then(|ts| ts.miller.columns.get_mut(column))
                {
                    let changed = col.selection.selected != sel;
                    col.selection.keyboard_cursor = cursor
                        .filter(|i| sel.contains(i))
                        .or_else(|| sel.iter().min().copied());
                    col.selection.selected = sel;
                    return changed;
                }
                false
            }
            RubberTarget::Dropzone => {
                let mut hits = std::collections::HashSet::new();
                for (path, b) in &self.dropzone_rects {
                    let (bl, br) = (f32::from(b.left()), f32::from(b.right()));
                    let (bt, bb) = (f32::from(b.top()), f32::from(b.bottom()));
                    if rr > rl && rb > rt && rl < br && bl < rr && rt < bb && bt < rb {
                        hits.insert(path.clone());
                    }
                }
                let Some(r) = self.rubber.as_ref() else {
                    return false;
                };
                let mut sel: std::collections::HashSet<std::path::PathBuf> = if r.additive {
                    r.dropzone_base.selected.clone()
                } else {
                    std::collections::HashSet::new()
                };
                sel.extend(hits);
                let changed = self.dropzone_selection.selected != sel;
                self.dropzone_selection.keyboard_cursor = r
                    .dropzone_base
                    .keyboard_cursor
                    .clone()
                    .filter(|i| sel.contains(i));
                self.dropzone_selection.selected = sel;
                changed
            }
        }
    }

    /// Called after live geometry is published, before a view reads selection.
    /// This also tracks smooth wheel frames without waiting for mouse movement.
    pub(crate) fn refresh_rubber_for_pane(&mut self, pane: TabId, cx: &mut Context<Self>) {
        let active = self.rubber.as_ref().is_some_and(|r| {
            r.started
                && matches!(r.target,
            RubberTarget::Pane(p) | RubberTarget::MillerColumn(p, _) if p == pane)
        });
        if active && self.apply_rubber() {
            self.notify_pane(pane, cx);
            cx.notify();
        }
    }

    /// Only the current layout can consume a background click.
    fn pane_selection_hit(&self, pane: TabId, pos: gpui::Point<gpui::Pixels>) -> bool {
        let Some(tab) = self.tab_data_for(pane) else {
            return false;
        };
        match tab.view_mode {
            ViewMode::Details => {
                return self
                    .details_hit
                    .get(&pane)
                    .is_some_and(|m| m.hit_entry(pos).is_some())
            }
            ViewMode::Grid => {
                return self
                    .grid_hit
                    .get(&pane)
                    .is_some_and(|m| m.hit_entry(pos).is_some())
            }
            ViewMode::List => {
                return self
                    .list_hit
                    .get(&pane)
                    .is_some_and(|m| m.hit_entry(pos).is_some())
            }
            _ => {}
        }
        self.row_rects
            .borrow()
            .get(&pane)
            .is_some_and(|rects| rects.iter().any(|(_, b)| b.contains(&pos)))
    }

    fn rubber_content_point(
        &self,
        target: RubberTarget,
        pos: gpui::Point<gpui::Pixels>,
    ) -> gpui::Point<gpui::Pixels> {
        let Some(bounds) = self.rubber_target_bounds(target) else {
            return pos;
        };
        let (scroll_x, scroll_y) = self
            .rubber_scroll_handle(target)
            .map(|(_, handle)| (-f32::from(handle.offset().x), -f32::from(handle.offset().y)))
            .unwrap_or((0.0, 0.0));
        gpui::point(
            gpui::px(f32::from(pos.x) - f32::from(bounds.left()) + scroll_x),
            gpui::px(f32::from(pos.y) - f32::from(bounds.top()) + scroll_y),
        )
    }

    fn rubber_window_point_from_content(
        &self,
        target: RubberTarget,
        pos: gpui::Point<gpui::Pixels>,
    ) -> gpui::Point<gpui::Pixels> {
        let Some(bounds) = self.rubber_target_bounds(target) else {
            return pos;
        };
        let (scroll_x, scroll_y) = self
            .rubber_scroll_handle(target)
            .map(|(_, handle)| (-f32::from(handle.offset().x), -f32::from(handle.offset().y)))
            .unwrap_or((0.0, 0.0));
        gpui::point(
            gpui::px(f32::from(bounds.left()) + f32::from(pos.x) - scroll_x),
            gpui::px(f32::from(bounds.top()) + f32::from(pos.y) - scroll_y),
        )
    }

    fn rubber_content_hits_for_pane(
        &mut self,
        pane: TabId,
        rl: f32,
        rr: f32,
        rt: f32,
        rb: f32,
    ) -> std::collections::HashSet<usize> {
        let mut hits = std::collections::HashSet::new();
        if rr <= rl || rb <= rt {
            return hits;
        }
        let zoom = self.state.settings.global_zoom;
        let Some((mode, thumbnail_size)) = self
            .tab_data_for(pane)
            .map(|td| (td.view_mode.clone(), td.thumbnail_size))
        else {
            return hits;
        };
        {
            let model = match mode {
                ViewMode::Grid => self.grid_hit.get(&pane),
                ViewMode::List => self.list_hit.get(&pane),
                _ => None,
            };
            if let Some(model) = model {
                return selection_geometry::grid_hits(model, rl, rr, rt, rb);
            }
            if mode == ViewMode::Tree {
                let row_h = crate::ui::views::tree_dg::TREE_ROW_H * zoom;
                let count = self
                    .state
                    .tab_states
                    .get(&pane)
                    .map_or(0, |ts| ts.visible_tree_rows_snapshot().len());
                if rr > 0.0 {
                    hits.extend(selection_geometry::row_span(rt, rb, row_h, count));
                }
                return hits;
            }
        }
        let visible = self.view_cells(pane);
        let Some(ts) = self.state.tab_states.get_mut(&pane) else {
            return hits;
        };
        match mode {
            ViewMode::Details => {
                let row_h = crate::ui::views::details::DETAILS_ROW_H * zoom;
                hits.extend(
                    visible[selection_geometry::row_span(rt, rb, row_h, visible.len())]
                        .iter()
                        .copied()
                        .filter(|cell| !atlas_model::grouping::is_virtual_cell(*cell)),
                );
            }
            ViewMode::Grid => {
                let gap = self.state.settings.grid_spacing.clamp(4.0, 48.0) * zoom;
                let pad = (self.state.settings.grid_spacing.clamp(4.0, 48.0) * 0.75)
                    .round()
                    .max(4.0)
                    * zoom;
                let thumb = thumbnail_size * zoom;
                let cell_h = thumb * crate::ui::views::grid_dg::GRID_ICON_AREA
                    + crate::ui::views::grid_dg::GRID_LABEL_H * zoom;
                let pane_w = ts.last_pane_width.max(thumb + 1.0);
                let inner = (pane_w - 2.0 * pad).max(thumb);
                let cols = (((inner + gap) / (thumb + gap)).floor() as usize).max(1);
                for (pos, idx) in visible.iter().copied().enumerate() {
                    let row = pos / cols;
                    let col = pos % cols;
                    let left = pad + col as f32 * (thumb + gap);
                    let top = pad + row as f32 * (cell_h + gap);
                    if rl < left + thumb && left < rr && rt < top + cell_h && top < rb {
                        hits.insert(idx);
                    }
                }
            }
            ViewMode::List => {
                let cell_w = 220.0 * zoom;
                let gap = 6.0 * zoom;
                let stride = cell_w + gap;
                let avail = (ts.last_pane_width - 16.0 * zoom).max(stride);
                let cols = ((avail / stride).floor() as usize).max(1);
                let row_h = 24.0 * zoom;
                for (pos, idx) in visible.iter().copied().enumerate() {
                    let row = pos / cols;
                    let col = pos % cols;
                    let left = 8.0 * zoom + col as f32 * stride;
                    let top = row as f32 * row_h;
                    if rl < left + cell_w && left < rr && rt < top + row_h && top < rb {
                        hits.insert(idx);
                    }
                }
            }
            ViewMode::Miller => {}
            ViewMode::Tree => {}
        }
        hits
    }

    fn rubber_content_hits_for_miller(
        &self,
        pane: TabId,
        column: usize,
        rt: f32,
        rb: f32,
    ) -> std::collections::HashSet<usize> {
        let mut hits = std::collections::HashSet::new();
        let row_h = self
            .miller_row_rects
            .borrow()
            .get(&(pane, column))
            .and_then(|rects| rects.first())
            .map_or(24.0 * self.state.settings.global_zoom, |(_, bounds)| {
                f32::from(bounds.size.height)
            });
        let Some(state) = self.state.tab_states.get(&pane) else {
            return hits;
        };
        let visible = state.miller_visible_indices(column);
        hits.extend(
            visible[selection_geometry::row_span(rt, rb, row_h, visible.len())]
                .iter()
                .copied(),
        );
        hits
    }

    fn rubber_scroll_handle(&self, target: RubberTarget) -> Option<(TabId, gpui::ScrollHandle)> {
        match target {
            RubberTarget::Pane(pane) => self
                .scroll_handles
                .get(&pane)
                .map(|handle| (pane, handle.0.borrow().base_handle.clone())),
            RubberTarget::MillerColumn(pane, column) => self
                .miller_scroll_handles
                .get(&(pane, column))
                .map(|handle| (pane, handle.0.borrow().base_handle.clone())),
            RubberTarget::Dropzone => None,
        }
    }

    fn rubber_scroll_pixels(&mut self, target: RubberTarget, delta_y: f32) -> bool {
        if delta_y.abs() < 0.5 {
            return false;
        }
        if !self.rubber_target_has_scrollable_content(target) {
            return false;
        }
        let Some((pane, handle)) = self.rubber_scroll_handle(target) else {
            return false;
        };
        let offset = handle.offset();
        let horizontal = self.rubber_scrolls_horizontally(target);
        let max = f32::from(if horizontal {
            handle.max_offset().x
        } else {
            handle.max_offset().y
        });
        let cur = f32::from(if horizontal { offset.x } else { offset.y });
        let next = (cur + delta_y).clamp_lenient(-max, 0.0);
        if (next - cur).abs() < 0.5 {
            return false;
        }
        handle.set_offset(if horizontal {
            gpui::point(gpui::px(next), offset.y)
        } else {
            gpui::point(offset.x, gpui::px(next))
        });
        self.dirty_panes.insert(pane);
        true
    }

    /// Column-major List panes scroll horizontally; every other target is vertical.
    fn rubber_scrolls_horizontally(&self, target: RubberTarget) -> bool {
        matches!(target, RubberTarget::Pane(pane) if self.list_column_step(pane).is_some())
    }

    fn rubber_target_has_scrollable_content(&self, target: RubberTarget) -> bool {
        match target {
            RubberTarget::Pane(pane) => self
                .state
                .tab_states
                .get(&pane)
                .is_some_and(|ts| !ts.entries.is_empty()),
            RubberTarget::MillerColumn(pane, column) => self
                .state
                .tab_states
                .get(&pane)
                .is_some_and(|state| !state.miller_visible_indices(column).is_empty()),
            RubberTarget::Dropzone => false,
        }
    }

    /// Rubber selection needs an explicit outward gesture. Entering an edge zone
    /// (or beginning a marquee there) is not enough to start scrolling.
    fn update_rubber_edge_intent(
        &mut self,
        previous: gpui::Point<gpui::Pixels>,
        current: gpui::Point<gpui::Pixels>,
    ) {
        const INTENT_DISTANCE: f32 = 4.0;
        let horizontal = self
            .rubber
            .as_ref()
            .is_some_and(|rubber| self.rubber_scrolls_horizontally(rubber.target));
        let Some(rubber) = self.rubber.as_mut() else {
            return;
        };
        if !rubber.started {
            rubber.edge_intent = None;
            rubber.edge_intent_origin_y = None;
            return;
        }
        let activation = if horizontal {
            edge_autoscroll::activation_at_horizontal(rubber.target_bounds, current)
        } else {
            edge_autoscroll::activation_at(rubber.target_bounds, current)
        };
        let Some(activation) = activation else {
            rubber.edge_intent = None;
            rubber.edge_intent_origin_y = None;
            return;
        };
        let direction = activation.direction;
        // The intent origin stores the pointer coordinate along the scroll axis.
        let along = |p: gpui::Point<gpui::Pixels>| if horizontal { p.x } else { p.y };
        let with_along = |v: gpui::Pixels| {
            if horizontal {
                gpui::point(v, current.y)
            } else {
                gpui::point(current.x, v)
            }
        };
        if rubber.edge_intent != Some(direction) && rubber.edge_intent_origin_y.is_none() {
            rubber.edge_intent_origin_y = Some(along(previous));
        }
        let origin = rubber.edge_intent_origin_y.unwrap_or(along(previous));
        let moved_from_origin = direction.outward(with_along(origin), current);
        let moved_this_event = direction.outward(previous, current);

        // A meaningful reversal disarms the stationary frame loop immediately.
        if rubber.edge_intent == Some(direction) && moved_this_event <= -INTENT_DISTANCE {
            rubber.edge_intent = None;
            rubber.edge_intent_origin_y = Some(along(current));
        } else if moved_from_origin >= INTENT_DISTANCE {
            rubber.edge_intent = Some(direction);
        } else if moved_from_origin < 0.0 {
            rubber.edge_intent = None;
            rubber.edge_intent_origin_y = Some(along(current));
        }
    }

    fn rubber_edge_can_scroll(&self) -> bool {
        let Some(r) = self.rubber.as_ref().filter(|r| r.started) else {
            return false;
        };
        let activation = if self.rubber_scrolls_horizontally(r.target) {
            edge_autoscroll::activation_at_horizontal(r.target_bounds, r.current)
        } else {
            edge_autoscroll::activation_at(r.target_bounds, r.current)
        };
        let Some(activation) = activation.filter(|a| r.edge_intent == Some(a.direction)) else {
            return false;
        };
        let Some((_, handle)) = self.rubber_scroll_handle(r.target) else {
            return false;
        };
        let horizontal = activation.direction.is_horizontal();
        let offset = handle.offset();
        let current = f32::from(if horizontal { offset.x } else { offset.y });
        let max = f32::from(if horizontal {
            handle.max_offset().x
        } else {
            handle.max_offset().y
        });
        (edge_autoscroll::next_offset(
            current,
            max,
            activation.direction,
            activation.strength,
            1.0 / 60.0,
        ) - current)
            .abs()
            >= 0.01
    }

    /// Advance one RubberSelect frame after directional intent has armed it.
    /// The selection is recomputed after every real offset change, so a stationary
    /// cursor still selects rows continuously while the viewport moves.
    fn tick_rubber_edge_autoscroll(&mut self, now: Instant, cx: &mut Context<Self>) -> bool {
        let Some((target, bounds, current, intent)) = self.rubber.as_ref().map(|rubber| {
            (
                rubber.target,
                rubber.target_bounds,
                rubber.current,
                rubber.edge_intent,
            )
        }) else {
            return false;
        };
        let activation = if self.rubber_scrolls_horizontally(target) {
            edge_autoscroll::activation_at_horizontal(bounds, current)
        } else {
            edge_autoscroll::activation_at(bounds, current)
        };
        let Some(activation) = activation else {
            return false;
        };
        if intent != Some(activation.direction)
            || !self.rubber_target_has_scrollable_content(target)
        {
            return false;
        }
        let elapsed = self
            .rubber
            .as_mut()
            .and_then(|rubber| rubber.last_scroll_frame.replace(now))
            .map_or(1.0 / 60.0, |last| now.duration_since(last).as_secs_f32());
        let Some((pane, handle)) = self.rubber_scroll_handle(target) else {
            return false;
        };
        if !edge_autoscroll::tick_handle(&handle, activation, elapsed) {
            return false;
        }
        self.dirty_panes.insert(pane);
        self.apply_rubber();
        self.notify_pane(pane, cx);
        cx.notify();
        true
    }

    fn rubber_wheel_delta(&self, ev: &ScrollWheelEvent) -> f32 {
        let handle = match self.rubber.as_ref().map(|rubber| rubber.target) {
            Some(RubberTarget::MillerColumn(pane, column)) => {
                self.miller_scroll_handles.get(&(pane, column))
            }
            Some(RubberTarget::Pane(pane)) => self.scroll_handles.get(&pane),
            _ => self.scroll_handles.get(&self.state.focused_tile),
        };
        let row_h = handle
            .and_then(|handle| handle.0.borrow().base_handle.line_scroll_height())
            .unwrap_or(gpui::px(20.0));
        match ev.delta {
            gpui::ScrollDelta::Pixels(p) => f32::from(p.y),
            gpui::ScrollDelta::Lines(l) | gpui::ScrollDelta::PreciseLines(l) => {
                l.y * f32::from(row_h)
            }
        }
    }

    pub(crate) fn scroll_grid_row_to(
        &mut self,
        pane: TabId,
        row: usize,
        strategy: gpui::ScrollStrategy,
    ) -> bool {
        let Some(Tile::Pane(pane_data)) = self.state.pane_tree.tiles.get(pane) else {
            return false;
        };
        if !matches!(pane_data.view_mode, ViewMode::Grid) {
            return false;
        }
        let Some(handle) = self.scroll_handles.get(&pane) else {
            return false;
        };

        let fallback_gap = self.state.settings.grid_spacing.clamp(4.0, 48.0);
        let fallback_pad = (fallback_gap * 0.75).round().max(4.0);
        let fallback_h = pane_data.thumbnail_size * crate::ui::views::grid_dg::GRID_ICON_AREA
            + crate::ui::views::grid_dg::GRID_LABEL_H;
        // Hit geometry is measured in the same zoom-scaled pixels as the
        // viewport. Recomputing unscaled dimensions can scroll a visible edit away.
        let (pad, row_pitch, row_h) = self.grid_hit.get(&pane).map_or(
            (fallback_pad, fallback_h + fallback_gap, fallback_h),
            |model| (model.pad_y, model.pitch_y, model.cell_h),
        );
        let gap = (row_pitch - row_h).max(0.0);
        let bands = self
            .grid_hit
            .get(&pane)
            .map(|model| model.bands.clone())
            .unwrap_or_default();
        let (_, row_count) = self.rubber_scroll_dims(pane);
        if row_count == 0 {
            return true;
        }

        let base = handle.0.borrow().base_handle.clone();
        let viewport_h = f32::from(base.bounds().size.height);
        if viewport_h <= 1.0 {
            return false;
        }

        let row_top = pad + bands.row_top(row, row_pitch);
        let row_bottom = row_top + row_h;
        let content_h = pad * 2.0 + row_count as f32 * row_pitch - gap + bands.total();
        let max_scroll = self.file_scroll_max(content_h, viewport_h);
        let offset = base.offset();
        let current_top = (-f32::from(offset.y)).max(0.0);
        let target = match strategy {
            gpui::ScrollStrategy::Top => row_top,
            gpui::ScrollStrategy::Center => row_top - (viewport_h - row_h) * 0.5,
            gpui::ScrollStrategy::Bottom => row_bottom - viewport_h,
            gpui::ScrollStrategy::Nearest => {
                if row_top < current_top {
                    row_top
                } else if row_bottom > current_top + viewport_h {
                    row_bottom - viewport_h
                } else {
                    current_top
                }
            }
        }
        .clamp_lenient(0.0, max_scroll);

        self.set_keyboard_scroll_offset(&base, gpui::point(offset.x, gpui::px(-target)));
        true
    }

    pub(crate) fn scroll_details_row_to(
        &mut self,
        pane: TabId,
        row: usize,
        strategy: gpui::ScrollStrategy,
    ) -> bool {
        let row = self.cell_position(pane, row);
        let row_count = self.view_cells(pane).len();
        let Some(Tile::Pane(pane_data)) = self.state.pane_tree.tiles.get(pane) else {
            return false;
        };
        if !matches!(pane_data.view_mode, ViewMode::Details) {
            return false;
        }
        let Some(handle) = self.scroll_handles.get(&pane) else {
            return false;
        };

        let row_pitch = self
            .details_hit
            .get(&pane)
            .map_or(crate::ui::views::details::DETAILS_ROW_H, |model| {
                model.row_h
            });
        if row_count == 0 {
            return true;
        }

        let base = handle.0.borrow().base_handle.clone();
        let viewport_h = f32::from(base.bounds().size.height);
        if viewport_h <= 1.0 {
            return false;
        }

        let row_top = row as f32 * row_pitch;
        let row_bottom = row_top + row_pitch;
        let content_h = row_count as f32 * row_pitch;
        let max_scroll = self.file_scroll_max(content_h, viewport_h);
        let offset = base.offset();
        let current_top = (-f32::from(offset.y)).max(0.0);
        let target = match strategy {
            gpui::ScrollStrategy::Top => row_top,
            gpui::ScrollStrategy::Center => row_top - (viewport_h - row_pitch) * 0.5,
            gpui::ScrollStrategy::Bottom => row_bottom - viewport_h,
            gpui::ScrollStrategy::Nearest => {
                if row_top < current_top {
                    row_top
                } else if row_bottom > current_top + viewport_h {
                    row_bottom - viewport_h
                } else {
                    current_top
                }
            }
        }
        .clamp_lenient(0.0, max_scroll);

        self.set_keyboard_scroll_offset(&base, gpui::point(offset.x, gpui::px(-target)));
        true
    }

    pub(crate) fn scroll_selected_into_view(
        &mut self,
        pane: TabId,
        strategy: gpui::ScrollStrategy,
    ) -> bool {
        let mode = self.tab_data_for(pane).map(|tab| tab.view_mode.clone());
        if mode == Some(ViewMode::Tree) {
            let row = self.state.tab_states.get(&pane).and_then(|tab| {
                tab.selection
                    .keyboard_cursor
                    .filter(|row| *row < tab.visible_tree_rows_snapshot().len())
            });
            if let (Some(row), Some(scroll)) = (row, self.scroll_handles.get(&pane)) {
                scroll.scroll_to_item(row, strategy);
                return true;
            }
            return false;
        }
        if mode == Some(ViewMode::Miller) {
            let selected = self.state.tab_states.get(&pane).and_then(|tab| {
                tab.miller
                    .columns
                    .get(tab.miller.focused)?
                    .primary_selected()
                    .and_then(|row| {
                        tab.miller_visible_indices(tab.miller.focused)
                            .iter()
                            .position(|i| *i == row)
                    })
                    .map(|row| (tab.miller.focused, row))
            });
            if let Some((column, row)) = selected {
                if let Some(scroll) = self.miller_scroll_handles.get(&(pane, column)) {
                    scroll.scroll_to_item(row, strategy);
                    return true;
                }
            }
            return false;
        }
        let flat_visible = self.flat_visible_indices(pane);
        let Some((visible_pos, mode)) = (|| {
            let ts = self.state.tab_states.get_mut(&pane)?;
            let cursor = ts
                .selection
                .keyboard_cursor
                .or_else(|| ts.selection.selected.iter().next().copied())?;
            let visible_pos = flat_visible.iter().position(|idx| *idx == cursor)?;
            let mode = self.tab_data_for(pane)?.view_mode.clone();
            Some((visible_pos, mode))
        })() else {
            return false;
        };

        match mode {
            ViewMode::Grid => {
                let (cols, _) = self.rubber_scroll_dims(pane);
                {
                    let cell = self.cell_position(pane, visible_pos);
                    self.scroll_grid_row_to(pane, cell / cols.max(1), strategy)
                }
            }
            ViewMode::List => {
                // List consumes the cell position (column-major, horizontal).
                let cell = self.cell_position(pane, visible_pos);
                if let Some(handle) = self.scroll_handles.get(&pane) {
                    handle.scroll_to_item(cell, strategy);
                    true
                } else {
                    false
                }
            }
            ViewMode::Details => self.scroll_details_row_to(pane, visible_pos, strategy),
            ViewMode::Miller => false,
            ViewMode::Tree => {
                if let Some(handle) = self.scroll_handles.get(&pane) {
                    handle.scroll_to_item(visible_pos, strategy);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Selected, renamable files of `pane` in display order (empty below 2).
    pub(crate) fn selected_rename_paths(
        &self,
        pane: TabId,
        visible: &[usize],
    ) -> Vec<std::path::PathBuf> {
        let Some(ts) = self.state.tab_states.get(&pane) else {
            return Vec::new();
        };
        let paths: Vec<_> = visible
            .iter()
            .filter(|i| ts.selection.selected.contains(i))
            .filter_map(|&i| ts.entries.get(i))
            .filter(|e| e.archive_root().is_none())
            .map(|e| e.path())
            .collect();
        if paths.len() < 2 {
            Vec::new()
        } else {
            paths
        }
    }

    /// Keep the other selected names in lockstep with the rename input.
    pub(crate) fn sync_batch_from_input(&mut self, cx: &mut Context<Self>) {
        let (Some(edit), Some(batch)) =
            (self.rename_edit.as_ref(), self.state.batch_rename.as_mut())
        else {
            return;
        };
        let input = edit.input.read(cx);
        let text = input.value().to_string();
        let cursor = input.cursor().min(text.len());
        let col = text.get(..cursor).map_or(0, |t| t.chars().count());
        let pane = edit.pane;
        // Selection covering exactly `0..caret` (stem selected on rename start) is
        // mirrored on every line; any other selection shows only carets.
        let range = input.selected_range();
        let select_to_caret = range.start == 0 && range.end > 0 && range.end == cursor;
        let moved = batch.apply_primary(&text, col);
        let selection_changed = batch.set_select_to_caret(select_to_caret);
        if moved || selection_changed {
            self.batch_blink_origin = std::time::Instant::now();
            self.batch_blink_visible = true;
            self.notify_pane(pane, cx);
        }
    }

    /// Write the primary line (after a token command) back into the real input.
    pub(crate) fn push_batch_to_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(edit), Some(batch)) =
            (self.rename_edit.as_ref(), self.state.batch_rename.as_ref())
        else {
            return;
        };
        let Some(text) = batch.multi_lines.get(batch.primary).cloned() else {
            return;
        };
        let col = batch.cursor_pos as u32;
        let pane = edit.pane;
        edit.input.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            state.set_cursor_position(gpui_component::input::Position::new(0, col), window, cx);
        });
        self.notify_pane(pane, cx);
    }

    /// `#`, Alt+N, Alt+D, Tab while a multi-file rename is live. True if handled.
    pub(crate) fn batch_rename_key(
        &mut self,
        key: &str,
        alt: bool,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(batch) = self.state.batch_rename.as_mut() else {
            return false;
        };
        match (key, alt) {
            ("n", true) => {
                batch.renumber(1);
            }
            ("d", true) => {
                batch.normalize_dates();
            }
            ("#", _) => {
                let pad = if batch.files.len() >= 100 { 3 } else { 2 };
                batch.insert_counter(pad);
            }
            ("tab", _) => batch.jump_boundary(!shift),
            _ => return false,
        }
        self.push_batch_to_input(window, cx);
        true
    }

    fn commit_batch_rename(&mut self, cx: &mut Context<Self>) -> bool {
        let count = self
            .state
            .batch_rename
            .as_ref()
            .map_or(0, |batch| batch.files.len());
        // This also protects the remote path, which dispatches individual actions.
        if !self.allow_edition_action(atlas_model::edition::Feature::BatchRename, count) {
            cx.notify();
            return false;
        }
        self.sync_batch_from_input(cx);
        let Some(batch) = self.state.batch_rename.as_ref() else {
            return false;
        };
        if batch.has_blocking_issues() {
            self.toast_error(rust_i18n::t!("batch_inline_conflict").to_string());
            cx.notify();
            return false;
        }
        let actions = self.batch_rename_actions();
        // Selection must follow the files to their new names (every selected row,
        // renamed or not), not just whichever one the caret was in.
        let new_paths: Vec<std::path::PathBuf> = batch
            .files
            .iter()
            .zip(&batch.previews)
            .filter_map(|(path, name)| path.parent().map(|parent| parent.join(name)))
            .collect();
        if let Some(edit) = self.rename_edit.take() {
            self.notify_pane(edit.pane, cx);
            if !actions.is_empty() {
                self.pending_batch_select = Some((edit.pane, new_paths, std::time::Instant::now()));
            }
        }
        self.close_batch_rename();
        if !actions.is_empty() {
            #[cfg(feature = "provider-host")]
            let remote = self.focused_remote_tab().is_some();
            #[cfg(not(feature = "provider-host"))]
            let remote = false;
            if remote {
                self.process_actions(actions);
            } else {
                let pairs = actions
                    .into_iter()
                    .filter_map(|action| match action {
                        Action::Rename { from, to } => Some((from, to)),
                        _ => None,
                    })
                    .collect();
                self.rename_batch(pairs);
            }
        }
        cx.notify();
        true
    }

    /// Begin inline rename of the focused pane's cursor item: pre-fill a native input
    /// with its current name and stash it in `rename_edit`.
    pub(crate) fn begin_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.state.focused_tile;
        let view_mode = self
            .tab_data_for(pane)
            .map(|tab| tab.view_mode.clone())
            .unwrap_or_default();
        let flat_visible = self.flat_visible_indices(pane);
        // 2+ files selected: the cursor row is a normal rename input and the
        // rest of the selection mirrors every edit.
        let batch_paths = if matches!(view_mode, ViewMode::Miller | ViewMode::Tree) {
            Vec::new()
        } else {
            self.selected_rename_paths(pane, &flat_visible)
        };
        let Some((entry_idx, visible_pos, path, name, initial_select_chars)) = (|| {
            let ts = self.state.tab_states.get_mut(&pane)?;
            if matches!(view_mode, ViewMode::Miller) {
                let col = ts.miller.columns.get(ts.miller.focused)?;
                let idx = col
                    .selection
                    .keyboard_cursor
                    .or_else(|| col.selection.selected.iter().copied().next())?;
                let entry = col.entries.get(idx)?;
                if entry.archive_root().is_some() {
                    return None;
                }
                let stem_chars = Some(rename_selection_chars(&entry.name, &entry.kind()));
                let position = ts
                    .miller_visible_indices(ts.miller.focused)
                    .iter()
                    .position(|i| *i == idx)?;
                return Some((
                    idx,
                    position,
                    entry.path(),
                    entry.name.to_string(),
                    stem_chars,
                ));
            }
            let idx = ts
                .selection
                .keyboard_cursor
                .or_else(|| ts.selection.selected.iter().copied().next())?;
            if matches!(view_mode, ViewMode::Tree) {
                // Tree selection indexes visible tree rows, not the flat listing.
                let rows = ts.visible_tree_rows_snapshot();
                let entry = rows.get(idx)?.0;
                if entry.archive_root().is_some() {
                    return None;
                }
                let stem_chars = Some(rename_selection_chars(&entry.name, &entry.kind()));
                return Some((idx, idx, entry.path(), entry.name.to_string(), stem_chars));
            }
            let visible_pos = flat_visible.iter().position(|&visible| visible == idx)?;
            let entry = ts.entries.get(idx)?;
            if entry.archive_root().is_some() {
                return None;
            }
            let stem_chars = Some(rename_selection_chars(&entry.name, &entry.kind()));
            Some((
                idx,
                visible_pos,
                entry.path(),
                entry.name.to_string(),
                stem_chars,
            ))
        })() else {
            return;
        };

        if batch_paths.contains(&path)
            && !self.allow_edition_action(
                atlas_model::edition::Feature::BatchRename,
                batch_paths.len(),
            )
        {
            cx.notify();
            return;
        }
        self.close_batch_rename();
        if batch_paths.contains(&path) {
            // Keep the whole selection; only the cursor row becomes the input.
            if let Some(ts) = self.state.tab_states.get_mut(&pane) {
                ts.selection.keyboard_cursor = Some(entry_idx);
                ts.selection.scroll_to_selected = false;
            }
            self.activate_tab(pane);
        } else {
            self.select_rename_target(pane, entry_idx);
        }
        match view_mode {
            ViewMode::Grid => {
                let (cols, _) = self.rubber_scroll_dims(pane);
                let cell = self.cell_position(pane, visible_pos);
                self.scroll_grid_row_to(pane, cell / cols.max(1), gpui::ScrollStrategy::Center);
            }
            ViewMode::Details => {
                self.scroll_details_row_to(pane, visible_pos, gpui::ScrollStrategy::Center);
            }
            ViewMode::List => {
                let cell = self.cell_position(pane, visible_pos);
                if let Some(handle) = self.scroll_handles.get(&pane) {
                    handle.scroll_to_item(cell, gpui::ScrollStrategy::Center);
                }
            }
            ViewMode::Miller => {
                // Reveal the edited row inside its (focused) column.
                let column = self.state.tab_states.get(&pane).map(|ts| ts.miller.focused);
                if let Some(handle) =
                    column.and_then(|c| self.miller_scroll_handles.get(&(pane, c)))
                {
                    handle.scroll_to_item(visible_pos, gpui::ScrollStrategy::Center);
                }
            }
            ViewMode::Tree => {
                if let Some(handle) = self.scroll_handles.get(&pane) {
                    handle.scroll_to_item(visible_pos, gpui::ScrollStrategy::Center);
                }
            }
        }

        let input = crate::ui::text_input::single_line(window, cx, "", name);
        input.update(cx, |state, cx| state.focus(window, cx));
        self.rename_edit = Some(crate::app::RenameEdit {
            pane,
            path,
            input,
            initial_select_chars,
        });
        let edited = self
            .rename_edit
            .as_ref()
            .map(|e| (e.path.clone(), e.input.clone()));
        if let Some((edited, input)) = edited {
            if let Some(primary) = batch_paths.iter().position(|p| *p == edited) {
                let mut batch = atlas_model::batch_rename::BatchRenameState::new(batch_paths);
                batch.primary = primary;
                self.batch_blink_origin = std::time::Instant::now();
                self.batch_blink_visible = true;
                self.state.batch_rename = Some(batch);
                self.batch_input_sub =
                    Some(cx.observe(&input, |this, _, cx| this.sync_batch_from_input(cx)));
            }
        }
        self.notify_pane(pane, cx);
        cx.notify();
    }

    /// Confirm the inline rename: validate the new name (cross-platform: reject path
    /// separators and the `.`/`..` specials) and dispatch the FS rename.
    pub(crate) fn commit_rename(&mut self, cx: &mut Context<Self>) {
        self.commit_rename_inner(true, cx);
    }

    fn commit_rename_inner(&mut self, select_target: bool, cx: &mut Context<Self>) -> bool {
        if self.state.batch_rename.is_some() && self.rename_edit.is_some() {
            return self.commit_batch_rename(cx);
        }
        let Some(edit) = self.rename_edit.as_ref() else {
            return false;
        };
        let new_name = edit.input.read(cx).value().to_string();
        let Some(parent) = edit.path.parent() else {
            return false;
        };
        let to = parent.join(&new_name);
        // Keep the editor and its text on validation errors, including Tab.
        if let Err(error) = atlas_core::fs::transfer::validate_filename(&new_name) {
            self.toast_error(crate::app::ops::filename_error_text(&error));
            cx.notify();
            return false;
        }
        #[cfg(feature = "provider-host")]
        let remote = self.focused_remote_tab().is_some();
        #[cfg(not(feature = "provider-host"))]
        let remote = false;
        if !remote {
            if let Some(error) = crate::app::ops::rename_target_error(&edit.path, &to) {
                self.toast_error(error);
                cx.notify();
                return false;
            }
        }
        let edit = self.rename_edit.take().expect("rename validated above");
        self.notify_pane(edit.pane, cx);
        cx.notify();
        self.state.focused_tile = edit.pane;
        if to != edit.path {
            if select_target {
                self.pending_select = Some((edit.pane, to.clone()));
            }
            self.process_actions(vec![Action::Rename {
                from: edit.path,
                to,
            }]);
        }
        true
    }

    pub(crate) fn commit_rename_and_move(
        &mut self,
        forward: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pane, path)) = self
            .rename_edit
            .as_ref()
            .map(|edit| (edit.pane, edit.path.clone()))
        else {
            return;
        };
        let next = self.rename_neighbor(pane, &path, forward);
        if !self.commit_rename_inner(false, cx) {
            return;
        }
        let Some((next_idx, next_path)) = next else {
            cx.notify();
            return;
        };
        self.select_rename_target(pane, next_idx);
        self.pending_rename = Some((pane, next_path, false));
        cx.notify();
    }

    pub(crate) fn rename_next_action(
        &mut self,
        _: &IndentInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_rename_and_move(true, window, cx);
        cx.stop_propagation();
    }

    pub(crate) fn rename_previous_action(
        &mut self,
        _: &OutdentInline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.commit_rename_and_move(false, window, cx);
        cx.stop_propagation();
    }

    fn rename_neighbor(
        &mut self,
        pane: TabId,
        path: &std::path::Path,
        forward: bool,
    ) -> Option<(usize, std::path::PathBuf)> {
        let view_mode = self.tab_data_for(pane)?.view_mode.clone();
        let flat_visible = self.flat_visible_indices(pane);
        let ts = self.state.tab_states.get_mut(&pane)?;
        if matches!(view_mode, ViewMode::Miller) {
            let col = ts.miller.columns.get(ts.miller.focused)?;
            let idx = col.entries.iter().position(|entry| entry.path() == path)?;
            let visible = ts.miller_visible_indices(ts.miller.focused);
            let position = visible.iter().position(|row| *row == idx)?;
            let next_idx = visible[neighbor_index(position, visible.len(), forward)?];
            let path = col.entries.get(next_idx)?.path();
            return Some((next_idx, path));
        }
        if matches!(view_mode, ViewMode::Tree) {
            // Tree rows (expanded children included) are the navigation order.
            let rows = ts.visible_tree_rows_snapshot();
            let pos = rows.iter().position(|row| row.0.path() == path)?;
            let next = neighbor_index(pos, rows.len(), forward)?;
            return Some((next, rows.get(next)?.0.path()));
        }

        let visible = flat_visible;
        let idx = ts.entries.iter().position(|entry| entry.path() == path)?;
        let pos = visible.iter().position(|&visible_idx| visible_idx == idx)?;
        let next_pos = neighbor_index(pos, visible.len(), forward)?;
        let next_idx = visible[next_pos];
        let path = ts.entries.get(next_idx)?.path();
        Some((next_idx, path))
    }

    fn select_rename_target(&mut self, pane: TabId, idx: usize) {
        let view_mode = self.tab_data_for(pane).map(|tab| tab.view_mode.clone());
        let Some(ts) = self.state.tab_states.get_mut(&pane) else {
            return;
        };
        if matches!(view_mode, Some(ViewMode::Miller)) {
            if let Some(col) = ts.miller.columns.get_mut(ts.miller.focused) {
                col.selection.selected.clear();
                col.selection.selected.insert(idx);
                col.selection.keyboard_cursor = Some(idx);
                col.selection.selection_anchor = Some(idx);
                col.selection.last_clicked_index = Some(idx);
            }
        } else {
            ts.selection.selected.clear();
            ts.selection.selected.insert(idx);
            ts.selection.keyboard_cursor = Some(idx);
            ts.selection.selection_anchor = Some(idx);
            ts.selection.last_clicked_index = Some(idx);
            ts.selection.scroll_to_selected = false;
        }
        self.activate_tab(pane);
    }

    /// Cancel the inline rename without touching the filesystem.
    pub(crate) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.close_batch_rename();
        if let Some(edit) = self.rename_edit.take() {
            self.notify_pane(edit.pane, cx);
            cx.notify();
        }
    }

    /// Cancel an in-flight rubber drag, restoring the pre-drag selection.
    pub(crate) fn cancel_rubber(&mut self) {
        self.pending_folder_click = None;
        if let Some(r) = self.rubber.take() {
            match r.target {
                RubberTarget::Pane(pane) => {
                    if let Some(ts) = self.state.tab_states.get_mut(&pane) {
                        ts.selection = r.base;
                    }
                }
                RubberTarget::MillerColumn(pane, column) => {
                    if let Some(col) = self
                        .state
                        .tab_states
                        .get_mut(&pane)
                        .and_then(|ts| ts.miller.columns.get_mut(column))
                    {
                        col.selection = r.base;
                    }
                }
                RubberTarget::Dropzone => {
                    self.dropzone_selection = r.dropzone_base;
                }
            }
        }
    }

    /// Phase C: start a rubber-band drag in `pane` at `pos` (window coords), capturing
    /// the current selection as the additive base. Shared by the pane's empty area and
    /// the Details "dead zone" column so the gesture begins the same way from either.
    pub(crate) fn begin_rubber(
        &mut self,
        pane: TabId,
        pos: gpui::Point<gpui::Pixels>,
        additive: bool,
    ) {
        // Context menu open: mousedown belongs to the menu, not rubber.
        if self.ctx_menu.is_some() {
            return;
        }
        // Miller has per-column selection — rubber band doesn't map onto it.
        if self
            .tab_data_for(pane)
            .is_some_and(|td| matches!(td.view_mode, ViewMode::Miller))
        {
            return;
        }
        // Clicking the empty list area dismisses a filter that has no text yet
        // (the "click outside to leave the field" affordance); a filter with a
        // query stays applied so the click can refine the selection instead.
        if self
            .state
            .tab_states
            .get(&pane)
            .is_some_and(|ts| ts.search.active && ts.search.query.trim().is_empty())
        {
            self.close_search_in_place(pane);
        }
        self.begin_selection_rubber(RubberTarget::Pane(pane), pos, additive);
    }

    pub(crate) fn begin_miller_rubber(
        &mut self,
        pane: TabId,
        column: usize,
        pos: gpui::Point<gpui::Pixels>,
        additive: bool,
    ) {
        if self.ctx_menu.is_some() {
            return;
        }
        self.activate_tab(pane);
        if let Some(ts) = self.state.tab_states.get_mut(&pane) {
            ts.miller.focused = column;
        }
        self.begin_selection_rubber(RubberTarget::MillerColumn(pane, column), pos, additive);
    }

    pub(crate) fn begin_dropzone_rubber(&mut self, pos: gpui::Point<gpui::Pixels>, additive: bool) {
        if self.ctx_menu.is_some() {
            return;
        }
        let staged = self
            .state
            .sidebar_drop_zone
            .iter()
            .map(|item| item.path.clone())
            .collect::<Vec<_>>();
        self.dropzone_selection.retain_visible(&staged);
        self.begin_selection_rubber(RubberTarget::Dropzone, pos, additive);
    }

    fn begin_selection_rubber(
        &mut self,
        target: RubberTarget,
        pos: gpui::Point<gpui::Pixels>,
        additive: bool,
    ) {
        let Some(target_bounds) = self.rubber_target_bounds(target) else {
            return;
        };
        let base = match target {
            RubberTarget::Pane(pane) => self
                .state
                .tab_states
                .get(&pane)
                .map(|ts| ts.selection.clone())
                .unwrap_or_default(),
            RubberTarget::MillerColumn(pane, column) => self
                .state
                .tab_states
                .get(&pane)
                .and_then(|ts| ts.miller.columns.get(column))
                .map(|col| col.selection.clone())
                .unwrap_or_default(),
            RubberTarget::Dropzone => Default::default(),
        };
        let dropzone_base = self.dropzone_selection.clone();
        let origin = Self::clamp_point_to_bounds(target_bounds, pos);
        self.rubber = Some(crate::app::RubberDrag {
            target,
            target_bounds,
            origin,
            current: origin,
            content_origin: self.rubber_content_point(target, origin),
            base,
            dropzone_base,
            additive,
            started: false,
            selected_click: None,
            edge_intent: None,
            edge_intent_origin_y: None,
            last_scroll_frame: None,
        });
    }

    /// True when the keyboard belongs to a text field or a managed modal, so the
    /// window-level type-to-filter must not steal the key. Uses gpui-component's
    /// own focus bookkeeping (`has_focused_input`) for component inputs, plus
    /// the DG menu's native input handle, which is not an `InputState` entity.
    pub(crate) fn any_text_input_focused(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.ctx_menu
            .as_ref()
            .is_some_and(|menu| menu.focus.is_focused(window))
            || window.has_focused_input(cx)
            || window.has_active_dialog(cx)
            || window.has_active_sheet(cx)
    }

    /// Window-level type-to-filter: a plain character (no modifiers, not space)
    /// typed while no text field is focused opens — or extends — the focused
    /// pane's local fuzzy filter.
    pub(crate) fn handle_global_type_to_filter(
        &mut self,
        ev: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The menu owns typing from the moment it opens, including the frame
        // before its DG input handler is painted or while focus is restored.
        if self.ctx_menu.is_some() {
            return;
        }
        let pane_id = self.state.focused_tile;
        let active = self
            .state
            .tab_states
            .get(&pane_id)
            .is_some_and(|ts| ts.search.active);
        if self.state.settings.vim_mode
            && !active
            && crate::input::shortcuts::is_vim_navigation_key(
                ev.keystroke.key.as_str(),
                ev.keystroke.modifiers.shift,
            )
        {
            return;
        }

        let Some(seed) = search_seed_from_key(ev) else {
            return;
        };
        if self.any_text_input_focused(window, cx) {
            return;
        }
        let o = &self.state.overlay;
        if o.show_goto_bar
            || o.show_palette
            || o.show_settings
            || o.show_guide
            || o.show_ext_palette
            || o.show_analyzer
            || o.transfer_conflict.is_some()
            || self.state.batch_rename.is_some()
            || self.rename_edit.is_some()
        {
            return;
        }
        let pane_id = self.state.focused_tile;
        if !matches!(self.state.pane_tree.tiles.get(pane_id), Some(Tile::Pane(_))) {
            return;
        }
        if self
            .state
            .tab_states
            .get(&pane_id)
            .is_some_and(|ts| ts.path_edit.editing)
        {
            return;
        }

        let active = self
            .state
            .tab_states
            .get(&pane_id)
            .is_some_and(|ts| ts.search.active);
        if !active {
            use atlas_model::settings::CharacterKeyAction;
            match self.state.settings.character_key_action {
                CharacterKeyAction::TriggerHotkey => return,
                CharacterKeyAction::SelectTypedItem => {
                    let now = Instant::now();
                    let previous = self.typed_selection.take().filter(|(pane, _, at)| {
                        *pane == pane_id
                            && now.duration_since(*at) < std::time::Duration::from_millis(900)
                    });
                    let mut query = previous.map_or_else(String::new, |(_, query, _)| query);
                    query.push_str(&seed);
                    let mode = self
                        .tab_data_for(pane_id)
                        .map(|tab| tab.view_mode.clone())
                        .unwrap_or_default();
                    let flat_visible = self.flat_visible_indices(pane_id);
                    let mut found = false;
                    if let Some(tab) = self.state.tab_states.get_mut(&pane_id) {
                        found = tab.select_typed_prefix_with_indices(
                            &mode,
                            &query,
                            false,
                            Some(&flat_visible),
                        );
                        if !found {
                            query = seed.clone();
                            found = tab.select_typed_prefix_with_indices(
                                &mode,
                                &query,
                                true,
                                Some(&flat_visible),
                            );
                        }
                    }
                    self.typed_selection = Some((pane_id, query, now));
                    if found {
                        if let Some(tab) = self.state.tab_states.get_mut(&pane_id) {
                            tab.selection.scroll_to_selected = true;
                        }
                        self.notify_pane(pane_id, cx);
                    }
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                CharacterKeyAction::TypeInFilter => {}
            }
        }
        if active {
            // Filter open but unfocused: append the char and refocus the field.
            if let Some(input) = self.search_inputs.get(&pane_id).cloned() {
                let new_value = format!("{}{}", input.read(cx).value(), seed);
                input.update(cx, |state, cx| state.set_value(new_value, window, cx));
                let focus = input.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
            if let Some(ts) = self.state.tab_states.get_mut(&pane_id) {
                ts.search.focus = true;
            }
        } else {
            self.open_search_in_place_seeded(pane_id, seed, window, cx);
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(crate) fn open_search_in_place_seeded(
        &mut self,
        pane_id: TabId,
        seed: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.state.focused_tile = pane_id;
        let input = self
            .search_inputs
            .entry(pane_id)
            .or_insert_with(|| crate::ui::text_input::single_line(window, cx, "", ""))
            .clone();
        input.update(cx, |state, cx| state.set_value(seed.clone(), window, cx));
        let focus = input.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        if let Some(ts) = self.state.tab_states.get_mut(&pane_id) {
            ts.search.active = true;
            ts.search.focus = true;
            ts.search.query = seed;
        }
        self.refresh_search_in_place(pane_id);
    }

    /// Ctrl+scroll on a pane: in Grid it zooms the thumbnail size; in every other
    /// view it cycles to the next/previous view mode.
    pub(crate) fn pane_ctrl_scroll(&mut self, pane_id: TabId, dy: f32) {
        let Some((mode, thumbnail_size)) = self
            .tab_data_for(pane_id)
            .map(|t| (t.view_mode.clone(), t.thumbnail_size))
        else {
            return;
        };
        if mode == ViewMode::Miller {
            return; // don't cycle out of Miller
        }
        let step = if dy > 0.0 { 1.0 } else { -1.0 };
        let value = Self::slider_value_for_view(&mode, thumbnail_size);
        self.set_pane_view_from_slider_value(pane_id, value + step);
    }
}

fn rename_selection_chars(name: &str, kind: &atlas_model::file_entry::FileKind) -> u32 {
    let all = name.chars().count() as u32;
    if matches!(kind, atlas_model::file_entry::FileKind::Dir) {
        all
    } else {
        rename_select_chars(name).unwrap_or(all)
    }
}

fn rename_select_chars(name: &str) -> Option<u32> {
    let lower = name.to_ascii_lowercase();
    let compound_exts = [
        ".tar.gz",
        ".tar.bz2",
        ".tar.xz",
        ".tar.zst",
        ".tar.lz",
        ".tar.lzma",
        ".tar.br",
        ".tar.z",
        ".d.ts",
        ".d.mts",
        ".d.cts",
        ".js.map",
        ".css.map",
        ".min.js",
        ".min.css",
    ];
    let stem = compound_exts
        .iter()
        .find_map(|ext| name.strip_suffix(ext).or_else(|| lower.strip_suffix(ext)))
        .or_else(|| {
            let dot = name.rfind('.')?;
            (dot > 0).then(|| &name[..dot])
        })?;
    let chars = stem.chars().count() as u32;
    (chars > 0).then_some(chars)
}

#[cfg(test)]
mod tests {
    use super::{rename_select_chars, rename_selection_chars};

    #[test]
    fn rename_selects_full_directory_or_extensionless_name_and_unicode_file_stem() {
        use atlas_model::file_entry::FileKind;
        assert_eq!(rename_selection_chars("folder.name", &FileKind::Dir), 11);
        assert_eq!(rename_selection_chars("README", &FileKind::File), 6);
        assert_eq!(rename_selection_chars(".gitignore", &FileKind::File), 10);
        assert_eq!(rename_selection_chars("caf\u{e9}.txt", &FileKind::File), 4);
        assert_eq!(rename_selection_chars("archive.tar.gz", &FileKind::File), 7);
    }

    #[test]
    fn rename_selects_name_before_extension() {
        assert_eq!(rename_select_chars("photo.png"), Some(5));
        assert_eq!(rename_select_chars("archive.tar.gz"), Some(7));
        assert_eq!(rename_select_chars("ARCHIVE.TAR.GZ"), Some(7));
        assert_eq!(rename_select_chars("types.d.ts"), Some(5));
        assert_eq!(rename_select_chars("app.min.js"), Some(3));
        assert_eq!(rename_select_chars("bundle.js.map"), Some(6));
        assert_eq!(rename_select_chars("README"), None);
        assert_eq!(rename_select_chars(".gitignore"), None);
    }

    #[test]
    fn slider_values_order_details_tree_list_icons() {
        use crate::app::App;
        use atlas_model::settings::ViewMode;

        assert_eq!(App::slider_value_for_view(&ViewMode::Details, 64.0), 0.0);
        assert_eq!(App::slider_value_for_view(&ViewMode::Tree, 64.0), 1.0);
        assert_eq!(App::slider_value_for_view(&ViewMode::List, 64.0), 2.0);
        assert_eq!(App::slider_value_for_view(&ViewMode::Grid, 64.0), 3.0);
        assert_eq!(App::slider_value_for_view(&ViewMode::Grid, 384.0), 23.0);
    }
}

/// Global rubber-band motor: a full-size invisible canvas painted last so its
/// `window.on_mouse_event` listeners fire even when the cursor leaves the pane.
/// Handles move (threshold → started, apply), up (clear on empty click), and
/// drives `request_animation_frame` while rubber is active for continuous autoscroll.
pub(crate) fn rubber_layer(view: Entity<App>) -> impl gpui::IntoElement {
    let v = view.clone();
    canvas(
        |_, _, _| {},
        move |_, (), window, cx| {
            // ── Draw the rubber band marquee rectangle ───────────────────────
            if let Some(r) = v.read(cx).rubber.as_ref() {
                let app = v.read(cx);
                let origin = match r.target {
                    RubberTarget::Pane(_) | RubberTarget::MillerColumn(_, _) => app
                        .clamp_rubber_target_point(
                            r.target,
                            app.rubber_window_point_from_content(r.target, r.content_origin),
                        ),
                    RubberTarget::Dropzone => app.clamp_rubber_target_point(r.target, r.origin),
                };
                let current = app.clamp_rubber_target_point(r.target, r.current);
                let left = f32::from(origin.x).min(f32::from(current.x));
                let top = f32::from(origin.y).min(f32::from(current.y));
                let right = f32::from(origin.x).max(f32::from(current.x));
                let bottom = f32::from(origin.y).max(f32::from(current.y));
                let w = (right - left).max(0.0);
                let h = (bottom - top).max(0.0);

                if r.started && w >= 2.0 && h >= 2.0 {
                    let rect = gpui::Bounds {
                        origin: gpui::point(gpui::px(left), gpui::px(top)),
                        size: gpui::size(gpui::px(w), gpui::px(h)),
                    };
                    let t = cx.theme();
                    let mut fill = t.accent;
                    fill.alpha = 0.15;

                    window.paint_quad(gpui::fill(rect, fill));

                    let mut border = t.accent;
                    border.alpha = 0.8;
                    let t_px = gpui::px(1.0);
                    window.paint_quad(gpui::fill(
                        gpui::Bounds {
                            origin: rect.origin,
                            size: gpui::size(rect.size.width, t_px),
                        },
                        border,
                    ));
                    window.paint_quad(gpui::fill(
                        gpui::Bounds {
                            origin: gpui::point(
                                rect.origin.x,
                                rect.origin.y + rect.size.height - t_px,
                            ),
                            size: gpui::size(rect.size.width, t_px),
                        },
                        border,
                    ));
                    window.paint_quad(gpui::fill(
                        gpui::Bounds {
                            origin: rect.origin,
                            size: gpui::size(t_px, rect.size.height),
                        },
                        border,
                    ));
                    window.paint_quad(gpui::fill(
                        gpui::Bounds {
                            origin: gpui::point(
                                rect.origin.x + rect.size.width - t_px,
                                rect.origin.y,
                            ),
                            size: gpui::size(t_px, rect.size.height),
                        },
                        border,
                    ));
                }
            }

            // A rubber drag must keep progressing while the pointer is still.
            // This runs from paint (where requesting the next frame is legal),
            // rather than relying on further MouseMove events.
            let rubber_scrolled = v.update(cx, |this, cx| {
                this.tick_rubber_edge_autoscroll(Instant::now(), cx)
            });
            if rubber_scrolled || v.read(cx).rubber_edge_can_scroll() {
                crate::perf::event(|| "request_animation_frame rubber_autoscroll".to_string());
                window.request_animation_frame();
            }

            // ── Global move ──────────────────────────────────────────────────
            let vm = v.clone();
            window.on_mouse_event::<MouseMoveEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let pos = ev.position;
                vm.update(cx, |this, cx| {
                    if this
                        .pending_folder_click
                        .as_ref()
                        .is_some_and(|click| !ev.dragging() || click.moved(pos))
                    {
                        this.pending_folder_click = None;
                    }
                    if this.rubber.is_some() && !ev.dragging() {
                        this.rubber = None;
                        cx.notify();
                        return false;
                    }

                    if let Some(resize) = this.sidebar_resize.as_ref() {
                        let dx = f32::from(pos.x) - resize.start_x;
                        this.state.settings.sidebar_width =
                            (resize.start_width + dx).clamp(180.0, 420.0);
                        cx.notify();
                        return false;
                    }

                    if let Some(resize) = this.inspector_resize.as_ref() {
                        let dx = resize.start_x - f32::from(pos.x);
                        this.state.settings.inspector_width =
                            (resize.start_width + dx).clamp(180.0, 2000.0);
                        cx.notify();
                        return false;
                    }

                    if let Some(resize) = this.dock_resize.as_ref() {
                        let delta = match resize.dir {
                            atlas_model::layout_tree::LinearDir::Horizontal => {
                                f32::from(pos.x) - resize.start_pos
                            }
                            atlas_model::layout_tree::LinearDir::Vertical => {
                                f32::from(pos.y) - resize.start_pos
                            }
                        };
                        if resize.span_px > 1.0 {
                            let mut shares = resize.start_shares.clone();
                            let left = resize.divider;
                            let right = left + 1;
                            if right < shares.len() {
                                let total_share = shares[left] + shares[right];
                                let share_delta = delta / resize.span_px
                                    * shares.iter().copied().sum::<f32>().max(1.0);
                                let min_share = 0.1;
                                if total_share <= min_share * 2.0 {
                                    shares[left] = total_share / 2.0;
                                } else {
                                    shares[left] = (resize.start_shares[left] + share_delta)
                                        .clamp_lenient(min_share, total_share - min_share);
                                }
                                shares[right] = (total_share - shares[left]).max(0.0);
                                if let Some(atlas_model::layout_tree::Tile::Container(
                                    atlas_model::layout_tree::Container::Linear(linear),
                                )) = this.state.pane_tree.tiles.get_mut(resize.container)
                                {
                                    crate::app::normalize_split_shares(&mut shares);
                                    linear.shares = shares;
                                }
                                cx.notify();
                            }
                        }
                        return false;
                    }

                    // Column resize takes priority over rubber-band selection.
                    if let Some((col, pane, start_x, start_width)) = this
                        .col_resize
                        .as_ref()
                        .map(|cr| (cr.col, cr.pane, cr.start_x, cr.start_width))
                    {
                        // Left-anchored columns with the handle on the RIGHT edge:
                        // dragging right widens the column (Explorer-style).
                        let dx = f32::from(pos.x) - start_x;
                        let min_w = match col {
                            "name" => 120.0,
                            "size" => 46.0,
                            "modified" => 92.0,
                            "type" => 56.0,
                            _ => 35.0,
                        };
                        let new_w = (start_width + dx).max(min_w);
                        let available = this
                            .pane_view_bounds
                            .get(&pane)
                            .or_else(|| this.pane_layout_bounds.get(&pane))
                            .map_or(650.0, |bounds| f32::from(bounds.size.width));
                        let mut prefs = this.details_prefs_for(pane);
                        prefs.widths.resize(col, new_w, available);
                        this.set_details_prefs(pane, prefs);
                        // Scoped re-render: only the dragged pane repaints per move —
                        // NOT App + sidebar + every other pane (that cascade is the lag).
                        this.notify_pane(pane, cx);
                        return false;
                    }

                    // A press on an already selected group belongs to file drag
                    // once it moves. It must never turn into a replacement marquee.
                    if this.rubber.as_ref().is_some_and(|r| {
                        r.selected_click.is_some() && {
                            let dx = f32::from(pos.x - r.origin.x);
                            let dy = f32::from(pos.y - r.origin.y);
                            dx * dx + dy * dy >= 16.0
                        }
                    }) {
                        this.rubber = None;
                        return false;
                    }

                    let Some((previous, started, target)) = this.rubber.as_mut().map(|r| {
                        let previous = r.current;
                        r.current = pos;
                        if !r.started {
                            let dx = f32::from(pos.x) - f32::from(r.origin.x);
                            let dy = f32::from(pos.y) - f32::from(r.origin.y);
                            r.started = dx * dx + dy * dy >= 16.0;
                        }
                        (previous, r.started, r.target)
                    }) else {
                        return false;
                    };
                    if started {
                        this.update_rubber_edge_intent(previous, pos);
                        this.apply_rubber();
                        let pane = match target {
                            RubberTarget::Pane(pane) | RubberTarget::MillerColumn(pane, _) => {
                                Some(pane)
                            }
                            RubberTarget::Dropzone => None,
                        };
                        if let Some(pane) = pane {
                            this.notify_pane(pane, cx);
                        }
                        cx.notify();
                        return false;
                    }
                    false
                });
                // Only force a full-window repaint while an interaction needs
                // cross-pane visual feedback (DnD drop zones, drag-hover, rubber-band).
                // Plain hover/move relies on gpui's own hover repaints and the scoped
                // `notify_pane` calls above; refreshing on every mouse move re-rendered
                // the entire App (all panes) and was the lag under many panes — and it
                // silently undid the scoped col/dock-resize notifies.
                let needs_full = cx.has_active_drag() || {
                    let app = vm.read(cx);
                    app.drag_hover_maybe_active() || app.rubber.is_some()
                };
                if needs_full {
                    window.refresh();
                }
            });

            let vw = v.clone();
            window.on_mouse_event::<ScrollWheelEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Capture || ev.modifiers.control || ev.modifiers.platform
                {
                    return;
                }
                {
                    let app = vw.read(cx);
                    if app.rubber.is_none() || app.state.settings.scroll_smoothness > 0.0 {
                        return;
                    }
                }
                let delta_y = vw.read(cx).rubber_wheel_delta(ev);
                if delta_y.abs() < 0.5 {
                    return;
                }
                vw.update(cx, |this, cx| {
                    let Some(target) = this.rubber.as_ref().map(|r| r.target) else {
                        return;
                    };
                    if this.rubber_scroll_pixels(target, delta_y) {
                        this.apply_rubber();
                        if let Some(pane) = match target {
                            RubberTarget::Pane(pane) | RubberTarget::MillerColumn(pane, _) => {
                                Some(pane)
                            }
                            RubberTarget::Dropzone => None,
                        } {
                            this.notify_pane(pane, cx);
                        }
                        cx.notify();
                        window.refresh();
                    }
                });
                cx.stop_propagation();
            });

            // ── Global up ────────────────────────────────────────────────────
            let vu = v.clone();
            window.on_mouse_event::<MouseUpEvent>(move |ev, phase, _win, cx| {
                if ev.button != gpui::MouseButton::Left {
                    return;
                }
                // Commit a marquee before item click/drop listeners can interpret
                // the release as navigation or a file drop.
                if phase == DispatchPhase::Capture {
                    if vu.read(cx).pending_folder_click.is_some() {
                        let opened =
                            vu.update(cx, |this, cx| this.finish_folder_click(ev.position, cx));
                        if opened {
                            cx.stop_propagation();
                            return;
                        }
                    }
                    if vu.read(cx).rubber.as_ref().is_some_and(|r| r.started) {
                        vu.update(cx, |this, cx| {
                            if let Some(r) = this.rubber.as_mut() {
                                r.current = ev.position;
                            }
                            this.apply_rubber();
                            if let Some(r) = this.rubber.take() {
                                if let RubberTarget::Pane(pane)
                                | RubberTarget::MillerColumn(pane, _) = r.target
                                {
                                    this.notify_pane(pane, cx);
                                }
                            }
                            cx.notify();
                        });
                        cx.stop_propagation();
                    }
                    return;
                }
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let pos = ev.position;
                vu.update(cx, |this, cx| {
                    if this.sidebar_resize.take().is_some() {
                        atlas_core::util::config::save_settings(&this.state.settings);
                        cx.notify();
                        return;
                    }

                    if this.inspector_resize.take().is_some() {
                        atlas_core::util::config::save_settings(&this.state.settings);
                        cx.notify();
                        return;
                    }

                    // End column resize — persist the new widths and let every pane
                    // (other Details views) catch up to the final widths.
                    if this.dock_resize.take().is_some() {
                        cx.notify();
                        return;
                    }

                    if let Some(resize) = this.col_resize.take() {
                        this.persist_details_columns(resize.pane);
                        this.notify_all_panes(cx);
                        return;
                    }

                    // End a column drag-reorder (fallback when the drop didn't land on
                    // the header — e.g. released over the rows or outside).
                    if this.col_dragging.is_some() {
                        this.finish_details_reorder();
                        this.notify_all_panes(cx);
                        return;
                    }

                    if let Some(r) = this.rubber.take() {
                        if let Some(index) = r.selected_click {
                            let dx = f32::from(pos.x - r.origin.x);
                            let dy = f32::from(pos.y - r.origin.y);
                            if dx * dx + dy * dy < 16.0 {
                                match r.target {
                                    RubberTarget::Pane(pane) => {
                                        if let Some(tab) = this.state.tab_states.get_mut(&pane) {
                                            tab.selection.select(index, &[index], false, false);
                                        }
                                    }
                                    RubberTarget::MillerColumn(pane, column) => {
                                        if let Some(col) = this
                                            .state
                                            .tab_states
                                            .get_mut(&pane)
                                            .and_then(|ts| ts.miller.columns.get_mut(column))
                                        {
                                            col.selection.select(index, &[index], false, false);
                                        }
                                    }
                                    RubberTarget::Dropzone => {
                                        if let Some(item) = this.state.sidebar_drop_zone.get(index)
                                        {
                                            this.dropzone_selection.select(
                                                item.path.clone(),
                                                &[item.path.clone()],
                                                false,
                                                false,
                                            );
                                        }
                                    }
                                }
                            }
                        } else if !r.started && !r.additive {
                            // Plain click on empty space → clear selection.
                            // If pos hit an item rect, that item's on_click already handled it.
                            match r.target {
                                RubberTarget::Pane(pane) => {
                                    let hit = this.pane_selection_hit(pane, pos);
                                    if !hit {
                                        if let Some(ts) = this.state.tab_states.get_mut(&pane) {
                                            ts.selection.clear();
                                        }
                                    }
                                }
                                RubberTarget::MillerColumn(pane, column) => {
                                    let hit = this
                                        .miller_row_rects
                                        .borrow()
                                        .get(&(pane, column))
                                        .is_some_and(|rects| {
                                            rects.iter().any(|(_, b)| {
                                                pos.x >= b.left()
                                                    && pos.x <= b.right()
                                                    && pos.y >= b.top()
                                                    && pos.y <= b.bottom()
                                            })
                                        });
                                    if !hit {
                                        if let Some(col) = this
                                            .state
                                            .tab_states
                                            .get_mut(&pane)
                                            .and_then(|ts| ts.miller.columns.get_mut(column))
                                        {
                                            col.selection.clear();
                                        }
                                    }
                                }
                                RubberTarget::Dropzone => {
                                    let hit = this.dropzone_rects.iter().any(|(_, b)| {
                                        pos.x >= b.left()
                                            && pos.x <= b.right()
                                            && pos.y >= b.top()
                                            && pos.y <= b.bottom()
                                    });
                                    if !hit {
                                        this.dropzone_selection.clear();
                                    }
                                }
                            }
                        }
                        if let RubberTarget::Pane(pane) | RubberTarget::MillerColumn(pane, _) =
                            r.target
                        {
                            this.notify_pane(pane, cx);
                        }
                        cx.notify();
                    }
                });
            });
        },
    )
    .absolute()
    .inset_0()
}

/// Capture Ctrl+wheel over file panes before GPUI's scrollables see it. Bubble
/// handlers on `uniform_list`/`overflow_y_scroll` are too late if registered
/// behind the internal scroll listener.
pub(crate) fn ctrl_scroll_layer(view: Entity<App>) -> impl gpui::IntoElement {
    canvas(
        |_, _, _| {},
        move |_, (), window, _cx| {
            let v = view.clone();
            window.on_mouse_event::<ScrollWheelEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Capture
                    || !(ev.modifiers.control || ev.modifiers.platform)
                {
                    return;
                }
                let dy = v.read(cx).rubber_wheel_delta(ev);
                if dy == 0.0 {
                    return;
                }

                let pane = {
                    let app = v.read(cx);
                    let overlay = &app.state.overlay;
                    if overlay.show_goto_bar
                        || overlay.show_palette
                        || overlay.show_settings
                        || overlay.show_guide
                        || overlay.show_ext_palette
                        || overlay.show_analyzer
                        || overlay.transfer_conflict.is_some()
                        || app.state.batch_rename.is_some()
                        || app.rename_edit.is_some()
                    {
                        return;
                    }
                    app.pane_view_bounds.iter().find_map(|(pane, bounds)| {
                        let handles_ctrl_scroll = app
                            .tab_data_for(*pane)
                            .is_some_and(|tab| tab.view_mode != ViewMode::Miller);
                        (handles_ctrl_scroll && bounds.contains(&ev.position)).then_some(*pane)
                    })
                };

                let Some(pane) = pane else {
                    return;
                };
                v.update(cx, |this, cx| {
                    this.pane_ctrl_scroll(pane, dy);
                    this.notify_pane(pane, cx);
                    cx.notify();
                });
                window.prevent_default();
                cx.stop_propagation();
            });
        },
    )
}

fn neighbor_index(idx: usize, len: usize, forward: bool) -> Option<usize> {
    if forward {
        (idx + 1 < len).then_some(idx + 1)
    } else {
        idx.checked_sub(1)
    }
}

pub(crate) fn search_seed_from_key(ev: &gpui::KeyDownEvent) -> Option<String> {
    if ev.keystroke.modifiers.control
        || ev.keystroke.modifiers.alt
        || ev.keystroke.modifiers.platform
    {
        return None;
    }
    let key = ev.keystroke.key.as_str();
    if key == "space" || key.chars().count() != 1 {
        return None;
    }
    Some(key.to_string())
}
