//! Digital Grove Grid (Icons) View Element for Atlas (`GridDgElement`).
//!
//! Architectural principles (Digital Grove / Pexer / Raddebugger):
//! 1. Stateless procedural generation: implements `gpui::Element`, `IntoElement`, `Styled`.
//! 2. Single-cycle O(1) arithmetic layout with `RectCut`:
//!    - Computes `cols = grid_column_count(pane_w, thumb_px, grid_gap_px, grid_pad_px)`.
//!    - Single-cycle O(1) cell slot projection: `(pos) -> (row, col, x, y)`.
//!    - Cell internal layout sliced with `RectCut`:
//!      * Checkbox slot (top-left, when multi-selecting or on hover).
//!      * Thumbnail / File icon slot (centered in the upper icon area, supporting texture thumbnails from `file_icon_for_entry` as well as fallback theme icons).
//!      * Filename slot: UTF-8 binary search truncation (`shape_text_truncated`).
//! 3. Hardware GPU clipping layer:
//!    - Each cell and text label wrapped in `window.paint_layer(bounds, ...)` to ensure zero bleeding into neighbor cells.
//! 4. 100% Drop Targets & Drag Rehydration:
//!    - For every directory cell visible in the viewport, register `DragHoverTarget` with `DragHoverId::Folder(pane_id, entry.path.clone())`.
//!    - On prepaint, purge stale targets for this pane with `app.pending_drag_hover_targets.retain(|t| !matches!(t.id, DragHoverId::Folder(tab, _) if tab == pane_id))`.
//!    - Visual blinking/highlighting when `app.drag_hover.blink_id == Some(DragHoverId::Folder(pane_id, path))`.
//! 5. Interactions & Hover:
//!    - Native GPUI hitbox registration (`insert_hitbox(bounds, HitboxBehavior::Normal)`).
//!    - Cell hover highlight background and cursor pointer.
//!    - Click handling: single click select, shift-click range, ctrl-click toggle, double click to activate file/folder.
//!    - Right click opens context menu (`open_ctx_menu`).
//!    - Tooltip on hover (`entry_hover_tooltip`).
//! 6. Rubber-band selection & drag-and-drop integration.

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
pub(crate) const GRID_GAP: f32 = 16.0;
pub(crate) const GRID_PAD: f32 = 12.0;
pub(crate) const GRID_ICON_AREA: f32 = 0.72;
pub(crate) const GRID_TEXTURE_ICON_MAX: f32 = 0.66;
pub(crate) const GRID_BUILTIN_ICON: f32 = 0.54;
pub(crate) const GRID_LABEL_H: f32 = 32.0;

pub(crate) fn grid_label_bounds(
    cell: Bounds<Pixels>,
    icon_area_h: f32,
    zoom: f32,
) -> Bounds<Pixels> {
    Bounds {
        origin: point(
            cell.origin.x + px(4.0 * zoom),
            cell.origin.y + px(icon_area_h),
        ),
        size: size(
            px((f32::from(cell.size.width) - 8.0 * zoom).max(0.0)),
            px(GRID_LABEL_H * zoom),
        ),
    }
}
use crate::ui::group_header::GROUP_BAND_H;
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
use gpui_component::scroll::ScrollableElement;
use gpui_component::{v_flex, ActiveTheme, ElementExt};
use std::path::PathBuf;
use std::sync::Arc;

/// Deterministic multi-column arithmetic for grid view.
/// Reserves 2 * pad horizontal padding before fitting columns of width `thumb` separated by `gap`.
#[inline]
pub fn grid_column_count(pane_w: f32, thumb: f32, gap: f32, pad: f32) -> usize {
    let inner_avail = (pane_w - 2.0 * pad).max(thumb);
    (((inner_avail + gap) / (thumb + gap)).floor() as usize).max(1)
}

/// Single-cycle O(1) cell slot projection: `(visible_pos) -> (row, col, x, y)`.
#[inline]
pub fn grid_cell_projection(
    visible_pos: usize,
    cols: usize,
    pitch_x: f32,
    pitch_y: f32,
    pad_x: f32,
    pad_y: f32,
) -> (usize, usize, f32, f32) {
    let cols = cols.max(1);
    let row = visible_pos / cols;
    let col = visible_pos % cols;
    let x = pad_x + col as f32 * pitch_x;
    let y = pad_y + row as f32 * pitch_y;
    (row, col, x, y)
}

/// Single-cycle O(1) reverse projection: `(lx, ly) -> Option<visible_pos>`.
#[inline]
pub fn grid_cell_hit_test(
    lx: f32,
    ly: f32,
    cols: usize,
    total_count: usize,
    cell_w: f32,
    cell_h: f32,
    pitch_x: f32,
    pitch_y: f32,
    pad_x: f32,
    pad_y: f32,
) -> Option<usize> {
    if cols == 0 || pitch_x <= 0.0 || pitch_y <= 0.0 {
        return None;
    }
    let rel_x = lx - pad_x;
    let rel_y = ly - pad_y;
    if rel_x < 0.0 || rel_y < 0.0 {
        return None;
    }
    let col = (rel_x / pitch_x).floor() as usize;
    let row = (rel_y / pitch_y).floor() as usize;
    if col >= cols {
        return None;
    }
    let intra_x = rel_x - col as f32 * pitch_x;
    let intra_y = rel_y - row as f32 * pitch_y;
    if intra_x >= cell_w || intra_y >= cell_h {
        return None;
    }
    let visible_pos = row * cols + col;
    if visible_pos < total_count {
        Some(visible_pos)
    } else {
        None
    }
}

/// Slices a grid cell using `RectCut` into checkbox, thumbnail/icon, and filename slots.
#[derive(Clone, Copy, Debug)]
pub struct GridDgCellSlots {
    pub cell: Bounds<Pixels>,
    pub check_slot: Option<Slot>,
    pub icon_slot: Slot,
    pub name_slot: Slot,
}

