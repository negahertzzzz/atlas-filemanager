//! Digital Grove Miller Columns View Element (`MillerDgElement`).
//!
//! Architectural principles (Digital Grove / Pexer / Raddebugger):
//! 1. Stateless procedural generation: implements `gpui::Element`, `IntoElement`, `Styled`.
//! 2. Single-cycle O(1) arithmetic layout with `RectCut`:
//!    - Columns: horizontal cascade with `MILLER_COL_W` (~250px) width, separator borders.
//!    - Column Header: Title (`display_name`), item count, load status (`LoadState::Loading`, `Error`, `Empty`).
//!    - Rows per column: Height `MILLER_ROW_H` (~26px):
//!      * Multi-selection checkbox slot (visible on hover or multi-select).
//!      * File/Folder icon slot (requests icon via `maybe_request_file_icon`, paints shell/theme icon).
//!      * File name label with binary-search UTF-8 truncation (`shape_text_truncated`).
//!      * Directory caret indicator (`AtlasIcon::CaretRight`) on the right side for folders.
//! 3. Hardware GPU clipping layer:
//!    - Column, body, header and filename masks intersect the pane viewport.
//!    - Stacking layers alone do not clip text or scrollbars.
//! 4. Total coverage of interactions:
//!    - Single-click: Folder dispatches `Action::MillerSelect { tile, column, idx, shift: false, ctrl: false }`; file selects for preview.
//!    - Double-click: Activates file or enters folder (`Action::MillerActivate`).
//!    - Checkbox click: Toggles multi-selection (`Action::MillerToggleSelect`).
//!    - Right-click context menu: Dispatches `this.open_ctx_menu(...)`.
//!    - Tooltips on hover: Registers or shows `entry_hover_tooltip`.
//!    - Drag & Drop: Drag source with `begin_drag_hover`, drop targets on visible directory rows (`DragHoverTarget` with `DragHoverId::Folder(pane_id, path)`), visual blinking highlight when `app.drag_hover.blink_id` matches.
//!    - Keyboard navigation: Left arrow (parent column), Right arrow (child column), Up/Down arrows (change selection).
//!    - Inline rename input integration (`active_input_for_path`).

use super::should_suppress_loading;
use crate::app::{App, RubberTarget};
use crate::input::action::Action;
use crate::ui::dg::cut::RectCut;
use crate::ui::drag_hover::{
    DragHoverCommand, DragHoverDropCommand, DragHoverId, DragHoverKind, DragHoverTarget,
};
use crate::ui::file_icons::FileIcon;
use crate::ui::glass::backdrop_tint;
use crate::ui::icons::{is_windows_theme, AtlasIcon};
use crate::ui::root::{ctrl_scroll, pane_bounds_recorder, zone_recorder};
use crate::ui::selection_checkbox;
use crate::ui::surface::{
    line_width, paint_fill, paint_rounded, paint_theme_icon, shape_text, shape_text_truncated,
    text_origin_y, Slot,
};
use atlas_core::util::display::display_name;
use atlas_model::clamp::ClampLenient;
use atlas_model::file_entry::{FileEntry, FileKind};
use atlas_model::pane::TabId;
use atlas_model::tab::LoadState;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, point, px, size, AnyElement, App as GpuiApp, Bounds, Context, Corners, CursorStyle,
    DispatchPhase, Element, ElementId, Font, GlobalElementId, Hitbox, HitboxBehavior, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels,
    Refineable, ScrollWheelEvent, ShapedLine, SharedString, Style, StyleRefinement, Styled,
    TextAlign, WeakEntity, Window,
};
use gpui_component::{v_flex, ActiveTheme, ElementExt};
use std::path::PathBuf;

pub const MILLER_COL_W: f32 = 250.0;
pub const MILLER_HEADER_H: f32 = 28.0;
pub const MILLER_ROW_H: f32 = 26.0;
pub const MILLER_CHECKBOX_SZ: f32 = 15.0;
pub const MILLER_ICON_SZ: f32 = 14.0;
pub const MILLER_CARET_SZ: f32 = 12.0;
pub const MILLER_PAD_X: f32 = 8.0;
pub const MILLER_PAD_Y: f32 = 2.0;

fn with_miller_clip(window: &mut Window, bounds: Bounds<Pixels>, draw: impl FnOnce(&mut Window)) {
    let bounds = bounds.intersect(&window.content_mask().bounds);
    if bounds.is_empty() {
        return;
    }
    window.with_content_mask(
        Some(gpui::ContentMask {
            bounds,
            fade_out: Default::default(),
        }),
        |window| window.paint_layer(bounds, draw),
    );
}

/// Arithmetic layout slots for a Miller column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MillerColumnSlots {
    pub col_slot: Slot,
    pub header_slot: Slot,
    pub body_slot: Slot,
    pub sep_slot: Slot,
}

/// Slices a column into Header, Body, and right Separator using `RectCut`.
pub fn miller_column_cut(
    col_x: f32,
    col_y: f32,
    col_w: f32,
    col_h: f32,
    header_h: f32,
) -> MillerColumnSlots {
    let mut cut = RectCut::new(col_x, col_y, col_w, col_h);
    let sep_slot = Slot::new(col_x + (col_w - 1.0).max(0.0), col_y, 1.0, col_h);
    let header_slot = cut.cut_top(header_h);
    let body_slot = cut.remaining();
    MillerColumnSlots {
        col_slot: Slot::new(col_x, col_y, col_w, col_h),
        header_slot,
        body_slot,
        sep_slot,
    }
}

/// Arithmetic layout slots for a Miller column header.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MillerHeaderSlots {
    pub title_slot: Slot,
    pub status_slot: Slot,
}

/// Slices a column header into title and load/item count status using `RectCut`.
pub fn miller_header_cut(slot: Slot, status_w: f32, zoom: f32) -> MillerHeaderSlots {
    let mut cut = RectCut::new(slot.x, slot.y, slot.w, slot.h);
    cut.pad(8.0 * zoom, 2.0 * zoom);
    let status_slot = cut.cut_right(status_w);
    let _ = cut.cut_right(4.0 * zoom);
    let title_slot = cut.remaining();
    MillerHeaderSlots {
        title_slot,
        status_slot,
    }
}

/// Arithmetic layout slots for a row inside a Miller column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MillerRowSlots {
    pub row_slot: Slot,
    pub check_slot: Option<Slot>,
    pub icon_slot: Slot,
    pub name_slot: Slot,
    pub caret_slot: Option<Slot>,
}

/// Single-cycle O(1) arithmetic layout calculation for a Miller row using `RectCut`.
pub fn miller_row_cut(
    row_x: f32,
    row_y: f32,
    row_w: f32,
    row_h: f32,
    zoom: f32,
    is_dir: bool,
    show_checkbox: bool,
) -> MillerRowSlots {
    let mut cut = RectCut::new(row_x, row_y, row_w, row_h);
    cut.pad(MILLER_PAD_X * zoom, MILLER_PAD_Y * zoom);

    let caret_slot = if is_dir {
        let caret = cut.cut_right(MILLER_CARET_SZ * zoom);
        let _ = cut.cut_right(4.0 * zoom);
        Some(caret)
    } else {
        None
    };

    let check_slot = if show_checkbox {
        let chk = cut.cut_left(MILLER_CHECKBOX_SZ * zoom);
        let _ = cut.cut_left(4.0 * zoom);
        Some(chk)
    } else {
        None
    };

    let icon_slot = cut.cut_left(MILLER_ICON_SZ * zoom);
    let _ = cut.cut_left(6.0 * zoom);
    let name_slot = cut.remaining();

    MillerRowSlots {
        row_slot: Slot::new(row_x, row_y, row_w, row_h),
        check_slot,
        icon_slot,
        name_slot,
        caret_slot,
    }
}

