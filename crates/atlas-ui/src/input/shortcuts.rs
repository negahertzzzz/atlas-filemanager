use crate::app::App;
use crate::input::action::{Action, SelectionMove};
use crate::input::commands::{command_remappable, default_command_shortcut, COMMANDS};
use atlas_model::profiles::ProfileScoped;
use atlas_model::settings::ViewMode;
use gpui::{
    Context, Focusable, InteractiveElement, KeyDownEvent, KeybindingKeystroke, Keystroke, Window,
};
use gpui_component::input;
use std::collections::HashMap;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CommandShortcutPrefs {
    #[serde(default)]
    bindings: HashMap<String, Option<String>>,
}

impl CommandShortcutPrefs {
    /// Whether the user overrode (or removed) this command's shortcut.
    pub(crate) fn customized(&self, command_id: &str) -> bool {
        self.bindings.contains_key(command_id)
    }

    pub(crate) fn shortcut_string(&self, command_id: &str) -> Option<String> {
        match self.bindings.get(command_id) {
            Some(Some(keys)) if Keystroke::parse(keys).is_ok() => Some(keys.clone()),
            Some(Some(_)) | Some(None) => None,
            None => default_command_shortcut(command_id).map(str::to_string),
        }
    }
}

pub(crate) struct CommandShortcuts;

impl ProfileScoped for CommandShortcuts {
    const KEY: &'static str = "command_shortcuts";
    type State = CommandShortcutPrefs;
}

const RESERVED_STATIC_SHORTCUTS: &[&str] = &[
    "ctrl-q",
    "up",
    "down",
    "left",
    "right",
    "shift-up",
    "shift-down",
    "shift-left",
    "shift-right",
    "home",
    "end",
    "pageup",
    "pagedown",
    "ctrl-tab",
    "ctrl-shift-tab",
    "ctrl-f",
    "space",
    "escape",
    "alt-tab",
    "ctrl-r",
    "ctrl-l",
    "alt-up",
];

/// A focused search input resolves these keys into actions before key listeners.
/// The palette installs this adapter; DG forwards raw keys to the same capture state.
pub(crate) fn capture_shortcut_input_actions<E: InteractiveElement>(
    element: E,
    cx: &mut Context<App>,
) -> E {
    fn bind<E: InteractiveElement, A: gpui::Action>(
        element: E,
        key: &'static str,
        cx: &mut Context<App>,
    ) -> E {
        element.capture_action(cx.listener(move |this, _: &A, _window, cx| {
            if this.capture_command_shortcut(key) {
                cx.stop_propagation();
                cx.notify();
            }
        }))
    }
    let element = bind::<_, input::IndentInline>(element, "tab", cx);
    let element = bind::<_, input::Backspace>(element, "backspace", cx);
    let element = bind::<_, input::Delete>(element, "delete", cx);
    let element = bind::<_, input::Escape>(element, "escape", cx);
    let element = bind::<_, input::Copy>(element, "ctrl-c", cx);
    let element = bind::<_, input::Cut>(element, "ctrl-x", cx);
    bind::<_, input::Paste>(element, "ctrl-v", cx)
}

impl App {
    pub(crate) fn handle_global_command_shortcut_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.handle_converter_key(ev) {
            cx.notify();
            return true;
        }
        if let Some(panel) = self.cleanup_panel.clone() {
            if ev.keystroke.key == "escape" {
                if panel.update(cx, |p, cx| p.escape(cx)) {
                    self.cleanup_panel = None;
                    window.focus(&self.focus_handle, cx);
                }
                cx.notify();
                return true;
            }
            if panel.update(cx, |p, cx| p.handle_key(ev, window, cx)) {
                cx.notify();
                return true;
            }
        }
        if let Some(panel) = self.compare_panel.clone() {
            if ev.keystroke.key == "escape" {
                // Closed here: the panel cannot update the App from inside
                // the App's own key handler.
                panel.update(cx, |p, _| p.cancel());
                self.compare_panel = None;
                window.focus(&self.focus_handle, cx);
                cx.notify();
                return true;
            }
            if panel.update(cx, |p, cx| p.handle_key(ev, window, cx)) {
                cx.notify();
            }
            // The panel is modal: keys never reach the panes behind it.
            return true;
        }
        if self.command_shortcut_capture.is_some() {
            return self.handle_command_shortcut_key(ev);
        }
        if self.ctx_menu.is_some() {
            return self.handle_ctx_menu_key(ev, window, cx);
        }