pub fn grid_cell_cut(
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    icon_area_h: f32,
    zoom: f32,
    show_checkbox: bool,
) -> GridDgCellSlots {
    let mut cut = RectCut::new(cell_x, cell_y, cell_w, cell_h);
    let icon_slot = cut.cut_top(icon_area_h);

    let check_slot = if show_checkbox {
        let mut check_cut = RectCut::new(icon_slot.x, icon_slot.y, icon_slot.w, icon_slot.h);
        check_cut.pad(4.0 * zoom, 4.0 * zoom);
        let top_strip = check_cut.cut_top(16.0 * zoom);
        let mut top_left = RectCut::new(top_strip.x, top_strip.y, top_strip.w, top_strip.h);
        Some(top_left.cut_left(16.0 * zoom))
    } else {
        None
    };

    let mut name_cut = cut;
    name_cut.pad(4.0 * zoom, 0.0);
    let name_slot = name_cut.remaining();

    GridDgCellSlots {
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
pub struct GridDgColors {
    pub fg: Hsla,
    pub accent: Hsla,
    pub accent_fg: Hsla,
    pub hover: Hsla,
    pub muted: Hsla,
}

impl GridDgColors {
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

pub struct GridDgElement {
    id: ElementId,
    pane_id: TabId,
    app: WeakEntity<App>,
    visible: Arc<Vec<usize>>,
    start_idx: usize,
    end_idx: usize,
    cols: usize,
    total_count: usize,
    cell_w: f32,
    cell_h: f32,
    pitch_x: f32,
    pitch_y: f32,
    pad_x: f32,
    pad_y: f32,
    thumb_px: f32,
    icon_area_h: f32,
    scroll_y: f32,
    viewport_h: f32,
    zoom_scale: f32,
    colors: GridDgColors,
    font: Font,
    font_size: Pixels,
    bands: atlas_model::grouping::RowBands,
    style: StyleRefinement,
}

impl GridDgElement {
    /// Group header bands between rows; empty when ungrouped.
    pub fn bands(mut self, bands: atlas_model::grouping::RowBands) -> Self {
        self.bands = bands;
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pane_id: TabId,
        app: WeakEntity<App>,
        visible: Arc<Vec<usize>>,
        start_idx: usize,
        end_idx: usize,
        cols: usize,
        total_count: usize,
        cell_w: f32,
        cell_h: f32,
        pitch_x: f32,
        pitch_y: f32,
        pad_x: f32,
        pad_y: f32,
        thumb_px: f32,
        icon_area_h: f32,
        scroll_y: f32,
        viewport_h: f32,
        zoom_scale: f32,
        colors: GridDgColors,
        font: Font,
        font_size: Pixels,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from(format!("grid-dg-{}", pane_id.0))),
            pane_id,
            app,
            visible,
            start_idx,
            end_idx,
            cols: cols.max(1),
            total_count,
            cell_w,
            cell_h,
            pitch_x,
            pitch_y,
            pad_x,
            pad_y,
            thumb_px,
            icon_area_h,
            scroll_y,
            viewport_h,
            zoom_scale,
            colors,
            font,
            font_size,
            bands: Default::default(),
            style: StyleRefinement::default(),
        }
    }
}

impl IntoElement for GridDgElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for GridDgElement {
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
    visual_w: f32,
    visual_h: f32,
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
pub struct GridDgPreparedCell {
    interactive: bool,
    pub texture_opacity: f32,
    pub opacity: f32,
    pub pos: usize,
    pub entry_idx: usize,
    pub rel_x: f32,
    pub rel_y: f32,
    pub visual_w: f32,
    pub visual_h: f32,
    pub slots: GridDgCellSlots,
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

pub struct GridDgPrepaint {
    hitbox: Hitbox,
    cells: Vec<GridDgPreparedCell>,
}

impl Element for GridDgElement {
    type RequestLayoutState = Style;
    type PrepaintState = GridDgPrepaint;

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
        crate::perf::count("grid_dg.layout");
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
        crate::perf::count("grid_dg.prepaint");

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        // Slit/sub-pixel culling: if pane is collapsing/narrower than 12px, bypass text shaping & processing
        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return GridDgPrepaint {
                hitbox,
                cells: Vec::new(),
            };
        }

        let Some(app_entity) = self.app.upgrade() else {
            return GridDgPrepaint {
                hitbox,
                cells: Vec::new(),
            };
        };

        let pane_id = self.pane_id;
        let visible = self.visible.clone();
        let start_idx = self.start_idx;
        let end_idx = self.end_idx;
        let cols = self.cols;
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let pitch_x = self.pitch_x;
        let pitch_y = self.pitch_y;
        let pad_x = self.pad_x;
        let pad_y = self.pad_y;
        let _thumb_px = self.thumb_px;
        let icon_area_h = self.icon_area_h;
        let scroll_y = self.scroll_y;
        let viewport_h = self.viewport_h;
        let zoom_scale = self.zoom_scale;
        let total_count = self.total_count;

        let mut model = GridHitModel {
            presented: Vec::new(),
            bounds: Bounds {
                origin: point(bounds.origin.x, bounds.origin.y + px(scroll_y)),
                size: size(bounds.size.width, px(viewport_h)),
            },
            pad_x,
            pad_y,
            scroll_x: 0.0,
            scroll_y,
            cell_w,
            cell_h,
            cols,
            major_rows: 0,
            pitch_y,
            pitch_x,
            visible: visible.clone(),
            start_idx,
            end_idx,
            total_count,
            bands: self.bands.clone(),
        };

        let viewport_bounds = Bounds {
            origin: bounds.origin,
            size: size(bounds.size.width, px(viewport_h)),
        };
        let content_mask = window.content_mask().bounds;

        // Hydrate state directly from App and TabState
        let has_gpui_drag = cx.has_active_drag();
        let mut raw_cells = app_entity.update(cx, |app, _| {
            // Register GridHitModel for hit testing
            app.grid_hit.insert(pane_id, model.clone());

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

                let (row, _, rel_x, rel_y) =
                    grid_cell_projection(pos, cols, pitch_x, pitch_y, pad_x, pad_y);
                let rel_y = rel_y + self.bands.shift(row);
                let is_dir = matches!(entry.kind(), FileKind::Dir);
                let selected = tab_state.selection.selected.contains(&entry_idx);
                let cursor = tab_state.selection.keyboard_cursor == Some(entry_idx);
                let hovered = app.grid_hover == Some((pane_id, entry_idx));
                let blinking = is_dir
                    && app.drag_hover.blink_id.as_ref()
                        == Some(&DragHoverId::Folder(pane_id, entry.path()));
                let editing =
                    crate::ui::views::rename::active_input(app, pane_id, entry_idx).is_some();
                let dimmed = crate::ui::views::rename::dim_other_entry(app, pane_id, entry_idx);
                let show_cell_check = selected && show_checkboxes;
                let icon = app.file_icon_for_entry(
                    &entry.path(),
                    &entry.kind(),
                    entry.extension().as_deref(),
                );

                items.push(RawCellData {
                    pos,
                    entry_idx,
                    rel_x,
                    rel_y,
                    visual_w: cell_w,
                    visual_h: cell_h,
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
        let motion = window.use_keyed_state(
            SharedString::from(format!("grid-cell-motion-{}", pane_id.0)),
            cx,
            |_, _| crate::ui::dg::motion::GridMotion::<PathBuf>::new(epoch),
        );
        let targets: Vec<_> = raw_cells
            .iter()
            .map(|c| {
                (
                    c.path.clone(),
                    c.pos,
                    crate::ui::dg::motion::GridItemBounds {
                        x: c.rel_x,
                        y: c.rel_y,
                        w: cell_w,
                        h: cell_h,
                    },
                )
            })
            .collect();
        let (bounds_list, moving) =
            motion.update(cx, |state, _| state.sample(&targets, epoch, instant, now));
        for (cell, (_, b)) in raw_cells.iter_mut().zip(bounds_list) {
            cell.rel_x = b.x;
            cell.rel_y = b.y;
            cell.visual_w = b.w;
            cell.visual_h = b.h;
        }
        model.presented = raw_cells
            .iter()
            .map(|c| {
                (
                    c.pos,
                    Bounds {
                        origin: point(bounds.origin.x + px(c.rel_x), bounds.origin.y + px(c.rel_y)),
                        size: size(px(c.visual_w), px(c.visual_h)),
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
            app.grid_hit.insert(pane_id, model);
            // Smooth wheel frames move content under a stationary marquee.
            app.refresh_rubber_for_pane(pane_id, view_cx);
        });
        if moving {
            window.request_animation_frame();
        }

        // Fast UTF-8 binary search truncation text shaping using window
        let feedback = window.use_keyed_state(
            SharedString::from(format!("grid-selection-feedback-{}", pane_id.0)),
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
        let mut cells = Vec::with_capacity(raw_cells.len());
        for (raw, selected_at) in raw_cells.into_iter().zip(selected_at) {
            let item_icon_area_h = (raw.visual_h - GRID_LABEL_H * zoom_scale).max(0.0);
            let slots = grid_cell_cut(
                raw.rel_x,
                raw.rel_y,
                raw.visual_w,
                raw.visual_h,
                item_icon_area_h,
                zoom_scale,
                raw.show_cell_check,
            );
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

            cells.push(GridDgPreparedCell {
                interactive: true,
                texture_opacity: 1.0,
                opacity: 1.0,
                pos: raw.pos,
                entry_idx: raw.entry_idx,
                rel_x: raw.rel_x,
                rel_y: raw.rel_y,
                visual_w: raw.visual_w,
                visual_h: raw.visual_h,
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

        let resources = window.use_keyed_state(
            SharedString::from(format!("grid-thumbnail-fade-{}", pane_id.0)),
            cx,
            |_, _| crate::ui::dg::motion::ResourceFade::<PathBuf>::new(),
        );
        let textures: Vec<_> = cells
            .iter()
            .map(|cell| {
                (
                    cell.path.clone(),
                    match &cell.icon {
                        FileIcon::PreviewTexture(texture) => Some(texture.id),
                        _ => None,
                    },
                )
            })
            .collect();
        let (opacities, active) = resources.update(cx, |s, _| s.sample(&textures, instant, now));
        for (cell, opacity) in cells.iter_mut().zip(opacities) {
            cell.texture_opacity = opacity;
        }
        if active {
            window.request_animation_frame();
        }
        let presence = window.use_keyed_state(
            SharedString::from(format!("grid-cell-presence-{}", pane_id.0)),
            cx,
            |_, _| crate::ui::dg::motion::PresenceMotion::<PathBuf, GridDgPreparedCell>::new(),
        );
        let viewport = [scroll_y, cell_h, viewport_h];
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
        GridDgPrepaint { hitbox, cells }
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
        crate::perf::count("grid_dg.paint");

        if bounds.size.width < px(12.0)
            || bounds.size.height < px(12.0)
            || prepaint.cells.is_empty()
        {
            return;
        }

        let zoom = self.zoom_scale;

        // Render visible cells with GPU hardware clipping layer
        for cell in &prepaint.cells {
            let abs_x = bounds.origin.x + px(cell.rel_x);
            let abs_y = bounds.origin.y + px(cell.rel_y);
            let cell_bounds = Bounds {
                origin: point(abs_x, abs_y),
                size: size(px(cell.visual_w), px(cell.visual_h)),
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
                            paint_rounded(window, cell_bounds, 6.0 * zoom, color);
                        }
                        if cell.interactive {
                            crate::ui::operation_feedback::paint_file_feedback(
                                &self.app,
                                &cell.path,
                                cell_bounds,
                                self.colors.accent,
                                6.0 * zoom,
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
                                    .corner_radii(Corners::all(px(6.0 * zoom))),
                            );
                        }

                        // Dimming when another item is being renamed
                        window.with_element_opacity(
                            cell.dimmed.then_some(crate::ui::views::rename::DIM_OPACITY),
                            |window| {
                                // 2. Shared checkbox for selected items during multi-selection.
                                if let Some(check_slot) = cell.slots.check_slot {
                                    let check_center = point(
                                        bounds.origin.x + px(check_slot.x + check_slot.w * 0.5),
                                        bounds.origin.y + px(check_slot.y + check_slot.h * 0.5),
                                    );
                                    selection_checkbox::paint_animated(
                                        window,
                                        cx,
                                        check_center,
                                        16.0 * zoom,
                                        cell.selected_at,
                                    );
                                }

                                // 3. Thumbnail / File icon slot (centered in the upper icon area)
                                let icon_slot = cell.slots.icon_slot;
                                let icon_center = point(
                                    bounds.origin.x + px(icon_slot.x + icon_slot.w * 0.5),
                                    bounds.origin.y + px(icon_slot.y + icon_slot.h * 0.5),
                                );

                                match &cell.icon {
                                    FileIcon::PreviewTexture(tex) => {
                                        window.with_element_opacity(
                                            Some(cell.texture_opacity),
                                            |window| {
                                                let visual_thumb =
                                                    (cell.visual_w * 0.9).min(icon_slot.h);
                                                crate::ui::thumbs::paint_thumbnail_state(
                                                    window,
                                                    cx,
                                                    tex,
                                                    icon_center,
                                                    visual_thumb,
                                                    icon_slot.h,
                                                    cell.hovered,
                                                );
                                            },
                                        );
                                    }
                                    FileIcon::IconTexture(tex) => {
                                        let sz = ((cell.visual_w * GRID_TEXTURE_ICON_MAX)
                                            .min(icon_slot.h))
                                        .round();
                                        let half = (sz * 0.5).round();
                                        let cx_pos = f32::from(icon_center.x).round();
                                        let cy_pos = f32::from(icon_center.y).round();
                                        let icon_bounds = Bounds {
                                            origin: point(px(cx_pos - half), px(cy_pos - half)),
                                            size: size(px(sz), px(sz)),
                                        };
                                        let resolved = tex.for_size(sz);
                                        let _ = window.paint_image(
                                            icon_bounds,
                                            icon_bounds,
                                            Corners::default(),
                                            resolved,
                                            0,
                                            false,
                                        );
                                    }
                                    FileIcon::Builtin { kind, .. } => {
                                        let icon = if matches!(kind, FileKind::Dir) {
                                            AtlasIcon::Folder
                                        } else {
                                            AtlasIcon::File
                                        };
                                        let sz =
                                            (cell.visual_w * GRID_BUILTIN_ICON).min(icon_slot.h);
                                        let icon_bounds = Bounds {
                                            origin: point(
                                                icon_center.x - px(sz * 0.5),
                                                icon_center.y - px(sz * 0.5),
                                            ),
                                            size: size(px(sz), px(sz)),
                                        };
                                        let icon_color = if cell.selected && !cell.editing {
                                            self.colors.accent_fg
                                        } else {
                                            self.colors.muted
                                        };
                                        paint_svg_icon(
                                            window,
                                            cx,
                                            icon_bounds,
                                            icon.path(),
                                            icon_color,
                                        );
                                    }
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
                                            TextAlign::Center,
                                            Some(px(name_slot.w)),
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
                .grid_hit
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
                    this.grid_hit
                        .get(&pane_id)
                        .and_then(|m| m.hit_entry(pos))
                        .map(|idx| (pane_id, idx))
                } else {
                    None
                };
                if this.grid_hover != hover_entry {
                    this.grid_hover = hover_entry;
                    gpui_component::tooltip::Tooltip::dismiss(window, cx);
                    this.notify_pane(pane_id, cx);
                }
            });
        });

        // MouseDown for selection, range, toggle, double-click activate, right-click context menu
        let down_view = app_entity;
        let down_hitbox = prepaint.hitbox.clone();
        window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let pos = ev.position;
            let is_in_surface = {
                let app = down_view.read(cx);
                app.grid_hit
                    .get(&pane_id)
                    .is_some_and(|m| m.bounds.contains(&pos))
            };
            if !down_hitbox.is_hovered(window) || !is_in_surface {
                return;
            }

            down_view.update(cx, |this, cx| {
                let hit = this.grid_hit.get(&pane_id).and_then(|m| m.hit_entry(pos));
                match ev.button {
                    MouseButton::Left => this.details_left_mouse_down(
                        pane_id,
                        hit,
                        pos,
                        ev.modifiers.shift,
                        ev.modifiers.control || ev.modifiers.platform,
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

/// Public render entrypoint for Atlas: renders the Grid (icons) view via Digital Grove architecture.
pub fn render_grid_view_dg(
    app: &mut App,
    pane_id: TabId,
    window: &mut Window,
    cx: &mut Context<App>,
) -> AnyElement {
    app.render_grid_view_dg(pane_id, window, cx)
}

impl App {
    /// Public render entrypoint on `App` for Digital Grove grid view.
    pub fn render_grid_view_dg(
        &mut self,
        pane_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        crate::perf::count("render.view.grid_dg");
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
        let (accent, accent_fg) = self.selection_colors(pane_id, accent, accent_fg, fg, muted);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let row_font = window.text_style().font();
        let crumb_strip = self.render_crumb_strip(pane_id, &view, fg, muted, border, header_bg, cx);
        let count = self.state.tab_states.get_mut(&pane_id).map_or(0, |ts| {
            ts.visible_indices();
            // Collapsed groups hide every file but still show their headers.
            ts.visible.len()
        });

        if count == 0 {
            self.grid_hit.remove(&pane_id);
            self.row_rects.borrow_mut().remove(&pane_id);
            if self
                .grid_hover
                .is_some_and(|(hover_pane, _)| hover_pane == pane_id)
            {
                self.grid_hover = None;
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

        let thumb = self
            .tab_data_for(pane_id)
            .map_or(128.0, |t| t.thumbnail_size);
        let zoom_scale = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let thumb_px = thumb * zoom_scale;

        let grid_gap = self.state.settings.grid_spacing.clamp(4.0, 48.0);
        let grid_pad = (grid_gap * 0.75).round().max(4.0);
        let grid_gap_px = grid_gap * zoom_scale;
        let grid_pad_px = grid_pad * zoom_scale;

        let pane_w = self
            .state
            .tab_states
            .get(&pane_id)
            .map(|ts| ts.last_pane_width)
            .filter(|&w| w > 1.0)
            .unwrap_or_else(|| (f32::from(window.viewport_size().width) - 217.0).max(0.0));
        let cols = grid_column_count(pane_w, thumb_px, grid_gap_px, grid_pad_px);

        let scroll = self.scroll_handle_for(pane_id);
        let base_scroll = scroll.0.borrow().base_handle.clone();
        let mut scroll_y = (-f32::from(base_scroll.offset().y)).max(0.0);

        let viewport_h = {
            let tracked = f32::from(base_scroll.bounds().size.height);
            if tracked > 1.0 {
                tracked
            } else {
                f32::from(window.viewport_size().height)
            }
        };

        let cell_h_px = thumb_px * GRID_ICON_AREA + GRID_LABEL_H * zoom_scale;
        let pitch_x = thumb_px + grid_gap_px;
        let pitch_y = cell_h_px + grid_gap_px;
        // Windows sends 3 lines per standard wheel notch. A full pitch_y per line
        // leaps 3 full rows (~500-750px) per notch. Dividing by 3 makes 1 notch
        // advance 1 row smoothly, matching Details and List views.
        base_scroll.set_line_scroll_height(px(pitch_y / 3.0));
        // Headers are bands between rows, so thumbnails keep their full pitch.
        let group_cells =
            self.grouped_cells(pane_id, atlas_model::grouping::GroupFlow::Wrap { cols });
        let cells = match &group_cells {
            Some(groups) => groups.cells.clone(),
            None => self
                .state
                .tab_states
                .get(&pane_id)
                .map(|ts| ts.visible.clone())
                .unwrap_or_default(),
        };
        let band_h = GROUP_BAND_H * zoom_scale;
        let bands = group_cells
            .as_ref()
            .map_or_else(Default::default, |groups| {
                atlas_model::grouping::RowBands::new(
                    groups
                        .headers
                        .iter()
                        .map(|header| header.cell / cols)
                        .collect(),
                    band_h,
                )
            });
        let count = cells.len();
        let rows = count.div_ceil(cols);
        let content_h = if rows == 0 && bands.is_empty() {
            0.0
        } else {
            grid_pad_px * 2.0 + rows as f32 * pitch_y - grid_gap_px + bands.total()
        };
        let max_scroll = self.file_scroll_max(content_h, viewport_h);
        if scroll_y > max_scroll {
            let offset = base_scroll.offset();
            base_scroll.set_offset(gpui::point(offset.x, gpui::px(-max_scroll)));
            scroll_y = max_scroll;
        }

        // The range is computed from the offset at render time while smooth
        // scrolling keeps moving in prepaint. Overscan one row on *both* edges so
        // scrolling up never exposes an unpainted strip at the top.
        let start_row = bands
            .row_at(scroll_y - grid_pad_px, pitch_y)
            .saturating_sub(1);
        let end_row = bands.rows_through(scroll_y + viewport_h - grid_pad_px, pitch_y) + 1;
        let start = (start_row * cols).min(count);
        let end = ((end_row * cols).min(count)).max(start);

        // Look ahead a full viewport on both sides so thumbnails are decoded
        // before their rows scroll in, whichever way the user is heading.
        let ahead_rows = ((viewport_h / pitch_y.max(1.0)).ceil() as usize).max(2);
        let ahead_start = (start_row.saturating_sub(ahead_rows) * cols).min(count);
        let ahead_end = (((end_row + ahead_rows) * cols).min(count)).max(end);
        self.prepare_grid_cells(pane_id, &cells, start..end, ahead_start..ahead_end);

        let surface_colors = GridDgColors {
            fg,
            accent,
            accent_fg,
            hover,
            muted,
        };

        let surface = GridDgElement::new(
            pane_id,
            view.downgrade(),
            cells.clone(),
            start,
            end,
            cols,
            count,
            thumb_px,
            cell_h_px,
            pitch_x,
            pitch_y,
            grid_pad_px,
            grid_pad_px,
            thumb_px,
            thumb_px * GRID_ICON_AREA,
            scroll_y,
            viewport_h,
            zoom_scale,
            surface_colors,
            row_font.clone(),
            font_size,
        )
        .bands(bands.clone())
        .absolute()
        .inset_0()
        .into_any_element();

        let mut rename_overlay = None;
        if let Some(_ts) = self.state.tab_states.get(&pane_id) {
            for visible_pos in start..end {
                let Some(&entry_idx) = cells.get(visible_pos) else {
                    continue;
                };
                let Some(rename_input) =
                    crate::ui::views::rename::active_input(self, pane_id, entry_idx)
                else {
                    continue;
                };
                let col = visible_pos % cols;
                let row = visible_pos / cols;
                let cell_x = grid_pad_px + col as f32 * pitch_x;
                let cell_y = grid_pad_px + bands.row_top(row, pitch_y);
                let cell_bounds = Bounds {
                    origin: point(px(cell_x), px(cell_y)),
                    size: size(px(thumb_px), px(cell_h_px)),
                };
                let label = grid_label_bounds(cell_bounds, thumb_px * GRID_ICON_AREA, zoom_scale);
                rename_overlay = Some(
                    div()
                        .absolute()
                        .left(label.origin.x)
                        .top(label.origin.y)
                        .w(label.size.width)
                        .h(label.size.height)
                        .text_color(accent_fg)
                        .child(crate::ui::views::rename::centered_input_element(
                            &rename_input,
                            &view,
                            label.size.height,
                            font_size,
                            label.size.width,
                            window,
                            cx,
                        ))
                        .into_any_element(),
                );
                break;
            }
        }

        let drag_layer = div()
            .id(("grid-dg-drag-layer", pane_id.0))
            .absolute()
            .top_0()
            .left_0()
            .w_full()
            .h(px(content_h))
            .opacity(0.0)
            .on_file_drag_grid(view.clone(), pane_id)
            .into_any_element();

        let tooltip_data = self
            .grid_hover
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
            groups
                .headers
                .iter()
                .enumerate()
                .map(|(k, header)| {
                    crate::ui::group_header::group_header(
                        pane_id,
                        header,
                        &view,
                        colors,
                        (
                            px(0.0),
                            px(grid_pad_px + bands.band_top(k, pitch_y)),
                            None,
                            px(band_h),
                        ),
                    )
                })
                .collect::<Vec<_>>()
        });
        let scroll_view = view.clone();
        let inner = div()
            .id(("anim-grid-dg-scroll", pane_id.0))
            .size_full()
            .overflow_y_scroll()
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
                    .w_full()
                    .h(px(content_h))
                    .relative()
                    .child(surface)
                    .when_some(rename_overlay, |el, overlay| el.child(overlay))
                    .child(drag_layer)
                    .children(group_overlays),
            )
            .into_any_element();

        let zone_view = view.clone();
        let prepaint_view = view.clone();
        let bounds_view = view.clone();
        let pane_view = view;
        crate::ui::views::pane_background_interactions(
            v_flex()
                .id(("pane", pane_id.0))
                .size_full()
                .overflow_hidden()
                .min_w(px(0.))
                .min_h(px(0.)),
            &pane_view,
            pane_id,
        )
        .on_prepaint_child({
            let v = prepaint_view.clone();
            move |bounds: gpui::Bounds<gpui::Pixels>, _window, cx| {
                crate::perf::count("prepaint.grid_dg_pane");
                let new_w = f32::from(bounds.size.width);
                let new_cols = grid_column_count(new_w, thumb_px, grid_gap_px, grid_pad_px);
                if new_cols != cols {
                    v.update(cx, |this, cx| {
                        if let Some(ts) = this.state.tab_states.get_mut(&pane_id) {
                            ts.last_pane_width = new_w;
                        }
                        this.notify_pane(pane_id, cx);
                    });
                }
            }
        })
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
                .rounded_bl(px(12.0))
                .rounded_br(px(12.0))
                .on_prepaint_child(pane_bounds_recorder(pane_id, &bounds_view))
                .child(layout_region(
                    "pane.viewport_body",
                    self.transition_viewport(pane_id, inner),
                ))
                .vertical_scrollbar(&base_scroll),
        ))
        .into_any_element()
    }

    pub(crate) fn prepare_grid_cells(
        &mut self,
        pane_id: TabId,
        cells: &[usize],
        range: std::ops::Range<usize>,
        prefetch: std::ops::Range<usize>,
    ) {
        #[cfg(feature = "provider-host")]
        if self.tab_data_for(pane_id).is_some_and(|pane| {
            matches!(
                pane.location,
                atlas_model::location::Location::Remote { .. }
            )
        }) {
            return;
        }
        let windows_theme = self.native_file_icons();
        let mut type_icon_paths: Vec<std::path::PathBuf> = Vec::new();
        let (to_request, to_request_ext, to_request_shell, request_folder_icon, all_visible) = {
            let Some(ts) = self.state.tab_states.get(&pane_id) else {
                return;
            };
            // Every request carries `ahead`: true for prefetch-only cells, which
            // never take the last decode slots from what is on screen.
            let mut imgs: Vec<(std::path::PathBuf, bool)> = Vec::new();
            let mut exts: Vec<(
                std::path::PathBuf,
                String,
                std::collections::HashMap<String, std::path::PathBuf>,
                bool,
            )> = Vec::new();
            // Windows: non-native-image files routed through IShellItemImageFactory.
            let mut shell_paths: Vec<(std::path::PathBuf, bool, bool)> = Vec::new();
            let mut request_folder_icon = false;
            let mut all_visible = Vec::new();
            for vi in prefetch_order(range.clone(), prefetch) {
                let ahead = !range.contains(&vi);
                let Some(&entry_idx) = cells.get(vi) else {
                    continue;
                };
                if atlas_model::grouping::is_virtual_cell(entry_idx) {
                    continue;
                }
                let Some(entry) = ts.entries.get(entry_idx) else {
                    continue;
                };
                if entry.kind() == FileKind::Dir {
                    request_folder_icon |= windows_theme;
                    continue;
                }

                if entry.kind() != FileKind::File {
                    continue;
                }
                if windows_theme {
                    type_icon_paths.push(entry.path());
                }

                let thumbable = is_thumbnailable(entry);
                #[cfg(target_os = "windows")]
                let shell_thumbable = !thumbable
                    && entry
                        .extension()
                        .as_deref()
                        .is_some_and(is_windows_shell_thumbnail_ext);
                #[cfg(not(target_os = "windows"))]
                let shell_thumbable = false;

                if thumbable {
                    all_visible.push(entry.path());
                    if !self.thumb_textures.contains_key(&entry.path()) {
                        imgs.push((entry.path(), ahead));
                    }
                } else if shell_thumbable {
                    all_visible.push(entry.path());
                    if !self.thumb_textures.contains_key(&entry.path()) {
                        shell_paths.push((entry.path(), false, ahead));
                    }
                } else if self.thumb_textures.contains_key(&entry.path()) {
                    all_visible.push(entry.path());
                } else {
                    let ext_cmd = entry.extension().as_deref().and_then(|ext| {
                        self.ext_registry
                            .thumbnailer_for(ext)
                            .map(|cmd| (cmd.command.run.clone(), cmd.tools))
                    });
                    if let Some((run, tools)) = ext_cmd {
                        all_visible.push(entry.path());
                        exts.push((entry.path(), run, tools, ahead));
                    }
                }
            }
            (imgs, exts, shell_paths, request_folder_icon, all_visible)
        };

        // Each pane owns its slice; the decode workers see the union so one grid
        // never cancels another pane's pending thumbnails.
        self.thumb_wanted_by_pane.insert(pane_id, all_visible);
        let panes = &self.state.tab_states;
        self.thumb_wanted_by_pane
            .retain(|pane, _| panes.contains_key(pane));
        {
            let mut wanted = self
                .thumb_wanted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            wanted.clear();
            for paths in self.thumb_wanted_by_pane.values() {
                wanted.extend(paths.iter().cloned());
            }
        }
        #[cfg(target_os = "windows")]
        if request_folder_icon {
            self.request_shell_folder_icon();
        }
        #[cfg(target_os = "windows")]
        for path in &type_icon_paths {
            self.request_shell_type_icon(path);
        }
        #[cfg(not(target_os = "windows"))]
        drop(type_icon_paths);
        for (path, ahead) in to_request {
            if !(ahead
                && self.native_thumb_inflight + PREFETCH_HEADROOM >= MAX_NATIVE_THUMB_INFLIGHT)
            {
                self.request_thumbnail(&path);
            }
        }
        for (path, run, tools, ahead) in to_request_ext {
            if !(ahead
                && self.ext_thumb_inflight.len() + PREFETCH_HEADROOM >= MAX_EXT_THUMB_INFLIGHT)
            {
                self.request_ext_thumbnail(&path, run, tools);
            }
        }
        #[cfg(target_os = "windows")]
        for (path, allow_icon, ahead) in to_request_shell {
            if ahead && self.ext_thumb_inflight.len() + PREFETCH_HEADROOM >= MAX_EXT_THUMB_INFLIGHT
            {
                continue;
            }
            if allow_icon {
                self.request_shell_thumbnail_or_icon(&path);
            } else {
                self.request_visible_shell_thumbnail(&path);
            }
        }
        #[cfg(not(target_os = "windows"))]
        drop(to_request_shell);
    }
}

/// Decode slots always kept free for on-screen cells while prefetching.
const PREFETCH_HEADROOM: usize = 4;
use crate::ui::thumbs::{MAX_EXT_THUMB_INFLIGHT, MAX_NATIVE_THUMB_INFLIGHT};

/// Cells to request, nearest first: the visible range, then the prefetch margin
/// alternating below/above so whichever direction the user scrolls is already warm.
fn prefetch_order(visible: std::ops::Range<usize>, window: std::ops::Range<usize>) -> Vec<usize> {
    let mut order: Vec<usize> = visible.clone().collect();
    let mut below = visible.end..window.end.max(visible.end);
    let mut above = window.start.min(visible.start)..visible.start;
    loop {
        let (next_below, next_above) = (below.next(), above.next_back());
        if next_below.is_none() && next_above.is_none() {
            break;
        }
        order.extend(next_below);
        order.extend(next_above);
    }
    order
}

/// Whether this entry should get a downscaled thumbnail (an image or audio file).
fn is_thumbnailable(entry: &atlas_model::file_entry::FileEntry) -> bool {
    matches!(entry.kind(), FileKind::File)
        && entry.extension().as_deref().is_some_and(|e| {
            atlas_model::preview_cache::is_image_ext(e)
                || atlas_model::preview_cache::is_audio_ext(e)
        })
}

#[cfg(target_os = "windows")]
fn is_windows_shell_thumbnail_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "3fr"
            | "3gp"
            | "arw"
            | "asf"
            | "avi"
            | "avif"
            | "cr2"
            | "cr3"
            | "crw"
            | "dng"
            | "flv"
            | "heic"
            | "heif"
            | "iiq"
            | "m2ts"
            | "m4v"
            | "mkv"
            | "mov"
            | "mp4"
            | "mpeg"
            | "mpg"
            | "mts"
            | "nef"
            | "orf"
            | "pdf"
            | "pef"
            | "psd"
            | "raf"
            | "rw2"
            | "srw"
            | "tga"
            | "ts"
            | "vob"
            | "webm"
            | "wmv"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_column_count_stride_boundary() {
        let thumb = 128.0;
        let gap = 8.0;

        assert_eq!(grid_column_count(263.99, thumb, gap, 0.0), 1);
        assert_eq!(grid_column_count(264.0, thumb, gap, 0.0), 2);
        assert_eq!(grid_column_count(399.99, thumb, gap, 0.0), 2);
        assert_eq!(grid_column_count(400.0, thumb, gap, 0.0), 3);
    }

    #[test]
    fn test_grid_columns_never_drop_below_one() {
        assert_eq!(grid_column_count(0.0, 128.0, 8.0, 0.0), 1);
        assert_eq!(grid_column_count(50.0, 128.0, 8.0, 16.0), 1);
    }

    #[test]
    fn test_grid_cell_projection() {
        let cols = 3;
        let pitch_x = 136.0;
        let pitch_y = 120.0;
        let pad_x = 12.0;
        let pad_y = 12.0;

        // pos 0: row 0, col 0
        let (row, col, x, y) = grid_cell_projection(0, cols, pitch_x, pitch_y, pad_x, pad_y);
        assert_eq!((row, col), (0, 0));
        assert_eq!((x, y), (12.0, 12.0));

        // pos 1: row 0, col 1
        let (row, col, x, y) = grid_cell_projection(1, cols, pitch_x, pitch_y, pad_x, pad_y);
        assert_eq!((row, col), (0, 1));
        assert_eq!((x, y), (148.0, 12.0));

        // pos 3: row 1, col 0
        let (row, col, x, y) = grid_cell_projection(3, cols, pitch_x, pitch_y, pad_x, pad_y);
        assert_eq!((row, col), (1, 0));
        assert_eq!((x, y), (12.0, 132.0));

        // pos 5: row 1, col 2
        let (row, col, x, y) = grid_cell_projection(5, cols, pitch_x, pitch_y, pad_x, pad_y);
        assert_eq!((row, col), (1, 2));
        assert_eq!((x, y), (284.0, 132.0));
    }

    #[test]
    fn test_grid_cell_hit_test_and_gap_rejection() {
        let cols = 3;
        let total_count = 8;
        let cell_w = 128.0;
        let cell_h = 112.0;
        let pitch_x = 136.0;
        let pitch_y = 120.0;
        let pad_x = 12.0;
        let pad_y = 12.0;

        // Inside cell 0 (at 20.0, 20.0) -> Some(0)
        assert_eq!(
            grid_cell_hit_test(
                20.0,
                20.0,
                cols,
                total_count,
                cell_w,
                cell_h,
                pitch_x,
                pitch_y,
                pad_x,
                pad_y
            ),
            Some(0)
        );

        // Inside cell 1 (at 150.0, 20.0) -> Some(1)
        assert_eq!(
            grid_cell_hit_test(
                150.0,
                20.0,
                cols,
                total_count,
                cell_w,
                cell_h,
                pitch_x,
                pitch_y,
                pad_x,
                pad_y
            ),
            Some(1)
        );

        // In horizontal gap between col 0 and col 1 (x = 142.0, y = 20.0) -> None
        assert_eq!(
            grid_cell_hit_test(
                142.0,
                20.0,
                cols,
                total_count,
                cell_w,
                cell_h,
                pitch_x,
                pitch_y,
                pad_x,
                pad_y
            ),
            None
        );

        // In vertical gap between row 0 and row 1 (x = 20.0, y = 126.0) -> None
        assert_eq!(
            grid_cell_hit_test(
                20.0,
                126.0,
                cols,
                total_count,
                cell_w,
                cell_h,
                pitch_x,
                pitch_y,
                pad_x,
                pad_y
            ),
            None
        );

        // In padding area before cell 0 (x = 5.0, y = 20.0) -> None
        assert_eq!(
            grid_cell_hit_test(
                5.0,
                20.0,
                cols,
                total_count,
                cell_w,
                cell_h,
                pitch_x,
                pitch_y,
                pad_x,
                pad_y
            ),
            None
        );

        // Beyond total count (pos 8 would be row 2, col 2 -> at 290.0, 260.0) -> None
        assert_eq!(
            grid_cell_hit_test(
                290.0,
                260.0,
                cols,
                total_count,
                cell_w,
                cell_h,
                pitch_x,
                pitch_y,
                pad_x,
                pad_y
            ),
            None
        );

        // Beyond column bounds -> None
        assert_eq!(
            grid_cell_hit_test(
                800.0,
                20.0,
                cols,
                total_count,
                cell_w,
                cell_h,
                pitch_x,
                pitch_y,
                pad_x,
                pad_y
            ),
            None
        );
    }

    #[test]
    fn test_rect_cut_grid_cell_layout_without_checkbox() {
        let zoom = 1.0;
        let cell_x = 10.0;
        let cell_y = 20.0;
        let cell_w = 128.0;
        let cell_h = 124.0;
        let icon_area_h = 92.0;

        let slots = grid_cell_cut(cell_x, cell_y, cell_w, cell_h, icon_area_h, zoom, false);

        assert!(slots.check_slot.is_none());

        // Upper icon area: exactly icon_area_h high
        assert_eq!(slots.icon_slot.x, 10.0);
        assert_eq!(slots.icon_slot.y, 20.0);
        assert_eq!(slots.icon_slot.w, 128.0);
        assert_eq!(slots.icon_slot.h, 92.0);

        // Name slot: padded horizontally by 4px on each side, height is cell_h - icon_area_h = 32px
        assert_eq!(slots.name_slot.x, 14.0);
        assert_eq!(slots.name_slot.y, 112.0);
        assert_eq!(slots.name_slot.w, 120.0);
        assert_eq!(slots.name_slot.h, 32.0);
    }

    #[test]
    fn test_rect_cut_grid_cell_layout_with_checkbox() {
        let zoom = 1.0;
        let cell_x = 0.0;
        let cell_y = 0.0;
        let cell_w = 128.0;
        let cell_h = 124.0;
        let icon_area_h = 92.0;

        let slots = grid_cell_cut(cell_x, cell_y, cell_w, cell_h, icon_area_h, zoom, true);

        let check = slots.check_slot.expect("checkbox slot cut");
        // Checkbox is padded top-left: 4px from cell top and left, 16px wide and high
        assert_eq!(check.x, 4.0);
        assert_eq!(check.y, 4.0);
        assert_eq!(check.w, 16.0);
        assert_eq!(check.h, 16.0);

        assert_eq!(slots.icon_slot.w, 128.0);
        assert_eq!(slots.icon_slot.h, 92.0);
        assert_eq!(slots.name_slot.w, 120.0);
        assert_eq!(slots.name_slot.h, 32.0);
    }

    #[test]
    fn test_rect_cut_grid_cell_saturates_cleanly_on_narrow_bounds() {
        let zoom = 1.0;
        let cell_x = 0.0;
        let cell_y = 0.0;
        let cell_w = 20.0;
        let cell_h = 20.0;
        let icon_area_h = 15.0;

        let slots = grid_cell_cut(cell_x, cell_y, cell_w, cell_h, icon_area_h, zoom, true);

        assert!(slots.check_slot.is_some());
        assert!(slots.icon_slot.w >= 0.0);
        assert!(slots.name_slot.w >= 0.0);
        assert!(slots.name_slot.h >= 0.0);
    }
}

#[cfg(test)]
mod prefetch_tests {
    use super::prefetch_order;

    #[test]
    fn prefetch_requests_visible_first_then_alternates_outward() {
        assert_eq!(prefetch_order(4..6, 2..9), vec![4, 5, 6, 3, 7, 2, 8]);
        assert_eq!(prefetch_order(0..2, 0..5), vec![0, 1, 2, 3, 4]);
        assert_eq!(prefetch_order(3..3, 3..3), Vec::<usize>::new());
    }
}