/// During a multi-selection the checkbox occupies the icon slot (same rule as
/// Details/List/Tree), so names never shift; clicking it still toggles the row.
fn miller_checkbox_in_icon(mut slots: MillerRowSlots, show: bool) -> MillerRowSlots {
    slots.check_slot = show.then_some(slots.icon_slot);
    slots
}

/// Computes visible row indices range given vertical scroll and viewport height.
#[inline]
pub fn miller_visible_row_range(
    scroll_y: f32,
    viewport_h: f32,
    row_h: f32,
    total_count: usize,
) -> (usize, usize) {
    if total_count == 0 || row_h <= 0.0 {
        return (0, 0);
    }
    let start_row = (scroll_y / row_h).floor().max(0.0) as usize;
    let end_row = (((scroll_y + viewport_h) / row_h).ceil() as usize + 2).min(total_count);
    let start = start_row.min(total_count);
    let end = end_row.max(start);
    (start, end)
}

#[derive(Clone, Copy, Debug)]
pub struct MillerDgColors {
    pub fg: Hsla,
    pub accent: Hsla,
    pub accent_fg: Hsla,
    pub hover: Hsla,
    pub muted: Hsla,
    pub border: Hsla,
    pub header_bg: Hsla,
}

impl MillerDgColors {
    pub fn from_theme(cx: &gpui::App, is_glass: bool) -> Self {
        let t = cx.theme();
        Self {
            fg: t.foreground,
            accent: t.accent,
            accent_fg: t.accent_foreground,
            hover: t.list_active,
            muted: t.muted_foreground,
            border: t.border,
            header_bg: if is_glass {
                t.list_head
            } else {
                backdrop_tint(t.secondary, 0.18)
            },
        }
    }
}

#[derive(Clone)]
pub struct MillerPreparedRow {
    pub opacity: f32,
    pub interactive: bool,
    pub col_idx: usize,
    pub row_idx: usize,
    pub slots: MillerRowSlots,
    pub entry: FileEntry,
    pub name_line: ShapedLine,
    pub icon: FileIcon,
    pub is_dir: bool,
    pub selected: bool,
    pub selected_at: Option<std::time::Instant>,
    pub cursor: bool,
    pub hovered: bool,
    pub blinking: bool,
    pub dimmed: bool,
    pub editing: bool,
}

#[derive(Clone)]
pub struct MillerPreparedColumn {
    pub identity: String,
    pub col_idx: usize,
    pub slots: MillerColumnSlots,
    pub title: String,
    pub title_line: ShapedLine,
    pub header_slots: MillerHeaderSlots,
    pub status_line: Option<ShapedLine>,
    pub status_msg: Option<String>,
    pub rows: Vec<MillerPreparedRow>,
    pub content_h: f32,
    pub scroll_y: f32,
    pub max_scroll_y: f32,
    pub is_flashing: bool,
    pub col_path: Option<PathBuf>,
    pub opacity: f32,
    pub interactive: bool,
}

pub struct MillerDgPrepaint {
    pub hitbox: Hitbox,
    pub columns: Vec<MillerPreparedColumn>,
}

pub struct MillerDgElement {
    id: ElementId,
    pane_id: TabId,
    app: WeakEntity<App>,
    col_w: f32,
    header_h: f32,
    row_h: f32,
    zoom: f32,
    colors: MillerDgColors,
    font: Font,
    font_size: Pixels,
    header_font_size: Pixels,
    windows_theme: bool,
    style: StyleRefinement,
}

impl MillerDgElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pane_id: TabId,
        app: WeakEntity<App>,
        col_w: f32,
        header_h: f32,
        row_h: f32,
        zoom: f32,
        colors: MillerDgColors,
        font: Font,
        font_size: Pixels,
        header_font_size: Pixels,
        windows_theme: bool,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from(format!("miller-dg-{}", pane_id.0))),
            pane_id,
            app,
            col_w,
            header_h,
            row_h,
            zoom,
            colors,
            font,
            font_size,
            header_font_size,
            windows_theme,
            style: StyleRefinement::default(),
        }
    }
}

