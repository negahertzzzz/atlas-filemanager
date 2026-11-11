//! Overlay focus arbiter — the single authority for "who owns the keyboard this
//! frame". Runs once per frame from `render_overlays`.
//!
//! # The recurring bug this solves
//! Secondary panels (modals, popovers, menus) need to capture keyboard input. The
//! arbiter reassigns focus every frame, so historically **any** new panel that
//! wasn't hand-wired here lost focus on the very next frame (its input died). It
//! also fought gpui-component's own overlays (`Dialog`/`Sheet`/`Popover`), which
//! manage focus themselves — the arbiter would yank focus back to the root.
//!
//! # The engine: two layers
//! 1. **Yield to gpui-component.** If a gpui-component `Dialog`/`Sheet` is active,
//!    the arbiter does nothing and lets the library own focus. This is why the
//!    **official way to add a modal/popover that captures input is a
//!    gpui-component `Dialog`/`Popover`** — it Just Works, no wiring here.
//! 2. **Claim resolver for the app's own overlays** ([`App::overlay_focus_target`]):
//!    a single, ordered place that maps app state → the focus handle that should
//!    own the keyboard. Adding a bespoke overlay = one arm here (not a branch
//!    scattered across the codebase). If no claim is active, focus returns to the
//!    root so pane keyboard-nav works.
//!
//! # Adding a new overlay
//! 1. Add a variant to [`OverlayClaim`].
//! 2. Add the state check in [`App::active_claim`] (detection, priority order).
//! 3. Add the focus-handle arm in [`App::overlay_focus_target`] (resolution).
//! That's it — `modal_overlay_active` and `sync_overlay_focus` update automatically.

use crate::app::{App, BatchRenameField};
use atlas_model::batch_rename::RenameMode;
use gpui::{Context, FocusHandle, Focusable, Window};
use gpui_component::input::{Position, SelectToStartOfLine};
use gpui_component::WindowExt;

/// Which of the app's bespoke overlays currently claims the keyboard.
///
/// This enum is the single source of truth shared by `modal_overlay_active` (pure
/// state check) and `overlay_focus_target` (focus-handle resolution). Adding a new
/// overlay requires only: one new variant + one arm in each of those two places.
#[derive(Debug, Clone, Copy)]
pub(crate) enum OverlayClaim {
    DiskCleanup,
    FolderCompare,
    Git,
    Vault,
    Converter,
    TagPicker,
    TypeFilter,
    BreadcrumbMenu,
    SizeFilter,
    CtxMenu,
    GotoBar,
    Commands,
    ExtPalette,
    ExtPrompt,
    ExtManager,
    Settings,
    BatchRename,
    RenamingProfile,
    GettingStarted,
    CreatingTag,
    #[cfg(feature = "provider-host")]
    CloudAdd,
    #[cfg(feature = "provider-host")]
    CloudRename,
    /// Covers show_settings, transfer_conflict and properties — all use a neutral focus handle.
    TextlessModal,
    InlineRename,
    SearchFilter,
}

impl App {
    /// First active overlay claim, in focus-priority order. Pure state read — no
    /// window or cx required. Used by both `modal_overlay_active` and
    /// `overlay_focus_target` so they can never desync.
    pub(crate) fn active_claim(&self) -> Option<OverlayClaim> {
        if self.vault.is_some() { return Some(OverlayClaim::Vault); }
        if self.cleanup_panel.is_some() {
            return Some(OverlayClaim::DiskCleanup);
        }
        if self.compare_panel.is_some() {
            return Some(OverlayClaim::FolderCompare);
        }
        if self.git_panel.is_some() {
            return Some(OverlayClaim::Git);
        }
        if self.converter.as_ref().is_some_and(|s| s.visible()) {
            return Some(OverlayClaim::Converter);
        }
        if self.tag_picker_menu.is_some() {
            return Some(OverlayClaim::TagPicker);
        }
        if self.type_filter_menu.is_some() {
            return Some(OverlayClaim::TypeFilter);
        }
        if self.breadcrumb_menu.is_some() {
            return Some(OverlayClaim::BreadcrumbMenu);
        }
        if self.size_filter_menu.is_some() {
            return Some(OverlayClaim::SizeFilter);
        }
        if self.ctx_menu.is_some() {
            return Some(OverlayClaim::CtxMenu);
        }
        if self.state.ext.pending.is_some() {
            return Some(OverlayClaim::ExtPrompt);
        }
        let o = &self.state.overlay;
        if o.show_goto_bar {
            return Some(OverlayClaim::GotoBar);
        }
        if o.show_palette {
            return Some(OverlayClaim::Commands);
        }
        if o.show_ext_palette {
            return Some(OverlayClaim::ExtPalette);
        }
        if self.state.ext.show_manager {
            return Some(OverlayClaim::ExtManager);
        }
        if o.show_settings || o.show_guide {
            return Some(OverlayClaim::Settings);
        }
        if self.state.batch_rename.is_some() && self.rename_edit.is_none() {
            return Some(OverlayClaim::BatchRename);
        }
        if o.renaming_profile {
            return Some(OverlayClaim::RenamingProfile);
        }
        if o.show_getting_started {
            return Some(OverlayClaim::GettingStarted);
        }
        if self.state.profile_ui.creating_tag {
            return Some(OverlayClaim::CreatingTag);
        }
        #[cfg(feature = "provider-host")]
        if self.state.cloud.show_add_dialog {
            return Some(OverlayClaim::CloudAdd);
        }
        #[cfg(feature = "provider-host")]
        if self.state.cloud.renaming_remote_id.is_some() {
            return Some(OverlayClaim::CloudRename);
        }
        if o.transfer_conflict.is_some() || o.properties.is_some() {
            return Some(OverlayClaim::TextlessModal);
        }
        if self.rename_edit.is_some() {
            return Some(OverlayClaim::InlineRename);
        }
        let tile = self.state.focused_tile;
        if self
            .state
            .tab_states
            .get(&tile)
            .is_some_and(|ts| ts.search.active)
        {
            return Some(OverlayClaim::SearchFilter);
        }
        None
    }

