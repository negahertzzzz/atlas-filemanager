//! Digital Grove Compact List View Element for Atlas (`ListDgElement`).
//!
//! Architectural principles (Digital Grove / Pexer / Raddebugger):
//! 1. Stateless procedural generation: implements `gpui::Element`, `IntoElement`, `Styled`.
//! 2. Column-major compact layout (classic Windows Explorer "List" view):
//!    - Items fill `rows = list_row_count(viewport_h, pitch)` rows top to bottom, then wrap
//!      into the next column; the surface scrolls horizontally.
//!    - Single-cycle O(1) cell slot projection: `pos -> (row, col) -> (x, y)` without flexbox solver.
//!    - Cell internal layout sliced with `RectCut`:
//!      * Checkbox slot (when multi-selecting or on hover).
//!      * File icon slot (16px scaled).
//!      * Filename slot: UTF-8 binary search truncation (`shape_text_truncated`).
//! 3. Hardware GPU clipping layer:
//!    - Each cell or text label wrapped in `window.paint_layer(bounds, ...)` to ensure zero bleeding into neighbor cells.
//! 4. Drop targets & drag rehydration:
//!    - For every directory cell visible in the viewport, register `DragHoverTarget` with `DragHoverId::Folder(pane_id, entry.path.clone())`.
//!    - On prepaint, purge stale targets for this pane with `app.pending_drag_hover_targets.retain(|t| !matches!(t.id, DragHoverId::Folder(tab, _) if tab == pane_id))`.
//!    - Visual blinking/highlighting when `app.drag_hover.blink_id == Some(DragHoverId::Folder(pane_id, path))`.
//! 5. Interactions & Hover:
//!    - Cell hover highlight background and cursor pointer.
//!    - Click handling: single click select, shift-click range, ctrl-click toggle, double click to activate file/folder.
//!    - Right click opens context menu (`open_ctx_menu`).
//!    - Tooltip on hover (`entry_hover_tooltip`).
//! 6. State Hydration:
//!    - Reads directly from live `App` and `TabState` during OLE drag loops.

use crate::app::{App, GridHitModel};
use crate::ui::dg::cut::RectCut;
use crate::ui::drag::FileDragSource as _;
use crate::ui::drag_hover::{
    DragHoverCommand, DragHoverDropCommand, DragHoverId, DragHoverKind, DragHoverTarget,
};
use crate::ui::file_icons::FileIcon;
use crate::ui::glass::backdrop_tint;
use crate::ui::icons::AtlasIcon;
use crate::ui::layout_region::{
    layout_region, timed_layout_region, PANE_CRUMBS_TIMINGS, PANE_VIEWPORT_TIMINGS,
};
use crate::ui::root::{ctrl_scroll, pane_bounds_recorder, zone_recorder};
use crate::ui::selection_checkbox;
use crate::ui::surface::{
    paint_rounded, paint_svg_icon, shape_text_truncated, text_origin_y, Slot,
};

pub(crate) fn list_name_bounds(cell: Bounds<Pixels>, zoom: f32) -> Bounds<Pixels> {
    let name_x = (24.0 + 16.0 + 4.0) * zoom;
    Bounds {
        origin: point(cell.origin.x + px(name_x), cell.origin.y),
        size: size(
            px((f32::from(cell.size.width) - name_x - 8.0 * zoom).max(0.0)),
            cell.size.height,
        ),
    }
}
use atlas_model::file_entry::FileKind;
use atlas_model::pane::TabId;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, point, px, size, AnyElement, App as GpuiApp, Bounds, Context, Corners, DispatchPhase,
    Element, ElementId, Font, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Refineable,
    ShapedLine, SharedString, Style, StyleRefinement, Styled, TextAlign, WeakEntity, Window,
};
use gpui_component::{v_flex, ActiveTheme, ElementExt};
use std::path::PathBuf;
use std::sync::Arc;

pub const CELL_W: f32 = 220.0;
/// Horizontal inset between the pane card edge and the first/last column.
const LIST_GUTTER_X: f32 = 8.0;

/// Rows per column for the column-major list: as many whole cells as fit the
/// viewport height, never fewer than one.
#[inline]
pub fn list_row_count(viewport_h: f32, pitch: f32) -> usize {
    if pitch <= 0.0 || !viewport_h.is_finite() {
        return 1;
    }
    ((viewport_h / pitch).floor() as usize).max(1)
}

/// Single-cycle O(1) column-major projection: `(visible_pos) -> (row, col, x, y)`.
#[inline]
pub fn list_cell_projection(
    visible_pos: usize,
    rows: usize,
    cell_stride: f32,
    pitch: f32,
    pad_x: f32,
    pad_y: f32,
) -> (usize, usize, f32, f32) {
    let rows = rows.max(1);
    let row = visible_pos % rows;
    let col = visible_pos / rows;
    let x = pad_x + col as f32 * cell_stride;
    let y = pad_y + row as f32 * pitch;
    (row, col, x, y)
}

/// Slices a compact list cell using `RectCut` into checkbox, 16px icon, and filename slots.
#[derive(Clone, Copy, Debug)]
pub struct ListDgCellSlots {
    pub cell: Bounds<Pixels>,
    pub check_slot: Option<Slot>,
    pub icon_slot: Slot,
    pub name_slot: Slot,
}

