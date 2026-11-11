use crate::app::App;
use crate::input::action::{Action, SelectionMove};
use atlas_model::file_entry::FileKind;
use atlas_model::layout_tree::Tile;
use atlas_model::settings::ViewMode;
use std::path::PathBuf;
use std::time::Instant;

impl App {
    pub(crate) fn open_location_finder_or_editor(&mut self) {
        let tile = self.state.focused_tile;
        let dir = self
            .tab_data_for(tile)
            .and_then(|td| td.location.fs_dir().map(|p| p.to_path_buf()));
        if let Some(dir) = dir {
            if let Some(tab) = self.state.tab_states.get_mut(&tile) {
                tab.path_edit.editing = false;
                tab.path_edit.focus = false;
            }
            self.goto_prefill = Some(format!("{}{}", dir.display(), std::path::MAIN_SEPARATOR));
            self.process_actions(vec![Action::OpenGotoBar]);
        } else {
            self.process_actions(vec![Action::EditLocation(tile)]);
        }
    }

    fn clear_focused_selection(&mut self) -> bool {
        let mut cleared = !self.dropzone_selection.selected.is_empty();
        self.dropzone_selection.clear();

        let tile = self.state.focused_tile;
        let is_miller = self
            .tab_data_for(tile)
            .is_some_and(|tab| tab.view_mode == ViewMode::Miller);
        let Some(ts) = self.state.tab_states.get_mut(&tile) else {
            return cleared;
        };
        if is_miller {
            if let Some(col) = ts.miller.columns.get_mut(ts.miller.focused) {
                cleared |= !col.selection.selected.is_empty();
                col.selection.clear();
            }
        } else {
            cleared |= !ts.selection.selected.is_empty();
            ts.selection.clear();
        }
        cleared
    }