impl IntoElement for MillerDgElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for MillerDgElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Element for MillerDgElement {
    type RequestLayoutState = Style;
    type PrepaintState = MillerDgPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) -> (LayoutId, Self::RequestLayoutState) {
        crate::perf::count("miller_dg.layout");
        let mut style = Style::default();
        style.refine(&self.style);
        style.size.width =
            gpui::Length::Definite(gpui::DefiniteLength::Relative(gpui::relative(1.0)));
        style.size.height =
            gpui::Length::Definite(gpui::DefiniteLength::Relative(gpui::relative(1.0)));
        (window.request_layout(style.clone(), [], cx), style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) -> Self::PrepaintState {
        crate::perf::count("miller_dg.prepaint");
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return MillerDgPrepaint {
                hitbox,
                columns: Vec::new(),
            };
        }

        let Some(app_entity) = self.app.upgrade() else {
            return MillerDgPrepaint {
                hitbox,
                columns: Vec::new(),
            };
        };

        let pane_id = self.pane_id;
        let col_w = self.col_w;
        let header_h = self.header_h;
        let row_h = self.row_h;
        let zoom = self.zoom;
        let colors = self.colors;
        let font = self.font.clone();
        let font_size = self.font_size;
        let header_font_size = self.header_font_size;
        let col_h = f32::from(bounds.size.height);
        let content_mask = window.content_mask().bounds;
        let has_gpui_drag = cx.has_active_drag();

        let mut prepaint_cols = app_entity.update(cx, |app, cx| {
            let is_drag_active = app.is_drag_active(has_gpui_drag);
            // Purge stale targets for this pane
            app.pending_drag_hover_targets.retain(|t| {
                !matches!(t.id, DragHoverId::Folder(tab, _) | DragHoverId::Column(tab, _) if tab == pane_id)
            });

            // Clear row rects for this pane
            app.miller_row_rects
                .borrow_mut()
                .retain(|(pane, _), _| *pane != pane_id);
            app.miller_column_bounds
                .retain(|(pane, _), _| *pane != pane_id);

            let Some((miller_columns, filter, show_files, show_folders)) = app
                .state
                .tab_states
                .get(&pane_id)
                .map(|ts| (ts.miller.columns.clone(), ts.filter.clone(), ts.show_files, ts.show_folders))
            else {
                return Vec::new();
            };

            let column_count = miller_columns.len();
            let mut prepaint_columns = Vec::with_capacity(column_count);

            for ci in 0..column_count {
                let col = &miller_columns[ci];
                let visible = col.visible_indices(&filter, show_files, show_folders);
                let col_x = ci as f32 * col_w;
                let col_slots = miller_column_cut(col_x, 0.0, col_w, col_h, header_h);
                let col_bounds = Bounds {
                    origin: point(bounds.origin.x + px(col_x), bounds.origin.y),
                    size: size(px(col_w), bounds.size.height),
                };

                // Record column bounds
                app.miller_column_bounds.insert((pane_id, ci), col_bounds);

                let col_loc = col.location.fs_dir().map(|p| p.to_path_buf());
                let is_col_flashing = col_loc.as_ref().is_some_and(|path| {
                    app.drag_hover.blink_id.as_ref()
                        == Some(&DragHoverId::Folder(pane_id, path.clone()))
                });

                if let Some(col_path) = &col_loc {
                    if is_drag_active {
                        let c_bounds = col_bounds.intersect(&bounds).intersect(&content_mask);
                        app.register_drag_hover_target(DragHoverTarget {
                            id: DragHoverId::Column(pane_id, col_path.clone()),
                            bounds: c_bounds,
                            kind: DragHoverKind::Folder,
                            command: DragHoverCommand::NavigatePathIn(pane_id, col_path.clone()),
                            drop_command: Some(DragHoverDropCommand::TransferTo(col_path.clone())),
                        });
                    }
                }

                let title = display_name(&col.location.display_path()).to_string();
                let status_msg = if visible.is_empty() {
                    match &col.load_state {
                        LoadState::Loading if should_suppress_loading(&col.location) => None,
                        LoadState::Loading => Some(rust_i18n::t!("miller_loading").to_string()),
                        LoadState::Error(error) if error.is_empty() => {
                            Some(rust_i18n::t!("miller_error").to_string())
                        }
                        LoadState::Error(error) => Some(error.clone()),
                        _ if col.has_excluded_items || !col.entries.is_empty() => Some(rust_i18n::t!("view_all_items_hidden").to_string()),
                        _ => Some(rust_i18n::t!("miller_empty").to_string()),
                    }
                } else {
                    None
                };

                // Long errors and empty-state messages belong in the body.
                let status_text = format!("{}/{}", visible.len(), col.entries.len());

                // Measure status text width
                let status_line_preview = shape_text(
                    status_text.as_str(),
                    &font,
                    header_font_size,
                    colors.muted,
                    window,
                );
                let status_w = line_width(&status_line_preview).max(20.0 * zoom);
                let header_slots = miller_header_cut(col_slots.header_slot, status_w, zoom);

                let title_line = shape_text_truncated(
                    &title,
                    &font,
                    header_font_size,
                    colors.muted,
                    header_slots.title_slot.w,
                    window,
                );

                let status_line = shape_text(
                    status_text.as_str(),
                    &font,
                    header_font_size,
                    colors.muted,
                    window,
                );

                let total_rows = visible.len();
                let content_h = total_rows as f32 * row_h;
                let body_h = col_slots.body_slot.h;

                let scroll = app
                    .miller_scroll_handles
                    .entry((pane_id, ci))
                    .or_default()
                    .clone();
                let base_scroll = scroll.0.borrow().base_handle.clone();
                app.configure_file_scroll(&scroll);
                base_scroll.set_line_scroll_height(px(row_h));
                if base_scroll.update_scroll_geometry(col_slots.body_slot.to_bounds(bounds.origin),
                    size(px(col_slots.body_slot.w), px(content_h)), cx.background_executor().now()) {
                    window.on_next_frame(|window, _| window.refresh());
                }
                let max_scroll_y = app.file_scroll_max(content_h, body_h);
                crate::ui::dg::scroll::apply_deferred(&scroll, true, body_h, max_scroll_y, |row| {
                    (row < total_rows).then(|| (row as f32 * row_h, row_h))
                });
                let mut scroll_y = (-f32::from(base_scroll.offset().y)).max(0.0);
                if scroll_y > max_scroll_y {
                    let offset = base_scroll.offset();
                    base_scroll.set_offset(gpui::point(offset.x, gpui::px(-max_scroll_y)));
                    scroll_y = max_scroll_y;
                }

                let (start_idx, end_idx) =
                    miller_visible_row_range(scroll_y, body_h, row_h, total_rows);

                // Request missing file icons
                // (a viewport of lookahead per side: icons resolve off thread)
                let ahead = ((body_h / row_h.max(1.0)).ceil() as usize).max(2);
                let icon_start = start_idx.saturating_sub(ahead);
                let icon_end = end_idx.saturating_add(ahead).min(visible.len());
                for idx in visible[icon_start..icon_end].iter().copied() {
                    if let Some(entry) = col.entries.get(idx) {
                        let file_ref = crate::ui::file_icons::FileIconRef {
                            path: entry.path(),
                            kind: entry.kind().clone(),
                            extension: entry
                                .extension()
                                .as_ref()
                                .map(|s| SharedString::from(s.to_string())),
                        };
                        app.maybe_request_file_icon(&file_ref);
                    }
                }

                let show_checkboxes = col.selection.selected.len() > 1;
                let mut prepared_rows = Vec::with_capacity(end_idx.saturating_sub(start_idx));

                for (position, idx) in visible.iter().copied().enumerate().take(end_idx).skip(start_idx) {
                    let Some(entry) = col.entries.get(idx) else {
                        continue;
                    };
                    let is_dir = matches!(entry.kind(), FileKind::Dir);
                    let selected = col.selection.selected.contains(&idx);
                    let cursor = col.selection.keyboard_cursor == Some(idx);
                    let row_y = col_slots.body_slot.y + (position as f32 * row_h) - scroll_y;
                    let show_check = show_checkboxes;

                    let row_slots = miller_checkbox_in_icon(
                        miller_row_cut(
                            col_slots.body_slot.x,
                            row_y,
                            col_slots.body_slot.w,
                            row_h,
                            zoom,
                            is_dir,
                            false,
                        ),
                        show_check,
                    );

                    let blinking = is_dir
                        && app.drag_hover.blink_id.as_ref()
                            == Some(&DragHoverId::Folder(pane_id, entry.path()));

                    let editing = crate::ui::views::rename::active_input_for_path(
                        app,
                        pane_id,
                        &entry.path(),
                    )
                    .is_some();
                    let dimmed = crate::ui::views::rename::dim_other_path(
                        app,
                        pane_id,
                        &entry.path(),
                    );

                    let file_ref = crate::ui::file_icons::FileIconRef {
                        path: entry.path(),
                        kind: entry.kind().clone(),
                        extension: entry
                            .extension()
                            .as_ref()
                            .map(|s| SharedString::from(s.to_string())),
                    };
                    let icon = app.file_icon(&file_ref);

                    let name_color = if selected && !editing {
                        colors.accent_fg
                    } else {
                        colors.fg
                    };
                    let name_line = shape_text_truncated(
                        &entry.name,
                        &font,
                        font_size,
                        name_color,
                        row_slots.name_slot.w,
                        window,
                    );

                    prepared_rows.push(MillerPreparedRow {
                        opacity: 1.0,
                        interactive: true,
                        col_idx: ci,
                        row_idx: idx,
                        slots: row_slots,
                        entry: entry.clone(),
                        name_line,
                        icon,
                        is_dir,
                        selected,
                        selected_at: None,
                        cursor,
                        hovered: false, // Updated dynamically from mouse position in paint
                        blinking,
                        dimmed,
                        editing,
                    });
                }

                prepaint_columns.push(MillerPreparedColumn {
                    identity: format!("{:?}", col.location),
                    col_idx: ci,
                    slots: col_slots,
                    title,
                    title_line,
                    header_slots,
                    status_line: Some(status_line),
                    status_msg,
                    rows: prepared_rows,
                    content_h,
                    scroll_y,
                    max_scroll_y,
                    is_flashing: is_col_flashing,
                    col_path: col_loc,
                    opacity: 1.0,
                    interactive: true,
                });
            }

            prepaint_columns
        });

        let feedback = window.use_keyed_state(
            SharedString::from(format!("miller-selection-feedback-{}", pane_id.0)),
            cx,
            |_, _| selection_checkbox::SelectionFeedback::<(usize, PathBuf)>::default(),
        );
        let selection: Vec<_> = prepaint_cols
            .iter()
            .flat_map(|col| {
                col.rows.iter().map(|row| {
                    (
                        (row.col_idx, row.entry.path()),
                        row.selected && row.slots.check_slot.is_some(),
                    )
                })
            })
            .collect();
        let selected_at = feedback.update(cx, |state, _| {
            state.sample(&selection, std::time::Instant::now())
        });
        for (row, at) in prepaint_cols
            .iter_mut()
            .flat_map(|col| &mut col.rows)
            .zip(selected_at)
        {
            row.selected_at = at;
        }
        let (epoch, instant) = {
            let app = app_entity.read(cx);
            (
                app.state
                    .tab_states
                    .get(&pane_id)
                    .map_or(0, |ts| ts.appear_seq),
                cx.reduce_motion()
                    || app.rubber.is_some()
                    || app.rename_edit.is_some()
                    || app.is_drag_active(has_gpui_drag),
            )
        };
        // Rows and their hit geometry share one presentation, just like the
        // flat views. Scroll and navigation invalidate retained snapshots.
        for col in &mut prepaint_cols {
            use std::hash::{Hash, Hasher};
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            epoch.hash(&mut hash);
            col.identity.hash(&mut hash);
            let row_epoch = hash.finish();
            let viewport = [col.scroll_y, row_h, col.slots.body_slot.h];
            let presence_epoch = crate::ui::dg::motion::presentation_epoch(
                row_epoch,
                &[
                    col.scroll_y,
                    row_h,
                    col.slots.body_slot.x,
                    col.slots.body_slot.y,
                    col.slots.body_slot.w,
                    col.slots.body_slot.h,
                ],
            );
            let state = window.use_keyed_state(
                SharedString::from(format!("miller-rows-{}-{}", pane_id.0, col.col_idx)),
                cx,
                |_, _| {
                    (
                        crate::ui::dg::motion::RowMotion::<PathBuf>::new(row_epoch, viewport),
                        crate::ui::dg::motion::PresenceMotion::<PathBuf, MillerPreparedRow>::new(),
                    )
                },
            );
            let now = std::time::Instant::now();
            let targets: Vec<_> = col
                .rows
                .iter()
                .map(|row| (row.entry.path(), row.row_idx, row.slots.row_slot.y))
                .collect();
            let rows = std::mem::take(&mut col.rows);
            let (samples, active) = state.update(cx, |state, _| {
                let (positions, moving) =
                    state.0.sample(&targets, row_epoch, viewport, instant, now);
                let values = rows
                    .into_iter()
                    .zip(positions)
                    .map(|(mut row, (_, y, opacity))| {
                        row.slots = miller_checkbox_in_icon(
                            miller_row_cut(
                                col.slots.body_slot.x,
                                y,
                                col.slots.body_slot.w,
                                row_h,
                                zoom,
                                row.is_dir,
                                false,
                            ),
                            row.slots.check_slot.is_some(),
                        );
                        row.opacity = opacity;
                        (row.entry.path(), row)
                    })
                    .collect();
                let (samples, fading) = state.1.sample(values, presence_epoch, instant, now);
                (samples, moving || fading)
            });
            col.rows = samples
                .into_iter()
                .map(|sample| {
                    let mut row = sample.value;
                    row.opacity *= sample.opacity;
                    row.interactive = sample.interactive;
                    if !row.interactive {
                        row.cursor = false;
                        row.blinking = false;
                        row.hovered = false;
                    }
                    row
                })
                .collect();
            if active {
                window.request_animation_frame();
            }
        }
        app_entity.update(cx, |app, _| {
            let dragging = app.is_drag_active(has_gpui_drag);
            for col in &prepaint_cols {
                let body = col
                    .slots
                    .body_slot
                    .to_bounds(bounds.origin)
                    .intersect(&content_mask);
                let rows: Vec<_> = col
                    .rows
                    .iter()
                    .filter(|row| row.interactive)
                    .map(|row| {
                        (
                            row.row_idx,
                            row.slots.row_slot.to_bounds(bounds.origin).intersect(&body),
                        )
                    })
                    .collect();
                app.miller_row_rects
                    .borrow_mut()
                    .insert((pane_id, col.col_idx), rows);
                if dragging {
                    for row in col.rows.iter().filter(|row| row.interactive && row.is_dir) {
                        app.register_drag_hover_target(DragHoverTarget {
                            id: DragHoverId::Folder(pane_id, row.entry.path()),
                            bounds: row.slots.row_slot.to_bounds(bounds.origin).intersect(&body),
                            kind: DragHoverKind::Folder,
                            command: DragHoverCommand::NavigatePathIn(pane_id, row.entry.path()),
                            drop_command: Some(DragHoverDropCommand::TransferTo(row.entry.path())),
                        });
                    }
                }
            }
        });
        let presence = window.use_keyed_state(
            SharedString::from(format!("miller-column-presence-{}", pane_id.0)), cx,
            |_, _| crate::ui::dg::motion::PresenceMotion::<(usize, PathBuf), MillerPreparedColumn>::new(),
        );
        // Reset stale snapshots on scroll, zoom or viewport changes. Changing
        // column count alone can retain a departing tail within the viewport.
        let epoch = crate::ui::dg::motion::presentation_epoch(
            epoch,
            &[
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.height),
                f32::from(content_mask.size.width),
                f32::from(content_mask.size.height),
                self.col_w,
                self.row_h,
            ],
        );
        let scrolling = window.use_keyed_state(
            SharedString::from(format!("miller-presence-scroll-{}", pane_id.0)),
            cx,
            |_, _| Vec::<f32>::new(),
        );
        let scrolled = scrolling.update(cx, |previous, _| {
            let next: Vec<_> = prepaint_cols.iter().map(|col| col.scroll_y).collect();
            let changed = previous.iter().zip(&next).any(|(a, b)| a != b);
            *previous = next;
            changed
        });
        let columns = prepaint_cols
            .into_iter()
            .map(|col| {
                (
                    (
                        col.col_idx,
                        col.col_path
                            .clone()
                            .unwrap_or_else(|| PathBuf::from(&col.title)),
                    ),
                    col,
                )
            })
            .collect();
        let (samples, active) = presence.update(cx, |state, _| {
            state.sample(
                columns,
                epoch,
                instant || scrolled,
                std::time::Instant::now(),
            )
        });
        let columns = samples
            .into_iter()
            .map(|sample| {
                let mut column = sample.value;
                column.opacity = sample.opacity;
                column.interactive = sample.interactive;
                column
            })
            .collect();
        if active {
            window.request_animation_frame();
        }
        MillerDgPrepaint { hitbox, columns }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) {
        crate::perf::count("miller_dg.paint");

        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return;
        }

        let origin = bounds.origin;
        let zoom = self.zoom;
        let colors = self.colors;
        let windows_theme = self.windows_theme;
        let mouse_pos = window.mouse_position();
        let is_over = prepaint.hitbox.is_hovered(window);

        // Find hovered column and row
        let mut hover_row_info: Option<(usize, usize)> = None;
        if is_over {
            let lx = f32::from(mouse_pos.x - origin.x);
            let ly = f32::from(mouse_pos.y - origin.y);
            for col in prepaint.columns.iter().filter(|col| col.interactive) {
                if col.slots.body_slot.contains(lx, ly) {
                    for row in col.rows.iter().filter(|row| row.interactive).rev() {
                        if row.slots.row_slot.contains(lx, ly) {
                            hover_row_info = Some((col.col_idx, row.row_idx));
                            break;
                        }
                    }
                    break;
                }
            }
        }

        if is_over && hover_row_info.is_some() {
            window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
        }

        // Paint each column with Hardware GPU Clipping Layer (Principle 4)
        for col in &prepaint.columns {
            let col_slot = col.slots.col_slot;
            let col_bounds = Bounds {
                origin: point(origin.x + px(col_slot.x), origin.y + px(col_slot.y)),
                size: size(px(col_slot.w), px(col_slot.h)),
            };

            window.with_element_opacity(Some(col.opacity), |window| {
            with_miller_clip(window, col_bounds, |window| {
                // 1. Column header
                let header_slot = col.slots.header_slot;
                let header_bounds = Bounds {
                    origin: point(origin.x + px(header_slot.x), origin.y + px(header_slot.y)),
                    size: size(px(header_slot.w), px(header_slot.h)),
                };
                paint_fill(window, header_bounds, colors.header_bg);

                // Header title text
                let header_slots = col.header_slots;
                let title_x = origin.x + px(header_slots.title_slot.x);
                let title_y = origin.y + px(text_origin_y(header_slots.title_slot.y, zoom));
                with_miller_clip(window, header_slots.title_slot.to_bounds(origin), |window| {
                let _ = col.title_line.paint(
                    point(title_x, title_y),
                    px(header_slots.title_slot.h),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                });

                // Header status / count text
                if let Some(status_line) = &col.status_line {
                    let status_x = origin.x + px(header_slots.status_slot.x);
                    let status_y = origin.y + px(text_origin_y(header_slots.status_slot.y, zoom));
                    with_miller_clip(window, header_slots.status_slot.to_bounds(origin), |window| {
                    let _ = status_line.paint(
                        point(status_x, status_y),
                        px(header_slots.status_slot.h),
                        TextAlign::Right,
                        None,
                        window,
                        cx,
                    );
                    });
                }

                // Header bottom border
                let header_sep_bounds = Bounds {
                    origin: point(header_bounds.origin.x, header_bounds.origin.y + header_bounds.size.height - px(1.0)),
                    size: size(header_bounds.size.width, px(1.0)),
                };
                paint_fill(window, header_sep_bounds, colors.border);

                // 2. Column body: empty state or rows
                if col.rows.is_empty() {
                    if let Some(msg) = &col.status_msg {
                        let empty_font = window.text_style().font();
                        let empty_line = shape_text_truncated(
                            msg.as_str(),
                            &empty_font,
                            self.font_size,
                            colors.muted,
                            (col.slots.body_slot.w - 16.0 * zoom).max(0.0),
                            window,
                        );
                        let w = line_width(&empty_line);
                        let cx_pt = origin.x + px(col.slots.body_slot.x + (col.slots.body_slot.w - w) * 0.5);
                        let cy_pt = origin.y + px(col.slots.body_slot.y + col.slots.body_slot.h * 0.4);
                        with_miller_clip(window, col.slots.body_slot.to_bounds(origin), |window| {
                        let _ = empty_line.paint(
                            point(cx_pt, cy_pt),
                            px(20.0 * zoom),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                        });
                    }
                } else {
                    with_miller_clip(window, col.slots.body_slot.to_bounds(origin), |window| {
                    for row in &col.rows {
                        window.with_element_opacity(Some(row.opacity * crate::ui::views::rename::cut_path_opacity(&self.app, &row.entry.path(), cx)), |window| {
                        let row_slot = row.slots.row_slot;
                        let row_bounds = Bounds {
                            origin: point(origin.x + px(row_slot.x), origin.y + px(row_slot.y)),
                            size: size(px(row_slot.w), px(row_slot.h)),
                        };

                        let is_row_hovered = col.interactive && row.interactive && hover_row_info == Some((row.col_idx, row.row_idx));

                        // Selection / Blinking / Hover background
                        if row.selected && !row.editing {
                            paint_rounded(window, row_bounds, 4.0 * zoom, colors.accent);
                        } else if row.blinking {
                            paint_rounded(window, row_bounds, 4.0 * zoom, colors.accent.opacity(0.38));
                        } else if is_row_hovered {
                            paint_rounded(window, row_bounds, 4.0 * zoom, colors.hover);
                        }

                        // Keyboard cursor outline
                        if col.interactive && row.interactive {
                            crate::ui::operation_feedback::paint_file_feedback(&self.app, &row.entry.path(), row_bounds, colors.accent, 4.0 * zoom, window, cx);
                        }
                        if row.cursor && !row.selected {
                            let outline_col = colors
                                .accent
                                .opacity(if row.selected { 0.95 } else { 0.65 });
                            window.paint_quad(
                                gpui::outline(row_bounds, outline_col, Default::default())
                                    .corner_radii(Corners::all(px(4.0 * zoom))),
                            );
                        }

                        // Dimming during rename
                        window.with_element_opacity(
                            row.dimmed.then_some(crate::ui::views::rename::DIM_OPACITY),
                            |window| {
                                // Checkbox slot
                                // File / folder icon slot
                                let icon_slot = row.slots.icon_slot;
                                let icon_sz = MILLER_ICON_SZ * zoom;
                                let icon_bounds = Bounds {
                                    origin: point(
                                        origin.x + px(icon_slot.x + (icon_slot.w - icon_sz) * 0.5),
                                        origin.y + px(icon_slot.y + (icon_slot.h - icon_sz) * 0.5),
                                    ),
                                    size: size(px(icon_sz), px(icon_sz)),
                                };

                                if row.selected && !row.editing && row.slots.check_slot.is_some() {
                                    selection_checkbox::paint_animated(
                                        window,
                                        cx,
                                        icon_bounds.center(),
                                        MILLER_CHECKBOX_SZ * zoom,
                                        row.selected_at,
                                    );
                                } else if let Some(tex) = row.icon.texture_for_size(icon_sz) {
                                    let _ = window.paint_image(
                                        icon_bounds,
                                        icon_bounds,
                                        Corners::default(),
                                        tex,
                                        0,
                                        false,
                                    );
                                } else {
                                    let icon_kind = if row.is_dir {
                                        AtlasIcon::Folder
                                    } else {
                                        AtlasIcon::File
                                    };
                                    let icon_color = if row.selected && !row.editing {
                                        colors.accent_fg
                                    } else {
                                        colors.accent
                                    };
                                    paint_theme_icon(
                                        window,
                                        cx,
                                        origin,
                                        icon_slot,
                                        icon_kind,
                                        windows_theme,
                                        icon_sz,
                                        icon_color,
                                    );
                                }

                                // Filename slot with nested GPU clipping layer
                                if !row.editing {
                                    let name_slot = row.slots.name_slot;
                                    let text_x = origin.x + px(name_slot.x);
                                    let text_y = origin.y + px(text_origin_y(name_slot.y, zoom));
                                    let name_bounds = Bounds {
                                        origin: point(text_x, origin.y + px(name_slot.y)),
                                        size: size(px(name_slot.w), px(name_slot.h)),
                                    };
                                    with_miller_clip(window, name_bounds, |window| {
                                        let _ = row.name_line.paint(
                                            point(text_x, text_y),
                                            px(row_slot.h),
                                            TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                    });
                                }

                                // Caret slot on right for directory
                                if let Some(caret_slot) = row.slots.caret_slot {
                                    let caret_color = if row.selected && !row.editing {
                                        colors.accent_fg
                                    } else {
                                        colors.accent
                                    };
                                    paint_theme_icon(
                                        window,
                                        cx,
                                        origin,
                                        caret_slot,
                                        AtlasIcon::CaretRight,
                                        windows_theme,
                                        MILLER_CARET_SZ * zoom,
                                        caret_color,
                                    );
                                }
                            },
                        );
                        });
                    }
                    });
                }

                // 3. Right column separator border
                let sep_slot = col.slots.sep_slot;
                let sep_bounds = Bounds {
                    origin: point(origin.x + px(sep_slot.x), origin.y + px(sep_slot.y)),
                    size: size(px(sep_slot.w), px(sep_slot.h)),
                };
                let border_col = if col.is_flashing {
                    colors.accent
                } else {
                    colors.header_bg
                };
                paint_fill(window, sep_bounds, border_col);

                // 4. Sleek scrollbar indicator if overflowing
                if col.content_h > col.slots.body_slot.h && col.slots.body_slot.h > 20.0 {
                    let track_h = col.slots.body_slot.h;
                    let thumb_h = ((track_h / col.content_h) * track_h).clamp_lenient((24.0 * zoom).min(track_h), track_h);
                    let scroll_ratio = (col.scroll_y / col.max_scroll_y.max(1.0)).clamp(0.0, 1.0);
                    let thumb_y = col.slots.body_slot.y + scroll_ratio * (track_h - thumb_h);
                    let thumb_bounds = Bounds {
                        origin: point(origin.x + px(col.slots.body_slot.x + col.slots.body_slot.w - 5.0 * zoom), origin.y + px(thumb_y)),
                        size: size(px(3.0 * zoom), px(thumb_h)),
                    };
                    paint_rounded(window, thumb_bounds, 1.5 * zoom, colors.muted.opacity(0.35));
                }
            });
            });
        }

        let Some(app_entity) = self.app.upgrade() else {
            return;
        };

        let pane_id = self.pane_id;

        // MouseMove for hover feedback & tooltip dismiss
        let move_view = app_entity.clone();
        let move_hitbox = prepaint.hitbox.clone();
        window.on_mouse_event::<MouseMoveEvent>(move |_ev, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            if move_hitbox.is_hovered(window) {
                move_view.update(cx, |this, cx| {
                    this.notify_pane(pane_id, cx);
                });
            }
        });

        // MouseDown for interactions: Left click, Double click, Checkbox toggle, Right click context menu
        let down_view = app_entity.clone();
        let down_hitbox = prepaint.hitbox.clone();
        let cols_for_down = prepaint
            .columns
            .iter()
            .filter(|c| c.interactive)
            .map(|c| {
                (
                    c.col_idx,
                    c.slots.col_slot,
                    c.slots.body_slot,
                    c.col_path.clone(),
                    c.rows
                        .iter()
                        .filter(|row| row.interactive)
                        .map(|r| {
                            (
                                r.row_idx,
                                r.slots.row_slot,
                                r.slots.check_slot,
                                r.entry.path(),
                                r.is_dir,
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();

        window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            if !down_hitbox.is_hovered(window) {
                return;
            }

            let lx = f32::from(ev.position.x - origin.x);
            let ly = f32::from(ev.position.y - origin.y);

            for (ci, col_slot, body_slot, col_path, rows) in &cols_for_down {
                if !col_slot.contains(lx, ly) {
                    continue;
                }

                let ci = *ci;

                if ev.button == MouseButton::Left {
                    // Check if clicked inside a row
                    let mut hit_row = None;
                    for (row_idx, row_slot, check_slot, path, is_dir) in rows {
                        if row_slot.contains(lx, ly) {
                            let on_chk = check_slot.is_some_and(|cs| cs.contains(lx, ly));
                            hit_row = Some((*row_idx, on_chk, path.clone(), *is_dir));
                            break;
                        }
                    }

                    if let Some((idx, on_chk, _path, _is_dir)) = hit_row {
                        let m = ev.modifiers;
                        let (shift, ctrl) = (m.shift, m.control || m.platform);

                        if on_chk {
                            // Checkbox click: toggle multi-selection via Action::MillerToggleSelect
                            down_view.update(cx, |this, cx| {
                                this.process_actions(vec![Action::MillerToggleSelect {
                                    tile: pane_id,
                                    column: ci,
                                    idx,
                                }]);
                                this.notify_pane(pane_id, cx);
                                cx.notify();
                            });
                        } else if ev.click_count >= 2 && !shift && !ctrl {
                            // Modified double-clicks keep the shared selection policy.
                            down_view.update(cx, |this, cx| {
                                if this.rename_edit.is_some() {
                                    this.commit_rename(cx);
                                }
                                this.process_actions(vec![Action::MillerActivate {
                                    tile: pane_id,
                                    column: ci,
                                    idx,
                                }]);
                                this.notify_pane(pane_id, cx);
                                cx.notify();
                            });
                        } else {
                            // Single-click: select folder (which cascades child column) or file
                            down_view.update(cx, |this, cx| {
                                if this.rename_edit.is_some() {
                                    this.commit_rename(cx);
                                }
                                let preserve_group = this
                                    .state
                                    .tab_states
                                    .get(&pane_id)
                                    .and_then(|state| state.miller.columns.get(ci))
                                    .is_some_and(|col| {
                                        col.selection.selected.len() > 1
                                            && col.selection.selected.contains(&idx)
                                    });
                                this.selection_left_mouse_down(
                                    RubberTarget::MillerColumn(pane_id, ci),
                                    Some(idx),
                                    ev.position,
                                    shift,
                                    ctrl,
                                    ev.click_count,
                                    cx,
                                );
                                if !shift && !ctrl && !preserve_group {
                                    this.process_actions(vec![Action::MillerSelect {
                                        tile: pane_id,
                                        column: ci,
                                        idx,
                                        shift: false,
                                        ctrl: false,
                                    }]);
                                }
                                this.notify_pane(pane_id, cx);
                                cx.notify();
                            });
                        }
                    } else if body_slot.contains(lx, ly) {
                        // Click on column background below rows: start rubber-band or clear selection
                        down_view.update(cx, |this, cx| {
                            if this.rename_edit.is_some() {
                                this.commit_rename(cx);
                            }
                            this.selection_left_mouse_down(
                                RubberTarget::MillerColumn(pane_id, ci),
                                None,
                                ev.position,
                                ev.modifiers.shift,
                                ev.modifiers.control || ev.modifiers.platform,
                                ev.click_count,
                                cx,
                            );
                            this.notify_pane(pane_id, cx);
                            cx.notify();
                        });
                    }
                    cx.stop_propagation();
                    return;
                } else if ev.button == MouseButton::Right {
                    let mut hit_row = None;
                    for (row_idx, row_slot, _, path, _) in rows {
                        if row_slot.contains(lx, ly) {
                            hit_row = Some((*row_idx, path.clone()));
                            break;
                        }
                    }

                    let pos = ev.position;
                    if let Some((idx, path)) = hit_row {
                        down_view.update(cx, |this, cx| {
                            let already_selected = this
                                .state
                                .tab_states
                                .get(&pane_id)
                                .and_then(|ts| ts.miller.columns.get(ci))
                                .is_some_and(|col| col.selection.selected.contains(&idx));
                            if !already_selected {
                                this.process_actions(vec![Action::MillerSelect {
                                    tile: pane_id,
                                    column: ci,
                                    idx,
                                    shift: false,
                                    ctrl: false,
                                }]);
                            } else {
                                this.activate_tab(pane_id);
                                if let Some(ts) = this.state.tab_states.get_mut(&pane_id) {
                                    ts.miller.focused = ci;
                                    // Menu "Rename"/F2 act on the right-clicked row.
                                    if let Some(col) = ts.miller.columns.get_mut(ci) {
                                        col.selection.keyboard_cursor = Some(idx);
                                    }
                                }
                            }
                            let paths = {
                                let p = this.miller_focused_selection_paths().unwrap_or_default();
                                if p.is_empty() {
                                    vec![path]
                                } else {
                                    p
                                }
                            };
                            this.open_ctx_menu(paths, pos, window, cx);
                            this.notify_pane(pane_id, cx);
                            cx.notify();
                        });
                    } else if let Some(path) = col_path {
                        let path = path.clone();
                        down_view.update(cx, |this, cx| {
                            this.open_background_ctx_menu(Some(path), pos, window, cx);
                            this.notify_pane(pane_id, cx);
                            cx.notify();
                        });
                    }
                    cx.stop_propagation();
                    return;
                }
            }
        });

        // Mouse wheel for vertical scrolling per column
        let wheel_view = app_entity;
        let wheel_hitbox = prepaint.hitbox.clone();
        let cols_for_wheel = prepaint
            .columns
            .iter()
            .filter(|c| c.interactive)
            .map(|c| (c.col_idx, c.slots.col_slot))
            .collect::<Vec<_>>();
        window.on_mouse_event::<ScrollWheelEvent>(move |ev, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            if !wheel_hitbox.is_hovered(window) {
                return;
            }

            if ev.modifiers.control || ev.modifiers.platform {
                // ctrl_scroll owns the App update; wrapping it in another
                // update leases the same entity twice and panics.
                ctrl_scroll(ev, pane_id, &wheel_view, window, cx);
                return;
            }

            let delta = ev.delta.pixel_delta(px(1.0));
            if ev.modifiers.shift || delta.x.abs() > delta.y.abs() {
                // Horizontal gestures belong to the cascade's outer scroller.
                return;
            }

            let lx = f32::from(ev.position.x - origin.x);
            let ly = f32::from(ev.position.y - origin.y);

            for (ci, col_slot) in &cols_for_wheel {
                if col_slot.contains(lx, ly) {
                    let ci = *ci;

                    wheel_view.update(cx, |this, cx| {
                        let scroll = this
                            .miller_scroll_handles
                            .entry((pane_id, ci))
                            .or_default()
                            .clone();
                        let base_scroll = scroll.0.borrow().base_handle.clone();
                        base_scroll.apply_scroll_delta(ev.delta, cx.background_executor().now());
                        this.notify_pane(pane_id, cx);
                        cx.notify();
                    });
                    cx.stop_propagation();
                    return;
                }
            }
        });
    }
}

impl App {
    /// Digital Grove Miller columns view renderer (`render_miller_dg`).
    ///
    /// Fully immediate Digital Grove element following the 20 Digital Grove principles:
    /// - Derives state downstream every frame from canonical upstream `app.state.tab_states[pane_id].miller`.
    /// - Single-cycle O(1) arithmetic layout with `RectCut`.
    /// - GPU hardware clipping layer per column and filename slot.
    /// - Total coverage of interactions: single/double-click, checkbox multi-select, context menu,
    ///   drag & drop hover/blink feedback, keyboard navigation, inline rename.
    pub(crate) fn render_miller_dg(
        &mut self,
        pane_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        crate::perf::count("render.view.miller_dg");
        let _perf = crate::perf::span_detail("miller_dg.render", || format!("pane={}", pane_id.0));

        // Re-seed cascade from pane location if needed
        let pane_loc = self.tab_data_for(pane_id).map(|t| t.location.clone());
        let need_init = match (&pane_loc, self.state.tab_states.get(&pane_id)) {
            (Some(loc), Some(ts)) => ts.miller.columns.first().map(|c| &c.location) != Some(loc),
            (Some(_), None) => true,
            _ => false,
        };
        if need_init {
            if let Some(loc) = pane_loc {
                self.process_actions(vec![Action::MillerInit {
                    tile: pane_id,
                    location: loc,
                }]);
            }
        }

        let view = cx.entity();
        let active_theme = self.state.profiles.active().theme.as_str();
        let is_glass = active_theme.ends_with(" Glass");
        let colors = MillerDgColors::from_theme(cx, is_glass);
        let crumb_strip = self.render_crumb_strip(
            pane_id,
            &view,
            colors.fg,
            colors.muted,
            colors.border,
            colors.header_bg,
            cx,
        );

        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let header_font_size = crate::ui::surface::chrome_font_size_header(window);
        let font = window.text_style().font();
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let windows_theme = is_windows_theme(self.state.profiles.active().theme.as_str());

        let column_count = self
            .state
            .tab_states
            .get(&pane_id)
            .map(|ts| ts.miller.columns.len())
            .unwrap_or(0);

        let col_w = MILLER_COL_W * zoom;
        let header_h = MILLER_HEADER_H * zoom;
        let row_h = MILLER_ROW_H * zoom;
        let total_w = (column_count as f32 * col_w).max(col_w);

        let miller_el = MillerDgElement::new(
            pane_id,
            view.downgrade(),
            col_w,
            header_h,
            row_h,
            zoom,
            colors,
            font.clone(),
            font_size,
            header_font_size,
            windows_theme,
        )
        .absolute()
        .inset_0()
        .into_any_element();

        // Check for active inline rename overlay
        let mut rename_overlay: Option<AnyElement> = None;
        if let Some(ts) = self.state.tab_states.get(&pane_id) {
            for ci in 0..ts.miller.columns.len() {
                let col = &ts.miller.columns[ci];
                let scroll_y = self
                    .miller_scroll_handles
                    .get(&(pane_id, ci))
                    .map_or(0.0, |h| {
                        (-f32::from(h.0.borrow().base_handle.offset().y)).max(0.0)
                    });

                for (position, idx) in ts.miller_visible_indices(ci).iter().copied().enumerate() {
                    let entry = &col.entries[idx];
                    if let Some(input) = crate::ui::views::rename::active_input_for_path(
                        self,
                        pane_id,
                        &entry.path(),
                    ) {
                        let is_dir = matches!(entry.kind(), FileKind::Dir);
                        let row_y = header_h + (position as f32 * row_h) - scroll_y;
                        let col_x = ci as f32 * col_w;
                        let row_slots = miller_row_cut(
                            col_x, row_y, col_w, row_h, zoom, is_dir,
                            false, // checkbox lives in the icon slot; no gutter
                        );
                        let name_slot = row_slots.name_slot;
                        let input_w = crate::ui::views::rename::input_width(
                            &entry.name,
                            name_slot.w,
                            font_size,
                            window,
                        );

                        rename_overlay = Some(
                            div()
                                .absolute()
                                .left(px(name_slot.x))
                                .top(px(name_slot.y))
                                .w(px(input_w))
                                .h(px(name_slot.h))
                                .items_center()
                                .child(crate::ui::views::rename::input_element(
                                    &input,
                                    &view,
                                    px(name_slot.h),
                                    font_size,
                                    window,
                                    cx,
                                ))
                                .into_any_element(),
                        );
                        break;
                    }
                }
                if rename_overlay.is_some() {
                    break;
                }
            }
        }

        // Hover tooltip for currently hovered row
        let mut hovered_entry: Option<(
            String,
            PathBuf,
            bool,
            Option<u64>,
            Option<std::time::SystemTime>,
        )> = None;
        let mouse_pos = window.mouse_position();
        if let Some(ts) = self.state.tab_states.get(&pane_id) {
            for ci in 0..ts.miller.columns.len() {
                if let Some(bounds) = self.miller_column_bounds.get(&(pane_id, ci)).copied() {
                    if bounds.contains(&mouse_pos) {
                        if let Some(rows) =
                            self.miller_row_rects.borrow().get(&(pane_id, ci)).cloned()
                        {
                            for (idx, row_b) in rows {
                                if row_b.contains(&mouse_pos) {
                                    if let Some(e) = ts.miller.columns[ci].entries.get(idx) {
                                        hovered_entry = Some((
                                            e.name.to_string(),
                                            e.path(),
                                            matches!(e.kind(), FileKind::Dir),
                                            e.size(),
                                            e.modified,
                                        ));
                                    }
                                    break;
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }

        let horizontal = window
            .use_keyed_state(
                SharedString::from(format!("miller-horizontal-handle-{}", pane_id.0)),
                cx,
                |_, _| gpui::ScrollHandle::new(),
            )
            .read(cx)
            .clone();
        let focus = self
            .state
            .tab_states
            .get(&pane_id)
            .map_or(0, |ts| ts.miller.focused);
        let reveal_handle = horizontal.clone();
        let instant = cx.reduce_motion()
            || self.is_drag_active(cx.has_active_drag())
            || self.rename_edit.is_some();
        let reveal_layer = gpui::canvas(
            move |bounds, window, cx| {
                let now = std::time::Instant::now();
                let current = (-f32::from(reveal_handle.offset().x)).max(0.0);
                let state = window.use_keyed_state(
                    SharedString::from(format!("miller-horizontal-reveal-{}", pane_id.0)),
                    cx,
                    |_, _| crate::ui::dg::motion::ColumnReveal::new(current, now),
                );
                let (offset, active) = state.update(cx, |s, _| {
                    s.sample(
                        focus,
                        column_count,
                        col_w,
                        f32::from(bounds.size.width),
                        current,
                        instant,
                        now,
                    )
                });
                if (offset - current).abs() > 0.01 {
                    reveal_handle.set_offset(point(px(-offset), px(0.0)));
                    window.refresh();
                }
                active
            },
            |_, active, window, _| {
                if active {
                    window.request_animation_frame();
                }
            },
        )
        .absolute()
        .inset_0();
        let body_container = div()
            .id(SharedString::from(format!(
                "miller-dg-scroll-{}",
                pane_id.0
            )))
            .size_full()
            .overflow_x_scroll()
            .track_scroll(&horizontal)
            .when_some(
                hovered_entry.filter(|_| self.ctx_menu.is_none()),
                |el, (name, path, is_dir, size, modified)| {
                    el.tooltip(move |window, cx| {
                        crate::ui::tooltips::entry_hover_tooltip(
                            name.clone(),
                            path.clone(),
                            is_dir,
                            size,
                            modified,
                            window,
                            cx,
                        )
                    })
                },
            )
            .child(
                div()
                    .w(px(total_w))
                    .h_full()
                    .relative()
                    .child(miller_el)
                    .child(miller_drag_layer(&view, pane_id, total_w))
                    .when_some(rename_overlay, |el, overlay| el.child(overlay)),
            );

        let key_view = view.clone();
        let keyboard_layer = v_flex()
            .relative()
            .size_full()
            .flex_1()
            .min_h(px(0.))
            .on_prepaint_child(pane_bounds_recorder(pane_id, &view))
            .child(body_container)
            .child(reveal_layer);

        let zone_view = key_view.clone();
        let pane_view = key_view;
        crate::ui::views::pane_background_interactions(
            v_flex()
                .id(SharedString::from(format!("pane-{}", pane_id.0)))
                .size_full(),
            &pane_view,
            pane_id,
        )
        .when(cfg!(windows), |el| {
            el.on_prepaint_child(zone_recorder(pane_id, &zone_view))
        })
        .child(crumb_strip)
        .child(keyboard_layer)
        .into_any_element()
    }
}

/// Transparent drag source over every column. The press already selected the
/// row (element mouse-down); the drag carries that column's selection.
fn miller_drag_layer(view: &gpui::Entity<App>, pane_id: TabId, total_w: f32) -> AnyElement {
    let drag_view = view.clone();
    div()
        .id(SharedString::from(format!(
            "miller-dg-drag-layer-{}",
            pane_id.0
        )))
        .absolute()
        .top_0()
        .left_0()
        .w(px(total_w))
        .h_full()
        .on_drag_alt(move |_cursor_offset, cursor_style, window, cx| {
            let pos = window.mouse_position();
            let hit = {
                let app = drag_view.read(&*cx);
                let rects = app.miller_row_rects.borrow();
                rects
                    .iter()
                    .filter(|((pane, _), _)| *pane == pane_id)
                    .find_map(|((_, column), rows)| {
                        rows.iter()
                            .find(|(_, bounds)| bounds.contains(&pos))
                            .map(|(idx, _)| (*column, *idx))
                    })
                    .and_then(|(column, idx)| {
                        let col = app
                            .state
                            .tab_states
                            .get(&pane_id)?
                            .miller
                            .columns
                            .get(column)?;
                        let entry = col.entries.get(idx)?;
                        col.selection
                            .selected
                            .contains(&idx)
                            .then(|| (column, entry.path()))
                    })
            };
            let Some((column, anchor)) = hit else {
                return crate::ui::drag::empty_any_drag(cx);
            };
            drag_view.update(cx, |this, _| {
                this.activate_tab(pane_id);
                if let Some(ts) = this.state.tab_states.get_mut(&pane_id) {
                    ts.miller.focused = column;
                }
            });
            // `None`: AppState::selected_paths resolves the focused Miller column.
            crate::ui::drag::build_file_any_drag(&drag_view, None, anchor, cursor_style, window, cx)
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_miller_column_cut() {
        let slots = miller_column_cut(0.0, 0.0, 250.0, 600.0, 28.0);
        assert_eq!(slots.col_slot.w, 250.0);
        assert_eq!(slots.col_slot.h, 600.0);
        assert_eq!(slots.header_slot.y, 0.0);
        assert_eq!(slots.header_slot.h, 28.0);
        assert_eq!(slots.header_slot.w, 250.0);
        assert_eq!(slots.body_slot.y, 28.0);
        assert_eq!(slots.body_slot.h, 572.0);
        assert_eq!(slots.sep_slot.x, 249.0);
        assert_eq!(slots.sep_slot.w, 1.0);
        assert_eq!(slots.sep_slot.h, 600.0);
    }

    #[test]
    fn test_miller_header_cut() {
        let slot = Slot::new(10.0, 0.0, 250.0, 28.0);
        let header = miller_header_cut(slot, 30.0, 1.0);
        // Padding: 8.0 left/right -> 250 - 16 = 234 available.
        // Status w: 30.0, gap: 4.0.
        // Title w: 234 - 34 = 200.0.
        assert_eq!(header.status_slot.w, 30.0);
        assert_eq!(header.title_slot.x, 18.0);
        assert_eq!(header.title_slot.w, 200.0);
    }

    #[test]
    fn test_miller_row_cut_file() {
        let row = miller_row_cut(0.0, 10.0, 250.0, 26.0, 1.0, false, false);
        assert!(row.caret_slot.is_none());
        assert!(row.check_slot.is_none());
        assert_eq!(row.icon_slot.x, MILLER_PAD_X);
        assert_eq!(row.icon_slot.w, MILLER_ICON_SZ);
        let expected_name_x = MILLER_PAD_X + MILLER_ICON_SZ + 6.0;
        assert_eq!(row.name_slot.x, expected_name_x);
        let expected_name_w = 250.0 - (MILLER_PAD_X * 2.0) - MILLER_ICON_SZ - 6.0;
        assert_eq!(row.name_slot.w, expected_name_w);
    }

    #[test]
    fn test_miller_row_cut_dir() {
        let row = miller_row_cut(0.0, 10.0, 250.0, 26.0, 1.0, true, false);
        assert!(row.caret_slot.is_some());
        let caret = row.caret_slot.unwrap();
        assert_eq!(caret.w, MILLER_CARET_SZ);
        assert_eq!(caret.x, 250.0 - MILLER_PAD_X - MILLER_CARET_SZ);
        assert!(row.name_slot.w < 250.0 - (MILLER_PAD_X * 2.0) - MILLER_ICON_SZ - 6.0);
    }

    #[test]
    fn test_miller_row_cut_with_checkbox() {
        let row = miller_row_cut(0.0, 10.0, 250.0, 26.0, 1.0, true, true);
        assert!(row.caret_slot.is_some());
        assert!(row.check_slot.is_some());
        let chk = row.check_slot.unwrap();
        assert_eq!(chk.x, MILLER_PAD_X);
        assert_eq!(chk.w, MILLER_CHECKBOX_SZ);
        let icon_x = MILLER_PAD_X + MILLER_CHECKBOX_SZ + 4.0;
        assert_eq!(row.icon_slot.x, icon_x);
    }

    #[test]
    fn test_miller_column_horizontal_layout() {
        let col_w = 250.0;
        let count = 4;
        let positions: Vec<f32> = (0..count).map(|i| i as f32 * col_w).collect();
        assert_eq!(positions, vec![0.0, 250.0, 500.0, 750.0]);
    }

    #[test]
    fn test_miller_bounds_clipping() {
        let col_bounds = Bounds {
            origin: point(px(100.0), px(50.0)),
            size: size(px(250.0), px(500.0)),
        };
        let viewport = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(200.0), px(600.0)),
        };
        let clipped = col_bounds.intersect(&viewport);
        assert_eq!(clipped.origin.x, px(100.0));
        assert_eq!(clipped.origin.y, px(50.0));
        assert_eq!(clipped.size.width, px(100.0));
        assert_eq!(clipped.size.height, px(500.0));
    }

    #[test]
    fn test_miller_visible_row_range() {
        let (start, end) = miller_visible_row_range(0.0, 100.0, 25.0, 50);
        assert_eq!(start, 0);
        assert_eq!(end, 6); // 100 / 25 = 4 + 2 overscan = 6

        let (start2, end2) = miller_visible_row_range(50.0, 100.0, 25.0, 50);
        assert_eq!(start2, 2); // 50 / 25 = 2
        assert_eq!(end2, 8); // (150 / 25) = 6 + 2 overscan = 8

        let (start3, end3) = miller_visible_row_range(0.0, 100.0, 25.0, 0);
        assert_eq!((start3, end3), (0, 0));
    }
}