pub fn list_cell_cut(
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    zoom: f32,
    show_checkbox: bool,
) -> ListDgCellSlots {
    let mut cut = RectCut::new(cell_x, cell_y, cell_w, cell_h);
    cut.pad(4.0 * zoom, 2.0 * zoom);

    let check_slot = if show_checkbox {
        let slot = cut.cut_left(16.0 * zoom);
        let _ = cut.cut_left(4.0 * zoom);
        Some(slot)
    } else {
        None
    };

    let icon_slot = cut.cut_left(16.0 * zoom);
    let _ = cut.cut_left(4.0 * zoom);

    let name_slot = cut.remaining();

    ListDgCellSlots {
        cell: Bounds {
            origin: point(px(cell_x), px(cell_y)),
            size: size(px(cell_w), px(cell_h)),
        },
        check_slot,
        icon_slot,
        name_slot,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ListDgColors {
    pub fg: Hsla,
    pub accent: Hsla,
    pub accent_fg: Hsla,
    pub hover: Hsla,
    pub muted: Hsla,
}

impl ListDgColors {
    pub fn from_theme(cx: &gpui::App) -> Self {
        let t = cx.theme();
        Self {
            fg: t.foreground,
            accent: t.accent,
            accent_fg: t.accent_foreground,
            hover: t.list_active,
            muted: t.muted_foreground,
        }
    }
}

pub struct ListDgElement {
    id: ElementId,
    pane_id: TabId,
    app: WeakEntity<App>,
    visible: Arc<Vec<usize>>,
    scroll: gpui::ScrollHandle,
    rows: usize,
    total_count: usize,
    cell_w: f32,
    cell_h: f32,
    cell_stride: f32,
    pitch: f32,
    zoom_scale: f32,
    colors: ListDgColors,
    font: Font,
    font_size: Pixels,
    style: StyleRefinement,
}

impl ListDgElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pane_id: TabId,
        app: WeakEntity<App>,
        visible: Arc<Vec<usize>>,
        scroll: gpui::ScrollHandle,
        rows: usize,
        total_count: usize,
        cell_w: f32,
        cell_h: f32,
        cell_stride: f32,
        pitch: f32,
        zoom_scale: f32,
        colors: ListDgColors,
        font: Font,
        font_size: Pixels,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from(format!("list-dg-{}", pane_id.0))),
            pane_id,
            app,
            visible,
            scroll,
            rows: rows.max(1),
            total_count,
            cell_w,
            cell_h,
            cell_stride,
            pitch,
            zoom_scale,
            colors,
            font,
            font_size,
            style: StyleRefinement::default(),
        }
    }
}

impl IntoElement for ListDgElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for ListDgElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

#[derive(Clone)]
struct RawCellData {
    pos: usize,
    entry_idx: usize,
    rel_x: f32,
    rel_y: f32,
    name: String,
    path: PathBuf,
    is_dir: bool,
    selected: bool,
    cursor: bool,
    hovered: bool,
    blinking: bool,
    editing: bool,
    dimmed: bool,
    show_cell_check: bool,
    icon: FileIcon,
}

#[derive(Clone)]
pub struct ListDgPreparedCell {
    interactive: bool,
    pub opacity: f32,
    pub pos: usize,
    pub entry_idx: usize,
    pub rel_x: f32,
    pub rel_y: f32,
    pub slots: ListDgCellSlots,
    pub line: ShapedLine,
    pub path: PathBuf,
    pub icon: FileIcon,
    pub is_dir: bool,
    pub selected: bool,
    pub selected_at: Option<std::time::Instant>,
    pub cursor: bool,
    pub hovered: bool,
    pub blinking: bool,
    pub editing: bool,
    pub dimmed: bool,
}

pub struct ListDgPrepaint {
    hitbox: Hitbox,
    cells: Vec<ListDgPreparedCell>,
}