    #[inline]
    pub(crate) fn process_actions(&mut self, actions: impl IntoIterator<Item = Action>) {
        for action in actions {
            self.observe_guide(&action);
            match action {
                Action::Navigate { tab, path } => self.navigate(tab, path),
                Action::NavigateLocation { tab, location } => {
                    self.navigate_to(tab, location, crate::app::nav::NavKind::Push)
                }
                Action::NavigateBack(tab_id) => self.navigate_back(tab_id),
                Action::NavigateForward(tab_id) => self.navigate_forward(tab_id),
                Action::NavigateUp(tab_id) => self.navigate_up(tab_id),
                Action::EditLocation(tab_id) => {
                    if self.tab_data_for(tab_id).and_then(|pane| pane.location.fs_dir()).is_some() {
                        self.activate_tab(tab_id);
                        self.open_location_finder_or_editor();
                        continue;
                    }
                    let path = self
                        .tab_data_for(tab_id)
                        .map(|pane| pane.location.display_path());
                    if let (Some(path), Some(tab_state)) =
                        (path, self.state.tab_states.get_mut(&tab_id))
                    {
                        tab_state.path_edit.text = path.to_string_lossy().into_owned();
                        tab_state.path_edit.editing = true;
                        tab_state.path_edit.focus = true;
                        tab_state.path_edit.error = None;
                    }
                }
                Action::NewTab(pane_id) => self.new_tab(pane_id),
                Action::NewTabFocused => {
                    let tabs_id = self
                        .state
                        .find_tabs_container(self.state.focused_tile)
                        .or_else(|| self.state.find_any_tabs_container())
                        .unwrap_or(self.state.focused_tile);
                    self.new_tab(tabs_id);
                }
                Action::NewTabWithPath { pane, path } => self.new_tab_with_path(pane, path),
                Action::CloseTab(tab_id) => self.close_tab(tab_id),
                Action::ReopenClosedTab => self.reopen_closed_tab(),
                Action::DetachFocusedTab => self.detach_focused_tab(),
                Action::CloseFocusedTab => {
                    let tile = self.state.focused_tile;
                    self.close_tab(tile);
                }
                Action::FocusTab(tab_id) => {
                    self.activate_tab(tab_id);
                }
                Action::FocusNextTab => self.focus_next_tab(),
                Action::FocusPreviousTab => self.focus_prev_tab(),
                Action::FocusTabIndex(index) => {
                    let cur = self.state.focused_tile;
                    if let Some(container) = self.state.find_tabs_container(cur) {
                        let target = match self.state.pane_tree.tiles.get(container) {
                            Some(Tile::Container(atlas_model::layout_tree::Container::Tabs(
                                tabs,
                            ))) => tabs.children.get(index).copied(),
                            _ => None,
                        };
                        if let Some(target) = target {
                            self.activate_tab(target);
                        }
                    }
                }
                Action::MoveFocusedTabLeft | Action::MoveFocusedTabRight => {
                    let cur = self.state.focused_tile;
                    if let Some(container) = self.state.find_tabs_container(cur) {
                        let target = match self.state.pane_tree.tiles.get(container) {
                            Some(Tile::Container(atlas_model::layout_tree::Container::Tabs(
                                tabs,
                            ))) => {
                                let pos = tabs.children.iter().position(|id| *id == cur);
                                match (pos, matches!(action, Action::MoveFocusedTabLeft)) {
                                    (Some(pos), true) if pos > 0 => {
                                        tabs.children.get(pos - 1).copied()
                                    }
                                    (Some(pos), false) => tabs.children.get(pos + 1).copied(),
                                    _ => None,
                                }
                            }
                            _ => None,
                        };
                        if let Some(target) = target {
                            if matches!(action, Action::MoveFocusedTabLeft) {
                                self.move_tab_before(cur, target);
                            } else {
                                self.move_tab_before(target, cur);
                                self.activate_tab(cur);
                            }
                        }
                    }
                }
                Action::ToggleHidden => {
                    self.state.settings.show_hidden = !self.state.settings.show_hidden;
                    atlas_core::util::config::save_settings(&self.state.settings);
                    let tab_id = self.state.focused_tile;
                    if let Some(tab_data) = self.tab_data_for(tab_id) {
                        let loc = tab_data.location.clone();
                        let col = tab_data.sort_col.clone();
                        let dir = tab_data.sort_dir.clone();
                        self.dispatch_list(tab_id, loc, col, dir);
                    }
                }
                Action::ToggleInspector => {
                    // One window-level panel that follows the focused pane; the
                    // legacy per-tab flag made it vanish when focus moved.
                    let show = !self.preview_visible_for_focused();
                    self.state.settings.show_inspector = show;
                    self.state.for_each_tab_data_mut(|tab| tab.preview_enabled = false);
                    if !show {
                        self.collapse_preview_expanded();
                        #[cfg(feature = "provider-host")]
                        for tile in self.state.all_pane_tile_ids() {
                            let _ = self.cloud_tx.send(atlas_core::cloud::CloudCmd::CancelPreview { tab: tile });
                            self.state.clear_preview(tile);
                        }
                    }
                    atlas_core::util::config::save_settings(&self.state.settings);
                }
                Action::ToggleVimMode => {
                    self.state.settings.vim_mode = !self.state.settings.vim_mode;
                    self.state.profiles.active_mut().workspace.vim_mode =
                        self.state.settings.vim_mode;
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                    atlas_core::util::config::save_settings(&self.state.settings);
                }
                Action::ToggleSidebar => {
                    self.state.settings.show_sidebar = !self.state.settings.show_sidebar;
                    atlas_core::util::config::save_settings(&self.state.settings);
                }
                Action::ToggleSidebarCollapsed => {
                    self.state.settings.sidebar_collapsed = !self.state.settings.sidebar_collapsed;
                    self.sidebar_width_dirty = true;
                    atlas_core::util::config::save_settings(&self.state.settings);
                }
                Action::OpenSettings => {
                    if !self.state.overlay.show_settings {
                        self.settings_ui = crate::ui::settings_palette::SettingsUi::default();
                    }
                    self.state.overlay.show_settings = true;
                }
                Action::OpenSettingAt(id) => {
                    self.settings_cancel_drill();
                    let category = crate::ui::settings_palette::find(id).map(|def| def.category);
                    self.settings_ui = crate::ui::settings_palette::SettingsUi::opened_at(category);
                    self.settings_ui.focus_id = Some(id);
                    self.settings_input = None;
                    self.state.overlay.show_settings = true;
                }
                Action::ToggleSetting(id) => {
                    if let Some(crate::ui::settings_palette::Control::Toggle { get, set, .. }) =
                        crate::ui::settings_palette::find(id).map(|def| &def.control)
                    {
                        let on = !get(self);
                        set(self, on);
                        self.commit_settings();
                        self.dirty_all_panes = true;
                    }
                }
                Action::OpenSettingsAbout => {
                    self.settings_cancel_drill();
                    self.settings_ui = crate::ui::settings_palette::SettingsUi::opened_at(Some(
                        crate::ui::settings_palette::Category::About,
                    ));
                    self.settings_input = None;
                    self.state.overlay.show_settings = true;
                }
                Action::ActivateLicense => {
                    let key = arboard::Clipboard::new()
                        .and_then(|mut clipboard| clipboard.get_text())
                        .map(|text| text.trim().to_owned())
                        .unwrap_or_default();
                    // Keys are single short tokens; anything else is not a key.
                    if key.is_empty() || key.len() > 100 || key.contains(char::is_whitespace) {
                        self.toast_info(rust_i18n::t!("license_clipboard_empty").into_owned());
                        continue;
                    }
                    self.toast_info(rust_i18n::t!("license_activating").into_owned());
                    let tx = self.tx.clone();
                    self.rt.spawn(async move {
                        let result = atlas_core::license::activate(&key)
                            .await
                            .map(|()| rust_i18n::t!("license_activated").into_owned())
                            .map_err(|error| super::edition::license_error_text(&error));
                        let _ = tx.send(atlas_model::app_state::BgResult::LicenseChanged(result));
                    });
                }
                Action::DeactivateLicense => {
                    let tx = self.tx.clone();
                    self.rt.spawn(async move {
                        let result = atlas_core::license::deactivate()
                            .await
                            .map(|()| rust_i18n::t!("license_deactivated").into_owned())
                            .map_err(|error| super::edition::license_error_text(&error));
                        let _ = tx.send(atlas_model::app_state::BgResult::LicenseChanged(result));
                    });
                }
                Action::OpenCrashReports => {
                    #[cfg(windows)]
                    {
                        let newest = atlas_win_space::crash::previous_crash()
                            .and_then(|crash| crash.reports.first().cloned());
                        match newest {
                            Some(report) => {
                                let _ = std::process::Command::new("explorer")
                                    .arg(format!("/select,{}", report.display()))
                                    .spawn();
                            }
                            None => {
                                if let Ok(dir) = atlas_win_space::crash::crash_directory() {
                                    let _ = std::fs::create_dir_all(&dir);
                                    let _ = open::that(dir);
                                }
                            }
                        }
                    }
                }
                Action::SendCrashReport => {
                    #[cfg(windows)]
                    {
                        let Some(crash) = atlas_win_space::crash::previous_crash() else {
                            self.toast_info(rust_i18n::t!("crash_report_none").into_owned());
                            continue;
                        };
                        if std::mem::replace(&mut self.crash_report_sent, true) {
                            self.toast_info(rust_i18n::t!("crash_report_already_sent").into_owned());
                            continue;
                        }
                        self.toast_info(rust_i18n::t!("crash_report_sending").into_owned());
                        let tx = self.tx.clone();
                        self.rt.spawn(async move {
                            let result = atlas_core::crash_report::send(
                                &crash.reports,
                                &crash.version,
                                crash.summary.as_deref(),
                            )
                            .await
                            .map(|_| rust_i18n::t!("crash_report_sent").into_owned())
                            .map_err(|error| {
                                tracing::warn!(%error, "crash report upload failed");
                                rust_i18n::t!("crash_report_failed").into_owned()
                            });
                            let _ = tx.send(atlas_model::app_state::BgResult::CrashReportSent(result));
                        });
                    }
                }
                Action::GetPremium => {
                    // Regional-price markets buy the Store add-on; the Store listing
                    // has no buy button for it, so the app must open the dialog.
                    if atlas_core::license::prefers_store_purchase() {
                        self.buy_premium_in_store();
                    } else {
                        let _ = open::that("https://atlasnative.com/#pricing");
                    }
                }
                Action::OpenGettingStarted => {
                    self.getting_started_profile_input = None;
                    self.state.overlay.show_getting_started = true;
                    self.state.overlay.getting_started_step = 0;
                    self.state.overlay.getting_started_loading_since = None;
                }
                Action::OpenGuide(topic) => {
                    self.state.overlay.guide_topic = topic;
                    self.state.overlay.show_guide = true;
                }
                Action::CloseGuide => {
                    self.state.overlay.show_guide = false;
                }
                Action::StartGuideCoach(topic) => {
                    self.state.overlay.show_guide = false;
                    self.state.overlay.guide_coach =
                        Some(atlas_model::overlay::GuideCoach { topic, step: 0, base: u64::MAX });
                }
                Action::StopGuideCoach => {
                    self.state.overlay.guide_coach = None;
                }
                Action::DownloadIconTheme(cmd_id) => {
                    self.toast_info(rust_i18n::t!("icons_download_started").to_string());
                    self.handle_ext_action(crate::app::ext::ExtAction::Run {
                        ext_id: "icon-manager".into(),
                        cmd_id,
                    });
                }
                Action::SetGuideTopic(topic) => {
                    self.state.overlay.guide_topic = topic;
                }
                Action::StartGettingStarted => {
                    self.state.overlay.getting_started_loading_since = Some(Instant::now());
                }
                Action::CloseGettingStarted => {
                    self.mark_getting_started_seen();
                    self.state.overlay.show_getting_started = false;
                    self.state.overlay.getting_started_loading_since = None;
                    self.getting_started_profile_input = None;
                }
                Action::SaveSession => {
                    self.save();
                }
                Action::OpenExtManager => {
                    self.handle_ext_action(crate::app::ext::ExtAction::OpenManager(true));
                    self.handle_ext_action(crate::app::ext::ExtAction::FetchRegistry);
                    self.ext_manager_input = None;
                }
                Action::ToggleAnalyzer => {
                    self.state.overlay.show_analyzer = !self.state.overlay.show_analyzer;
                }
                Action::CloseOverlays => {
                    if self.rubber.is_some() {
                        self.cancel_rubber();
                        continue;
                    }
                    // Esc with no palette open also dismisses the focused pane's
                    // local filter (the "obvious" exit key for the filter pill).
                    let any_palette = self.state.overlay.show_goto_bar
                        || self.state.overlay.show_palette
                        || self.state.overlay.show_ext_palette;
                    let search_active = {
                        let tile = self.state.focused_tile;
                        self.state
                            .tab_states
                            .get(&tile)
                            .is_some_and(|ts| ts.search.active)
                    };
                    if !any_palette {
                        let tile = self.state.focused_tile;
                        if search_active {
                            self.close_search_in_place(tile);
                        }
                    }
                    let had_overlay = any_palette
                        || search_active
                        || self.breadcrumb_menu.is_some()
                        || self.state.overlay.show_settings
                        || self.state.overlay.show_guide
                        || self.state.ext.show_manager;
                    self.breadcrumb_menu = None;
                    self.breadcrumb_menu_input = None;
                    self.breadcrumb_menu_sub = None;
                    if self.state.overlay.show_settings {
                        self.state.overlay.show_settings = false;
                        self.settings_input = None;
                    }
                    self.state.overlay.show_guide = false;
                    // Goto dismisses with a fade-out: keep its view + input mounted and
                    // start the exit animation; `tick_palette_close` drops them when it
                    // finishes. If it was already closing, leave the in-flight fade.
                    if self.state.overlay.show_goto_bar {
                        self.goto_service_generation
                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        self.goto_service_inflight = None;
                        self.state.overlay.show_goto_bar = false;
                        self.palette_closing =
                            Some((crate::ui::palette::PaletteKind::Goto, Instant::now()));
                    }
                    if self.state.overlay.show_palette {
                        self.restore_theme_preview();
                    }
                    self.state.overlay.show_palette = false;
                    self.palette_input = None;
                    self.command_shortcut_capture = None;

                    // Close the extension manager sheet if open.
                    if self.state.ext.show_manager {
                        self.state.ext.show_manager = false;
                        self.ext_manager_input = None;
                    }

                    self.state.overlay.show_ext_palette = false;
                    self.ext_palette_input = None;
                    if let Some(tx) = self.state.overlay.ext_palette_reply.take() {
                        let _ = tx.send(String::new()); // Unblock WASM on Esc
                    }
                    // Keep the host mounted only for the goto's exit fade; otherwise
                    // drop it so the next open remounts fresh.
                    if self.palette_closing.is_none() {
                        self.palette_host = None;
                    }

                    // Esc also collapses an expanded image preview.
                    let preview_collapsed = self.collapse_preview_expanded();
                    if !had_overlay && !preview_collapsed {
                        self.clear_focused_selection();
                    }
                }
                Action::SubmitExtPalette(selection) => {
                    self.state.overlay.show_ext_palette = false;
                    self.ext_palette_input = None;
                    self.palette_host = None;
                    if let Some(tx) = self.state.overlay.ext_palette_reply.take() {
                        let _ = tx.send(selection);
                    }
                }
                Action::OpenGotoBar => {
                    if self.state.overlay.show_palette {
                        self.restore_theme_preview();
                    }
                    self.state.overlay.show_goto_bar = true;
                    self.state.overlay.show_palette = false;
                    self.state.overlay.goto_state.reset();
                    self.goto_service_generation
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    self.goto_service_cache = None;
                    self.goto_snapshot = Default::default();
                    self.goto_service_inflight = None;
                    self.goto_service_failed = None;
                    // Cancel any in-flight exit fade and start fresh.
                    self.palette_closing = None;
                    self.goto_input = None;
                    self.palette_host = None;
                    if self.goto_prefill.is_some() {
                        // Pre-filled open (Ctrl+L): the first render scopes the
                        // index from the path in the query; skip the global build.
                        self.nucleo.reset();
                        self.goto_index_root = None;
                    } else {
                        // Rebuild the global index from the current pane's directory.
                        let root = self
                            .state
                            .active_tab_data()
                            .and_then(|td| td.location.fs_dir().map(|p| p.to_path_buf()))
                            .unwrap_or_else(|| {
                                dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
                            });
                        if self.goto_index_root.as_ref() != Some(&(root.clone(), false)) {
                            self.nucleo.reset();
                            let injector = self.nucleo.injector();
                            let extended = self.goto_extended_index();
                            atlas_core::fs::index::build_goto_index(root.clone(), injector, extended);
                            self.goto_index_root = Some((root, false));
                        }
                    }
                }
                Action::OpenPalette => {
                    self.state.overlay.show_palette = true;
                    self.state.overlay.show_goto_bar = false;
                    self.state.overlay.palette_state.reset();
                    self.palette_input = None;
                    self.command_shortcut_capture = None;
                    // Hard-dismiss the goto (no fade) when switching to the palette.
                    self.palette_closing = None;
                    self.goto_input = None;
                    self.palette_host = None;
                }
                Action::ToggleCommandPin(id) => {
                    let pinned = &mut self.state.profiles.active_mut().pinned_commands;
                    if let Some(pos) = pinned.iter().position(|p| *p == id) {
                        pinned.remove(pos);
                    } else {
                        pinned.push(id);
                    }
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                }
                Action::StartCommandShortcutCapture(id) => {
                    self.start_command_shortcut_capture(id);
                }
                Action::OpenSearchInPlace => {
                    let tile = self.state.focused_tile;
                    if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                        ts.search.active = true;
                        ts.search.focus = true;
                    }
                }
                Action::CloseSearchInPlace => {
                    let tile = self.state.focused_tile;
                    self.close_search_in_place(tile);
                }
                Action::ToggleSearchInSubfolders => {
                    let tile = self.state.focused_tile;
                    if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                        ts.search.recursive = !ts.search.recursive;
                        ts.search.status = atlas_model::tab::SearchStatus::Idle;
                        ts.search.error = None;
                        ts.search.focus = true;
                    }
                    self.refresh_search_in_place(tile);
                }
                Action::ToggleFoldersFirst(tile) => {
                    if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get(tile) {
                        self.process_actions([Action::SetFolderOrder(tile, pane.folder_order.next())]);
                    }
                }
                Action::SetFolderOrder(tile, order) => {
                    let sort = match self.state.pane_tree.tiles.get_mut(tile) {
                        Some(Tile::Pane(p)) => {
                            p.folder_order = order;
                            Some((p.sort_col.clone(), p.sort_dir.clone(), p.folder_order))
                        }
                        _ => None,
                    };
                    if let Some((col, dir, folder_order)) = sort {
                        self.resort_entries_in_place(tile, &col, &dir, folder_order);
                    }
                }
                Action::ToggleShowFiles(tile) | Action::ToggleShowFolders(tile) => {
                    let toggled = match self.state.pane_tree.tiles.get_mut(tile) {
                        Some(Tile::Pane(p)) => {
                            if matches!(action, Action::ToggleShowFiles(_)) {
                                p.show_files = !p.show_files;
                                if !p.show_files && !p.show_folders {
                                    p.show_folders = true;
                                }
                            } else {
                                p.show_folders = !p.show_folders;
                                if !p.show_folders && !p.show_files {
                                    p.show_files = true;
                                }
                            }
                            Some((p.show_files, p.show_folders))
                        }
                        _ => None,
                    };
                    if let Some((show_files, show_folders)) = toggled {
                        if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                            ts.set_visibility(show_files, show_folders);
                            ts.refresh_miller_visibility();
                            // Drop selection entries that just became invisible so
                            // keyboard/file ops can't act on hidden items.
                            let entries = &ts.entries;
                            let kind_visible = |i: usize| {
                                entries.get(i).is_some_and(|e| {
                                    if matches!(e.kind(), FileKind::Dir) {
                                        show_folders
                                    } else {
                                        show_files
                                    }
                                })
                            };
                            ts.selection.selected.retain(|&i| kind_visible(i));
                            if ts
                                .selection
                                .keyboard_cursor
                                .is_some_and(|i| !kind_visible(i))
                            {
                                ts.selection.keyboard_cursor = None;
                                ts.selection.selection_anchor = None;
                            }
                        }
                    }
                }
                Action::ToggleFoldersFirstFocused => {
                    let tile = self.state.focused_tile;
                    self.process_actions([Action::ToggleFoldersFirst(tile)]);
                }
                Action::ToggleShowFilesFocused => {
                    let tile = self.state.focused_tile;
                    self.process_actions([Action::ToggleShowFiles(tile)]);
                }
                Action::ToggleShowFoldersFocused => {
                    let tile = self.state.focused_tile;
                    self.process_actions([Action::ToggleShowFolders(tile)]);
                }
                Action::ToggleBranchView(tile) => {
                    let mut switched_mode = false;
                    let toggled = match self.state.pane_tree.tiles.get_mut(tile) {
                        Some(Tile::Pane(p)) => {
                            p.is_branch_view = !p.is_branch_view;
                            if p.is_branch_view && p.view_mode == ViewMode::Grid {
                                switched_mode = true;
                            }
                            Some(p.is_branch_view)
                        }
                        _ => None,
                    };
                    if switched_mode {
                        self.set_pane_view_mode(tile, ViewMode::Details);
                    }
                    if let Some(is_branch) = toggled {
                        if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                            ts.is_branch_view = is_branch;
                            ts.entries = std::sync::Arc::new([]);
                            ts.entries_version = ts.entries_version.wrapping_add(1);
                            ts.visible_valid = false;
                        }
                        self.process_actions([Action::Refresh(tile)]);
                    }
                }
                Action::ToggleBranchViewFocused => {
                    let tile = self.state.focused_tile;
                    self.process_actions([Action::ToggleBranchView(tile)]);
                }
                Action::ToggleTreeExpand(path) => {
                    let tile = self.state.focused_tile;
                    // Rows below the folder shift: keep the selection on its files
                    // (a collapsed child hands it to the folder) and the view still.
                    self.mutate_tab(tile, |ts| ts.tree.toggle_expand(&path));
                    let needs_fetch = self.state.tab_states.get(&tile).is_some_and(|ts| {
                        !ts.tree.children.contains_key(&path) && ts.tree.is_expanded(&path)
                    });
                    if needs_fetch {
                        self.spawn_list_tree(tile, path);
                    }
                }
                Action::ClosePalette => {
                    self.restore_theme_preview();
                    self.state.overlay.show_palette = false;
                    self.palette_closing = None;
                    self.palette_input = None;
                    self.command_shortcut_capture = None;
                    self.palette_host = None;
                }
                Action::Refresh(tab_id) => {
                    if let Some(tab_data) = self.tab_data_for(tab_id) {
                        let loc = tab_data.location.clone();
                        let col = tab_data.sort_col.clone();
                        let dir = tab_data.sort_dir.clone();
                        self.dispatch_list(tab_id, loc, col, dir);
                    }
                }
                Action::Select {
                    tile,
                    idx,
                    shift,
                    ctrl,
                } => {
                    let visible = self.flat_visible_indices(tile);
                    if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                        ts.selection.select(idx, &visible, shift, ctrl);
                    }
                    self.state.focused_tile = tile;
                }
                Action::MoveSelection { target, extend } => match target {
                    SelectionMove::Previous => self.nav_up(extend, false),
                    SelectionMove::Next => self.nav_down(extend, false),
                    SelectionMove::Left => self.nav_left(extend, false),
                    SelectionMove::Right => self.nav_right(extend, false),
                    SelectionMove::First => self.nav_home(extend, false),
                    SelectionMove::Last => self.nav_end(extend, false),
                    SelectionMove::PageUp => self.nav_page(false, extend, false),
                    SelectionMove::PageDown => self.nav_page(true, extend, false),
                },
                Action::MoveCursor(target) => match target {
                    SelectionMove::Previous => self.nav_up(false, true),
                    SelectionMove::Next => self.nav_down(false, true),
                    SelectionMove::Left => self.nav_left(false, true),
                    SelectionMove::Right => self.nav_right(false, true),
                    SelectionMove::First => self.nav_home(false, true),
                    SelectionMove::Last => self.nav_end(false, true),
                    SelectionMove::PageUp => self.nav_page(false, false, true),
                    SelectionMove::PageDown => self.nav_page(true, false, true),
                },
                Action::ToggleSelectionCursor => self.toggle_selection_cursor(),
                Action::OpenPropertiesFocused => self.open_properties_focused(),
                Action::OpenCursor => self.open_cursor(),
                Action::OpenSelectedDirsInNewTabs => {
                    let focused = self.state.focused_tile;
                    let pane = self
                        .state
                        .find_tabs_container(focused)
                        .or_else(|| self.state.find_any_tabs_container())
                        .unwrap_or(focused);
                    let dirs: Vec<PathBuf> = self
                        .state
                        .tab_states
                        .get(&focused)
                        .map(|ts| {
                            ts.selection
                                .selected
                                .iter()
                                .filter_map(|&idx| {
                                    ts.entries.get(idx).and_then(|entry| {
                                        matches!(entry.kind(), FileKind::Dir)
                                            .then(|| entry.path())
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    for path in dirs {
                        self.new_tab_with_path(pane, path);
                    }
                }
                Action::Activate { tile, idx } => {
                    let entry = self
                        .state
                        .tab_states
                        .get(&tile)
                        .and_then(|ts| ts.entries.get(idx))
                        .map(|e| (e.path(), e.kind().clone(), e.archive_root().clone()));
                    if let Some((path, kind, archive_root)) = entry {
                        if let Some(archive) = archive_root {
                            // Virtual entry inside an archive.
                            if kind == FileKind::Dir {
                                self.navigate_into_archive(tile, archive, path);
                            } else {
                                self.activate_file_path(tile, path, Some(archive));
                            }
                        } else if kind == FileKind::Dir {
                            self.navigate_entry(tile, path);
                        } else {
                            self.activate_file_path(tile, path, None);
                        }
                    }
                }
                Action::SelectAll => {
                    let tile = self.state.focused_tile;
                    let flat_visible = self.flat_visible_indices(tile);
                    let is_miller = self
                        .tab_data_for(tile)
                        .is_some_and(|tab| tab.view_mode == ViewMode::Miller);
                    if let Some(ts) = self.state.active_tab_state_mut() {
                        if is_miller {
                            let visible = ts.miller_visible_indices(ts.miller.focused);
                            if let Some(col) = ts.miller.columns.get_mut(ts.miller.focused) {
                                col.selection.selected = visible.iter().copied().collect();
                                col.selection.keyboard_cursor =
                                    visible.first().copied();
                            }
                            continue;
                        }
                        ts.selection.selected = flat_visible.iter().copied().collect();
                        ts.selection.keyboard_cursor = ts.selection.selected.iter().next().copied();
                    }
                }
                Action::SetGroupBy(mode) => {
                    let tile = self.state.focused_tile;
                    if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(tile) {
                        pane.group_by = mode;
                        pane.collapsed_groups.clear();
                        if mode != atlas_model::tab::GroupBy::None && !matches!(pane.view_mode, ViewMode::Details | ViewMode::List | ViewMode::Grid) {
                            pane.view_mode = ViewMode::Details;
                        }
                    }
                    if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                        ts.set_grouping(mode, Default::default());
                        ts.selection.scroll_to_selected = true;
                    }
                    self.dirty_panes.insert(tile);
                }
                Action::ToggleGroup { tab, id } => {
                    let grouping = if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(tab) {
                        if !pane.collapsed_groups.remove(&id) {
                            pane.collapsed_groups.insert(id);
                        }
                        Some((pane.group_by, pane.collapsed_groups.clone()))
                    } else { None };
                    if let Some((mode, collapsed)) = grouping {
                        if let Some(ts) = self.state.tab_states.get_mut(&tab) {
                            ts.set_grouping(mode, collapsed);
                            let visible = ts.visible_indices_shared();
                            ts.selection.retain_visible(&visible);
                        }
                    }
                    self.dirty_panes.insert(tab);
                }
                Action::SetAllGroupsCollapsed(collapse) => {
                    let tile = self.state.focused_tile;
                    let ids: std::collections::BTreeSet<String> = if collapse {
                        self.state.tab_states.get_mut(&tile).map_or_else(Default::default, |ts| {
                            ts.grouped_rows_shared().iter().filter_map(|row| match row {
                                atlas_model::tab::GroupRow::Header { id, .. } => Some(id.clone()),
                                _ => None,
                            }).collect()
                        })
                    } else {
                        Default::default()
                    };
                    let mode = if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(tile) {
                        pane.collapsed_groups = ids.clone();
                        pane.group_by
                    } else { atlas_model::tab::GroupBy::None };
                    if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                        ts.set_grouping(mode, ids);
                        let visible = ts.visible_indices_shared();
                        ts.selection.retain_visible(&visible);
                    }
                    self.dirty_panes.insert(tile);
                }
                Action::SetSort { col, dir } => {
                    let mut folder_pref_write = None;
                    if let Some(Tile::Pane(pane)) =
                        self.state.pane_tree.tiles.get_mut(self.state.focused_tile)
                    {
                        pane.sort_col = col;
                        pane.sort_dir = dir;
                        let loc = pane.location.clone();
                        let sort_col = pane.sort_col.clone();
                        let sort_dir = pane.sort_dir.clone();
                        if let atlas_model::location::Location::Directory(path) = &loc {
                            folder_pref_write = Some((
                                path.clone(),
                                atlas_model::profiles::FolderViewPref {
                        recursive: false,
                        columns: None,
                                    view_mode: pane.view_mode.clone(),
                                    sort_col: sort_col.clone(),
                                    sort_dir: sort_dir.clone(),
                                },
                            ));
                        }
                        self.dispatch_list(self.state.focused_tile, loc, sort_col, sort_dir);
                    }
                    self.save_folder_view_pref(folder_pref_write);
                }
                Action::Open(paths) => {
                    if paths.iter().any(|path| atlas_core::vault::live::is_private(path) && !self.owns_native_vault_path(path)) {
                        self.toast_error(rust_i18n::t!("vault_access_denied").to_string()); continue;
                    }
                    if paths.iter().any(|p| atlas_core::vault::is_private_path(p)) {
                        for path in &paths { self.try_open_vault(path); }
                        continue;
                    }
                    #[cfg(feature = "provider-host")]
                    if self.open_remote_paths(&paths) {
                        continue;
                    }
                    self.spawn_open_files(paths);
                }
                Action::CopyPathToClipboard(path) => {
                    self.copy_paths_as_text(&[path]);
                }
                Action::CopyPathsToClipboard(paths) => self.copy_paths_as_text(&paths),
                #[cfg(windows)]
                Action::PasteShortcutFocused => self.paste_shortcuts_focused(),
                Action::Copy(paths) => {
                    #[cfg(feature = "provider-host")]
                    if let Some((remote_id, _)) = self.focused_remote_tab() {
                        self.set_clipboard_with_remote(paths, false, Some(remote_id));
                        continue;
                    }
                    let (selected, target_names) = self.selected_paths_with_target_names();
                    if selected == paths {
                        self.set_clipboard_with_names(paths, target_names, false);
                    } else {
                        self.set_clipboard(paths, false);
                    }
                }
                Action::Cut(paths) => {
                    #[cfg(feature = "provider-host")]
                    if let Some((remote_id, _)) = self.focused_remote_tab() {
                        self.set_clipboard_with_remote(paths, true, Some(remote_id));
                        continue;
                    }
                    let (selected, target_names) = self.selected_paths_with_target_names();
                    if selected == paths {
                        self.set_clipboard_with_names(paths, target_names, true);
                    } else {
                        self.set_clipboard(paths, true);
                    }
                }
                Action::CompressToZip(paths) => {
                    self.compress_to_zip(paths);
                }
                Action::RestoreSelectedFromRecycleBin => {
                    self.restore_selected_from_recycle_bin();
                }
                Action::PurgeSelectedFromRecycleBin => {
                    self.purge_selected_from_recycle_bin();
                }
                Action::EmptyRecycleBin => {
                    self.empty_recycle_bin();
                }
                Action::Delete { paths, to_trash } => {
                    // In a recycle-bin listing the paths are `$R` payloads: deleting
                    // them directly would orphan their `$I` metadata (and re-trashing
                    // makes no sense) — purge the pair instead.
                    if self
                        .tab_data_for(self.state.focused_tile)
                        .is_some_and(|t| t.location == atlas_model::location::Location::RecycleBin)
                    {
                        self.purge_selected_from_recycle_bin();
                        continue;
                    }
                    #[cfg(feature = "provider-host")]
                    if let Some((remote_id, current_path)) = self.focused_remote_tab() {
                        let tile = self.state.focused_tile;
                        let entries: Vec<(String, bool)> = paths
                            .iter()
                            .map(|p| {
                                let path_str = p.to_string_lossy().replace('\\', "/");
                                let is_dir = self
                                    .state
                                    .tab_states
                                    .get(&tile)
                                    .and_then(|ts| ts.entries.iter().find(|e| e.path() == *p))
                                    .is_some_and(|e| {
                                        matches!(e.kind(), atlas_model::file_entry::FileKind::Dir)
                                    });
                                (path_str, is_dir)
                            })
                            .collect();
                        if !entries.is_empty() {
                            let _ = self
                                .cloud_tx
                                .send(atlas_core::cloud::CloudCmd::DeleteRemote {
                                    id: remote_id,
                                    entries,
                                    parent: current_path,
                                });
                        }
                        continue;
                    }
                    self.delete_files(paths, to_trash);
                }
                Action::Rename { from, to } => {
                    #[cfg(feature = "provider-host")]
                    if let Some((remote_id, current_path)) = self.focused_remote_tab() {
                        let from_str = from.to_string_lossy().replace('\\', "/");
                        let to_str = to.to_string_lossy().replace('\\', "/");
                        let _ =
                            self.cloud_tx
                                .send(atlas_core::cloud::CloudCmd::RenameRemoteEntry {
                                    id: remote_id,
                                    from: from_str,
                                    to: to_str,
                                    parent: current_path,
                                });
                        continue;
                    }
                    self.rename_file(from, to);
                }
                Action::CreateFolder { parent, name } => {
                    #[cfg(feature = "provider-host")]
                    if let Some((remote_id, current_path)) = self.focused_remote_tab() {
                        let parent_str = parent.to_string_lossy().replace('\\', "/");
                        let folder_path = if parent_str.is_empty() {
                            name.clone()
                        } else {
                            format!("{}/{}", parent_str.trim_end_matches('/'), name)
                        };
                        let _ = self
                            .cloud_tx
                            .send(atlas_core::cloud::CloudCmd::MkdirRemote {
                                id: remote_id,
                                path: folder_path,
                                parent: current_path,
                            });
                        continue;
                    }
                    let path = parent.join(&name);
                    self.create_dir(path);
                }
                Action::CreateFolderFocused => {
                    #[cfg(feature = "provider-host")]
                    if let Some((_remote_id, current_path)) = self.focused_remote_tab() {
                        let tile = self.state.focused_tile;
                        let base_name = rust_i18n::t!("root_new_folder");
                        let existing_names: std::collections::HashSet<String> = self
                            .state
                            .tab_states
                            .get(&tile)
                            .map(|ts| ts.entries.iter().map(|e| e.name.to_string()).collect())
                            .unwrap_or_default();
                        let name = if !existing_names.contains(&*base_name) {
                            base_name.to_string()
                        } else {
                            let mut i = 1;
                            loop {
                                let candidate = format!("{base_name} ({i})");
                                if !existing_names.contains(&candidate) {
                                    break candidate;
                                }
                                i += 1;
                            }
                        };
                        let parent_str = current_path.replace('\\', "/");
                        let folder_path = if parent_str.is_empty() {
                            name.clone()
                        } else {
                            format!("{}/{}", parent_str.trim_end_matches('/'), name)
                        };
                        let path = PathBuf::from(folder_path);
                        self.pending_select = Some((tile, path.clone()));
                        self.pending_rename = Some((tile, path, true));
                        self.process_actions([Action::CreateFolder {
                            parent: PathBuf::from(&current_path),
                            name,
                        }]);
                        continue;
                    }
                    let target_dir = self
                        .state
                        .active_tab_data()
                        .and_then(|t| t.location.fs_dir().map(PathBuf::from))
                        .or_else(dirs::home_dir);
                    if let Some(dir) = target_dir {
                        let tile = self.state.focused_tile;
                        let name = unique_child_name(&dir, &rust_i18n::t!("root_new_folder"));
                        let path = dir.join(&name);
                        self.pending_select = Some((tile, path.clone()));
                        self.pending_rename = Some((tile, path, true));
                        self.process_actions([Action::CreateFolder { parent: dir, name }]);
                    }
                }
                Action::CreateFile { parent, name } => {
                    #[cfg(feature = "provider-host")]
                    if let Some((remote_id, current_path)) = self.focused_remote_tab() {
                        let parent_str = parent.to_string_lossy().replace('\\', "/");
                        let file_path = if parent_str.is_empty() {
                            name.clone()
                        } else {
                            format!("{}/{}", parent_str.trim_end_matches('/'), name)
                        };
                        let tmp = std::env::temp_dir().join(format!(
                            "atlas_new_{}_{}_{}",
                            std::process::id(),
                            std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis(),
                            name
                        ));
                        let _ = std::fs::write(&tmp, b"");
                        let _ = self.cloud_tx.send(atlas_core::cloud::CloudCmd::EnqueueUpload {
                            id: remote_id,
                            local_path: tmp,
                            remote_path: file_path,
                            label: name,
                            delete_source: true,
                        });
                        continue;
                    }
                    let path = parent.join(&name);
                    self.create_file(path);
                }
                Action::CreateFileFocused => {
                    #[cfg(feature = "provider-host")]
                    if let Some((_remote_id, current_path)) = self.focused_remote_tab() {
                        let tile = self.state.focused_tile;
                        let base_name = rust_i18n::t!("root_new_file");
                        let existing_names: std::collections::HashSet<String> = self
                            .state
                            .tab_states
                            .get(&tile)
                            .map(|ts| ts.entries.iter().map(|e| e.name.to_string()).collect())
                            .unwrap_or_default();
                        let name = if !existing_names.contains(&*base_name) {
                            base_name.to_string()
                        } else {
                            let mut i = 1;
                            loop {
                                let candidate = format!("{base_name} ({i})");
                                if !existing_names.contains(&candidate) {
                                    break candidate;
                                }
                                i += 1;
                            }
                        };
                        let parent_str = current_path.replace('\\', "/");
                        let file_path = if parent_str.is_empty() {
                            name.clone()
                        } else {
                            format!("{}/{}", parent_str.trim_end_matches('/'), name)
                        };
                        let path = PathBuf::from(file_path);
                        self.pending_select = Some((tile, path.clone()));
                        self.pending_rename = Some((tile, path, true));
                        self.process_actions([Action::CreateFile {
                            parent: PathBuf::from(&current_path),
                            name,
                        }]);
                        continue;
                    }
                    let target_dir = self
                        .state
                        .active_tab_data()
                        .and_then(|t| t.location.fs_dir().map(PathBuf::from))
                        .or_else(dirs::home_dir);
                    if let Some(dir) = target_dir {
                        let tile = self.state.focused_tile;
                        let name = unique_child_name(&dir, &rust_i18n::t!("root_new_file"));
                        let path = dir.join(&name);
                        self.pending_select = Some((tile, path.clone()));
                        self.pending_rename = Some((tile, path, true));
                        self.process_actions([Action::CreateFile { parent: dir, name }]);
                    }
                }
                Action::RenameFocused => {
                    let tile = self.state.focused_tile;
                    let path = self.state.tab_states.get(&tile).and_then(|ts| {
                        let (selection, entries) = if self.tab_data_for(tile).is_some_and(|tab| {
                            tab.view_mode == atlas_model::settings::ViewMode::Miller
                        }) {
                            let col = ts.miller.columns.get(ts.miller.focused)?;
                            (&col.selection, &col.entries)
                        } else {
                            (&ts.selection, &ts.entries)
                        };
                        let idx = selection
                            .keyboard_cursor
                            .or_else(|| selection.selected.iter().copied().next())?;
                        entries.get(idx).map(|entry| entry.path())
                    });
                    if let Some(path) = path {
                        self.pending_rename = Some((tile, path, false));
                    }
                }
                Action::OpenPathInNewWindow(path) => {
                    self.spawn_new_window(Some(path));
                }
                Action::OpenSelectedInNewWindow => {
                    let focused = self.state.focused_tile;
                    let target = self
                        .selected_paths()
                        .into_iter()
                        .next()
                        .or_else(|| {
                            self.state.tab_states.get_mut(&focused).and_then(|ts| {
                                if let Some(idx) = ts.selection.keyboard_cursor {
                                    if let Some(entry) = ts.entries.get(idx) {
                                        return Some(entry.path());
                                    }
                                }
                                if ts.search.active {
                                    if let Some(&idx) = ts.visible_indices().first() {
                                        if let Some(entry) = ts.entries.get(idx) {
                                            return Some(entry.path());
                                        }
                                    }
                                }
                                None
                            })
                        })
                        .or_else(|| {
                            self.state
                                .active_tab_data()
                                .and_then(|tab| tab.location.fs_dir().map(PathBuf::from))
                        });
                    let target_dir = target
                        .and_then(|p| {
                            if p.is_dir() {
                                Some(p)
                            } else if p.is_file() {
                                p.parent().map(std::path::Path::to_path_buf)
                            } else {
                                Some(p)
                            }
                        })
                        .or_else(dirs::home_dir);
                    self.spawn_new_window(target_dir);
                }
                Action::OpenDirInRightSplit { pane, path } => {
                    let dir = if path.is_file() {
                        path.parent().map(std::path::Path::to_path_buf).unwrap_or(path)
                    } else {
                        path
                    };
                    let tabs_id = self
                        .state
                        .find_tabs_container(pane)
                        .or_else(|| self.state.find_any_tabs_container())
                        .unwrap_or(pane);
                    self.split_pane(tabs_id, false);
                    let new_pane = self.state.focused_tile;
                    self.navigate(new_pane, dir);
                }
                Action::OpenDirInBottomSplit { pane, path } => {
                    let dir = if path.is_file() {
                        path.parent().map(std::path::Path::to_path_buf).unwrap_or(path)
                    } else {
                        path
                    };
                    let tabs_id = self
                        .state
                        .find_tabs_container(pane)
                        .or_else(|| self.state.find_any_tabs_container())
                        .unwrap_or(pane);
                    self.split_pane(tabs_id, true);
                    let new_pane = self.state.focused_tile;
                    self.navigate(new_pane, dir);
                }
                Action::OpenSelectedDirInRightSplit | Action::OpenSelectedDirInBottomSplit => {
                    let focused = self.state.focused_tile;
                    let path = self
                        .state
                        .tab_states
                        .get_mut(&focused)
                        .and_then(|ts| {
                            // 1. From selected
                            let from_selected = ts.selection.selected.iter().find_map(|&idx| {
                                ts.entries.get(idx).map(|entry| {
                                    if entry.kind() == FileKind::Dir {
                                        entry.path()
                                    } else {
                                        entry
                                            .path()
                                            .parent()
                                            .map(std::path::Path::to_path_buf)
                                            .unwrap_or_else(|| entry.path())
                                    }
                                })
                            });
                            if from_selected.is_some() {
                                return from_selected;
                            }
                            // 2. From keyboard cursor
                            if let Some(idx) = ts.selection.keyboard_cursor {
                                if let Some(entry) = ts.entries.get(idx) {
                                    return Some(if entry.kind() == FileKind::Dir {
                                        entry.path()
                                    } else {
                                        entry
                                            .path()
                                            .parent()
                                            .map(std::path::Path::to_path_buf)
                                            .unwrap_or_else(|| entry.path())
                                    });
                                }
                            }
                            // 3. From in-place fuzzy filter visible entries
                            if ts.search.active {
                                if let Some(&idx) = ts.visible_indices().first() {
                                    if let Some(entry) = ts.entries.get(idx) {
                                        return Some(if entry.kind() == FileKind::Dir {
                                            entry.path()
                                        } else {
                                            entry
                                                .path()
                                                .parent()
                                                .map(std::path::Path::to_path_buf)
                                                .unwrap_or_else(|| entry.path())
                                        });
                                    }
                                }
                            }
                            None
                        })
                        .or_else(|| {
                            // 4. From active tab's location
                            self.state
                                .active_tab_data()
                                .and_then(|tab| tab.location.fs_dir().map(PathBuf::from))
                                .or_else(dirs::home_dir)
                        });
                    if let Some(path) = path {
                        let tabs_id = self
                            .state
                            .find_tabs_container(focused)
                            .or_else(|| self.state.find_any_tabs_container())
                            .unwrap_or(focused);
                        let vertical = matches!(action, Action::OpenSelectedDirInBottomSplit);
                        self.split_pane(tabs_id, vertical);
                        let new_pane = self.state.focused_tile;
                        self.navigate(new_pane, path);
                    }
                }
                Action::OpenTerminal { dir } => {
                    self.spawn_terminal(&dir);
                }
                Action::Paste { dest } => {
                    #[cfg(feature = "provider-host")]
                    {
                        let remote_dest = self.focused_remote_tab();
                        let clipboard_remote = self
                            .state
                            .ops
                            .clipboard
                            .as_ref()
                            .and_then(|c| c.remote_id.clone());
                        match (&remote_dest, &clipboard_remote) {
                            // Local clipboard → remote tab: upload each file.
                            (Some((dst_id, dst_path)), None) => {
                                self.paste_local_into_remote(dst_id.clone(), dst_path.clone());
                                continue;
                            }
                            // Remote clipboard → local tab (or different remote): download.
                            (None, Some(_src_id)) => {
                                self.paste_remote_into_local(dest.clone());
                                continue;
                            }
                            // Same-remote clipboard → same-remote tab: server-side copy.
                            (Some((dst_id, dst_path)), Some(src_id)) if dst_id == src_id => {
                                let dst_id = dst_id.clone();
                                let dst_path = dst_path.clone();
                                let src_id = src_id.clone();
                                self.paste_remote_within(src_id, dst_id, dst_path);
                                continue;
                            }
                            (Some((destination_id, destination_path)), Some(source_id)) => {
                                self.paste_remote_within(source_id.clone(), destination_id.clone(), destination_path.clone());
                                continue;
                            }
                            // Both local.
                            _ => {}
                        }
                    }
                    self.paste_files(dest);
                }
                Action::ResolveTransferConflict(policy) => self.apply_transfer_resolution(policy),
                Action::Transfer(req) => self.request_transfer(req),
                // gpui drop handlers: `Move` carries an internal drag,
                // `CopyTo` external paths dropped onto the window.
                Action::Move { paths, dest } => {
                    self.move_files(paths, dest, atlas_model::ops::TransferOrigin::DndInternal);
                }
                Action::CopyTo { paths, dest } => {
                    self.copy_files(paths, dest, atlas_model::ops::TransferOrigin::DndExternal);
                }
                Action::Undo => self.undo(),
                Action::Redo => self.redo(),
                Action::OpenConverter => self.open_converter(),
                Action::OpenBatchRename => {
                    let tile = self.state.focused_tile;
                    // Same path as F2: the cursor row becomes the live input.
                    let path = self.state.tab_states.get(&tile).and_then(|ts| {
                        let idx = ts
                            .selection
                            .keyboard_cursor
                            .or_else(|| ts.selection.selected.iter().copied().min())?;
                        Some(ts.entries.get(idx)?.path())
                    });
                    if let Some(path) = path {
                        self.pending_rename = Some((tile, path, false));
                    }
                }
                Action::SplitHorizontalFocused => {
                    let tabs_id = self
                        .state
                        .find_tabs_container(self.state.focused_tile)
                        .or_else(|| self.state.find_any_tabs_container())
                        .unwrap_or(self.state.focused_tile);
                    self.split_pane(tabs_id, false);
                }
                Action::SplitVerticalFocused => {
                    let tabs_id = self
                        .state
                        .find_tabs_container(self.state.focused_tile)
                        .or_else(|| self.state.find_any_tabs_container())
                        .unwrap_or(self.state.focused_tile);
                    self.split_pane(tabs_id, true);
                }
                Action::CloseFocusedPane => {
                    let tabs_id = self
                        .state
                        .find_tabs_container(self.state.focused_tile)
                        .or_else(|| self.state.find_any_tabs_container());
                    if let Some(id) = tabs_id {
                        self.close_pane(id);
                    }
                }
                Action::InvertSelection => {
                    let tile = self.state.focused_tile;
                    let flat_visible = self.flat_visible_indices(tile);
                    let is_miller = self
                        .tab_data_for(tile)
                        .is_some_and(|tab| tab.view_mode == ViewMode::Miller);
                    if let Some(ts) = self.state.active_tab_state_mut() {
                        if is_miller {
                            let visible = ts.miller_visible_indices(ts.miller.focused);
                            if let Some(col) = ts.miller.columns.get_mut(ts.miller.focused) {
                                let all: std::collections::HashSet<usize> =
                                    visible.iter().copied().collect();
                                col.selection.selected =
                                    all.difference(&col.selection.selected).copied().collect();
                            }
                            continue;
                        }
                        // Invert over the *visible* set (SelectAll already does),
                        // otherwise an active filter selects hidden entries.
                        let visible: std::collections::HashSet<usize> =
                            flat_visible.iter().copied().collect();
                        ts.selection.selected = visible
                            .difference(&ts.selection.selected)
                            .copied()
                            .collect();
                    }
                }
                Action::MillerInit { tile, location } => {
                    let loc = location;
                    let already = self
                        .state
                        .tab_states
                        .get(&tile)
                        .and_then(|ts| ts.miller.columns.first())
                        .is_some_and(|c| c.location == loc);
                    if !already {
                        if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                            ts.miller = atlas_model::tab::MillerState::default();
                            ts.miller
                                .columns
                                .push(atlas_model::tab::MillerColumn::new(loc.clone()));
                        }
                        self.spawn_miller_list(tile, loc);
                    }
                }
                Action::MillerSelect {
                    tile,
                    column,
                    idx,
                    shift,
                    ctrl,
                } => {
                    self.miller_select(tile, column, idx, shift, ctrl);
                }
                Action::MillerActivate { tile, column, idx } => {
                    let info = self
                        .state
                        .tab_states
                        .get(&tile)
                        .filter(|state| state.miller_visible_indices(column).contains(&idx))
                        .and_then(|ts| ts.miller.columns.get(column))
                        .and_then(|c| c.entries.get(idx))
                        .map(|e| (e.path(), e.kind().clone(), e.archive_root()));
                    if let Some((path, kind, archive_root)) = info {
                        if kind == FileKind::Dir || (archive_root.is_none() && atlas_core::fs::archive::is_archive_path(&path)) {
                            self.miller_select(tile, column, idx, false, false);
                        } else {
                            self.activate_file_path(tile, path, archive_root);
                        }
                    }
                }
                Action::MillerToggleSelect { tile, column, idx } => {
                    self.miller_select(tile, column, idx, false, true);
                }
                Action::FocusNextPane => {
                    let panes: Vec<_> = self.state.pane_tree.visible_panes().into_iter()
                        .filter(|id| !self.pane_closing.contains_key(id) && !self.tab_closing.contains_key(id)).collect();
                    if panes.len() > 1 {
                        let cur = self.state.focused_tile;
                        let next = panes
                            .iter()
                            .skip_while(|&&id| id != cur)
                            .nth(1)
                            .copied()
                            .unwrap_or(panes[0]);
                        self.activate_tab(next);
                    }
                }
                Action::SelectLeftPanel => self.select_spatial_panel(crate::ui::tab_reorder::PanelDirection::Left),
                Action::SelectRightPanel => self.select_spatial_panel(crate::ui::tab_reorder::PanelDirection::Right),
                Action::SelectPanelAbove => self.select_spatial_panel(crate::ui::tab_reorder::PanelDirection::Above),
                Action::SelectPanelBelow => self.select_spatial_panel(crate::ui::tab_reorder::PanelDirection::Below),
                Action::EvenPanelSiblings => self.even_panels(false),
                Action::EvenAllPanes => self.even_panels(true),
                Action::MoveTabToPanel(direction) => self.move_tab_to_spatial_panel(direction),
                Action::ToggleFolderViewRecursive => self.toggle_folder_view_recursive(),
                Action::SetViewMode(mode) => {
                    let tab = self.state.focused_tile;
                    self.set_pane_view_mode(tab, mode);
                }
                Action::SetTheme(name) => self.set_theme(name),
                Action::PreviewTheme(name) => self.preview_theme(name),
                Action::MapNetworkDrive => {
                    #[cfg(windows)]
                    {
                        let remote = self
                            .tab_data_for(self.state.focused_tile)
                            .and_then(|td| td.location.fs_dir())
                            .filter(|dir| atlas_core::fs::network::share_root(dir).is_some())
                            .map(std::path::Path::to_path_buf);
                        self.map_network_drive(remote);
                    }
                }
                Action::CompareFolders => self.open_folder_compare(),
                Action::ToggleThemeAppearance => self.toggle_theme_appearance(),
                Action::ToggleThemeFollowSystem => {
                    let follow = !self.theme_follows_system();
                    self.set_theme_follow_system(follow);
                    self.toast_info(
                        rust_i18n::t!(if follow {
                            "theme_follow_system_on"
                        } else {
                            "theme_follow_system_off"
                        })
                        .to_string(),
                    );
                }
                Action::SwitchProfile(idx) => self.switch_profile(idx),
                Action::CreateProfile(name) => self.create_profile(name),
                Action::RenameActiveProfile(name) => self.rename_active_profile(name),
                Action::RenameProfile { index, name } => self.rename_profile(index, name),
                Action::OpenRenameProfile => {
                    // Re-seed the input with the current name on next render.
                    self.rename_profile_input = None;
                    self.renaming_profile_idx = None;
                    self.state.overlay.renaming_profile = true;
                }
                Action::OpenRenameProfileAt(index) => {
                    self.rename_profile_input = None;
                    self.renaming_profile_idx = Some(index);
                    self.state.profile_ui.confirming_delete_profile = None;
                    self.state.overlay.renaming_profile = true;
                }
                Action::DuplicateProfile(index) => self.duplicate_profile(index),
                Action::DeleteActiveProfile => self.delete_active_profile(),
                Action::DeleteProfile(index) => self.delete_profile(index),
                Action::ToggleSidebarSection(i) => {
                    if let Some(section) =
                        self.state.profiles.active_mut().sidebar_sections.get_mut(i)
                    {
                        section.collapsed ^= true;
                    }
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                }
                Action::MoveSidebarSectionUp(i) => {
                    let sections = &mut self.state.profiles.active_mut().sidebar_sections;
                    if i > 0 && i < sections.len() {
                        sections.swap(i, i - 1);
                    }
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                }
                Action::MoveSidebarSectionDown(i) => {
                    let sections = &mut self.state.profiles.active_mut().sidebar_sections;
                    if i + 1 < sections.len() {
                        sections.swap(i, i + 1);
                    }
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                }
                Action::SetSidebarSectionHidden { index, hidden } => {
                    if let Some(section) = self
                        .state
                        .profiles
                        .active_mut()
                        .sidebar_sections
                        .get_mut(index)
                    {
                        section.hidden = hidden;
                    }
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                }
                Action::ResetSidebarSections => {
                    self.state.profiles.active_mut().sidebar_sections =
                        atlas_model::profiles::default_sections();
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                }
                Action::RemoveBookmark(idx) => {
                    if idx < self.state.profiles.active().bookmarks.len() {
                        self.state.profiles.active_mut().bookmarks.remove(idx);
                        atlas_core::util::config::save_profiles(&self.state.profiles);
                    }
                }
                Action::AddBookmark { name, path } => {
                    let bookmarks = &mut self.state.profiles.active_mut().bookmarks;
                    if let Some(existing) = bookmarks.iter_mut().find(|b| b.path == path) {
                        existing.name = name;
                    } else {
                        bookmarks.push(atlas_model::profiles::Bookmark::new(name, path));
                    }
                    atlas_core::util::config::refresh_bookmark_volumes(&mut self.state.profiles);
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                }
                Action::CreateTag {
                    name,
                    color,
                    profile,
                } => {
                    let paths = std::mem::take(&mut self.state.profile_ui.new_tag_paths);
                    let command = if paths.is_empty() {
                        atlas_core::fs::tag_db::TagDbCmd::CreateTag {
                            name,
                            color,
                            profile,
                        }
                    } else {
                        atlas_core::fs::tag_db::TagDbCmd::CreateAndAssignTag {
                            name,
                            color,
                            profile,
                            paths,
                        }
                    };
                    let _ = self.tag_db_tx.send(command);
                }
                Action::RenameTag { tag, name } => {
                    if let Some(def) = self.state.tag_cache.defs.iter_mut().find(|d| d.id == tag) {
                        def.name = name.clone();
                    }
                    let _ = self
                        .tag_db_tx
                        .send(atlas_core::fs::tag_db::TagDbCmd::RenameTag { tag, name });
                }
                Action::OpenCreateTagOverlay { global } => {
                    self.state.profile_ui.creating_tag = true;
                    self.state.profile_ui.adding_tag = true;
                    self.state.profile_ui.editing_tag = None;
                    self.state.profile_ui.new_tag_global = global;
                    self.state.profile_ui.new_tag_name.clear();
                    self.state.profile_ui.new_tag_color = atlas_model::profiles::PRESET_COLORS[0];
                    self.state.profile_ui.new_tag_paths.clear();
                }
                Action::OpenRenameTagOverlay {
                    tag,
                    name,
                    color,
                    global,
                } => {
                    self.state.profile_ui.creating_tag = true;
                    self.state.profile_ui.adding_tag = true;
                    self.state.profile_ui.editing_tag = Some(tag);
                    self.state.profile_ui.new_tag_global = global;
                    self.state.profile_ui.new_tag_name = name;
                    self.state.profile_ui.new_tag_color = color;
                    self.state.profile_ui.new_tag_paths.clear();
                }
                Action::CloseCreateTagOverlay => {
                    self.state.profile_ui.creating_tag = false;
                    self.state.profile_ui.adding_tag = false;
                    self.state.profile_ui.editing_tag = None;
                    self.state.profile_ui.new_tag_name.clear();
                    self.state.profile_ui.new_tag_paths.clear();
                    self.create_tag_input = None;
                }
                Action::DeleteTag(tag) => {
                    self.state.tag_cache.defs.retain(|d| d.id != tag);
                    for tags in self.state.tag_cache.for_path.values_mut() {
                        tags.retain(|t| *t != tag);
                    }
                    let _ = self
                        .tag_db_tx
                        .send(atlas_core::fs::tag_db::TagDbCmd::DeleteTag(tag));
                }
                Action::AssignTagToFiles { tag, paths } => {
                    // Only real filesystem paths belong in the tag DB: archive/remote
                    // entries are virtual and `$R` recycle payloads are transient.
                    let paths: Vec<_> = paths
                        .into_iter()
                        .filter(|p| p.exists() && !atlas_core::fs::paths::is_recycle_bin_path(p))
                        .collect();
                    if paths.is_empty() {
                        self.toast_error(rust_i18n::t!("tags_paths_blocked").into_owned());
                        continue;
                    }
                    self.operation_feedback.borrow_mut().begin_tag(tag, &paths);
                    if self
                        .tag_db_tx
                        .send(atlas_core::fs::tag_db::TagDbCmd::AssignTag { tag, paths: paths.clone() }).is_err() {
                        self.operation_feedback.borrow_mut().finish_tag(tag, &paths);
                        for path in &paths { self.dropzone_pending_tags.remove(path); }
                        self.toast_error(rust_i18n::t!("tags_assignment_failed").into_owned());
                    }
                    let _ = self
                        .tag_db_tx
                        .send(atlas_core::fs::tag_db::TagDbCmd::QueryPathsForTag(tag));
                }
                Action::UnassignTagFromFiles { tag, paths } => {
                    self.state.tag_cache.unassign_local(tag, &paths);
                    let _ = self
                        .tag_db_tx
                        .send(atlas_core::fs::tag_db::TagDbCmd::UnassignTag { tag, paths });
                    let _ = self
                        .tag_db_tx
                        .send(atlas_core::fs::tag_db::TagDbCmd::QueryPathsForTag(tag));
                }
                Action::OpenTagResults { pane, tag } => self.open_tag_results(pane, tag),
                Action::OpenSpaceLens { pane, path } => self.open_space_lens(pane, path),
                Action::ZoomIn => {
                    self.state.settings.global_zoom =
                        (self.state.settings.global_zoom * 1.1).min(3.0);
                    atlas_core::util::config::save_settings(&self.state.settings);
                    self.theme_dirty = true;
                }
                Action::ZoomOut => {
                    self.state.settings.global_zoom =
                        (self.state.settings.global_zoom / 1.1).max(0.5);
                    atlas_core::util::config::save_settings(&self.state.settings);
                    self.theme_dirty = true;
                }
                Action::ZoomReset => {
                    self.state.settings.global_zoom = 1.0;
                    atlas_core::util::config::save_settings(&self.state.settings);
                    self.theme_dirty = true;
                }
                Action::PaneContentZoomIn => {
                    self.pane_ctrl_scroll(self.state.focused_tile, 1.0);
                }
                Action::PaneContentZoomOut => {
                    self.pane_ctrl_scroll(self.state.focused_tile, -1.0);
                }
                Action::MountDevice(dev) => self.mount_device(dev),
                Action::UnmountDevice(dev) => self.unmount_device(dev),
                Action::NavigateIntoArchive {
                    tab,
                    archive,
                    inner,
                } => {
                    self.navigate_into_archive(tab, archive, inner);
                }
                Action::ExtractArchiveTo { tab, items, dest } => {
                    self.extract_archive_items(tab, items, dest);
                }
                #[cfg(feature = "provider-host")]
                Action::Cloud(c) => self.handle_cloud_action(c),
                Action::Ext(e) => self.handle_ext_action(e),

                // ── Preview editor ────────────────────────────────────────────
                Action::SavePreviewFile => {
                    if let Some(editor) = &mut self.preview_editor {
                        if editor.path.to_string_lossy().starts_with("remote-preview:") { continue; }
                        if editor.dirty {
                            // `cached_text` is kept current by the InputState Change
                            // subscription; no `cx` needed here.
                            let text = editor.cached_text.clone();
                            let path = editor.path.clone();
                            editor.dirty = false;
                            // Write on a blocking thread so the UI stays responsive.
                            let rt = self.rt.clone();
                            rt.spawn(async move {
                                let _ = tokio::task::spawn_blocking(move || {
                                    #[allow(clippy::disallowed_methods)]
                                    let _ = std::fs::write(&path, text.as_bytes());
                                })
                                .await;
                            });
                        }
                    }
                }
                Action::TogglePreviewEditMode => {
                    use atlas_model::profiles::PreviewFeature;
                    let mut prefs = self.state.profiles.active().scoped::<PreviewFeature>();
                    prefs.edit_mode = !prefs.edit_mode;
                    // Update the live editor state; the render uses editor.editing.
                    if let Some(editor) = &mut self.preview_editor {
                        editor.editing = prefs.edit_mode;
                    }
                    self.state
                        .profiles
                        .active_mut()
                        .set_scoped::<PreviewFeature>(&prefs);
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                }
                Action::TogglePreviewLineNumbers => {
                    use atlas_model::profiles::PreviewFeature;
                    let mut prefs = self.state.profiles.active().scoped::<PreviewFeature>();
                    prefs.show_line_numbers = !prefs.show_line_numbers;
                    // Mark the live editor so render_text_preview syncs InputState.
                    if let Some(editor) = &mut self.preview_editor {
                        editor.line_numbers = prefs.show_line_numbers;
                        editor.ln_needs_sync = true;
                    }
                    self.state
                        .profiles
                        .active_mut()
                        .set_scoped::<PreviewFeature>(&prefs);
                    atlas_core::util::config::save_profiles(&self.state.profiles);
                }
                Action::PreviewZoomIn => {
                    let tab = self.state.focused_tile;
                    let v = self.preview_view.entry(tab).or_default();
                    v.zoom = (v.zoom * 1.25).clamp(0.1, 48.0);
                }
                Action::PreviewZoomOut => {
                    let tab = self.state.focused_tile;
                    let v = self.preview_view.entry(tab).or_default();
                    v.zoom = (v.zoom / 1.25).clamp(0.1, 48.0);
                }
                Action::PreviewZoomFit => {
                    let tab = self.state.focused_tile;
                    let v = self.preview_view.entry(tab).or_default();
                    v.zoom = 1.0;
                    v.pan_x = 0.0;
                    v.pan_y = 0.0;
                }
            }
        }
    }
}

