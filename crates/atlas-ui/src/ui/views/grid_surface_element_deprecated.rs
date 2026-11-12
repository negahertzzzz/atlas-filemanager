//! Retained, flat Grid surface.
//!
//! Grid used to build one GPUI subtree per visible cell.  This element keeps the
//! same interaction contract as Details, but projects every cell with the small
//! `(row, col) -> (x, y)` grid equation and paints the result directly.

use crate::app::{App, GridHitModel};
use crate::ui::drag_hover::{
    DragHoverCommand, DragHoverDropCommand, DragHoverId, DragHoverKind, DragHoverTarget,
};
use crate::ui::file_icons::FileIcon;
use crate::ui::icons::AtlasIcon;
use crate::ui::motion::ease_out_quart;
use crate::ui::selection_checkbox;
use crate::ui::surface::paint_rounded;
use crate::ui::views::grid_deprecated::{
    GRID_BUILTIN_ICON, GRID_ICON_AREA, GRID_LABEL_H, GRID_TEXTURE_ICON_MAX,
};
use atlas_model::file_entry::FileKind;
use atlas_model::pane::TabId;
use gpui::ColorExt as _;
use gpui::{
    point, px, size, App as GpuiApp, Bounds, Corners, DispatchPhase, Element, ElementId, Font,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Refineable, ShapedLine, SharedString,
    Style, StyleRefinement, Styled, TextAlign, WeakEntity, Window,
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FILE_SURFACE_SHAPE_CAPACITY: usize = 8_192;

/// Shared by retained paint and the editor overlay, in already-scaled pixels.
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

pub(crate) fn list_name_bounds(cell: Bounds<Pixels>, zoom: f32) -> Bounds<Pixels> {
    let name_x = (24.0 + 16.0 + 4.0) * zoom;
    Bounds {
        origin: point(cell.origin.x + px(name_x), cell.origin.y),
        size: size(
            px((f32::from(cell.size.width) - name_x - 24.0 * zoom).max(0.0)),
            cell.size.height,
        ),
    }
}

/// Bounded cross-pane cache for the expensive text shaping done on local ring misses.
///
/// Split clones commonly show the same directory and therefore the same labels. The
/// pane-local ring remains the hot path; this FIFO cache only prevents every clone
/// from shaping an identical cold set independently.
pub(crate) struct FileSurfaceShapeCache {
    lines: HashMap<u64, ShapedLine>,
    insertion_order: VecDeque<u64>,
    capacity: usize,
}

impl Default for FileSurfaceShapeCache {
    fn default() -> Self {
        Self {
            lines: HashMap::new(),
            insertion_order: VecDeque::new(),
            capacity: FILE_SURFACE_SHAPE_CAPACITY,
        }
    }
}

impl FileSurfaceShapeCache {
    fn get_or_insert_with(&mut self, key: u64, fill: impl FnOnce() -> ShapedLine) -> ShapedLine {
        if let Some(line) = self.lines.get(&key) {
            crate::perf::count("file_surface.shared_shape.hit");
            return line.clone();
        }

        crate::perf::count("file_surface.shared_shape.miss");
        let line = fill();
        while self.lines.len() >= self.capacity {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            self.lines.remove(&oldest);
        }
        self.lines.insert(key, line.clone());
        self.insertion_order.push_back(key);
        line
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GridSurfaceLayout {
    Thumbnail,
    CompactList,
}

impl GridSurfaceLayout {
    /// Select a stable, per-view perf metric without constructing a string on
    /// every visible row. `grid` is used for thumbnails, `list` for compact
    /// list mode.
    fn metric(self, grid: &'static str, list: &'static str) -> &'static str {
        match self {
            Self::Thumbnail => grid,
            Self::CompactList => list,
        }
    }

    fn layout_metric(self) -> &'static str {
        match self {
            Self::Thumbnail => "grid.surface.layout",
            Self::CompactList => "list.surface.layout",
        }
    }

    fn prepaint_metric(self) -> &'static str {
        match self {
            Self::Thumbnail => "grid.surface.prepaint",
            Self::CompactList => "list.surface.prepaint",
        }
    }

    fn paint_metric(self) -> &'static str {
        match self {
            Self::Thumbnail => "grid.surface.paint",
            Self::CompactList => "list.surface.paint",
        }
    }

    fn ring_hit_metric(self) -> &'static str {
        match self {
            Self::Thumbnail => "grid.surface.ring.hit",
            Self::CompactList => "list.surface.ring.hit",
        }
    }

    fn ring_miss_metric(self) -> &'static str {
        match self {
            Self::Thumbnail => "grid.surface.ring.miss",
            Self::CompactList => "list.surface.ring.miss",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ItemBounds2D {
    visual_x: f32,
    visual_y: f32,
    visual_w: f32,
    visual_h: f32,
    target_x: f32,
    target_y: f32,
    target_w: f32,
    target_h: f32,
}

#[derive(Clone, Copy, Debug)]
struct ActiveFlipAnim {
    start_time: Instant,
    start_dx: f32,
    start_dy: f32,
    start_dw: f32,
    start_dh: f32,
}

#[derive(Clone)]
struct GridSurfaceSlot {
    row: usize,
    entry_idx: usize,
    shape_key: u64,
    selected: bool,
    cursor: bool,
    selected_at: Option<Instant>,
    hovered: bool,
    flashing: bool,
    dimmed: bool,
    editing: bool,
    name: ShapedLine,
    icon: FileIcon,
}

#[derive(Clone, Default)]
struct SharedGridSurfaceState(std::rc::Rc<std::cell::RefCell<GridSurfaceState>>);

#[derive(Default)]
struct GridSurfaceState {
    slots: Vec<Option<GridSurfaceSlot>>,
    last_bounds: HashMap<usize, ItemBounds2D>,
    animations: HashMap<usize, ActiveFlipAnim>,
    last_layout: Option<GridSurfaceLayout>,
}

impl GridSurfaceState {
    fn ensure_capacity(&mut self, n: usize) {
        let n = n.max(1);
        if self.slots.len() < n {
            self.slots.resize_with(n, || None);
        }
    }

    fn index(&self, row: usize) -> usize {
        row % self.slots.len()
    }

    fn sync(
        &mut self,
        row: usize,
        key: u64,
        fill: impl FnOnce() -> GridSurfaceSlot,
        hit_metric: &'static str,
        miss_metric: &'static str,
    ) -> &GridSurfaceSlot {
        if self.slots.is_empty() {
            self.ensure_capacity(1);
        }
        let ix = self.index(row);
        let hit = matches!(&self.slots[ix], Some(slot) if slot.row == row && slot.shape_key == key);
        if hit {
            crate::perf::count(hit_metric);
        } else {
            crate::perf::count(miss_metric);
            self.slots[ix] = Some(fill());
        }
        self.slots[ix]
            .as_ref()
            .expect("grid surface slot just filled")
    }

    fn touch_dynamic(
        &mut self,
        row: usize,
        key: u64,
        selected: bool,
        cursor: bool,
        hovered: bool,
        flashing: bool,
        dimmed: bool,
        editing: bool,
        icon: FileIcon,
    ) {
        if self.slots.is_empty() {
            return;
        }
        let ix = self.index(row);
        if let Some(slot) = self.slots[ix].as_mut() {
            if slot.row == row && slot.shape_key == key {
                if slot.selected != selected {
                    slot.selected_at = selected.then(Instant::now);
                }
                slot.selected = selected;
                slot.cursor = cursor;
                slot.hovered = hovered;
                slot.flashing = flashing;
                slot.dimmed = dimmed;
                slot.editing = editing;
                // Thumbnail/icon readiness is asynchronous and intentionally not
                // part of the text shape key. Always refresh this paint-only value.
                slot.icon = icon;
            }
        }
    }

    fn collect(&self, start: usize, end: usize) -> Vec<GridSurfaceSlot> {
        (start..end)
            .filter_map(|row| {
                let slot = self.slots.get(self.index(row))?.as_ref()?;
                (slot.row == row).then(|| slot.clone())
            })
            .collect()
    }
}

pub(crate) struct GridSurfaceElement {
    id: ElementId,
    pane_id: TabId,
    app: WeakEntity<App>,
    visible: Arc<Vec<usize>>,
    start_idx: usize,
    end_idx: usize,
    total_count: usize,
    cols: usize,
    cell_w: f32,
    cell_h: f32,
    pitch_x: f32,
    pitch_y: f32,
    thumb: f32,
    scroll_y: f32,
    viewport_h: f32,
    colors: GridSurfaceColors,
    font: Font,
    font_size: Pixels,
    style: StyleRefinement,
    layout: GridSurfaceLayout,
    animate: bool,
    pad_x: f32,
    pad_y: f32,
}

#[derive(Clone, Copy)]
pub(crate) struct GridSurfaceColors {
    pub(crate) fg: Hsla,
    pub(crate) accent: Hsla,
    pub(crate) accent_fg: Hsla,
    pub(crate) hover: Hsla,
    pub(crate) muted: Hsla,
}

pub(crate) struct GridSurfacePrepaint {
    state: SharedGridSurfaceState,
    start_idx: usize,
    end_idx: usize,
    show_checkboxes: bool,
    hitbox: Hitbox,
}

impl GridSurfaceElement {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
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
        thumb: f32,
        scroll_y: f32,
        viewport_h: f32,
        colors: GridSurfaceColors,
        font: Font,
        font_size: Pixels,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from(format!("grid-surface-{}", pane_id.0))),
            pane_id,
            app,
            visible,
            start_idx,
            end_idx,
            total_count,
            cols: cols.max(1),
            cell_w,
            cell_h,
            pitch_x,
            pitch_y,
            thumb,
            scroll_y,
            viewport_h,
            colors,
            font,
            font_size,
            style: StyleRefinement::default(),
            layout: GridSurfaceLayout::Thumbnail,
            animate: false,
            pad_x: 0.0,
            pad_y: 0.0,
        }
    }

    pub(crate) fn padding(mut self, pad_x: f32, pad_y: f32) -> Self {
        self.pad_x = pad_x;
        self.pad_y = pad_y;
        self
    }

    pub(crate) fn animated(mut self, animate: bool) -> Self {
        self.animate = animate;
        self
    }

    pub(crate) fn compact_list(mut self) -> Self {
        self.id = ElementId::Name(SharedString::from(format!(
            "list-surface-{}",
            self.pane_id.0
        )));
        self.layout = GridSurfaceLayout::CompactList;
        self
    }
}