impl Element for ListDgElement {
    type RequestLayoutState = Style;
    type PrepaintState = ListDgPrepaint;

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
        crate::perf::count("list_dg.layout");
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
        crate::perf::count("list_dg.prepaint");

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        // Slit/sub-pixel culling: if pane is collapsing/narrower than 12px, bypass text shaping & processing
        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return ListDgPrepaint {
                hitbox,
                cells: Vec::new(),
            };
        }

        let Some(app_entity) = self.app.upgrade() else {
            return ListDgPrepaint {
                hitbox,
                cells: Vec::new(),
            };
        };

        let pane_id = self.pane_id;
        let visible = self.visible.clone();
        let rows = self.rows;
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let cell_stride = self.cell_stride;
        let pitch = self.pitch;
        let zoom_scale = self.zoom_scale;
        let total_count = self.total_count;

        // The parent scroller applies this frame's offset before child prepaint, so
        // the live handle (not the render-time snapshot) decides what is painted.
        // Smooth scrolling therefore never exposes an unpainted strip.
        let tracked = self.scroll.bounds();
        let viewport_bounds = if tracked.size.width > px(1.0) && tracked.size.height > px(1.0) {
            tracked
        } else {
            bounds
        };
        let scroll_x = (-f32::from(self.scroll.offset().x)).max(0.0);
        let viewport_w = f32::from(viewport_bounds.size.width);
        let viewport_h = f32::from(viewport_bounds.size.height);
        let total_cols = total_count.div_ceil(rows);
        let (first_col, last_col) = crate::ui::dg::scroll::live_span(
            scroll_x,
            scroll_x + viewport_w,
            cell_stride,
            1,
            total_cols,
        );
        let start_idx = (first_col * rows).min(total_count);
        let end_idx = (last_col * rows).min(total_count).max(start_idx);

        let mut model = GridHitModel {
            presented: Vec::new(),
            bounds: viewport_bounds,
            pad_x: 0.0,
            pad_y: 0.0,
            scroll_x,
            scroll_y: 0.0,
            cell_w,
            cell_h,
            cols: total_cols.max(1),
            major_rows: rows,
            pitch_y: pitch,
            pitch_x: cell_stride,
            visible: visible.clone(),
            start_idx,
            end_idx,
            total_count,
            bands: Default::default(),
        };

        let content_mask = window.content_mask().bounds;

        // Hydrate state directly from App and TabState
        let has_gpui_drag = cx.has_active_drag();
        let mut raw_cells = app_entity.update(cx, |app, _| {
            // Register GridHitModel for hit testing
            app.list_hit.insert(pane_id, model.clone());

            // Record row rects for selection and OLE drag bounds
            let row_rects: Vec<(usize, Bounds<Pixels>)> = (start_idx..end_idx)
                .filter_map(|pos| {
                    visible
                        .get(pos)
                        .copied()
                        .filter(|entry_idx| !atlas_model::grouping::is_virtual_cell(*entry_idx))
                        .map(|entry_idx| (entry_idx, model.cell_bounds(pos)))
                })
                .collect();
            app.row_rects.borrow_mut().insert(pane_id, row_rects);

            let is_drag_active = app.is_drag_active(has_gpui_drag);
            if is_drag_active {
                // Drop targets & drag rehydration:
                // 1. Purge stale targets for this pane
                app.pending_drag_hover_targets
                    .retain(|t| !matches!(t.id, DragHoverId::Folder(tab, _) if tab == pane_id));
            } else if !app.pending_drag_hover_targets.is_empty() {
                app.pending_drag_hover_targets
                    .retain(|t| !matches!(t.id, DragHoverId::Folder(tab, _) if tab == pane_id));
            }

            let Some(tab_state) = app.state.tab_states.get(&pane_id) else {
                return Vec::new();
            };

            let show_checkboxes = tab_state.selection.selected.len() > 1;

            // 2. Collect visible folder targets only if drag is active
            let mut folder_targets = Vec::new();
            if is_drag_active {
                for pos in start_idx..end_idx {
                    if let Some(&entry_idx) = visible.get(pos) {
                        if let Some(entry) = tab_state.entries.get(entry_idx) {
                            if matches!(entry.kind(), FileKind::Dir) {
                                let cell_b = model.cell_bounds(pos);
                                folder_targets.push(DragHoverTarget {
                                    id: DragHoverId::Folder(pane_id, entry.path()),
                                    bounds: cell_b
                                        .intersect(&viewport_bounds)
                                        .intersect(&content_mask),
                                    kind: DragHoverKind::Folder,
                                    command: DragHoverCommand::NavigatePathIn(
                                        pane_id,
                                        entry.path(),
                                    ),
                                    drop_command: Some(DragHoverDropCommand::TransferTo(
                                        entry.path(),
                                    )),
                                });
                            }
                        }
                    }
                }
            }

            // Collect raw cell details for text shaping
            let mut items = Vec::with_capacity(end_idx.saturating_sub(start_idx));
            for pos in start_idx..end_idx {
                let Some(&entry_idx) = visible.get(pos) else {
                    continue;
                };
                let Some(entry) = tab_state.entries.get(entry_idx) else {
                    continue;
                };

                let (_, _, rel_x, rel_y) =
                    list_cell_projection(pos, rows, cell_stride, pitch, 0.0, 0.0);
                let is_dir = matches!(entry.kind(), FileKind::Dir);
                let selected = tab_state.selection.selected.contains(&entry_idx);
                let cursor = tab_state.selection.keyboard_cursor == Some(entry_idx);
                let hovered = app.list_hover == Some((pane_id, entry_idx));
                let blinking = is_dir
                    && app.drag_hover.blink_id.as_ref()
                        == Some(&DragHoverId::Folder(pane_id, entry.path()));
                let editing =
                    crate::ui::views::rename::active_input(app, pane_id, entry_idx).is_some();
                let dimmed = crate::ui::views::rename::dim_other_entry(app, pane_id, entry_idx);
                // Keep names aligned. Hover must never insert a control or shift a file.
                let show_cell_check = show_checkboxes;
                let icon = app.row_icon_for_entry(
                    &entry.path(),
                    &entry.kind(),
                    entry.extension().as_deref(),
                );

                items.push(RawCellData {
                    pos,
                    entry_idx,
                    rel_x,
                    rel_y,
                    name: entry.name.to_string(),
                    path: entry.path(),
                    is_dir,
                    selected,
                    cursor,
                    hovered,
                    blinking,
                    editing,
                    dimmed,
                    show_cell_check,
                    icon,
                });
            }

            for target in folder_targets {
                app.register_drag_hover_target(target);
            }

            items
        });

        let now = std::time::Instant::now();
        let (epoch, instant) = {
            let app = app_entity.read(cx);
            (
                app.state.tab_states.get(&pane_id).map_or(0, |ts| {
                    super::details_element::row_content_epoch(ts, app.type_filters.get(&pane_id))
                }),
                cx.reduce_motion()
                    || app.rubber.is_some()
                    || app.rename_edit.is_some()
                    || app.is_drag_active(has_gpui_drag),
            )
        };
        let epoch = crate::ui::dg::motion::presentation_epoch(
            epoch,
            &[viewport_h, cell_w, cell_h, rows as f32],
        );
        let viewport = [scroll_x, cell_h, viewport_w];
        let motion = window.use_keyed_state(
            SharedString::from(format!("list-cell-motion-{}", pane_id.0)),
            cx,
            |_, _| crate::ui::dg::motion::CellMotion::<PathBuf>::new(epoch, viewport),
        );
        let targets: Vec<_> = raw_cells
            .iter()
            .map(|c| (c.path.clone(), c.pos, c.rel_x, c.rel_y))
            .collect();
        let (positions, moving) = motion.update(cx, |state, _| {
            state.sample(&targets, epoch, viewport, instant, now)
        });
        for (cell, (_, x, y)) in raw_cells.iter_mut().zip(positions) {
            cell.rel_x = x;
            cell.rel_y = y;
        }
        model.presented = raw_cells
            .iter()
            .map(|c| {
                (
                    c.pos,
                    Bounds {
                        origin: point(bounds.origin.x + px(c.rel_x), bounds.origin.y + px(c.rel_y)),
                        size: size(px(cell_w), px(cell_h)),
                    },
                )
            })
            .collect();
        app_entity.update(cx, |app, view_cx| {
            app.row_rects.borrow_mut().insert(
                pane_id,
                raw_cells
                    .iter()
                    .zip(&model.presented)
                    .map(|(cell, (_, b))| (cell.entry_idx, *b))
                    .collect(),
            );
            app.list_hit.insert(pane_id, model);
            // Smooth wheel frames move content under a stationary marquee.
            app.refresh_rubber_for_pane(pane_id, view_cx);
        });
        if moving {
            window.request_animation_frame();
        }

        let feedback = window.use_keyed_state(
            SharedString::from(format!("list-selection-feedback-{}", pane_id.0)),
            cx,
            |_, _| selection_checkbox::SelectionFeedback::<PathBuf>::default(),
        );
        let selection: Vec<_> = raw_cells
            .iter()
            .map(|cell| (cell.path.clone(), cell.selected && cell.show_cell_check))
            .collect();
        let selected_at = feedback.update(cx, |state, _| {
            state.sample(&selection, std::time::Instant::now())
        });

        // Fast UTF-8 binary search truncation text shaping using window
        let mut cells = Vec::with_capacity(raw_cells.len());
        for (raw, selected_at) in raw_cells.into_iter().zip(selected_at) {
            // The checkbox replaces the icon (same rule as Details): starting a
            // multi-selection never shifts names sideways.
            let slots = list_cell_cut(raw.rel_x, raw.rel_y, cell_w, cell_h, zoom_scale, false);
            let name_color = if raw.selected && !raw.editing {
                self.colors.accent_fg
            } else {
                self.colors.fg
            };
            let line = shape_text_truncated(
                &raw.name,
                &self.font,
                self.font_size,
                name_color,
                slots.name_slot.w,
                window,
            );

            cells.push(ListDgPreparedCell {
                interactive: true,
                opacity: 1.0,
                pos: raw.pos,
                entry_idx: raw.entry_idx,
                rel_x: raw.rel_x,
                rel_y: raw.rel_y,
                slots,
                line,
                path: raw.path,
                icon: raw.icon,
                is_dir: raw.is_dir,
                selected: raw.selected,
                selected_at,
                cursor: raw.cursor,
                hovered: raw.hovered,
                blinking: raw.blinking,
                editing: raw.editing,
                dimmed: raw.dimmed,
            });
        }

        let presence = window.use_keyed_state(
            SharedString::from(format!("list-cell-presence-{}", pane_id.0)),
            cx,
            |_, _| crate::ui::dg::motion::PresenceMotion::<PathBuf, ListDgPreparedCell>::new(),
        );
        let epoch = crate::ui::dg::motion::presentation_epoch(epoch, &viewport);
        let (samples, active) = presence.update(cx, |s, _| {
            s.sample(
                cells.into_iter().map(|c| (c.path.clone(), c)).collect(),
                epoch,
                instant,
                now,
            )
        });
        let cells = samples
            .into_iter()
            .map(|s| {
                let mut c = s.value;
                c.opacity = s.opacity;
                c.interactive = s.interactive;
                if !s.interactive {
                    c.hovered = false;
                    c.cursor = false;
                }
                c
            })
            .collect();
        if active {
            window.request_animation_frame();
        }
        ListDgPrepaint { hitbox, cells }
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
        crate::perf::count("list_dg.paint");

        if bounds.size.width < px(12.0)
            || bounds.size.height < px(12.0)
            || prepaint.cells.is_empty()
        {
            return;
        }

        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let zoom = self.zoom_scale;

        // Render visible cells with GPU hardware clipping layer
        for cell in &prepaint.cells {
            let abs_x = bounds.origin.x + px(cell.rel_x);
            let abs_y = bounds.origin.y + px(cell.rel_y);
            let cell_bounds = Bounds {
                origin: point(abs_x, abs_y),
                size: size(px(cell_w), px(cell_h)),
            };

            // Principle 3: Hardware GPU clipping layer ensures zero bleeding into neighbor cells
            window.with_element_opacity(
                Some(
                    cell.opacity
                        * crate::ui::views::rename::cut_path_opacity(&self.app, &cell.path, cx),
                ),
                |window| {
                    window.paint_layer(cell_bounds, |window| {
                        // 1. Hover/selection/blinking background highlight
                        let bg = if cell.selected && !cell.editing {
                            Some(self.colors.accent)
                        } else if cell.blinking {
                            Some(self.colors.accent.opacity(0.38))
                        } else if cell.hovered {
                            Some(self.colors.hover)
                        } else {
                            None
                        };

                        if let Some(color) = bg {
                            paint_rounded(window, cell_bounds, 4.0 * zoom, color);
                        }
                        if cell.interactive {
                            crate::ui::operation_feedback::paint_file_feedback(
                                &self.app,
                                &cell.path,
                                cell_bounds,
                                self.colors.accent,
                                4.0 * zoom,
                                window,
                                cx,
                            );
                        }

                        // Focus cursor outline
                        if cell.cursor && !cell.selected {
                            let outline_color =
                                self.colors
                                    .accent
                                    .opacity(if cell.selected { 0.95 } else { 0.65 });
                            window.paint_quad(
                                gpui::outline(cell_bounds, outline_color, Default::default())
                                    .corner_radii(Corners::all(px(4.0 * zoom))),
                            );
                        }

                        // Dimming when another item is being renamed
                        window.with_element_opacity(
                            cell.dimmed.then_some(crate::ui::views::rename::DIM_OPACITY),
                            |window| {
                                // 2/3. Icon slot: during a multi-selection the shared
                                // checkbox takes the icon's place on selected items
                                // (`selected_at` is set exactly while it is shown).
                                let icon_slot = cell.slots.icon_slot;
                                let icon_sz = 16.0 * zoom;
                                let icon_bounds = Bounds {
                                    origin: point(
                                        bounds.origin.x
                                            + px(icon_slot.x + (icon_slot.w - icon_sz) * 0.5),
                                        bounds.origin.y
                                            + px(icon_slot.y + (icon_slot.h - icon_sz) * 0.5),
                                    ),
                                    size: size(px(icon_sz), px(icon_sz)),
                                };

                                if cell.selected && !cell.editing && cell.selected_at.is_some() {
                                    selection_checkbox::paint_animated(
                                        window,
                                        cx,
                                        icon_bounds.center(),
                                        selection_checkbox::ROW_SIZE * zoom,
                                        cell.selected_at,
                                    );
                                } else if let Some(tex) = cell.icon.texture_for_size(icon_sz) {
                                    let _ = window.paint_image(
                                        icon_bounds,
                                        icon_bounds,
                                        Corners::default(),
                                        tex,
                                        0,
                                        false,
                                    );
                                } else {
                                    let icon = if cell.is_dir {
                                        AtlasIcon::Folder
                                    } else {
                                        AtlasIcon::File
                                    };
                                    let icon_color = if cell.selected && !cell.editing {
                                        self.colors.accent_fg
                                    } else {
                                        self.colors.accent
                                    };
                                    paint_svg_icon(
                                        window,
                                        cx,
                                        icon_bounds,
                                        icon.path(),
                                        icon_color,
                                    );
                                }

                                // 4. Filename slot: UTF-8 binary search truncation (`shape_text_truncated`)
                                if !cell.editing {
                                    let name_slot = cell.slots.name_slot;
                                    let text_x = bounds.origin.x + px(name_slot.x);
                                    let text_y =
                                        bounds.origin.y + px(text_origin_y(name_slot.y, zoom));
                                    let name_bounds = Bounds {
                                        origin: point(text_x, bounds.origin.y + px(name_slot.y)),
                                        size: size(px(name_slot.w), px(name_slot.h)),
                                    };
                                    window.paint_layer(name_bounds, |window| {
                                        let _ = cell.line.paint(
                                            point(text_x, text_y),
                                            px(name_slot.h),
                                            TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                    });
                                }
                            },
                        );
                    });
                },
            );
        }

        let Some(app_entity) = self.app.upgrade() else {
            return;
        };

        let pane_id = self.pane_id;

        // Set cursor pointer if hovered over any cell
        if prepaint.hitbox.is_hovered(window) {
            let pos = window.mouse_position();
            let has_hit = app_entity
                .read(cx)
                .list_hit
                .get(&pane_id)
                .and_then(|m| m.hit_entry(pos))
                .is_some();
            if has_hit {
                window.set_cursor_style(gpui::CursorStyle::PointingHand, &prepaint.hitbox);
            }
        }

        // MouseMove for hover highlight & marquee tooltip
        let move_view = app_entity.clone();
        let move_hitbox = prepaint.hitbox.clone();
        window.on_mouse_event::<MouseMoveEvent>(move |ev, phase, window, cx| {
            if phase != DispatchPhase::Bubble || ev.dragging() {
                return;
            }
            let pos = ev.position;
            move_view.update(cx, |this, cx| {
                let hover_entry = if move_hitbox.is_hovered(window) {
                    this.list_hit
                        .get(&pane_id)
                        .and_then(|m| m.hit_entry(pos))
                        .map(|idx| (pane_id, idx))
                } else {
                    None
                };
                if this.list_hover != hover_entry {
                    this.list_hover = hover_entry;
                    gpui_component::tooltip::Tooltip::dismiss(window, cx);
                    this.notify_pane(pane_id, cx);
                }
            });
        });

        // MouseDown for selection, range, toggle, double-click activate, right-click context menu
        let down_view = app_entity;
        let down_hitbox = prepaint.hitbox.clone();
        let (check_cell_w, check_cell_h) = (self.cell_w, self.cell_h);
        // Cells currently showing the checkbox (it replaces the icon).
        let check_cells: Vec<usize> = prepaint
            .cells
            .iter()
            .filter(|cell| cell.interactive && cell.selected && cell.selected_at.is_some())
            .map(|cell| cell.pos)
            .collect();
        window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let pos = ev.position;
            let is_in_surface = {
                let app = down_view.read(cx);
                app.list_hit
                    .get(&pane_id)
                    .is_some_and(|m| m.bounds.contains(&pos))
            };
            if !down_hitbox.is_hovered(window) || !is_in_surface {
                return;
            }

            down_view.update(cx, |this, cx| {
                let hit = this.list_hit.get(&pane_id).and_then(|m| m.hit_entry(pos));
                // Clicking a visible checkbox toggles that item (Ctrl semantics)
                // instead of collapsing the multi-selection.
                let on_check = this.list_hit.get(&pane_id).is_some_and(|m| {
                    m.hit_visible_pos(pos).is_some_and(|visible_pos| {
                        check_cells.contains(&visible_pos) && {
                            let cell = m.cell_bounds(visible_pos);
                            let icon = list_cell_cut(
                                f32::from(cell.left()),
                                f32::from(cell.top()),
                                check_cell_w,
                                check_cell_h,
                                zoom,
                                false,
                            )
                            .icon_slot;
                            icon.contains(f32::from(pos.x), f32::from(pos.y))
                        }
                    })
                });
                match ev.button {
                    MouseButton::Left => this.details_left_mouse_down(
                        pane_id,
                        hit,
                        pos,
                        ev.modifiers.shift && !on_check,
                        ev.modifiers.control || ev.modifiers.platform || on_check,
                        ev.click_count,
                        cx,
                    ),
                    MouseButton::Right => {
                        this.details_right_mouse_down(pane_id, hit, pos, window, cx);
                    }
                    _ => {}
                }
            });
            if matches!(ev.button, MouseButton::Left | MouseButton::Right) {
                cx.stop_propagation();
            }
        });
    }
}

