//! gpui UI layer.

pub const RADIUS: f32 = 6.0;
pub const SIDE_PANEL_VERTICAL_INSET: f32 = 8.0;

/// Base rem size in px — matches the zoom baseline set in `app/theme.rs`
/// (`Theme.font_size = BASE_PX * global_zoom`).
pub const BASE_PX: f32 = 16.0;

/// Zoom-aware replacement for `px(n)` in any `Styled` call
/// (`.w`/`.h`/`.gap`/`.p`/`.text_size`/`.size`/`.min_w`/`.max_w`/...).
/// Scales with the window's rem_size the same way gpui's own `.p_1()`/`.gap_2()`
/// shorthand already does, so it composes with the app's global zoom.
pub fn rem_px(n: f32) -> gpui::Rems {
    gpui::rems(n / BASE_PX)
}

/// Same idea, but resolved to raw `Pixels` for `canvas()` paint closures, where
/// gpui's length system (`Rems`/`DefiniteLength`) doesn't apply.
pub fn rem_px_abs(n: f32, window: &gpui::Window) -> gpui::Pixels {
    gpui::px(n * f32::from(window.rem_size()) / BASE_PX)
}

pub mod breadcrumb_menu;
pub mod chrome;
pub mod chrome_dg;
pub mod cleanup;
pub(crate) mod compare;
pub(crate) mod converter;
pub(crate) mod current_bounds;
pub mod dg;
pub(crate) mod dir_ring;
pub mod dock;
pub mod dock_host;
pub mod drag;
pub mod drag_hover;
pub mod edge_autoscroll;
pub(crate) mod file_filter;
pub mod file_icons;
pub(crate) mod git_panel;
pub mod glass;
pub mod icons;
pub mod inspector;
pub(crate) mod preview_delay;
pub(crate) mod preview_image;
#[cfg(feature = "ui-lab")]
pub(crate) mod preview_lab;
// Historical layout fixtures; the application uses the retained preview.
pub(crate) mod click_navigation;
pub(crate) mod group_header;
#[cfg(test)]
mod inspector_dg;
pub mod interactions;
pub mod layout_region;
pub(crate) mod loading_spinner;
pub mod motion;
pub(crate) mod operation_feedback;
pub mod overlay_focus;
pub mod overlays;
pub mod palette;
pub mod palette_host;
pub mod pane_panel;
pub(crate) mod reveal;
pub mod root;
pub mod search_shell;
pub mod selection_checkbox;
pub(crate) mod settings_palette;
pub mod shell_context_menu;
pub mod sidebar;
pub(crate) mod sidebar_bookmarks;
pub(crate) mod sidebar_collection;
pub(crate) mod sidebar_feedback;
pub(crate) mod sidebar_ink;
// Historical layout fixture; production uses the composed sidebar + RowInk.
#[cfg(test)]
mod sidebar_dg;
pub mod size_filter_menu;
pub mod surface;
pub mod syntax_provider;
pub(crate) mod tab_reorder;
pub mod tag_picker_menu;
pub mod text_input;
pub(crate) mod theme_editor;
pub(crate) mod thumbnail_cache;
pub mod thumbs;
pub mod tooltips;
pub mod type_filter_menu;
pub(crate) mod view_continuity;
pub mod views;
pub mod window_chrome;