    /// THE single source of truth for "is a keyboard-owning overlay open?".
    ///
    /// Used by the window root to swap its key context from `Atlas` to `AtlasModal`.
    /// Derived from `active_claim` so it can never drift out of sync with the focus
    /// resolver. `palette_closing` is the only extra: it's modal (blocks pane nav)
    /// but needs no focus (the palette is animating out).
    pub(crate) fn modal_overlay_active(&self) -> bool {
        self.palette_closing.is_some() || self.active_claim().is_some()
    }

    /// Decide which focus handle should own the keyboard this frame and move focus
    /// to it (only when not already focused, to avoid fighting the cursor).
    pub(crate) fn sync_overlay_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.active_claim(),Some(OverlayClaim::DiskCleanup)) {
            if let Some(panel)=&self.cleanup_panel {
                if panel.read(cx).focus.contains_focused(window,cx) {return;}
            }
        }
        // The Git panel's text fields and menus hold focus inside the panel.
        if matches!(self.active_claim(), Some(OverlayClaim::Git)) {
            if let Some(panel) = &self.git_panel {
                if panel.read(cx).focus.contains_focused(window, cx) {
                    return;
                }
            }
        }
        if self.vault.is_some() {
            let focus = self.overlay_focus(cx);
            if focus.contains_focused(window, cx) { return; }
        }
        // Layer 1 — yield to gpui-component overlays (Dialog/Sheet manage focus themselves).
        if window.has_active_dialog(cx) || window.has_active_sheet(cx) {
            return;
        }
        // Layer 2 — resolve the app's own overlay claims.
        let target = self.overlay_focus_target(window, cx);
        if let Some(handle) = target {
            if !handle.is_focused(window) {
                window.focus(&handle, cx);
            }
        } else {
            let root = self.focus_handle.clone();
            if !root.is_focused(window) {
                window.focus(&root, cx);
            }
        }
    }

    /// Resolve the active claim to a concrete focus handle. Driven by `active_claim`
    /// so detection and resolution always agree on which overlay is "the active one".
    fn overlay_focus_target(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<FocusHandle> {
        use crate::ui::palette::PaletteKind;
        match self.active_claim()? {
            OverlayClaim::DiskCleanup => self.cleanup_panel.as_ref().map(|p|p.read(cx).focus.clone()),
            OverlayClaim::FolderCompare => self.compare_panel.as_ref().map(|p| p.read(cx).focus.clone()),
            OverlayClaim::Git => self.git_panel.as_ref().map(|p| p.read(cx).focus.clone()),
            OverlayClaim::Vault => Some(self.vault_focus(window, cx)),
            OverlayClaim::Converter => Some(self.overlay_focus(cx)),
            OverlayClaim::TagPicker => self
                .tag_picker_input
                .as_ref()
                .map(|i| i.read(cx).focus_handle(cx)),
            OverlayClaim::TypeFilter => self
                .type_filter_input
                .as_ref()
                .map(|i| i.read(cx).focus_handle(cx)),
            OverlayClaim::BreadcrumbMenu => self
                .breadcrumb_menu_input
                .as_ref()
                .map(|i| i.read(cx).focus_handle(cx)),
            OverlayClaim::SizeFilter => Some(self.overlay_focus(cx)),
            OverlayClaim::CtxMenu => self.ctx_menu_focus(cx),
            OverlayClaim::GotoBar => {
                let input = self.palette_input_for(PaletteKind::Goto, window, cx);
                Some(input.read(cx).focus_handle(cx))
            }
            OverlayClaim::Commands => {
                let input = self.palette_input_for(PaletteKind::Commands, window, cx);
                Some(input.read(cx).focus_handle(cx))
            }
            OverlayClaim::ExtPalette => {
                let input = self.palette_input_for(PaletteKind::Ext, window, cx);
                Some(input.read(cx).focus_handle(cx))
            }
            OverlayClaim::ExtPrompt => {
                self.ensure_ext_prompt_inputs(window, cx);
                // Keep the field the user clicked; default to the first one.
                let handles: Vec<FocusHandle> = self
                    .ext_prompt_inputs
                    .iter()
                    .map(|i| i.read(cx).focus_handle(cx))
                    .collect();
                handles
                    .iter()
                    .find(|h| h.is_focused(window))
                    .or_else(|| handles.first())
                    .cloned()
                    .or_else(|| Some(self.overlay_focus(cx)))
            }
            OverlayClaim::ExtManager => Some(self.ext_manager_focus_handle(window, cx)),
            OverlayClaim::Settings => Some(self.settings_focus_handle(window, cx)),
            OverlayClaim::BatchRename => Some(self.batch_focus_handle(window, cx)),
            OverlayClaim::RenamingProfile => Some(self.rename_profile_focus(window, cx)),
            OverlayClaim::GettingStarted => {
                if self.state.overlay.getting_started_step == 0 {
                    Some(self.getting_started_profile_focus(window, cx))
                } else {
                    Some(self.overlay_focus(cx))
                }
            }
            OverlayClaim::CreatingTag => Some(self.create_tag_focus(window, cx)),
            #[cfg(feature = "provider-host")]
            OverlayClaim::CloudAdd => Some(self.cloud_add_focus(window, cx)),
            #[cfg(feature = "provider-host")]
            OverlayClaim::CloudRename => Some(self.rename_remote_focus(window, cx)),
            OverlayClaim::TextlessModal => Some(self.overlay_focus(cx)),
            OverlayClaim::InlineRename => {
                // Cursor-positioning side-effect fires once (initial_select_chars is taken).
                let pending = self.rename_edit.as_mut().and_then(|edit| {
                    edit.initial_select_chars
                        .take()
                        .map(|chars| (edit.input.clone(), chars))
                });
                if let Some((input, chars)) = pending {
                    input.update(cx, |state, cx| {
                        state.set_cursor_position(Position::new(0, chars), window, cx);
                        state.select_leading(chars, cx);
                    });
                }
                self.rename_edit
                    .as_ref()
                    .map(|edit| edit.input.read(cx).focus_handle(cx))
            }
            OverlayClaim::SearchFilter => {
                let tile = self.state.focused_tile;
                self.search_inputs
                    .get(&tile)
                    .map(|input| input.read(cx).focus_handle(cx))
            }
        }
    }

    /// Ensure both batch-rename inputs exist (pre-filled from state) and return the
    /// focus handle of whichever field is currently active for the selected mode.
    pub(crate) fn batch_focus_handle(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        let (pat0, rep0) = self
            .state
            .batch_rename
            .as_ref()
            .map(|b| (b.pattern.clone(), b.replacement.clone()))
            .unwrap_or_default();
        if self.batch_pattern_input.is_none() {
            self.batch_pattern_input =
                Some(crate::ui::text_input::single_line(window, cx, "", pat0));
        }
        if self.batch_replacement_input.is_none() {
            self.batch_replacement_input =
                Some(crate::ui::text_input::single_line(window, cx, "", rep0));
        }
        let mode = self.state.batch_rename.as_ref().map(|b| b.mode.clone());
        if matches!(mode, Some(RenameMode::MultiCursor)) {
            return self.overlay_focus(cx);
        }
        let use_replacement = matches!(mode, Some(RenameMode::Number))
            || (matches!(mode, Some(RenameMode::Find))
                && self.batch_rename_field == BatchRenameField::Replacement);
        let input = if use_replacement {
            self.batch_replacement_input.clone()
        } else {
            self.batch_pattern_input.clone()
        };
        if let Some(i) = input {
            i.read(cx).focus_handle(cx)
        } else {
            self.overlay_focus(cx)
        }
    }

    /// A neutral focus handle for text-less modals (so they own the keyboard for
    /// Esc/button handling without an input field stealing it).
    pub(crate) fn overlay_focus(&mut self, cx: &mut Context<Self>) -> FocusHandle {
        self.overlay_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Ensure the extension-manager search input exists and return its focus handle.
    pub(crate) fn ext_manager_focus_handle(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        if self.ext_manager_input.is_none() {
            self.ext_manager_input = Some(crate::ui::text_input::single_line(
                window,
                cx,
                rust_i18n::t!("ext_manager_search").as_ref(),
                "",
            ));
        }
        if let Some(input) = &self.ext_manager_input {
            input.read(cx).focus_handle(cx)
        } else {
            self.overlay_focus(cx)
        }
    }

    pub(crate) fn settings_focus_handle(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        // Recording a shortcut: keys must reach the root capture handler, not
        // be typed into (or resolved as actions by) the search field.
        if self.command_shortcut_capture.is_some() {
            return self.overlay_focus(cx);
        }
        // An open choice dropdown owns the keyboard (its own search field).
        if let Some(input) = self.settings_ui.drill.as_ref().and_then(|d| d.input.as_ref()) {
            return input.read(cx).focus_handle(cx);
        }
        if self.settings_input.is_none() {
            self.settings_input = Some(crate::ui::text_input::single_line(
                window,
                cx,
                rust_i18n::t!("sp_search_placeholder"),
                "",
            ));
        }
        if let Some(input) = &self.settings_input {
            input.read(cx).focus_handle(cx)
        } else {
            self.overlay_focus(cx)
        }
    }
}