impl IntoElement for GridSurfaceElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for GridSurfaceElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

const FLIP_DURATION: Duration = Duration::from_millis(180);

fn compute_flip(
    entry_idx: usize,
    target_x: f32,
    target_y: f32,
    target_w: f32,
    target_h: f32,
    animate: bool,
    now: Instant,
    last_bounds: &HashMap<usize, ItemBounds2D>,
    animations: &mut HashMap<usize, ActiveFlipAnim>,
    has_active_anims: &mut bool,
) -> (f32, f32, f32, f32) {
    if !animate {
        return (target_x, target_y, target_w, target_h);
    }

    let mut cur_dx = 0.0_f32;
    let mut cur_dy = 0.0_f32;
    let mut cur_dw = 0.0_f32;
    let mut cur_dh = 0.0_f32;

    if let Some(prev) = last_bounds.get(&entry_idx) {
        let target_moved = (prev.target_x - target_x).abs() > 0.5
            || (prev.target_y - target_y).abs() > 0.5
            || (prev.target_w - target_w).abs() > 0.5
            || (prev.target_h - target_h).abs() > 0.5;

        if target_moved {
            let dx = prev.visual_x - target_x;
            let dy = prev.visual_y - target_y;
            let dw = prev.visual_w - target_w;
            let dh = prev.visual_h - target_h;

            if dx.abs() > 0.5 || dy.abs() > 0.5 || dw.abs() > 0.5 || dh.abs() > 0.5 {
                animations.insert(
                    entry_idx,
                    ActiveFlipAnim {
                        start_time: now,
                        start_dx: dx,
                        start_dy: dy,
                        start_dw: dw,
                        start_dh: dh,
                    },
                );
                cur_dx = dx;
                cur_dy = dy;
                cur_dw = dw;
                cur_dh = dh;
                *has_active_anims = true;
            }
        } else if let Some(anim) = animations.get_mut(&entry_idx) {
            let elapsed = now.saturating_duration_since(anim.start_time);
            if elapsed < FLIP_DURATION {
                let t = (elapsed.as_secs_f32() / FLIP_DURATION.as_secs_f32()).clamp(0.0, 1.0);
                let factor = 1.0 - ease_out_quart(t);
                cur_dx = anim.start_dx * factor;
                cur_dy = anim.start_dy * factor;
                cur_dw = anim.start_dw * factor;
                cur_dh = anim.start_dh * factor;
                *has_active_anims = true;
            } else {
                animations.remove(&entry_idx);
            }
        }
    }

    (
        target_x + cur_dx,
        target_y + cur_dy,
        (target_w + cur_dw).max(1.0),
        (target_h + cur_dh).max(1.0),
    )
}