        let text_input_focused = self.any_text_input_focused(window, cx);
        let pane_filter_active = self
            .state
            .active_tab_state()
            .is_some_and(|ts| ts.search.active);
        let app_modified = ev.keystroke.modifiers.control
            || ev.keystroke.modifiers.alt
            || ev.keystroke.modifiers.platform;

        if text_input_focused && !(pane_filter_active && app_modified) {
            return false;
        }

        if self
            .state
            .active_tab_state()
            .is_some_and(|ts| ts.path_edit.editing)
        {
            return false;
        }

        if !text_input_focused
            && !pane_filter_active
            && ev.keystroke.key == "backspace"
            && !ev.keystroke.modifiers.control
            && !ev.keystroke.modifiers.alt
            && !ev.keystroke.modifiers.platform
        {
            self.process_actions(vec![Action::NavigateUp(self.state.focused_tile)]);
            return true;
        }

        if ev.keystroke.key == "menu" || ev.keystroke.key == "apps" {
            self.open_context_menu_focused(window, cx);
            return true;
        }

        if let Some(keys) = self.command_shortcut_string("files.context-menu") {
            if let Ok(target) = Keystroke::parse(&keys) {
                let target = KeybindingKeystroke::from_keystroke(target);
                if ev.keystroke.should_match(&target) {
                    self.open_context_menu_focused(window, cx);
                    return true;
                }
            }
        }

        if self.handle_vim_mode_key(ev, window, cx) {
            return true;
        }

        if self.state.settings.character_key_action != atlas_model::settings::CharacterKeyAction::TriggerHotkey
            && crate::ui::interactions::search_seed_from_key(ev).is_some() {
            return false;
        }