/// Public render entrypoint for Atlas: renders the compact List view via Digital Grove architecture.
pub fn render_list_view_dg(
    app: &mut App,
    pane_id: TabId,
    window: &mut Window,
    cx: &mut Context<App>,
) -> AnyElement {
    app.render_list_view_dg(pane_id, window, cx)
}

impl App {
    /// Public render entrypoint on `App` for Digital Grove compact list view.
    pub fn render_list_view_dg(
        &mut self,
        pane_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        crate::perf::count("render.view.list_dg");
        let view = cx.entity();
        let is_glass = self.state.profiles.active().theme.ends_with(" Glass");
        let (fg, accent, accent_fg, hover, muted, border, header_bg) = {
            let t = cx.theme();
            (
                t.foreground,
                t.accent,
                t.accent_foreground,
                t.list_active,
                t.muted_foreground,
                t.border,
                if is_glass {
                    t.list_head
                } else {
                    backdrop_tint(t.secondary, 0.18)
                },
            )
        };
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let row_font = window.text_style().font();
        let crumb_strip = self.render_crumb_strip(pane_id, &view, fg, muted, border, header_bg, cx);
        let count = self.state.tab_states.get_mut(&pane_id).map_or(0, |ts| {
            ts.visible_indices();
            // Collapsed groups hide every file but still show their headers.
            ts.visible.len()
        });

        if count == 0 {
            self.list_hit.remove(&pane_id);
            self.row_rects.borrow_mut().remove(&pane_id);
            if self
                .list_hover
                .is_some_and(|(hover_pane, _)| hover_pane == pane_id)
            {
                self.list_hover = None;
            }
            let msg = self.empty_pane_message(pane_id, rust_i18n::t!("empty_folder").into_owned());
            // Empty folders still accept drops and offer the background menu.
            return crate::ui::views::pane_background_interactions(
                v_flex()
                    .id(("pane", pane_id.0))
                    .size_full()
                    .overflow_hidden(),
                &view,
                pane_id,
            )
            .when(cfg!(windows), |el| {
                el.on_prepaint_child(zone_recorder(pane_id, &view))
            })
            .child(timed_layout_region(
                "pane.crumbs",
                PANE_CRUMBS_TIMINGS,
                crumb_strip,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_color(muted)
                    .on_prepaint_child(pane_bounds_recorder(pane_id, &view))
                    .children(msg),
            )
            .into_any_element();
        }

        let scroll = self.scroll_handle_for(pane_id);
        let base_scroll = scroll.0.borrow().base_handle.clone();
        // Column-major List scrolls horizontally only; the trailing overscroll
        // margin of the vertical views would add a stray vertical scroll range.
        base_scroll.set_bottom_scroll_margin(0.0);

        let zoom_scale = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let cell_stride = CELL_W * zoom_scale + 6.0 * zoom_scale;
        let cell_h_px = f32::from(window.line_height()).max(16.0 * zoom_scale) + 4.0 * zoom_scale;
        let pitch = cell_h_px;
        let viewport = base_scroll.bounds();
        let viewport_h = {
            let tracked = f32::from(viewport.size.height);
            if tracked > 1.0 {
                tracked
            } else {
                (f32::from(window.viewport_size().height) - 120.0).max(pitch)
            }
        };
        let viewport_w = {
            let tracked = f32::from(viewport.size.width);
            if tracked > 1.0 {
                tracked
            } else {
                self.state
                    .tab_states
                    .get(&pane_id)
                    .map(|ts| ts.last_pane_width)
                    .filter(|&w| w > 1.0)
                    .unwrap_or_else(|| (f32::from(window.viewport_size().width) - 217.0).max(0.0))
            }
        };
        let rows = list_row_count(viewport_h, pitch);
        // Each group opens a fresh column topped by its header cell.
        let group_cells =
            self.grouped_cells(pane_id, atlas_model::grouping::GroupFlow::Columns { rows });
        let visible_arc = match &group_cells {
            Some(groups) => groups.cells.clone(),
            None => self
                .state
                .tab_states
                .get(&pane_id)
                .map(|ts| ts.visible.clone())
                .unwrap_or_default(),
        };
        let count = visible_arc.len();
        let total_cols = count.div_ceil(rows);
        // One wheel notch (three lines) advances one column smoothly, like Explorer.
        // Divide by boost so the effective eased travel lands squarely on the column boundary.
        let (_, boost) = crate::app::scroll_smooth_params(self.state.settings.scroll_smoothness);
        base_scroll.set_line_scroll_height(px((cell_stride / 3.0) / boost.max(1.0)));
        let content_w = total_cols as f32 * cell_stride;
        let overscroll_margin = if content_w > viewport_w {
            20.0 * zoom_scale
        } else {
            0.0
        };
        let max_scroll = (content_w + overscroll_margin - viewport_w).max(0.0);
        crate::ui::dg::scroll::apply_deferred(&scroll, false, viewport_w, max_scroll, |pos| {
            (pos < count).then(|| ((pos / rows) as f32 * cell_stride, CELL_W * zoom_scale))
        });
        let mut scroll_x = (-f32::from(base_scroll.offset().x)).max(0.0);
        if scroll_x > max_scroll {
            let offset = base_scroll.offset();
            base_scroll.set_offset(gpui::point(gpui::px(-max_scroll), offset.y));
            scroll_x = max_scroll;
        }

        // Icons for the columns around the viewport; prepaint paints the exact
        // live span, so this only needs to be a close superset.
        let ahead_cols = ((viewport_w / cell_stride).ceil() as usize).max(2);
        let first_col = ((scroll_x / cell_stride).floor() as usize).saturating_sub(ahead_cols);
        let last_col = ((scroll_x + viewport_w) / cell_stride).ceil() as usize + ahead_cols;
        let start = (first_col * rows).min(count);
        let end = (last_col * rows).min(count).max(start);
        let missing_icons = {
            let mut m = Vec::new();
            if let Some(ts) = self.state.tab_states.get(&pane_id) {
                for i in start..end {
                    if let Some(&idx) = visible_arc.get(i) {
                        if let Some(e) = ts.entries.get(idx) {
                            let fref = crate::ui::file_icons::FileIconRef {
                                path: e.path(),
                                kind: e.kind().clone(),
                                extension: e
                                    .extension()
                                    .as_ref()
                                    .map(|s| gpui::SharedString::from(s.to_string())),
                            };
                            m.push(fref);
                        }
                    }
                }
            }
            m
        };
        for fref in missing_icons {
            self.maybe_request_file_icon(&fref);
        }

        let surface_colors = ListDgColors {
            fg,
            accent,
            accent_fg,
            hover,
            muted,
        };

        let surface = ListDgElement::new(
            pane_id,
            view.downgrade(),
            visible_arc.clone(),
            base_scroll.clone(),
            rows,
            count,
            CELL_W * zoom_scale,
            cell_h_px,
            cell_stride,
            pitch,
            zoom_scale,
            surface_colors,
            row_font.clone(),
            font_size,
        )
        .absolute()
        .inset_0()
        .into_any_element();

        let mut rename_overlay = None;
        if let Some(ts) = self.state.tab_states.get(&pane_id) {
            for visible_pos in start..end {
                let Some(&entry_idx) = visible_arc.get(visible_pos) else {
                    continue;
                };
                let Some(rename_input) =
                    crate::ui::views::rename::active_input(self, pane_id, entry_idx)
                else {
                    continue;
                };
                let (_, _, cell_x, cell_y) =
                    list_cell_projection(visible_pos, rows, cell_stride, pitch, 0.0, 0.0);
                let cell_bounds = Bounds {
                    origin: point(px(cell_x), px(cell_y)),
                    size: size(px(CELL_W * zoom_scale), px(cell_h_px)),
                };
                let label = list_name_bounds(cell_bounds, zoom_scale);
                rename_overlay = Some(
                    div()
                        .absolute()
                        .left(label.origin.x)
                        .top(label.origin.y)
                        .w(label.size.width)
                        .h(label.size.height)
                        .text_color(accent_fg)
                        .child(crate::ui::views::rename::input_element(
                            &rename_input,
                            &view,
                            label.size.height,
                            font_size,
                            window,
                            cx,
                        ))
                        .into_any_element(),
                );
                break;
            }
        }

        let drag_layer = div()
            .id(("list-dg-drag-layer", pane_id.0))
            .absolute()
            .top_0()
            .left_0()
            .w(px(content_w))
            .h_full()
            .opacity(0.0)
            .on_file_drag_list(view.clone(), pane_id)
            .into_any_element();

        let tooltip_data = self
            .list_hover
            .filter(|_| self.ctx_menu.is_none())
            .filter(|(hover_pane, _)| *hover_pane == pane_id)
            .and_then(|(_, entry_idx)| {
                self.state
                    .tab_states
                    .get(&pane_id)
                    .and_then(|ts| ts.entries.get(entry_idx))
                    .map(|entry| {
                        (
                            entry.name.to_string(),
                            entry.path(),
                            matches!(entry.kind(), FileKind::Dir),
                            entry.size(),
                            entry.modified,
                        )
                    })
            });

        let group_overlays = group_cells.as_ref().map_or_else(Vec::new, |groups| {
            let colors = crate::ui::group_header::GroupHeaderColors { fg, muted, border };
            let gap = 6.0 * zoom_scale;
            groups
                .headers
                .iter()
                .map(|header| {
                    let (_, _, x, y) =
                        list_cell_projection(header.cell, rows, cell_stride, pitch, 0.0, 0.0);
                    crate::ui::group_header::group_header(
                        pane_id,
                        header,
                        &view,
                        colors,
                        (
                            px(x),
                            px(y),
                            Some(px(header.span as f32 * cell_stride - gap)),
                            px(cell_h_px),
                        ),
                    )
                })
                .collect::<Vec<_>>()
        });
        let scroll_view = view.clone();
        let inner = div()
            .id(("anim-list-dg-scroll", pane_id.0))
            .size_full()
            .overflow_x_scroll()
            .track_scroll(&base_scroll)
            .on_mouse_down(
                MouseButton::Left,
                crate::ui::views::on_pane_rubber_mouse_down(&view, pane_id),
            )
            .on_scroll_wheel(move |ev, window, cx| {
                if ev.modifiers.control || ev.modifiers.platform {
                    ctrl_scroll(ev, pane_id, &scroll_view, window, cx);
                }
            })
            .when_some(tooltip_data, |el, (name, path, is_dir, size, modified)| {
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
            })
            .child(
                div()
                    .w(px(content_w))
                    .h_full()
                    .relative()
                    .child(surface)
                    .when_some(rename_overlay, |el, overlay| el.child(overlay))
                    .child(drag_layer)
                    .children(group_overlays),
            )
            .into_any_element();

        let zone_view = view.clone();
        let reflow_view = view.clone();
        let bounds_view = view.clone();
        let pane_view = view;
        crate::ui::views::pane_background_interactions(
            v_flex()
                .id(("pane", pane_id.0))
                .size_full()
                .overflow_hidden(),
            &pane_view,
            pane_id,
        )
        .when(cfg!(windows), |el| {
            el.on_prepaint_child(zone_recorder(pane_id, &zone_view))
        })
        .child(timed_layout_region(
            "pane.crumbs",
            PANE_CRUMBS_TIMINGS,
            crumb_strip,
        ))
        .child(timed_layout_region(
            "pane.viewport",
            PANE_VIEWPORT_TIMINGS,
            div()
                .flex_1()
                .min_h(px(0.))
                .relative()
                // Side gutter: columns never touch (or get clipped by) the card edge.
                // The scroller is inset, so hit-testing follows its own bounds.
                .px(px(LIST_GUTTER_X * zoom_scale))
                .rounded_bl(px(12.0))
                .rounded_br(px(12.0))
                .on_prepaint_child(pane_bounds_recorder(pane_id, &bounds_view))
                .on_prepaint_child(move |bounds: gpui::Bounds<gpui::Pixels>, _window, cx| {
                    crate::perf::count("prepaint.list_dg_pane");
                    // Rows per column follow the viewport height; re-render the
                    // pane when a resize changes the column-major flow.
                    let new_w = f32::from(bounds.size.width);
                    let new_rows = list_row_count(f32::from(bounds.size.height), pitch);
                    if new_rows != rows {
                        reflow_view.update(cx, |this, cx| {
                            if let Some(ts) = this.state.tab_states.get_mut(&pane_id) {
                                ts.last_pane_width = new_w;
                            }
                            if this.is_pane_or_dock_animating() {
                                this.defer_column_reflow(pane_id);
                            } else {
                                this.notify_pane(pane_id, cx);
                            }
                        });
                    }
                })
                .child(layout_region("pane.viewport_body", inner)),
        ))
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_row_count_fits_whole_cells_and_never_drops_below_one() {
        assert_eq!(list_row_count(239.9, 24.0), 9);
        assert_eq!(list_row_count(240.0, 24.0), 10);
        assert_eq!(list_row_count(0.0, 24.0), 1);
        assert_eq!(list_row_count(100.0, 0.0), 1);
    }

    #[test]
    fn test_column_major_coordinate_projection() {
        let rows = 3;
        let cell_stride = 226.0;
        let pitch = 24.0;

        // Items flow down the first column, then wrap into the next one.
        assert_eq!(
            list_cell_projection(0, rows, cell_stride, pitch, 0.0, 0.0),
            (0, 0, 0.0, 0.0)
        );
        assert_eq!(
            list_cell_projection(1, rows, cell_stride, pitch, 0.0, 0.0),
            (1, 0, 0.0, 24.0)
        );
        assert_eq!(
            list_cell_projection(2, rows, cell_stride, pitch, 0.0, 0.0),
            (2, 0, 0.0, 48.0)
        );
        assert_eq!(
            list_cell_projection(3, rows, cell_stride, pitch, 0.0, 0.0),
            (0, 1, 226.0, 0.0)
        );
        assert_eq!(
            list_cell_projection(7, rows, cell_stride, pitch, 0.0, 0.0),
            (1, 2, 452.0, 24.0)
        );
    }

    #[test]
    fn test_column_major_hit_model_round_trips_and_rejects_gaps() {
        let model = GridHitModel {
            presented: Vec::new(),
            bounds: Bounds {
                origin: point(px(10.0), px(20.0)),
                size: size(px(500.0), px(72.0)),
            },
            pad_x: 0.0,
            pad_y: 0.0,
            scroll_x: 226.0,
            scroll_y: 0.0,
            cell_w: 220.0,
            cell_h: 20.0,
            cols: 3,
            major_rows: 3,
            pitch_y: 24.0,
            pitch_x: 226.0,
            visible: Arc::new((100..108).collect()),
            start_idx: 0,
            end_idx: 8,
            total_count: 8,
            bands: Default::default(),
        };
        // Scrolled one column: the left edge shows column 1 (positions 3..6).
        assert_eq!(model.hit_entry(point(px(15.0), px(25.0))), Some(103));
        assert_eq!(model.hit_entry(point(px(15.0), px(49.0))), Some(104));
        // Horizontal and vertical gaps are background.
        assert_eq!(model.hit_entry(point(px(233.0), px(25.0))), None);
        assert_eq!(model.hit_entry(point(px(15.0), px(42.0))), None);
        // Position 8 does not exist.
        assert_eq!(model.hit_entry(point(px(241.0), px(73.0))), None);
        // cell_bounds is the inverse of the hit test.
        for pos in 3..8 {
            let b = model.cell_bounds(pos);
            assert_eq!(
                model.hit_visible_pos(point(b.left() + px(1.0), b.top() + px(1.0))),
                Some(pos)
            );
        }
        assert_eq!(model.slot(7), (1, 2));
    }

    #[test]
    fn test_rect_cut_compact_cell_layout_without_checkbox() {
        let zoom = 1.0;
        let cell_x = 10.0;
        let cell_y = 20.0;
        let cell_w = 220.0;
        let cell_h = 24.0;

        let slots = list_cell_cut(cell_x, cell_y, cell_w, cell_h, zoom, false);

        assert!(slots.check_slot.is_none());

        // Cell padded by 4px on left/right: remaining x starts at 10 + 4 = 14.0
        assert_eq!(slots.icon_slot.x, 14.0);
        assert_eq!(slots.icon_slot.w, 16.0);
        assert_eq!(slots.icon_slot.h, 20.0); // 24 - 2*2

        // Icon slot is 16px + 4px gap = 20px advance
        assert_eq!(slots.name_slot.x, 34.0);
        // Remaining width: 220 - 8 (pad) - 16 (icon) - 4 (gap) = 192.0
        assert_eq!(slots.name_slot.w, 192.0);
        assert_eq!(slots.name_slot.h, 20.0);
    }

    #[test]
    fn test_rect_cut_compact_cell_layout_with_checkbox() {
        let zoom = 1.0;
        let cell_x = 0.0;
        let cell_y = 0.0;
        let cell_w = 220.0;
        let cell_h = 24.0;

        let slots = list_cell_cut(cell_x, cell_y, cell_w, cell_h, zoom, true);

        let check = slots.check_slot.expect("checkbox slot cut");
        assert_eq!(check.x, 4.0);
        assert_eq!(check.w, 16.0);

        // Icon after check (16px) + gap (4px)
        assert_eq!(slots.icon_slot.x, 24.0);
        assert_eq!(slots.icon_slot.w, 16.0);

        // Name after icon (16px) + gap (4px)
        assert_eq!(slots.name_slot.x, 44.0);
        // Remaining: 220 - 8 (pad) - 20 (check+gap) - 20 (icon+gap) = 172.0
        assert_eq!(slots.name_slot.w, 172.0);
    }

    #[test]
    fn test_rect_cut_compact_cell_saturates_cleanly_on_narrow_bounds() {
        let zoom = 1.0;
        let cell_x = 0.0;
        let cell_y = 0.0;
        let cell_w = 30.0;
        let cell_h = 20.0;

        let slots = list_cell_cut(cell_x, cell_y, cell_w, cell_h, zoom, true);

        assert!(slots.check_slot.is_some());
        assert!(slots.icon_slot.w >= 0.0);
        assert!(slots.name_slot.w >= 0.0);
    }
}