impl Element for GridSurfaceElement {
    type RequestLayoutState = Style;
    type PrepaintState = GridSurfacePrepaint;

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
        crate::perf::count(self.layout.layout_metric());
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
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) -> Self::PrepaintState {
        let _perf = crate::perf::span_detail(self.layout.prepaint_metric(), || {
            format!(
                "pane={} visible={}..{} cols={}",
                self.pane_id.0, self.start_idx, self.end_idx, self.cols
            )
        });
        crate::perf::count(self.layout.prepaint_metric());
        crate::perf::count_by(
            match self.layout {
                GridSurfaceLayout::Thumbnail => "grid.surface.slots",
                GridSurfaceLayout::CompactList => "list.surface.slots",
            },
            self.end_idx.saturating_sub(self.start_idx) as u64,
        );

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let Some(app_entity) = self.app.upgrade() else {
            return GridSurfacePrepaint {
                state: SharedGridSurfaceState::default(),
                start_idx: 0,
                end_idx: 0,
                show_checkboxes: false,
                hitbox,
            };
        };

        let state_restore_t0 = std::time::Instant::now();
        let state_rc = window.with_optional_element_state(
            id,
            |state_opt: Option<Option<SharedGridSurfaceState>>, _window| {
                let s = state_opt.flatten().unwrap_or_default();
                (s.clone(), Some(s))
            },
        );
        let mut state = state_rc.0.borrow_mut();

        crate::perf::time_us(
            self.layout
                .metric("grid.surface.state_restore", "list.surface.state_restore"),
            state_restore_t0.elapsed().as_micros() as u64,
        );
        state.ensure_capacity((self.end_idx.saturating_sub(self.start_idx) + 8).max(16));

        let pane_id = self.pane_id;
        let visible = self.visible.clone();
        let start_idx = self.start_idx;
        let end_idx = self.end_idx;
        let cols = self.cols;
        let colors = self.colors;
        let font = self.font.clone();
        let font_size = self.font_size;
        let layout = self.layout;
        let model = GridHitModel {
            presented: Vec::new(),
            bounds: Bounds {
                origin: point(bounds.origin.x, bounds.origin.y + px(self.scroll_y)),
                size: size(bounds.size.width, px(self.viewport_h)),
            },
            pad_x: self.pad_x,
            pad_y: self.pad_y,
            scroll_x: 0.0,
            scroll_y: self.scroll_y,
            cell_w: self.cell_w,
            cell_h: self.cell_h,
            cols,
            bands: Default::default(),
            major_rows: 0,
            pitch_y: self.pitch_y,
            pitch_x: self.pitch_x,
            visible: visible.clone(),
            start_idx,
            end_idx,
            total_count: self.total_count,
        };

        let mut row_rects_us = 0_u64;
        let mut shape_cache_us = 0_u64;
        let mut ring_collect_us = 0_u64;
        let app_update_t0 = std::time::Instant::now();
        let show_checkboxes = app_entity.update(cx, |app, _| {
            match layout {
                GridSurfaceLayout::Thumbnail => {
                    app.grid_hit.insert(pane_id, model.clone());
                }
                GridSurfaceLayout::CompactList => {
                    app.list_hit.insert(pane_id, model.clone());
                    let row_rects_t0 = Instant::now();
                    let row_rects = (start_idx..end_idx)
                        .filter_map(|visible_pos| {
                            visible
                                .get(visible_pos)
                                .copied()
                                .map(|entry_idx| (entry_idx, model.cell_bounds(visible_pos)))
                        })
                        .collect();
                    row_rects_us += row_rects_t0.elapsed().as_micros() as u64;
                    app.row_rects.borrow_mut().insert(pane_id, row_rects);
                }
            }

            {
                app.pending_drag_hover_targets.retain(
                    |target| !matches!(target.id, DragHoverId::Folder(tab, _) if tab == pane_id),
                );
                let targets = app
                    .state
                    .tab_states
                    .get(&pane_id)
                    .map(|tab_state| {
                        (start_idx..end_idx)
                            .filter_map(|visible_pos| {
                                let entry_idx = *visible.get(visible_pos)?;
                                let entry = tab_state.entries.get(entry_idx)?;
                                matches!(entry.kind(), FileKind::Dir).then(|| DragHoverTarget {
                                    id: DragHoverId::Folder(pane_id, entry.path()),
                                    bounds: model
                                        .cell_bounds(visible_pos)
                                        .intersect(&model.bounds)
                                        .intersect(&window.content_mask().bounds),
                                    kind: DragHoverKind::Folder,
                                    command: DragHoverCommand::NavigatePathIn(
                                        pane_id,
                                        entry.path(),
                                    ),
                                    drop_command: Some(DragHoverDropCommand::TransferTo(
                                        entry.path(),
                                    )),
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for target in targets {
                    app.register_drag_hover_target(target);
                }
            }

            let Some(tab_state) = app.state.tab_states.get(&pane_id) else {
                return false;
            };
            let show_checkboxes = tab_state.selection.selected.len() > 1;
            let text_system = window.text_system();

            for row in start_idx..end_idx {
                let Some(&entry_idx) = visible.get(row) else {
                    continue;
                };
                let Some(entry) = tab_state.entries.get(entry_idx) else {
                    continue;
                };
                let editing =
                    crate::ui::views::rename::active_input(app, pane_id, entry_idx).is_some();
                let selected = tab_state.selection.selected.contains(&entry_idx);
                let cursor = tab_state.selection.keyboard_cursor == Some(entry_idx);
                let dimmed = crate::ui::views::rename::dim_other_entry(app, pane_id, entry_idx);
                let hovered = match layout {
                    GridSurfaceLayout::Thumbnail => app.grid_hover == Some((pane_id, entry_idx)),
                    GridSurfaceLayout::CompactList => app.list_hover == Some((pane_id, entry_idx)),
                };
                let flashing = matches!(entry.kind(), FileKind::Dir)
                    && app.drag_hover.blink_id.as_ref()
                        == Some(&DragHoverId::Folder(pane_id, entry.path()));
                let name_color = if selected {
                    colors.accent_fg
                } else {
                    colors.fg
                };
                let max_chars = match layout {
                    GridSurfaceLayout::CompactList => {
                        let text_w = self.cell_w - 50.0; // padding and icon approx
                        (text_w / (f32::from(font_size) * 0.55)) as usize
                    }
                    GridSurfaceLayout::Thumbnail => {
                        let text_w = self.cell_w - 8.0;
                        (text_w / (f32::from(font_size) * 0.55)) as usize
                    }
                }
                .max(5);
                let key = grid_shape_key(
                    entry_idx,
                    &entry.name,
                    max_chars,
                    selected,
                    dimmed,
                    &font,
                    font_size,
                    name_color,
                );
                let icon = app.file_icon_for_entry(
                    &entry.path(),
                    &entry.kind(),
                    entry.extension().as_deref(),
                );
                let previous_selected = state
                    .slots
                    .get(state.index(row))
                    .and_then(Option::as_ref)
                    .filter(|slot| slot.row == row && slot.shape_key == key)
                    .is_some_and(|slot| slot.selected);
                let _ = state.sync(
                    row,
                    key,
                    || {
                        let name = if entry.name.chars().count() > max_chars {
                            let half = max_chars.saturating_sub(3) / 2;
                            let start: String = entry.name.chars().take(half).collect();
                            let mut end: String = entry.name.chars().rev().take(half).collect();
                            end = end.chars().rev().collect();
                            format!("{}...{}", start, end)
                        } else {
                            entry.name.to_string()
                        };
                        GridSurfaceSlot {
                            row,
                            entry_idx,
                            shape_key: key,
                            selected,
                            cursor,
                            selected_at: (selected && !previous_selected).then(Instant::now),
                            hovered,
                            flashing,
                            dimmed,
                            editing,
                            name: {
                                let shape_cache_t0 = Instant::now();
                                let line = app.file_surface_shapes.get_or_insert_with(key, || {
                                    text_system.shape_line(
                                        name.clone().into(),
                                        font_size,
                                        &[gpui::TextRun {
                                            len: name.len(),
                                            font: font.clone(),
                                            color: name_color,
                                            background_color: None,
                                            underline: None,
                                            strikethrough: None,
                                            letter_spacing: None,
                                        }],
                                        None,
                                    )
                                });
                                shape_cache_us += shape_cache_t0.elapsed().as_micros() as u64;
                                line
                            },
                            icon: icon.clone(),
                        }
                    },
                    layout.ring_hit_metric(),
                    layout.ring_miss_metric(),
                );
                state.touch_dynamic(
                    row, key, selected, cursor, hovered, flashing, dimmed, editing, icon,
                );
            }

            show_checkboxes
        });

        crate::perf::time_us(
            self.layout
                .metric("grid.surface.app_update", "list.surface.app_update"),
            app_update_t0.elapsed().as_micros() as u64,
        );
        crate::perf::time_us(
            self.layout
                .metric("grid.surface.row_rects", "list.surface.row_rects"),
            row_rects_us,
        );
        crate::perf::time_us(
            self.layout
                .metric("grid.surface.text_shape", "list.surface.text_shape"),
            shape_cache_us,
        );
        // ring collect is now bypassed since we reference state during paint
        crate::perf::time_us(
            self.layout
                .metric("grid.surface.ring_collect", "list.surface.ring_collect"),
            0,
        );
        let state_store_t0 = Instant::now();
        // State is already updated inside the Rc in GPUI, no need to push it back
        crate::perf::time_us(
            self.layout
                .metric("grid.surface.state_store", "list.surface.state_store"),
            state_store_t0.elapsed().as_micros() as u64,
        );
        crate::perf::count_by(
            match self.layout {
                GridSurfaceLayout::Thumbnail => "grid.surface.rows",
                GridSurfaceLayout::CompactList => "list.surface.rows",
            },
            (end_idx - start_idx) as u64,
        );
        GridSurfacePrepaint {
            state: state_rc.clone(),
            start_idx,
            end_idx,
            show_checkboxes,
            hitbox,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        style: &mut Style,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) {
        let _perf = crate::perf::span(self.layout.paint_metric());
        crate::perf::count(self.layout.paint_metric());
        let pane_id = self.pane_id;
        let app = self.app.clone();
        let colors = self.colors;
        let cols = self.cols;
        let cell_w = self.cell_w;
        let cell_h = self.cell_h;
        let pitch_x = self.pitch_x;
        let pitch_y = self.pitch_y;
        let thumb = self.thumb;
        let layout = self.layout;
        let animate = self.animate;
        let grid_pad_x = self.pad_x;
        let grid_pad_y = self.pad_y;

        let mut state_rc = prepaint.state.0.borrow_mut();
        let state = &mut *state_rc;

        if state.last_layout != Some(layout) {
            state.last_bounds.clear();
            state.animations.clear();
            state.last_layout = Some(layout);
        }

        let now = Instant::now();
        let mut has_active_anims = false;
        let mut next_bounds =
            HashMap::with_capacity(prepaint.end_idx.saturating_sub(prepaint.start_idx));

        let GridSurfaceState {
            slots,
            last_bounds,
            animations,
            ..
        } = state;

        style.paint(bounds, window, cx, |window, cx| {
            let origin_x = px(f32::from(bounds.origin.x).round());
            let origin_y = px(f32::from(bounds.origin.y).round());
            let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
            let icon_area_h = thumb * GRID_ICON_AREA;
            let label_h = GRID_LABEL_H * zoom;

            if layout == GridSurfaceLayout::CompactList {
                let pad_x = 24.0 * zoom;
                let icon_slot_w = 16.0 * zoom;
                let icon_size = 14.0 * zoom;
                for r in prepaint.start_idx..prepaint.end_idx {
                    let Some(slot) = slots.get(r % slots.len()).and_then(|s| s.as_ref()) else {
                        continue;
                    };
                    if slot.row != r {
                        continue;
                    }
                    let col = slot.row % cols;
                    let row = slot.row / cols;
                    let target_x = grid_pad_x + col as f32 * pitch_x;
                    let target_y = grid_pad_y + row as f32 * pitch_y;
                    let (visual_x, visual_y, visual_w, visual_h) = compute_flip(
                        slot.entry_idx,
                        target_x,
                        target_y,
                        cell_w,
                        cell_h,
                        animate,
                        now,
                        last_bounds,
                        animations,
                        &mut has_active_anims,
                    );
                    next_bounds.insert(
                        slot.entry_idx,
                        ItemBounds2D {
                            visual_x,
                            visual_y,
                            visual_w,
                            visual_h,
                            target_x,
                            target_y,
                            target_w: cell_w,
                            target_h: cell_h,
                        },
                    );
                    let x = origin_x + px(visual_x);
                    let y = origin_y + px(visual_y);
                    let cell = Bounds {
                        origin: point(x, y),
                        size: size(px(visual_w), px(visual_h)),
                    };

                    window.with_element_opacity(
                        Some(
                            crate::ui::views::rename::cut_entry_opacity(
                                &app,
                                pane_id,
                                slot.entry_idx,
                                cx,
                            ) * if slot.dimmed {
                                crate::ui::views::rename::DIM_OPACITY
                            } else {
                                1.0
                            },
                        ),
                        |window| {
                            let background = if slot.selected {
                                Some(colors.accent)
                            } else if slot.flashing {
                                Some(colors.accent.opacity(0.38))
                            } else if slot.hovered {
                                Some(colors.hover)
                            } else {
                                None
                            };
                            if let Some(color) = background {
                                paint_rounded(window, cell, 4.0 * zoom, color);
                            }
                            if slot.cursor {
                                let outline_color =
                                    colors
                                        .accent
                                        .opacity(if slot.selected { 0.95 } else { 0.65 });
                                window.paint_quad(
                                    gpui::outline(cell, outline_color, Default::default())
                                        .corner_radii(Corners::all(px(4.0 * zoom))),
                                );
                            }

                            let icon_center =
                                point(x + px(pad_x + icon_slot_w * 0.5), y + px(visual_h * 0.5));
                            if slot.selected && prepaint.show_checkboxes {
                                selection_checkbox::paint_animated(
                                    window,
                                    cx,
                                    icon_center,
                                    15.0 * zoom,
                                    slot.selected_at,
                                );
                            } else {
                                match &slot.icon {
                                    FileIcon::PreviewTexture(tex) => {
                                        crate::ui::thumbs::paint_thumbnail(
                                            window,
                                            cx,
                                            tex,
                                            icon_center,
                                            icon_size,
                                            icon_size,
                                        );
                                    }
                                    FileIcon::IconTexture(tex) => {
                                        let resolved = tex.for_size(icon_size);
                                        let natural = resolved.size(0);
                                        let natural_w = i32::from(natural.width).max(1) as f32;
                                        let natural_h = i32::from(natural.height).max(1) as f32;
                                        let scale = (icon_size / natural_w)
                                            .min(icon_size / natural_h)
                                            .min(1.0);
                                        let w = (natural_w * scale).round();
                                        let h = (natural_h * scale).round();
                                        let half_w = (w * 0.5).round();
                                        let half_h = (h * 0.5).round();
                                        let cx = f32::from(icon_center.x).round();
                                        let cy = f32::from(icon_center.y).round();
                                        let icon_bounds = Bounds {
                                            origin: point(px(cx - half_w), px(cy - half_h)),
                                            size: size(px(w), px(h)),
                                        };
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
                                        let icon_bounds = Bounds {
                                            origin: point(
                                                icon_center.x - px(icon_size * 0.5),
                                                icon_center.y - px(icon_size * 0.5),
                                            ),
                                            size: size(px(icon_size), px(icon_size)),
                                        };
                                        let _ = window.paint_svg(
                                            icon_bounds,
                                            SharedString::from(icon.path()),
                                            None,
                                            Default::default(),
                                            colors.accent,
                                            cx,
                                        );
                                    }
                                }
                            }

                            let name_bounds = list_name_bounds(cell, zoom);
                            if !slot.editing {
                                let _ = slot.name.paint(
                                    name_bounds.origin,
                                    px(visual_h),
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            }
                        },
                    );
                }
            } else {
                for r in prepaint.start_idx..prepaint.end_idx {
                    let Some(slot) = slots.get(r % slots.len()).and_then(|s| s.as_ref()) else {
                        continue;
                    };
                    if slot.row != r {
                        continue;
                    }
                    let col = slot.row % cols;
                    let row = slot.row / cols;
                    let target_x = grid_pad_x + col as f32 * pitch_x;
                    let target_y = grid_pad_y + row as f32 * pitch_y;
                    let (visual_x, visual_y, visual_w, visual_h) = compute_flip(
                        slot.entry_idx,
                        target_x,
                        target_y,
                        cell_w,
                        cell_h,
                        animate,
                        now,
                        last_bounds,
                        animations,
                        &mut has_active_anims,
                    );
                    next_bounds.insert(
                        slot.entry_idx,
                        ItemBounds2D {
                            visual_x,
                            visual_y,
                            visual_w,
                            visual_h,
                            target_x,
                            target_y,
                            target_w: cell_w,
                            target_h: cell_h,
                        },
                    );
                    let x = origin_x + px(visual_x);
                    let y = origin_y + px(visual_y);
                    let cell = Bounds {
                        origin: point(x, y),
                        size: size(px(visual_w), px(visual_h)),
                    };

                    window.with_element_opacity(
                        Some(
                            crate::ui::views::rename::cut_entry_opacity(
                                &app,
                                pane_id,
                                slot.entry_idx,
                                cx,
                            ) * if slot.dimmed {
                                crate::ui::views::rename::DIM_OPACITY
                            } else {
                                1.0
                            },
                        ),
                        |window| {
                            let background = if slot.selected {
                                Some(colors.accent)
                            } else if slot.flashing {
                                Some(colors.accent.opacity(0.38))
                            } else if slot.hovered {
                                Some(colors.hover)
                            } else {
                                None
                            };
                            if let Some(color) = background {
                                paint_rounded(window, cell, 6.0 * zoom, color);
                            }
                            if slot.cursor {
                                let outline_color =
                                    colors
                                        .accent
                                        .opacity(if slot.selected { 0.95 } else { 0.65 });
                                window.paint_quad(
                                    gpui::outline(cell, outline_color, Default::default())
                                        .corner_radii(Corners::all(px(6.0 * zoom))),
                                );
                            }

                            let scale_h = if cell_h > 1.0 {
                                (visual_h / cell_h).clamp(0.2, 5.0)
                            } else {
                                1.0
                            };
                            let item_icon_area_h =
                                (icon_area_h * scale_h).min(visual_h - label_h).max(0.0);
                            let icon_center =
                                point(x + px(visual_w * 0.5), y + px(item_icon_area_h * 0.5));
                            if slot.selected && prepaint.show_checkboxes {
                                selection_checkbox::paint_animated(
                                    window,
                                    cx,
                                    icon_center,
                                    16.0 * zoom,
                                    slot.selected_at,
                                );
                            } else {
                                match &slot.icon {
                                    FileIcon::PreviewTexture(tex) => {
                                        let item_thumb = (thumb * scale_h).max(1.0);
                                        crate::ui::thumbs::paint_thumbnail(
                                            window,
                                            cx,
                                            tex,
                                            icon_center,
                                            item_thumb,
                                            item_icon_area_h,
                                        );
                                    }
                                    FileIcon::IconTexture(tex) => {
                                        let item_thumb = (thumb * scale_h).max(1.0);
                                        let sz = ((item_thumb * GRID_TEXTURE_ICON_MAX)
                                            .min(item_icon_area_h))
                                        .round();
                                        let half = (sz * 0.5).round();
                                        let cx = f32::from(icon_center.x).round();
                                        let cy = f32::from(icon_center.y).round();
                                        let icon_bounds = Bounds {
                                            origin: point(px(cx - half), px(cy - half)),
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
                                        let item_thumb = (thumb * scale_h).max(1.0);
                                        let sz =
                                            (item_thumb * GRID_BUILTIN_ICON).min(item_icon_area_h);
                                        let icon_bounds = Bounds {
                                            origin: point(
                                                icon_center.x - px(sz * 0.5),
                                                icon_center.y - px(sz * 0.5),
                                            ),
                                            size: size(px(sz), px(sz)),
                                        };
                                        let _ = window.paint_svg(
                                            icon_bounds,
                                            SharedString::from(icon.path()),
                                            None,
                                            Default::default(),
                                            if slot.selected {
                                                colors.accent_fg
                                            } else {
                                                colors.muted
                                            },
                                            cx,
                                        );
                                    }
                                }
                            }

                            let name_bounds = grid_label_bounds(cell, item_icon_area_h, zoom);
                            if !slot.editing {
                                let _ = slot.name.paint(
                                    point(name_bounds.origin.x, name_bounds.origin.y),
                                    px(label_h),
                                    TextAlign::Center,
                                    Some(name_bounds.size.width),
                                    window,
                                    cx,
                                );
                            }
                        },
                    );
                }
            }

            if has_active_anims {
                window.request_animation_frame();
            }
            animations
                .retain(|_, anim| now.saturating_duration_since(anim.start_time) < FLIP_DURATION);
            *last_bounds = next_bounds;

            let Some(app_entity) = app.upgrade() else {
                return;
            };
            let move_view = app_entity.clone();
            let move_hitbox = prepaint.hitbox.clone();
            window.on_mouse_event::<MouseMoveEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble || ev.dragging() {
                    return;
                }
                let pos = ev.position;
                move_view.update(cx, |this, cx| {
                    let hover_entry = move_hitbox
                        .is_hovered(window)
                        .then(|| {
                            let hit_model = match layout {
                                GridSurfaceLayout::Thumbnail => this.grid_hit.get(&pane_id),
                                GridSurfaceLayout::CompactList => this.list_hit.get(&pane_id),
                            };
                            hit_model
                                .and_then(|model| model.hit_entry(pos))
                                .map(|idx| (pane_id, idx))
                        })
                        .flatten();
                    let hover_changed = match layout {
                        GridSurfaceLayout::Thumbnail => {
                            if this.grid_hover == hover_entry {
                                false
                            } else {
                                this.grid_hover = hover_entry;
                                true
                            }
                        }
                        GridSurfaceLayout::CompactList => {
                            if this.list_hover == hover_entry {
                                false
                            } else {
                                this.list_hover = hover_entry;
                                true
                            }
                        }
                    };
                    if hover_changed {
                        gpui_component::tooltip::Tooltip::dismiss(window, cx);
                        this.notify_pane(pane_id, cx);
                    }
                });
            });

            let down_view = app_entity;
            let down_hitbox = prepaint.hitbox.clone();
            window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let pos = ev.position;
                let is_in_surface = {
                    let app = down_view.read(cx);
                    let hit_model = match layout {
                        GridSurfaceLayout::Thumbnail => app.grid_hit.get(&pane_id),
                        GridSurfaceLayout::CompactList => app.list_hit.get(&pane_id),
                    };
                    hit_model.is_some_and(|model| model.bounds.contains(&pos))
                };
                if !down_hitbox.is_hovered(window) || !is_in_surface {
                    return;
                }
                down_view.update(cx, |this, cx| {
                    let hit_model = match layout {
                        GridSurfaceLayout::Thumbnail => this.grid_hit.get(&pane_id),
                        GridSurfaceLayout::CompactList => this.list_hit.get(&pane_id),
                    };
                    let hit = hit_model.and_then(|model| model.hit_entry(pos));
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
                            this.details_right_mouse_down(pane_id, hit, pos, window, cx)
                        }
                        _ => {}
                    }
                });
                if matches!(ev.button, MouseButton::Left | MouseButton::Right) {
                    cx.stop_propagation();
                }
            });
        });
    }
}

fn grid_shape_key(
    entry_idx: usize,
    name: &str,
    max_chars: usize,
    selected: bool,
    dimmed: bool,
    font: &Font,
    font_size: Pixels,
    color: Hsla,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    entry_idx.hash(&mut hasher);
    name.hash(&mut hasher);
    max_chars.hash(&mut hasher);
    selected.hash(&mut hasher);
    dimmed.hash(&mut hasher);
    font.hash(&mut hasher);
    f32::from(font_size).to_bits().hash(&mut hasher);
    (color.hue.into_positive_degrees() / 360.0)
        .to_bits()
        .hash(&mut hasher);
    color.saturation.to_bits().hash(&mut hasher);
    color.lightness.to_bits().hash(&mut hasher);
    color.alpha.to_bits().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::{FileSurfaceShapeCache, GridSurfaceSlot, GridSurfaceState};
    use crate::ui::file_icons::FileIcon;
    use atlas_model::file_entry::FileKind;
    use gpui::ShapedLine;
    use std::cell::Cell;

    fn slot(row: usize) -> GridSurfaceSlot {
        GridSurfaceSlot {
            row,
            entry_idx: row,
            shape_key: row as u64,
            selected: true,
            cursor: true,
            selected_at: None,
            hovered: false,
            flashing: false,
            dimmed: false,
            editing: false,
            name: ShapedLine::default(),
            icon: FileIcon::Builtin {
                kind: FileKind::File,
                extension: None,
            },
        }
    }

    #[test]
    fn rename_keeps_retained_icon_selection_and_geometry_while_replacing_only_the_name() {
        let mut state = GridSurfaceState::default();
        state.ensure_capacity(4);
        for row in 0..3 {
            state.sync(row, row as u64, || slot(row), "test.hit", "test.miss");
        }
        let before = state.collect(0, 3);
        state.touch_dynamic(1, 1, true, true, false, false, false, true, slot(1).icon);
        let edited = state.slots[state.index(1)]
            .as_ref()
            .expect("edited cell retained");
        assert!(edited.editing && edited.selected && edited.cursor);
        assert_eq!(edited.shape_key, before[1].shape_key);
        assert!(matches!(
            edited.icon,
            FileIcon::Builtin {
                kind: FileKind::File,
                ..
            }
        ));
        assert_eq!(
            state
                .collect(0, 3)
                .iter()
                .map(|s| s.row)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        state.touch_dynamic(1, 1, true, true, false, false, false, false, slot(1).icon);
        assert!(!state.collect(1, 2)[0].editing);
        assert!(!before[1].editing);
    }

    #[test]
    fn shared_shape_cache_fills_each_resident_key_once() {
        let mut cache = FileSurfaceShapeCache::default();
        let fills = Cell::new(0);

        let mut fill = || {
            fills.set(fills.get() + 1);
            ShapedLine::default()
        };
        let _ = cache.get_or_insert_with(7, &mut fill);
        let _ = cache.get_or_insert_with(7, &mut fill);

        assert_eq!(fills.get(), 1);
    }

    #[test]
    fn shared_shape_cache_is_bounded_and_evicts_oldest() {
        let mut cache = FileSurfaceShapeCache {
            capacity: 2,
            ..Default::default()
        };
        let _ = cache.get_or_insert_with(1, ShapedLine::default);
        let _ = cache.get_or_insert_with(2, ShapedLine::default);
        let _ = cache.get_or_insert_with(3, ShapedLine::default);

        assert_eq!(cache.lines.len(), 2);
        assert!(!cache.lines.contains_key(&1));
        assert!(cache.lines.contains_key(&2));
        assert!(cache.lines.contains_key(&3));
    }

    #[test]
    fn compute_flip_interpolates_target_motion() {
        use super::{compute_flip, ActiveFlipAnim, ItemBounds2D, FLIP_DURATION};
        use std::collections::HashMap;
        use std::time::Instant;

        let mut last_bounds = HashMap::new();
        let mut animations = HashMap::new();
        let mut has_active_anims = false;

        // Baseline: item at (100, 100, 80, 80)
        last_bounds.insert(
            42,
            ItemBounds2D {
                visual_x: 100.0,
                visual_y: 100.0,
                visual_w: 80.0,
                visual_h: 80.0,
                target_x: 100.0,
                target_y: 100.0,
                target_w: 80.0,
                target_h: 80.0,
            },
        );

        let t0 = Instant::now();
        // Zoom happens: target shifts to (150, 120, 100, 100)
        let (vx0, vy0, vw0, vh0) = compute_flip(
            42,
            150.0,
            120.0,
            100.0,
            100.0,
            true,
            t0,
            &last_bounds,
            &mut animations,
            &mut has_active_anims,
        );

        // At t0, visual position is preserved at old position (100, 100, 80, 80)
        assert!(has_active_anims);
        assert!((vx0 - 100.0).abs() < 0.01);
        assert!((vy0 - 100.0).abs() < 0.01);
        assert!((vw0 - 80.0).abs() < 0.01);
        assert!((vh0 - 80.0).abs() < 0.01);

        // Frame ends: last_bounds updated with frame 0 output
        last_bounds.insert(
            42,
            ItemBounds2D {
                visual_x: vx0,
                visual_y: vy0,
                visual_w: vw0,
                visual_h: vh0,
                target_x: 150.0,
                target_y: 120.0,
                target_w: 100.0,
                target_h: 100.0,
            },
        );

        // Mid-flight: halfway through FLIP_DURATION
        has_active_anims = false;
        let t_mid = t0 + FLIP_DURATION / 2;
        let (vx_mid, vy_mid, _, _) = compute_flip(
            42,
            150.0,
            120.0,
            100.0,
            100.0,
            true,
            t_mid,
            &last_bounds,
            &mut animations,
            &mut has_active_anims,
        );
        // Visual position has progressed towards target (between 100 and 150)
        assert!(has_active_anims);
        assert!(vx_mid > 100.0 && vx_mid < 150.0);
        assert!(vy_mid > 100.0 && vy_mid < 120.0);

        // Frame ends: last_bounds updated with mid-flight output
        last_bounds.insert(
            42,
            ItemBounds2D {
                visual_x: vx_mid,
                visual_y: vy_mid,
                visual_w: 100.0,
                visual_h: 100.0,
                target_x: 150.0,
                target_y: 120.0,
                target_w: 100.0,
                target_h: 100.0,
            },
        );

        // Settled: after FLIP_DURATION
        has_active_anims = false;
        let t_end = t0 + FLIP_DURATION + std::time::Duration::from_millis(10);
        let (vx_end, vy_end, vw_end, vh_end) = compute_flip(
            42,
            150.0,
            120.0,
            100.0,
            100.0,
            true,
            t_end,
            &last_bounds,
            &mut animations,
            &mut has_active_anims,
        );
        assert!(!has_active_anims);
        assert!((vx_end - 150.0).abs() < 0.01);
        assert!((vy_end - 120.0).abs() < 0.01);
        assert!((vw_end - 100.0).abs() < 0.01);
        assert!((vh_end - 100.0).abs() < 0.01);
    }

    #[test]
    fn compute_flip_is_noop_when_disabled() {
        use super::{compute_flip, ItemBounds2D};
        use std::collections::HashMap;
        use std::time::Instant;

        let mut last_bounds = HashMap::new();
        let mut animations = HashMap::new();
        let mut has_active_anims = false;

        last_bounds.insert(
            1,
            ItemBounds2D {
                visual_x: 0.0,
                visual_y: 0.0,
                visual_w: 50.0,
                visual_h: 50.0,
                target_x: 0.0,
                target_y: 0.0,
                target_w: 50.0,
                target_h: 50.0,
            },
        );

        let (vx, vy, vw, vh) = compute_flip(
            1,
            200.0,
            200.0,
            100.0,
            100.0,
            false,
            Instant::now(),
            &last_bounds,
            &mut animations,
            &mut has_active_anims,
        );

        assert!(!has_active_anims);
        assert_eq!((vx, vy, vw, vh), (200.0, 200.0, 100.0, 100.0));
    }
}