        self.handle_command_shortcut_key(ev)
    }

    pub(crate) fn handle_vim_mode_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.state.settings.vim_mode {
            self.vim_pending_key = None;
            return false;
        }

        let text_input_focused = self.any_text_input_focused(window, cx);
        let pane_filter_active = self
            .state
            .active_tab_state()
            .is_some_and(|ts| ts.search.active);

        if text_input_focused
            || pane_filter_active
            || self.modal_overlay_active()
            || self.command_shortcuts_blocked_by_app_state()
            || self
                .state
                .active_tab_state()
                .is_some_and(|ts| ts.path_edit.editing)
        {
            self.vim_pending_key = None;
            return false;
        }

        let key = ev.keystroke.key.as_str();
        let shift = ev.keystroke.modifiers.shift;
        let ctrl = ev.keystroke.modifiers.control;
        let alt = ev.keystroke.modifiers.alt;
        let platform = ev.keystroke.modifiers.platform;

        if ctrl || alt || platform {
            self.vim_pending_key = None;
            return false;
        }

        if self.vim_pending_key == Some('g') {
            self.vim_pending_key = None;
            if key == "g" && !shift {
                self.process_actions(vec![Action::MoveSelection {
                    target: SelectionMove::First,
                    extend: false,
                }]);
                return true;
            }
        }

        match (key, shift) {
            ("j", false) => {
                self.process_actions(vec![Action::MoveSelection {
                    target: SelectionMove::Next,
                    extend: false,
                }]);
                true
            }
            ("k", false) => {
                self.process_actions(vec![Action::MoveSelection {
                    target: SelectionMove::Previous,
                    extend: false,
                }]);
                true
            }
            ("h", false) => {
                self.process_actions(vec![Action::NavigateUp(self.state.focused_tile)]);
                true
            }
            ("l", false) | ("enter", false) => {
                self.process_actions(vec![Action::OpenCursor]);
                true
            }
            ("y", false) => {
                self.copy_focused();
                true
            }
            ("d", false) => {
                self.cut_focused();
                true
            }
            ("p", false) => {
                self.paste_focused();
                true
            }
            ("g", false) => {
                self.vim_pending_key = Some('g');
                true
            }
            ("G", _) | ("g", true) => {
                self.process_actions(vec![Action::MoveSelection {
                    target: SelectionMove::Last,
                    extend: false,
                }]);
                true
            }
            ("/", _) | ("slash", _) => {
                self.process_actions(vec![Action::OpenSearchInPlace]);
                if let Some(input) = self.search_inputs.get(&self.state.focused_tile).cloned() {
                    let focus = input.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                }
                true
            }
            _ => {
                self.vim_pending_key = None;
                false
            }
        }
    }

    pub(crate) fn command_shortcut_string(&self, command_id: &str) -> Option<String> {
        let prefs = self.state.profiles.active().scoped::<CommandShortcuts>();
        prefs.shortcut_string(command_id)
    }

    pub(crate) fn command_shortcut_keystroke(&self, command_id: &str) -> Option<Keystroke> {
        self.command_shortcut_string(command_id)
            .and_then(|keys| Keystroke::parse(&keys).ok())
    }

    pub(crate) fn start_command_shortcut_capture(&mut self, command_id: String) {
        self.command_shortcut_conflict = None;
        if command_remappable(&command_id) {
            self.command_shortcut_capture = Some(command_id);
        }
    }

    pub(crate) fn cancel_command_shortcut_capture(&mut self) {
        self.command_shortcut_capture = None;
        self.command_shortcut_conflict = None;
    }

    pub(crate) fn set_command_shortcut(&mut self, command_id: String, keystroke: String) {
        if !command_remappable(&command_id) || Keystroke::parse(&keystroke).is_err() {
            return;
        }
        if reserved_static_shortcut(&keystroke) {
            self.toast_error(rust_i18n::t!("palette_shortcut_reserved").to_string());
            self.command_shortcut_capture = None;
            return;
        }

        let Ok(proposed) = Keystroke::parse(&keystroke) else { return; };
        let proposed = KeybindingKeystroke::from_keystroke(proposed);
        for other in COMMANDS.iter().filter(|command| command.remappable) {
            if other.id != command_id
                && self.command_shortcut_keystroke(other.id).is_some_and(|keys| keys.should_match(&proposed))
            {
                self.command_shortcut_conflict = Some(other.id.to_string());
                return;
            }
        }
        let mut prefs = self.state.profiles.active().scoped::<CommandShortcuts>();
        prefs.bindings.insert(command_id, Some(keystroke));
        self.state
            .profiles
            .active_mut()
            .set_scoped::<CommandShortcuts>(&prefs);
        atlas_core::util::config::save_profiles(&self.state.profiles);
        self.command_shortcut_capture = None;
        self.command_shortcut_conflict = None;
    }

    pub(crate) fn remove_command_shortcut(&mut self, command_id: String) {
        self.command_shortcut_conflict = None;
        if !command_remappable(&command_id) {
            return;
        }
        let mut prefs = self.state.profiles.active().scoped::<CommandShortcuts>();
        prefs.bindings.insert(command_id, None);
        self.state
            .profiles
            .active_mut()
            .set_scoped::<CommandShortcuts>(&prefs);
        atlas_core::util::config::save_profiles(&self.state.profiles);
        self.command_shortcut_capture = None;
    }

    /// Drop the user's override so the command uses its built-in shortcut.
    pub(crate) fn reset_command_shortcut(&mut self, command_id: &str) {
        self.command_shortcut_conflict = None;
        let mut prefs = self.state.profiles.active().scoped::<CommandShortcuts>();
        if prefs.bindings.remove(command_id).is_none() {
            return;
        }
        self.state
            .profiles
            .active_mut()
            .set_scoped::<CommandShortcuts>(&prefs);
        atlas_core::util::config::save_profiles(&self.state.profiles);
    }

    /// The active profile's shortcut overrides (read once per frame by lists).
    pub(crate) fn command_shortcut_prefs(&self) -> CommandShortcutPrefs {
        self.state.profiles.active().scoped::<CommandShortcuts>()
    }

    /// Shared by palette/menu key listeners and input actions (which GPUI
    /// dispatches before raw key listeners). Never executes the command.
    pub(crate) fn capture_command_shortcut(&mut self, keystroke: &str) -> bool {
        if let Some(command_id) = self.command_shortcut_capture.clone() {
            match keystroke {
                "escape" => self.cancel_command_shortcut_capture(),
                "backspace" | "delete" => self.remove_command_shortcut(command_id),
                _ => self.set_command_shortcut(command_id, keystroke.to_string()),
            }
            return true;
        }
        false
    }

    pub(crate) fn handle_command_shortcut_key(&mut self, ev: &KeyDownEvent) -> bool {
        let key = ev.keystroke.key.as_str();
        let capture_key = if matches!(key, "escape" | "backspace" | "delete") {
            key.to_string()
        } else {
            ev.keystroke.unparse()
        };
        if self.capture_command_shortcut(&capture_key) {
            return true;
        }

        if self.command_shortcuts_blocked_by_app_state() {
            return false;
        }

        for command in COMMANDS.iter().filter(|command| command.remappable) {
            let Some(keys) = self.command_shortcut_string(command.id) else {
                continue;
            };
            let Ok(target) = Keystroke::parse(&keys) else {
                continue;
            };
            let target = KeybindingKeystroke::from_keystroke(target);
            if ev.keystroke.should_match(&target) {
                match command.id {
                    "files.copy" => self.copy_focused(),
                    "files.cut" => self.cut_focused(),
                    "files.paste" => self.paste_focused(),
                    _ => {
                        if let Some(action) = self.action_for_command_shortcut(command.id) {
                            self.process_actions(vec![action]);
                        }
                    }
                }
                return true;
            }
        }

        // Additive keyboard ranges mirror Ctrl+Shift pointer selection. Explicit
        // user shortcuts above keep priority over these modifier combinations.
        let modifiers = ev.keystroke.modifiers;
        if modifiers.control && modifiers.shift && !modifiers.alt && !modifiers.platform {
            match key {
                "up" => self.nav_up(true, true),
                "down" => self.nav_down(true, true),
                "left" => self.nav_left(true, true),
                "right" => self.nav_right(true, true),
                "home" => self.nav_home(true, true),
                "end" => self.nav_end(true, true),
                "pageup" => self.nav_page(false, true, true),
                "pagedown" => self.nav_page(true, true, true),
                _ => return false,
            }
            return true;
        }

        false
    }

    fn action_for_command_shortcut(&self, command_id: &str) -> Option<Action> {
        let focused = self.state.focused_tile;
        let active_location = self.state.active_tab_data().map(|tab| &tab.location);
        let archive_focused =
            active_location.is_some_and(atlas_model::location::Location::is_archive);
        let recycle_focused = active_location
            .is_some_and(|location| *location == atlas_model::location::Location::RecycleBin);
        match command_id {
            "tab.new" => Some(Action::NewTabFocused),
            "tab.close" => Some(Action::CloseFocusedTab),
            "tab.reopen" => Some(Action::ReopenClosedTab),
            "tab.detach" => Some(Action::DetachFocusedTab),
            "tab.next" => Some(Action::FocusNextTab),
            "tab.previous" => Some(Action::FocusPreviousTab),
            "tab.move-left" => Some(Action::MoveFocusedTabLeft),
            "tab.move-right" => Some(Action::MoveFocusedTabRight),
            "tab.1" => Some(Action::FocusTabIndex(0)),
            "tab.2" => Some(Action::FocusTabIndex(1)),
            "tab.3" => Some(Action::FocusTabIndex(2)),
            "tab.4" => Some(Action::FocusTabIndex(3)),
            "tab.5" => Some(Action::FocusTabIndex(4)),
            "tab.6" => Some(Action::FocusTabIndex(5)),
            "tab.7" => Some(Action::FocusTabIndex(6)),
            "tab.8" => Some(Action::FocusTabIndex(7)),
            "tab.9" => Some(Action::FocusTabIndex(8)),
            "pane.split-h" => Some(Action::SplitHorizontalFocused),
            "pane.split-v" => Some(Action::SplitVerticalFocused),
            "pane.close" => Some(Action::CloseFocusedPane),
            "pane.focus-next" => Some(Action::FocusNextPane),
            "pane.focus-left" => Some(Action::SelectLeftPanel),
            "pane.focus-right" => Some(Action::SelectRightPanel),
            "pane.focus-above" => Some(Action::SelectPanelAbove),
            "pane.focus-below" => Some(Action::SelectPanelBelow),
            "pane.even-siblings" => Some(Action::EvenPanelSiblings),
            "pane.even-all" => Some(Action::EvenAllPanes),
            "pane.move-left" => Some(Action::MoveTabToPanel(crate::ui::tab_reorder::PanelDirection::Left)),
            "pane.move-right" => Some(Action::MoveTabToPanel(crate::ui::tab_reorder::PanelDirection::Right)),
            "pane.move-above" => Some(Action::MoveTabToPanel(crate::ui::tab_reorder::PanelDirection::Above)),
            "pane.move-below" => Some(Action::MoveTabToPanel(crate::ui::tab_reorder::PanelDirection::Below)),
            "folder.inherit-view" => Some(Action::ToggleFolderViewRecursive),
            "nav.back" => Some(Action::NavigateBack(focused)),
            "nav.forward" => Some(Action::NavigateForward(focused)),
            "nav.parent" => Some(Action::NavigateUp(focused)),
            "nav.goto" => Some(Action::OpenGotoBar),
            "nav.refresh" => Some(Action::Refresh(focused)),
            #[cfg(target_os = "windows")]
            "nav.recycle-bin" => Some(Action::NavigateLocation {
                tab: focused,
                location: atlas_model::location::Location::RecycleBin,
            }),
            #[cfg(target_os = "windows")]
            "net.map-drive" => Some(Action::MapNetworkDrive),
            "pane.compare" => Some(Action::CompareFolders),
            "view.hidden" => Some(Action::ToggleHidden),
            "view.theme-toggle" => Some(Action::ToggleThemeAppearance),
            "view.theme-follow-system" => Some(Action::ToggleThemeFollowSystem),
            "view.vim-mode" => Some(Action::ToggleVimMode),
            "view.inspector" => Some(Action::ToggleInspector),
            "view.branch-view" => Some(Action::ToggleBranchViewFocused),
            "view.sidebar" => Some(Action::ToggleSidebar),
            "view.sidebar-collapse" => Some(Action::ToggleSidebarCollapsed),
            "view.details" => Some(Action::SetViewMode(ViewMode::Details)),
            "view.grid" => Some(Action::SetViewMode(ViewMode::Grid)),
            "view.list" => Some(Action::SetViewMode(ViewMode::List)),
            "view.miller" => Some(Action::SetViewMode(ViewMode::Miller)),
            "view.tree" => Some(Action::SetViewMode(ViewMode::Tree)),
            "view.group-none" => Some(Action::SetGroupBy(atlas_model::tab::GroupBy::None)),
            "view.group-date" => Some(Action::SetGroupBy(atlas_model::tab::GroupBy::Date)),
            "view.group-size" => Some(Action::SetGroupBy(atlas_model::tab::GroupBy::Size)),
            "view.group-type" => Some(Action::SetGroupBy(atlas_model::tab::GroupBy::Type)),
            "view.group-name" => Some(Action::SetGroupBy(atlas_model::tab::GroupBy::Name)),
            "view.group-created" => Some(Action::SetGroupBy(atlas_model::tab::GroupBy::Created)),
            "view.group-accessed" => Some(Action::SetGroupBy(atlas_model::tab::GroupBy::Accessed)),
            "view.group-deleted" => Some(Action::SetGroupBy(atlas_model::tab::GroupBy::Deleted)),
            "view.group-origin" => Some(Action::SetGroupBy(atlas_model::tab::GroupBy::Origin)),
            "view.group-expand" => Some(Action::SetAllGroupsCollapsed(false)),
            "view.group-collapse" => Some(Action::SetAllGroupsCollapsed(true)),
            "view.zoom-in" => Some(Action::ZoomIn),
            "view.zoom-out" => Some(Action::ZoomOut),
            "view.zoom-reset" => Some(Action::ZoomReset),
            "view.content-zoom-in" => Some(Action::PaneContentZoomIn),
            "view.content-zoom-out" => Some(Action::PaneContentZoomOut),
            "folder.toggle-folders-first" => Some(Action::ToggleFoldersFirstFocused),
            "folder.toggle-files" => Some(Action::ToggleShowFilesFocused),
            "folder.toggle-folders" => Some(Action::ToggleShowFoldersFocused),
            "folder.search" => Some(Action::OpenSearchInPlace),
            "folder.search-subfolders" => Some(Action::ToggleSearchInSubfolders),
            "selection.previous" => Some(Action::MoveSelection {
                target: SelectionMove::Previous,
                extend: false,
            }),
            "selection.next" => Some(Action::MoveSelection {
                target: SelectionMove::Next,
                extend: false,
            }),
            "selection.left" => Some(Action::MoveSelection {
                target: SelectionMove::Left,
                extend: false,
            }),
            "selection.right" => Some(Action::MoveSelection {
                target: SelectionMove::Right,
                extend: false,
            }),
            "selection.first" => Some(Action::MoveSelection {
                target: SelectionMove::First,
                extend: false,
            }),
            "selection.last" => Some(Action::MoveSelection {
                target: SelectionMove::Last,
                extend: false,
            }),
            "selection.page-up" => Some(Action::MoveSelection {
                target: SelectionMove::PageUp,
                extend: false,
            }),
            "selection.page-down" => Some(Action::MoveSelection {
                target: SelectionMove::PageDown,
                extend: false,
            }),
            "selection.extend-previous" => Some(Action::MoveSelection {
                target: SelectionMove::Previous,
                extend: true,
            }),
            "selection.extend-next" => Some(Action::MoveSelection {
                target: SelectionMove::Next,
                extend: true,
            }),
            "selection.extend-left" => Some(Action::MoveSelection {
                target: SelectionMove::Left,
                extend: true,
            }),
            "selection.extend-right" => Some(Action::MoveSelection {
                target: SelectionMove::Right,
                extend: true,
            }),
            "selection.extend-first" => Some(Action::MoveSelection {
                target: SelectionMove::First,
                extend: true,
            }),
            "selection.extend-last" => Some(Action::MoveSelection {
                target: SelectionMove::Last,
                extend: true,
            }),
            "selection.extend-page-up" => Some(Action::MoveSelection {
                target: SelectionMove::PageUp,
                extend: true,
            }),
            "selection.extend-page-down" => Some(Action::MoveSelection {
                target: SelectionMove::PageDown,
                extend: true,
            }),
            "selection.cursor-previous" => Some(Action::MoveCursor(SelectionMove::Previous)),
            "selection.cursor-next" => Some(Action::MoveCursor(SelectionMove::Next)),
            "selection.cursor-left" => Some(Action::MoveCursor(SelectionMove::Left)),
            "selection.cursor-right" => Some(Action::MoveCursor(SelectionMove::Right)),
            "selection.toggle" => Some(Action::ToggleSelectionCursor),
            "selection.open" => Some(Action::OpenCursor),
            "edit.select-all" => Some(Action::SelectAll),
            "edit.invert" => Some(Action::InvertSelection),
            "edit.undo" => Some(Action::Undo),
            "edit.redo" => Some(Action::Redo),
            "edit.batch-rename" => Some(Action::OpenBatchRename),
            "files.convert" if !archive_focused && !recycle_focused => Some(Action::OpenConverter),
            "files.rename" if !archive_focused => Some(Action::RenameFocused),
            "files.copy" | "files.cut" if !archive_focused && !recycle_focused => {
                let paths = self.selected_paths();
                if paths.is_empty() {
                    None
                } else if command_id == "files.copy" {
                    Some(Action::Copy(paths))
                } else {
                    Some(Action::Cut(paths))
                }
            }
            "files.paste" if !archive_focused && !recycle_focused => {
                active_location.and_then(|location| {
                    let dest = location.fs_dir().map(std::path::Path::to_path_buf);
                    #[cfg(feature = "provider-host")]
                    let dest = dest.or_else(|| {
                        matches!(location, atlas_model::location::Location::Remote { .. })
                            .then(std::path::PathBuf::new)
                    });
                    dest.map(|dest| Action::Paste { dest })
                })
            }
            "files.open-terminal" => {
                active_location
                    .and_then(|location| location.fs_dir())
                    .map(|dir| Action::OpenTerminal {
                        dir: dir.to_path_buf(),
                    })
            }
            "files.create-folder" if !archive_focused => Some(Action::CreateFolderFocused),
            "files.create-file" if !archive_focused => Some(Action::CreateFileFocused),
            "files.copy-path" if !archive_focused => {
                let paths = self.selected_paths();
                (!paths.is_empty()).then_some(Action::CopyPathsToClipboard(paths))
            }
            #[cfg(windows)]
            "files.paste-shortcut" if active_location.is_some_and(|location| location.fs_dir().is_some()) => Some(Action::PasteShortcutFocused),
            "files.properties" if !archive_focused => Some(Action::OpenPropertiesFocused),
            "files.trash" if !archive_focused => {
                let paths = self.selected_paths();
                (!paths.is_empty()).then_some(Action::Delete {
                    paths,
                    to_trash: true,
                })
            }
            "files.delete" if !archive_focused => {
                let paths = self.selected_paths();
                (!paths.is_empty()).then_some(Action::Delete {
                    paths,
                    to_trash: false,
                })
            }
            "files.compress" if !archive_focused => {
                let paths = self.selected_paths();
                (!paths.is_empty()).then_some(Action::CompressToZip(paths))
            }
            "files.open-dirs-in-tabs" if !archive_focused => {
                Some(Action::OpenSelectedDirsInNewTabs)
            }
            "files.open-in-window" if !archive_focused => Some(Action::OpenSelectedInNewWindow),
            "files.open-dir-right-split" if !archive_focused => {
                Some(Action::OpenSelectedDirInRightSplit)
            }
            "files.open-dir-bottom-split" if !archive_focused => {
                Some(Action::OpenSelectedDirInBottomSplit)
            }
            "recycle.restore" if recycle_focused => Some(Action::RestoreSelectedFromRecycleBin),
            "recycle.purge" if recycle_focused => Some(Action::PurgeSelectedFromRecycleBin),
            "recycle.empty" => Some(Action::EmptyRecycleBin),
            "profile.new" => Some(Action::CreateProfile(String::new())),
            "profile.rename" => Some(Action::OpenRenameProfile),
            "profile.delete" => {
                (self.state.profiles.profiles.len() > 1).then_some(Action::DeleteActiveProfile)
            }
            "ext.manager" => Some(Action::OpenExtManager),
            "sys.palette" => Some(Action::OpenPalette),
            "sys.settings" => Some(Action::OpenSettings),
            "sys.guide" => Some(Action::OpenGuide(0)),
            _ => None,
        }
    }

    fn command_shortcuts_blocked_by_app_state(&self) -> bool {
        let o = &self.state.overlay;
        o.show_goto_bar
            || self.palette_closing.is_some()
            || o.show_palette
            || o.show_ext_palette
            || o.show_settings
            || o.show_guide
            || o.show_analyzer
            || o.transfer_conflict.is_some()
            || o.renaming_profile
            || self.state.profile_ui.creating_tag
            || self.state.batch_rename.is_some()
            || self.rename_edit.is_some()
            || self.ctx_menu.is_some()
            || self.type_filter_menu.is_some()
            || self.breadcrumb_menu.is_some()
            || self.size_filter_menu.is_some()
            || self.tag_picker_menu.is_some()
            || self.state.ext.show_manager
            || {
                #[cfg(feature = "provider-host")]
                {
                    self.state.cloud.show_add_dialog
                        || self.state.cloud.renaming_remote_id.is_some()
                }
                #[cfg(not(feature = "provider-host"))]
                {
                    false
                }
            }
    }
}