fn unique_child_name(dir: &std::path::Path, base: &str) -> String {
    atlas_core::fs::transfer::unique_target(dir, std::ffi::OsStr::new(base))
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

impl App {
    fn copy_paths_as_text(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() { return; }
        let text = atlas_core::util::display::clipboard_path_text(paths);
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(text.clone());
            #[cfg(target_os = "linux")]
            { use arboard::{LinuxClipboardKind, SetExtLinux};
                let _ = cb.set().clipboard(LinuxClipboardKind::Primary).text(text); }
        }
    }

    pub(crate) fn copy_focused(&mut self) {
        if self
            .state
            .active_tab_data()
            .map_or(false, |t| t.location.is_archive())
        {
            self.toast_error(rust_i18n::t!("root_copy_archive_error").into());
            return;
        }
        let paths = self.selected_paths();
        if !paths.is_empty() {
            self.process_actions(vec![Action::Copy(paths)]);
        }
    }

    pub(crate) fn cut_focused(&mut self) {
        if self
            .state
            .active_tab_data()
            .map_or(false, |t| t.location.is_archive())
        {
            self.toast_error(rust_i18n::t!("root_cut_archive_error").into());
            return;
        }
        let paths = self.selected_paths();
        if !paths.is_empty() {
            self.process_actions(vec![Action::Cut(paths)]);
        }
    }

    pub(crate) fn paste_focused(&mut self) {
        if self
            .state
            .active_tab_data()
            .map_or(false, |t| t.location.is_archive())
        {
            self.toast_error(rust_i18n::t!("root_paste_archive_error").into());
            return;
        }
        let dest = self.state.active_tab_data().and_then(|t| {
            if let Some(p) = t.location.fs_dir() {
                return Some(p.to_path_buf());
            }
            #[cfg(feature = "provider-host")]
            if matches!(t.location, atlas_model::location::Location::Remote { .. }) {
                return Some(PathBuf::new());
            }
            None
        });
        if let Some(dest) = dest {
            self.process_actions(vec![Action::Paste { dest }]);
        }
    }

    pub(crate) fn detach_focused_tab(&mut self) {
        #[cfg(windows)]
        if let Some(data) = self.tab_data_for(self.state.focused_tile).cloned() {
            self.transfer_tab_to_window(data, true);
        }
        #[cfg(not(windows))]
        self.toast_error("Tab detach through OLE is available on Windows".to_owned());
    }

    #[cfg(windows)]
    fn transfer_tab_to_window(&mut self, data: atlas_model::pane::TabData, close_source: bool) {
        #[cfg(windows)]
        {
            let tab = self.state.focused_tile;
            let Some(queue) = self.dnd.as_ref().map(|dnd| dnd.action_queue.clone()) else { return };
            let token = format!("{}-{}", std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
            use crate::dnd::windows::{ole_clipboard, DndEvent};
            let result = (|| -> Result<(), String> {
                let executable = std::env::current_exe().map_err(|error| error.to_string())?;
                let same_location = self.tab_data_for(tab).is_some_and(|current| current.location == data.location);
                let runtime = self.state.tab_states.get(&tab);
                let offset = same_location.then(|| self.scroll_handles.get(&tab)).flatten().map(|handle| handle.0.borrow().base_handle.offset())
                    .unwrap_or_else(|| gpui::point(gpui::px(0.0), gpui::px(0.0)));
                let miller_offsets: Vec<_> = runtime.into_iter().flat_map(|state| state.miller.columns.iter().enumerate())
                    .map(|(index, _)| self.miller_scroll_handles.get(&(tab, index))
                        .map(|handle| { let offset = handle.0.borrow().base_handle.offset(); [f32::from(offset.x), f32::from(offset.y)] })
                        .unwrap_or_default()).collect();
                ole_clipboard::publish_tab_transfer(&token, &data, runtime,
                    [f32::from(offset.x), f32::from(offset.y)], &miller_offsets, same_location,
                    ole_clipboard::ViewFilters { types: self.type_filters.get(&tab).cloned().unwrap_or_default(),
                        sizes: self.size_filters.get(&tab).cloned().unwrap_or_default(),
                        show_hidden: Some(self.state.settings.show_hidden) })?;
                let event = ole_clipboard::prepare_transfer_ack(&token)?;
                let mut command = std::process::Command::new(executable);
                command.env("ATLAS_TAB_TRANSFER_TOKEN", &token);
                command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x00000008 | 0x00000200);
                let spawned = command.spawn().map_err(|error| error.to_string());
                let launched = spawned.is_ok();
                std::thread::spawn(move || {
                    let accepted = ole_clipboard::wait_transfer_ack(event);
                    if launched {
                        if let Some(mut events) = queue.lock().or_else(atlas_model::sync::recover).ok() {
                            if accepted && close_source { events.push(DndEvent::TabTransferAccepted { tab }); }
                            else if !accepted { events.push(DndEvent::Error("Tab transfer was not acknowledged; the original tab remains open".into())); }
                        }
                        crate::ui_wake::wake();
                    }
                });
                spawned.map(|_| ())
            })();
            if let Err(error) = result { self.toast_error(error); }
        }
    }

    pub(crate) fn spawn_new_window(&mut self, target_dir: Option<PathBuf>) {
        let target_dir = target_dir
            .map(|p| {
                if p.is_file() {
                    p.parent().map(std::path::Path::to_path_buf).unwrap_or(p)
                } else {
                    p
                }
            })
            .or_else(dirs::home_dir);
        #[cfg(windows)]
        {
            if let Some(mut data) = self.tab_data_for(self.state.focused_tile).cloned() {
                if let Some(path) = target_dir { data.location = atlas_model::location::Location::Directory(path); }
                self.transfer_tab_to_window(data, false);
                return;
            }
        }
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("atlas"));
        let mut cmd = std::process::Command::new(&exe);
        if let Some(ref dir) = target_dir {
            cmd.arg(dir);
            cmd.current_dir(dir);
        }

        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x00000008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }

        match cmd.spawn() {
            Ok(_) => {
                tracing::info!(exe = ?exe, target = ?target_dir, "spawned new Atlas window");
            }
            Err(err) => {
                tracing::error!(exe = ?exe, target = ?target_dir, ?err, "failed to spawn new Atlas window");
                self.toast_error(format!("Failed to open new window: {err}"));
            }
        }
    }

    pub(crate) fn close_search_in_place(&mut self, tile: atlas_model::layout_tree::TileId) {
        self.restore_search_original_entries(tile);
        if let Some(ts) = self.state.tab_states.get_mut(&tile) {
            ts.search.active = false;
            ts.search.focus = false;
            ts.search.query.clear();
            ts.search.status = atlas_model::tab::SearchStatus::Idle;
            ts.search.error = None;
            ts.filter.clear();
            ts.visible_valid = false;
            ts.selection.clear();
        }
        self.search_inputs.remove(&tile);
    }

    pub(crate) fn refresh_search_in_place(&mut self, tile: atlas_model::layout_tree::TileId) {
        let Some((query, recursive)) = self.state.tab_states.get(&tile).map(|ts| {
            (
                ts.search.query.trim().to_string(),
                ts.search.recursive && ts.search.active,
            )
        }) else {
            return;
        };

        if query.is_empty() {
            self.restore_search_original_entries(tile);
            if let Some(ts) = self.state.tab_states.get_mut(&tile) {
                ts.filter.clear();
                ts.visible_valid = false;
                ts.search.status = atlas_model::tab::SearchStatus::Idle;
                ts.search.error = None;
                ts.selection.clear();
            }
            return;
        }

        if !recursive {
            self.restore_search_original_entries(tile);
            let Some((generation, entries, show_files, show_folders)) =
                self.state.tab_states.get_mut(&tile).map(|ts| {
                    ts.search.generation = ts.search.generation.wrapping_add(1);
                    ts.search.status = atlas_model::tab::SearchStatus::Searching;
                    ts.search.error = None;
                    (
                        ts.search.generation,
                        ts.entries.clone(),
                        ts.show_files,
                        ts.show_folders,
                    )
                })
            else {
                return;
            };
            let tx = self.tx.clone();
            self.rt.spawn_blocking(move || {
                let (visible, counts) = atlas_model::tab::filter_visible_entries(
                    &entries,
                    &query,
                    show_files,
                    show_folders,
                );
                let _ = tx.send(atlas_model::app_state::BgResult::FolderFilterResults {
                    tab: tile,
                    gen: generation,
                    query,
                    visible,
                    counts,
                });
            });
            return;
        }

        let Some(root) = self
            .tab_data_for(tile)
            .and_then(|td| td.location.fs_dir().map(|p| p.to_path_buf()))
        else {
            return;
        };
        let show_hidden = self.state.settings.show_hidden;
        let generation = {
            let Some(ts) = self.state.tab_states.get_mut(&tile) else {
                return;
            };
            if ts.search.original_entries.is_none() {
                ts.search.original_entries = Some(ts.entries.clone());
            }
            ts.search.generation = ts.search.generation.wrapping_add(1);
            ts.search.status = atlas_model::tab::SearchStatus::Searching;
            ts.search.error = None;
            ts.filter.clear();
            ts.visible_valid = false;
            ts.search.generation
        };

        let tx = self.tx.clone();
        self.rt.spawn(async move {
            match atlas_core::search::name::search_names_recursive(root, query, show_hidden).await {
                Ok(entries) => {
                    let _ = tx.send(atlas_model::app_state::BgResult::SearchInPlaceResults {
                        tab: tile,
                        gen: generation,
                        entries,
                    });
                }
                Err(err) => {
                    let _ = tx.send(atlas_model::app_state::BgResult::SearchInPlaceError {
                        tab: tile,
                        gen: generation,
                        error: err.to_string(),
                    });
                }
            }
        });
    }

    fn restore_search_original_entries(&mut self, tile: atlas_model::layout_tree::TileId) {
        let Some(ts) = self.state.tab_states.get_mut(&tile) else {
            return;
        };
        if let Some(entries) = ts.search.original_entries.take() {
            ts.set_entries(entries);
        }
    }

    pub(crate) fn spawn_terminal(&self, dir: &std::path::Path) {
        spawn_terminal(dir);
    }
}

