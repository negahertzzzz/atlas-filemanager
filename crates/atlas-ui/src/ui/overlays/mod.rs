//! Phase5 — Overlays as gpui layers (port of the egui overlay modules).
//!
//! Returned as extra absolutely-positioned children of the root `v_flex`, painted
//! above everything via `deferred()` (so they sit over the panes/inspector). Each
//! overlay is a full-window backdrop (click-to-close) plus a centered panel.
//!
//! The fuzzy palettes (goto bar, command palette, ext palette) are built on the
//! unified engine in `ui::palette` and hosted in `ui::palette_host`; this module
//! keeps their thin per-frame entry points plus the non-fuzzy modals:
//!   - `paste_conflict` — overwrite/skip/rename/cancel modal.
//!   - settings — rendered by `ui::settings_palette`.
//!   - `batch_rename` — mode, text fields, preview, apply.
//!   - `create_tag`, `op_errors` toasts; context menu lives in `ui/root.rs`.

use crate::app::App;
use gpui::prelude::*;
use gpui::{AnyElement, Context, Window};

impl App {
    /// Build the overlay layers for the current frame. Empty when nothing is open.
    pub(crate) fn render_overlays(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut layers: Vec<AnyElement> = Vec::new();

        // Route keyboard focus: text overlays focus their native `InputState` (so
        // editing + clipboard work), modals without text use the neutral overlay
        // focus handle, and an active inline rename focuses its row input.
        self.sync_overlay_focus(window, cx);

        if self.converter.is_some() {
            layers.push(self.render_converter(window, cx));
        }

        // Live previews belong to the open settings picker; any other way of
        // closing Settings (Esc elsewhere, CloseOverlays, links) reverts them.
        let show_settings = self.state.overlay.show_settings;
        if !show_settings && self.settings_ui.drill.is_some() {
            self.settings_cancel_drill();
        }
        let drilling = self
            .settings_ui
            .drill
            .as_ref()
            .filter(|_| show_settings)
            .map(|d| d.setting);
        let mut reverted = false;
        if self.settings_theme_previewing && drilling != Some("theme") {
            self.settings_theme_previewing = false;
            self.restore_theme_preview();
            reverted = true;
        }
        if self.view_preview_before.is_some() && drilling != Some("default-view") {
            self.restore_pane_view_preview();
            reverted = true;
        }
        if self.icon_style_preview_before.is_some() && drilling != Some("icon-style") {
            self.restore_icon_style_preview();
            reverted = true;
        }
        if reverted {
            self.notify_all_panes(cx);
            cx.notify();
        }
        if self.state.overlay.show_settings {
            layers.push(self.render_settings(window, cx));
        }
        if self.state.overlay.show_guide {
            layers.push(self.render_guide(cx));
        }
        if self.state.overlay.guide_coach.is_some() {
            layers.push(self.render_guide_coach(cx));
        }
        if self.state.overlay.show_getting_started {
            layers.push(self.render_getting_started(window, cx));
        }
        if self.state.overlay.getting_started_welcome_since.is_some() {
            layers.push(self.render_getting_started_welcome(window, cx));
        }
        if self.state.ext.show_manager {
            layers.push(self.render_ext_manager(window, cx));
        }

        // Each palette is hosted in its own view so the open animation only
        // re-renders the overlay, not App + every file pane. The goto is kept
        // mounted while `palette_closing` is set so the exit animation can play.
        // Palettes and prompt dialogs render above full-screen sheets like ExtManager.
        use crate::ui::palette::PaletteKind;
        let goto_closing = matches!(self.palette_closing, Some((PaletteKind::Goto, _)));
        if self.state.overlay.show_goto_bar || goto_closing {
            layers.push(
                self.ensure_palette_host(PaletteKind::Goto, window, cx)
                    .into_any_element(),
            );
        }
        if self.state.overlay.show_palette {
            layers.push(
                self.ensure_palette_host(PaletteKind::Commands, window, cx)
                    .into_any_element(),
            );
        }
        if self.state.overlay.show_ext_palette {
            layers.push(
                self.ensure_palette_host(PaletteKind::Ext, window, cx)
                    .into_any_element(),
            );
        }
        if self.state.ext.pending.is_some() {
            layers.push(self.render_ext_prompt(window, cx));
        }
        if self.state.overlay.transfer_conflict.is_some() {
            layers.push(self.render_transfer_conflict(cx));
        }
        if self.state.overlay.properties.is_some() {
            layers.push(self.render_properties(window, cx));
        }
        if self.state.profile_ui.creating_tag {
            layers.push(self.render_create_tag(window, cx));
        }
        if self.state.batch_rename.is_some() {
            layers.push(self.render_batch_rename(window, cx));
        }
        if self.state.overlay.renaming_profile {
            layers.push(self.render_rename_profile(window, cx));
        }
        #[cfg(feature = "provider-host")]
        if self.state.cloud.show_add_dialog {
            layers.push(self.render_add_cloud(window, cx));
        }
        #[cfg(feature = "provider-host")]
        if self.state.cloud.renaming_remote_id.is_some() {
            layers.push(self.render_rename_remote(window, cx));
        }
        let has_local_transfers = !self.state.ops.active_ops.is_empty();
        #[cfg(feature = "provider-host")]
        let has_visible_cloud_transfers =
            self.state.cloud.show_transfers && !self.state.cloud.transfers.is_empty();
        #[cfg(not(feature = "provider-host"))]
        let has_visible_cloud_transfers = false;
        if has_local_transfers || has_visible_cloud_transfers {
            layers.push(self.render_transfer_center(window, cx));
        }
        if !self.state.overlay.op_errors.is_empty() || !self.toast_stack.is_empty() {
            layers.push(self.render_op_errors(window, cx));
        }
        // Details type-filter overlay. Painted below the right-click menu.
        if self.type_filter_menu.is_some() {
            layers.push(self.render_type_filter_layer(window, cx));
        }
        if self.breadcrumb_menu.is_some() {
            layers.push(self.render_breadcrumb_menu_layer(window, cx));
        }
        if self.size_filter_menu.is_some() {
            layers.push(self.render_size_filter_layer(window, cx));
        }
        // Tag-picker overlay (inspector add-tag button).
        if self.tag_picker_menu.is_some() {
            layers.push(self.render_tag_picker_layer(window, cx));
        }
        // Right-click menu (Atlas items + native shell items). Painted last so it
        // sits above the other overlays; it owns the keyboard via the branch in
        // `sync_overlay_focus` above.
        if self.ctx_menu.is_some() {
            layers.push(self.render_ctx_menu_layer(window, cx));
        }
        if self.cleanup_panel.is_some() {
            layers.push(self.render_disk_cleanup(cx));
        }
        self.mount_pending_compare(cx);
        if self.compare_panel.is_some() {
            layers.push(self.render_folder_compare(cx));
        }
        self.mount_pending_git(window, cx);
        if self.git_panel.is_some() {
            layers.push(self.render_git_panel(cx));
        }
        if self.vault.is_some() {
            layers.push(self.render_vault(window, cx));
        }
        layers
    }