pub(crate) fn is_vim_navigation_key(key: &str, shift: bool) -> bool {
    match key {
        "h" | "j" | "k" | "l" | "y" | "d" | "p" | "/" | "slash" | "enter" => true,
        "g" | "G" => true,
        _ => shift && key.eq_ignore_ascii_case("g"),
    }
}

fn reserved_static_shortcut(keystroke: &str) -> bool {
    let Ok(source) = Keystroke::parse(keystroke) else {
        return true;
    };
    RESERVED_STATIC_SHORTCUTS.iter().any(|reserved| {
        Keystroke::parse(reserved)
            .map(KeybindingKeystroke::from_keystroke)
            .is_ok_and(|target| source.should_match(&target))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_shortcut_authority_resolves_defaults_overrides_and_removed_bindings() {
        let mut prefs = CommandShortcutPrefs::default();
        assert_eq!(
            prefs.shortcut_string("files.copy").as_deref(),
            Some("ctrl-c")
        );
        prefs
            .bindings
            .insert("files.copy".into(), Some("ctrl-alt-c".into()));
        assert_eq!(
            prefs.shortcut_string("files.copy").as_deref(),
            Some("ctrl-alt-c")
        );
        prefs.bindings.insert("files.copy".into(), None);
        assert_eq!(prefs.shortcut_string("files.copy"), None);
        prefs
            .bindings
            .insert("files.copy".into(), Some("invalidmodifier-c".into()));
        assert_eq!(prefs.shortcut_string("files.copy"), None);
        assert!(!reserved_static_shortcut("ctrl-c"));
        assert!(!reserved_static_shortcut("ctrl-x"));
        assert!(!reserved_static_shortcut("ctrl-v"));
    }

    #[test]
    fn reserved_shortcuts_reject_static_input_bindings() {
        assert!(reserved_static_shortcut("ctrl-q"));
        assert!(reserved_static_shortcut("escape"));
    }

    #[test]
    fn non_reserved_shortcuts_are_allowed() {
        assert!(!reserved_static_shortcut("ctrl-alt-1"));
    }

    #[test]
    fn vim_navigation_keys_identified() {
        assert!(is_vim_navigation_key("h", false));
        assert!(is_vim_navigation_key("j", false));
        assert!(is_vim_navigation_key("k", false));
        assert!(is_vim_navigation_key("l", false));
        assert!(is_vim_navigation_key("y", false));
        assert!(is_vim_navigation_key("d", false));
        assert!(is_vim_navigation_key("p", false));
        assert!(is_vim_navigation_key("g", false));
        assert!(is_vim_navigation_key("G", false));
        assert!(is_vim_navigation_key("g", true));
        assert!(is_vim_navigation_key("/", false));
        assert!(is_vim_navigation_key("slash", false));
        assert!(is_vim_navigation_key("enter", false));

        assert!(!is_vim_navigation_key("a", false));
        assert!(!is_vim_navigation_key("b", false));
        assert!(!is_vim_navigation_key("x", false));
        assert!(!is_vim_navigation_key("z", false));
    }
}