#[cfg(target_os = "linux")]
fn is_in_path(name: &str) -> bool {
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

#[cfg(target_os = "linux")]
fn launch_emulator(bin: &str, dir: &std::path::Path) -> std::io::Result<std::process::Child> {
    let bin_name = std::path::Path::new(bin)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(bin);

    let mut cmd = std::process::Command::new(bin);
    cmd.current_dir(dir);

    match bin_name {
        "kitty" => {
            cmd.arg("--directory").arg(dir);
        }
        "alacritty" | "foot" | "ghostty" | "gnome-terminal" | "xfce4-terminal" => {
            cmd.arg("--working-directory").arg(dir);
        }
        "wezterm" => {
            cmd.arg("start").arg("--cwd").arg(dir);
        }
        "konsole" => {
            cmd.arg("--workdir").arg(dir);
        }
        _ => {}
    }
    cmd.spawn()
}

pub(crate) fn spawn_terminal(dir: &std::path::Path) {
    #[cfg(target_os = "linux")]
    {
        // 1. Evalúa primero la variable de entorno $TERMINAL
        if let Ok(term) = std::env::var("TERMINAL") {
            let term = term.trim();
            if !term.is_empty() && launch_emulator(term, dir).is_ok() {
                return;
            }
        }

        // 2. Si no existe, intenta x-terminal-emulator
        if is_in_path("x-terminal-emulator") && launch_emulator("x-terminal-emulator", dir).is_ok() {
            return;
        }

        // 3. Si no existe, busca en $PATH terminales comunes:
        // kitty, alacritty, foot, ghostty, wezterm, gnome-terminal, konsole, xfce4-terminal, xterm
        const COMMON_TERMINALS: &[&str] = &[
            "kitty",
            "alacritty",
            "foot",
            "ghostty",
            "wezterm",
            "gnome-terminal",
            "konsole",
            "xfce4-terminal",
            "xterm",
        ];
        for &term in COMMON_TERMINALS {
            if is_in_path(term) && launch_emulator(term, dir).is_ok() {
                return;
            }
        }

        let _ = launch_emulator("x-terminal-emulator", dir).or_else(|_| launch_emulator("xterm", dir));
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        // Launch the system cmd.exe by absolute path in its own console. The
        // old `cmd /c start cmd` let cmd.exe resolve "cmd" in the opened folder
        // first, so a planted cmd.bat/cmd.com there ran instead of the shell.
        let system_root = std::env::var_os("SystemRoot")
            .map_or_else(|| std::path::PathBuf::from(r"C:\Windows"), std::path::PathBuf::from);
        let _ = std::process::Command::new(system_root.join("System32").join("cmd.exe"))
            .current_dir(dir)
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("-a")
            .arg("Terminal")
            .arg(dir)
            .spawn();
    }
}