    // Overlay focus arbiter (`sync_overlay_focus`, `batch_focus_handle`,
    // `overlay_focus`) lives in `ui/overlay_focus.rs`.

    // ── palettes (unified engine in `ui::palette`) ───────────────────────────

    /// Dispatch from `PaletteHost` to the right palette layer for this frame.
    pub(crate) fn render_palette_layer(
        &mut self,
        kind: crate::ui::palette::PaletteKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match kind {
            crate::ui::palette::PaletteKind::Goto => self.render_goto_bar(window, cx),
            crate::ui::palette::PaletteKind::Commands => self.render_command_palette(window, cx),
            crate::ui::palette::PaletteKind::Ext => self.render_ext_palette(window, cx),
        }
    }

    fn render_goto_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let kind = crate::ui::palette::PaletteKind::Goto;
        let input = self.palette_input_for(kind, window, cx);
        let changed = self.sync_palette_query(kind, &input, cx);
        // In content mode, re-run the background grep whenever the query changes.
        if changed && self.state.profiles.active().search_in_content {
            self.relaunch_content_search(cx);
        }
        let spec = self.goto_spec(cx);
        self.palette_panel(spec, &input, window, cx)
    }

    // ── command palette ──────────────────────────────────────────────────────

    fn render_command_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let kind = crate::ui::palette::PaletteKind::Commands;
        let input = self.palette_input_for(kind, window, cx);
        self.sync_palette_query(kind, &input, cx);
        let spec = self.command_palette_spec(cx);
        self.palette_panel(spec, &input, window, cx)
    }

    fn render_ext_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let kind = crate::ui::palette::PaletteKind::Ext;
        let input = self.palette_input_for(kind, window, cx);
        self.sync_palette_query(kind, &input, cx);
        let spec = self.ext_palette_spec();
        self.palette_panel(spec, &input, window, cx)
    }

    // ── transfer conflict ──────────────────────────────────────────────────────
}

#[cfg(feature = "provider-host")]
pub(crate) mod add_cloud;
mod batch_rename;
mod brand;
mod create_tag;
mod ext_manager;
mod ext_prompt;
mod getting_started;
mod guide;
pub(crate) mod legal;
mod properties;
mod rename_profile;
#[cfg(feature = "provider-host")]
mod rename_remote;
mod scrim;
pub(crate) mod toasts;
mod transfer_conflict;
mod transfers_bar;
mod vault;
