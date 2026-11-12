//! Single-node Details column header (replaces ~30–40 nested divs per pane).
//!
//! Paints labels / sort glyphs / resize grips; handles sort click, column reorder,
//! and resize press. Live resize completion still goes through `interactions.rs`.

use crate::app::{App, ColResizeState, DetailsColumn};
use crate::input::action::Action;
use crate::ui::icons::AtlasIcon;
use crate::ui::selection_checkbox::{self, CheckState};
use crate::ui::surface::{
    chrome_font_size_header, hit_first, paint_rounded, paint_theme_icon, shape_text,
    shape_text_truncated, Slot,
};
use crate::ui::views::details::{
    DetailsColors, DetailsContentLod, DetailsRowLayout, CELL_PAD, DETAILS_HEADER_H,
    ROW_TEXT_Y_NUDGE,
};
use atlas_model::file_entry::{SortColumn, SortDir};
use atlas_model::pane::TabId;
use gpui::ColorExt as _;
use gpui::{
    point, px, size, App as GpuiApp, Bounds, ContentMask, CursorStyle, DispatchPhase, Element,
    ElementId, Font, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    IntoElement, KeyDownEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Refineable, ShapedLine, SharedString, Style, StyleRefinement, Styled, TextAlign,
    WeakEntity, Window,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

const RESIZE_HALF: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeaderHit {
    SelectAll,
    FilterType,
    ResizeName,
    ResizeMeta(DetailsColumn),
    SortName,
    SortMeta(DetailsColumn),
}

struct HeaderPress {
    hit: HeaderHit,
    start_screen_x: f32,
    moved: bool,
    start_order: [DetailsColumn; 3],
    grab_x: f32,
}

pub(crate) struct DetailsHeaderElement {
    id: ElementId,
    pane_id: TabId,
    app: WeakEntity<App>,
    layout: DetailsRowLayout,
    visible: Arc<Vec<usize>>,
    colors: DetailsColors,
    font: Font,
    font_size: Pixels,
    sort_col: SortColumn,
    sort_dir: SortDir,
    has_type_filter: bool,
    /// Snapshot for right-click type filter menu (avoid mut borrow in paint).
    type_labels: Vec<String>,
    dragging: Option<DetailsColumn>,
    content_lod: DetailsContentLod,
    style: StyleRefinement,
}

pub(crate) struct DetailsHeaderPrepaint {
    name_line: Option<ShapedLine>,
    meta_lines: Vec<(DetailsColumn, ShapedLine)>,
    layout: DetailsRowLayout,
    header_h: f32,
    hitbox: Hitbox,
    targets: Vec<(Slot, HeaderHit)>,
    control_hitboxes: Vec<(HeaderHit, Hitbox)>,
    check_state: CheckState,
}

impl DetailsHeaderElement {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        pane_id: TabId,
        app: WeakEntity<App>,
        layout: DetailsRowLayout,
        visible: Arc<Vec<usize>>,
        colors: DetailsColors,
        font: Font,
        font_size: Pixels,
        sort_col: SortColumn,
        sort_dir: SortDir,
        has_type_filter: bool,
        type_labels: Vec<String>,
        dragging: Option<DetailsColumn>,
        _drag_x: f32,
        content_lod: DetailsContentLod,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from(format!(
                "details-header-el-{}",
                pane_id.0
            ))),
            pane_id,
            app,
            layout,
            visible,
            colors,
            font,
            font_size,
            sort_col,
            sort_dir,
            has_type_filter,
            type_labels,
            dragging,
            content_lod,
            style: StyleRefinement::default(),
        }
    }
}

impl IntoElement for DetailsHeaderElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for DetailsHeaderElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

fn shape_label(
    text: &str,
    font: &Font,
    font_size: Pixels,
    color: Hsla,
    window: &Window,
) -> ShapedLine {
    shape_text(text, font, font_size, color, window)
}

fn column_label(column: DetailsColumn) -> String {
    match column {
        DetailsColumn::Size => rust_i18n::t!("column_size").into_owned(),
        DetailsColumn::Modified => rust_i18n::t!("column_modified").into_owned(),
        DetailsColumn::Type => rust_i18n::t!("column_type").into_owned(),
    }
}

fn sort_for_column(column: DetailsColumn) -> SortColumn {
    match column {
        DetailsColumn::Size => SortColumn::Size,
        DetailsColumn::Modified => SortColumn::Modified,
        DetailsColumn::Type => SortColumn::Extension,
    }
}

fn header_targets(
    layout: &DetailsRowLayout,
    width: f32,
    height: f32,
    zoom: f32,
) -> Vec<(Slot, HeaderHit)> {
    let mut targets = Vec::new();
    if layout.name_right >= 30.0 * zoom {
        targets.push((
            Slot::new(4.0 * zoom, 0.0, 24.0 * zoom, height),
            HeaderHit::SelectAll,
        ));
    }
    if !layout.columns.is_empty() {
        targets.push((
            Slot::new(
                layout.name_right - RESIZE_HALF * zoom,
                0.0,
                2.0 * RESIZE_HALF * zoom,
                height,
            ),
            HeaderHit::ResizeName,
        ));
    }
    // The trailing edge only mirrors the previous divider, so it gets no grip.
    let last = layout.columns.len().saturating_sub(1);
    for (i, col) in layout.columns.iter().enumerate() {
        let edge = col.x + col.w;
        if i != last && col.w >= 16.0 * zoom {
            targets.push((
                Slot::new(
                    edge - RESIZE_HALF * zoom,
                    0.0,
                    2.0 * RESIZE_HALF * zoom,
                    height,
                ),
                HeaderHit::ResizeMeta(col.column),
            ));
        }
    }
    for col in &layout.columns {
        if col.column == DetailsColumn::Type && col.w >= 52.0 * zoom {
            targets.push((
                Slot::new(col.x + col.w - 30.0 * zoom, 0.0, 22.0 * zoom, height),
                HeaderHit::FilterType,
            ));
        }
    }
    targets.push((
        Slot::new(
            layout.name_x,
            0.0,
            (layout.name_right - layout.name_x).max(0.0),
            height,
        ),
        HeaderHit::SortName,
    ));
    for col in &layout.columns {
        targets.push((
            Slot::new(col.x, 0.0, col.w, height),
            HeaderHit::SortMeta(col.column),
        ));
    }
    for (slot, _) in &mut targets {
        slot.x = slot.x.max(0.0);
        slot.w = slot.w.min((width - slot.x).max(0.0));
    }
    targets
}

fn hit_test(targets: &[(Slot, HeaderHit)], x: f32, y: f32) -> Option<HeaderHit> {
    hit_first(targets, x, y)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_and_resize_grips_win_over_sorting_at_every_zoom() {
        for zoom in [0.75, 1.0, 1.5, 2.0] {
            let layout = DetailsRowLayout {
                name_x: 34.0 * zoom,
                name_col_w: 155.0 * zoom,
                name_right: 200.0 * zoom,
                columns: vec![crate::ui::views::details::DetailsColumnLayout {
                    column: DetailsColumn::Type,
                    x: 200.0 * zoom,
                    w: 120.0 * zoom,
                }],
            };
            let targets = header_targets(&layout, 340.0 * zoom, DETAILS_HEADER_H * zoom, zoom);
            assert_eq!(
                hit_test(&targets, 16.0 * zoom, 12.0 * zoom),
                Some(HeaderHit::SelectAll)
            );
            assert_eq!(
                hit_test(&targets, 200.0 * zoom, 12.0 * zoom),
                Some(HeaderHit::ResizeName)
            );
            assert_eq!(
                hit_test(&targets, 318.0 * zoom, 12.0 * zoom),
                Some(HeaderHit::SortMeta(DetailsColumn::Type))
            );
            assert_eq!(
                hit_test(&targets, 300.0 * zoom, 12.0 * zoom),
                Some(HeaderHit::FilterType)
            );
            assert_eq!(
                hit_test(&targets, 245.0 * zoom, 12.0 * zoom),
                Some(HeaderHit::SortMeta(DetailsColumn::Type))
            );
            assert_eq!(
                hit_test(&targets, 300.0 * zoom, (DETAILS_HEADER_H + 1.0) * zoom),
                None
            );
        }
    }
}

impl Element for DetailsHeaderElement {
    type RequestLayoutState = Style;
    type PrepaintState = DetailsHeaderPrepaint;

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
        crate::perf::count("details.header.layout");
        let zoom_scale = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let header_h = DETAILS_HEADER_H * zoom_scale;
        let mut style = Style::default();
        style.refine(&self.style);
        style.size.width =
            gpui::Length::Definite(gpui::DefiniteLength::Relative(gpui::relative(1.0)));
        style.size.height = gpui::Length::Definite(gpui::DefiniteLength::Absolute(
            gpui::AbsoluteLength::Pixels(px(header_h)),
        ));
        let layout_id = window.request_layout(style.clone(), [], cx);
        (layout_id, style)
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
        let _perf = crate::perf::span("details.header.prepaint");
        crate::perf::count("details.header.prepaint");

        let zoom_scale = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let header_h = DETAILS_HEADER_H * zoom_scale;
        // Header labels use text_xs chrome scale.
        self.font_size = chrome_font_size_header(window);
        let font = &self.font;
        let font_size = self.font_size;
        // Header input is registered on the window from paint; this makes it
        // respect popup/modal occlusion instead of only checking raw bounds.
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let targets = header_targets(
            &self.layout,
            f32::from(bounds.size.width),
            header_h,
            zoom_scale,
        );
        let control_hitboxes = targets
            .iter()
            .filter(|(_, hit)| {
                matches!(
                    hit,
                    HeaderHit::SelectAll
                        | HeaderHit::FilterType
                        | HeaderHit::ResizeName
                        | HeaderHit::ResizeMeta(_)
                )
            })
            .map(|(slot, hit)| {
                (
                    *hit,
                    window.insert_hitbox(slot.to_bounds(bounds.origin), HitboxBehavior::Normal),
                )
            })
            .collect();
        let check_state = self
            .app
            .upgrade()
            .and_then(|app| {
                let app = app.read(cx);
                let selection = &app.state.tab_states.get(&self.pane_id)?.selection;
                Some(if selection.all_visible_selected(&self.visible) {
                    CheckState::Checked
                } else if self
                    .visible
                    .iter()
                    .any(|idx| selection.selected.contains(idx))
                {
                    CheckState::Mixed
                } else {
                    CheckState::Empty
                })
            })
            .unwrap_or(CheckState::Empty);

        let name_active = self.sort_col == SortColumn::Name;
        let name_color = if name_active {
            self.colors.accent
        } else {
            self.colors.muted
        };
        let name_line = self.content_lod.paints_name_header().then(|| {
            shape_label(
                &rust_i18n::t!("column_name"),
                font,
                font_size,
                name_color,
                window,
            )
        });

        let mut meta_lines = Vec::with_capacity(self.layout.columns.len());
        if self.content_lod.paints_metadata() {
            for col in &self.layout.columns {
                let sort = sort_for_column(col.column);
                let active = self.sort_col == sort
                    || (col.column == DetailsColumn::Type && self.has_type_filter);
                let color = if active {
                    self.colors.accent
                } else {
                    self.colors.muted
                };
                let reserve = if col.column == DetailsColumn::Type && col.w >= 52.0 * zoom_scale {
                    42.0
                } else {
                    18.0
                };
                let line = shape_text_truncated(
                    &column_label(col.column),
                    font,
                    font_size,
                    color,
                    (col.w - (CELL_PAD + reserve) * zoom_scale).max(0.0),
                    window,
                );
                meta_lines.push((col.column, line));
            }
        }

        DetailsHeaderPrepaint {
            name_line,
            meta_lines,
            layout: self.layout.clone(),
            header_h,
            hitbox,
            targets,
            control_hitboxes,
            check_state,
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
        let _perf = crate::perf::span("details.header.paint");
        crate::perf::count("details.header.paint");

        let pane_id = self.pane_id;
        let app = self.app.clone();
        let sort_col = self.sort_col.clone();
        let sort_dir = self.sort_dir.clone();
        let has_type_filter = self.has_type_filter;
        let type_labels = self.type_labels.clone();
        let dragging = self.dragging;
        let accent = self.colors.accent;
        let header_bg = self.colors.header_bg;
        let border = self.colors.border;
        let layout_snap = prepaint.layout.clone();
        let header_h = prepaint.header_h;
        // A paint closure is rebuilt every frame; the gesture must survive it.
        let press_state = window.use_keyed_state(
            SharedString::from(format!("details-header-press-{}", pane_id.0)),
            cx,
            |_, _| Rc::new(RefCell::new(None::<HeaderPress>)),
        );
        let press = press_state.read(cx).clone();
        let hover_state = window.use_keyed_state(
            SharedString::from(format!("details-header-hover-{}", pane_id.0)),
            cx,
            |_, _| Rc::new(Cell::new(None::<HeaderHit>)),
        );
        let last_hover = hover_state.read(cx).clone();

        style.paint(bounds, window, cx, |window, cx| {
            let zoom_scale = f32::from(window.rem_size()) / crate::ui::BASE_PX;
            let cell_pad = CELL_PAD * zoom_scale;
            let text_y = bounds.origin.y + px(ROW_TEXT_Y_NUDGE * zoom_scale);
            let hover_hit = if prepaint.hitbox.is_hovered(window) {
                hit_test(&prepaint.targets, f32::from(window.mouse_position().x - bounds.origin.x), f32::from(window.mouse_position().y - bounds.origin.y))
            } else { None };
            window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
            for (hit, hitbox) in &prepaint.control_hitboxes {
                // Register the cursor against fixed geometry; pointer movement
                // then changes it even when the header has no reason to repaint.
                window.set_cursor_style(if matches!(hit, HeaderHit::ResizeName | HeaderHit::ResizeMeta(_)) { CursorStyle::ResizeColumn } else { CursorStyle::PointingHand }, hitbox);
            }

            window.paint_quad(gpui::fill(bounds, header_bg));
            window.paint_quad(gpui::fill(
                Bounds {
                    origin: point(bounds.origin.x, bounds.origin.y + px(header_h - 1.0)),
                    size: size(bounds.size.width, px(1.0)),
                },
                if self.colors.glass { border } else { border.opacity(0.45) },
            ));

            if let Some((slot, _)) = prepaint.targets.iter().find(|(_, hit)| *hit == HeaderHit::SelectAll) {
                if hover_hit == Some(HeaderHit::SelectAll) && !self.visible.is_empty() {
                    paint_rounded(window, Slot::new(slot.x, 3.0 * zoom_scale, slot.w, header_h - 6.0 * zoom_scale).to_bounds(bounds.origin), 4.0 * zoom_scale, accent.opacity(0.08));
                }
                selection_checkbox::paint_control(window, cx, point(bounds.left() + px(slot.x + slot.w * 0.5), bounds.top() + px(header_h * 0.5)), 14.0 * zoom_scale, prepaint.check_state, self.visible.is_empty());
            }

            let paint_sort_glyph = |window: &mut Window,
                                    cx: &mut GpuiApp,
                                    active: bool,
                                    dir: &SortDir,
                                    glyph_right: f32,
                                    mid_y: f32| {
                if !active {
                    return;
                }
                let icon_sz = 9.0 * zoom_scale;
                let kind = if *dir == SortDir::Asc {
                    AtlasIcon::ArrowUp
                } else {
                    AtlasIcon::ArrowDown
                };
                let local_x = glyph_right - icon_sz - f32::from(bounds.origin.x);
                let local_y = mid_y - icon_sz * 0.5 - f32::from(bounds.origin.y);
                // Tiny sort arrows: Phosphor SVG stays sharper at 9px than Fluent.
                paint_theme_icon(
                    window,
                    cx,
                    bounds.origin,
                    Slot::new(local_x, local_y, icon_sz, icon_sz),
                    kind,
                    false,
                    icon_sz,
                    accent,
                );
            };

            // Name
            {
                let name_x =
                    bounds.origin.x + px(layout_snap.name_x + cell_pad.min(layout_snap.name_col_w));
                let name_clip = Bounds {
                    origin: point(bounds.origin.x + px(layout_snap.name_x), bounds.origin.y),
                    size: size(px(layout_snap.name_col_w.max(0.0)), px(header_h)),
                };
                if self.content_lod.paints_name_header()
                    && layout_snap.name_col_w > 15.0 * zoom_scale
                {
                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: name_clip.intersect(&bounds),
                            fade_out: Default::default(),
                        }),
                        |window| {
                            if let Some(line) = &prepaint.name_line {
                                let _ = line.paint(
                                    point(name_x, text_y),
                                    px(header_h),
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            }
                        },
                    );
                }
                if self.content_lod.paints_name_header()
                    && layout_snap.name_col_w > 24.0 * zoom_scale
                {
                    paint_sort_glyph(
                        window,
                        cx,
                        sort_col == SortColumn::Name,
                        &sort_dir,
                        f32::from(bounds.origin.x) + layout_snap.name_right - 4.0 * zoom_scale,
                        f32::from(bounds.origin.y) + header_h * 0.5,
                    );
                }
            }

            // The attached column is painted last, using the very same x as its cells.
            let mut painted_columns = layout_snap.columns.clone();
            painted_columns.sort_by_key(|c| dragging == Some(c.column));
            for col in &painted_columns {
                let sort = sort_for_column(col.column);
                let active = sort_col == sort;
                let Some((_, line)) = prepaint.meta_lines.iter().find(|(c, _)| *c == col.column)
                else {
                    continue;
                };
                let col_x = bounds.origin.x + px(col.x);
                let text_x = col_x + px(cell_pad.min(col.w));
                let clip = Bounds {
                    origin: point(col_x, bounds.origin.y),
                    size: size(px(col.w.max(0.0)), px(header_h)),
                };
                if dragging == Some(col.column) {
                    if !self.colors.glass {
                        window.paint_quad(gpui::fill(clip, header_bg));
                    }
                    paint_rounded(window, Slot::new(col.x + 2.0 * zoom_scale, 3.0 * zoom_scale, (col.w - 4.0 * zoom_scale).max(0.0), header_h - 6.0 * zoom_scale).to_bounds(bounds.origin), 5.0 * zoom_scale, self.colors.fg.opacity(0.09));
                }
                if col.w > 15.0 * zoom_scale {
                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: clip.intersect(&bounds),
                            fade_out: Default::default(),
                        }),
                        |window| {
                            let _ = line.paint(
                                point(text_x, text_y),
                                px(header_h),
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        },
                    );
                }
                let filter_slot = prepaint.targets.iter().find(|(_, hit)| *hit == HeaderHit::FilterType).map(|(slot, _)| *slot);
                if col.column == DetailsColumn::Type {
                    if let Some(slot) = filter_slot {
                        if has_type_filter || hover_hit == Some(HeaderHit::FilterType) {
                            paint_rounded(window, Slot::new(slot.x, 4.0 * zoom_scale, slot.w, header_h - 8.0 * zoom_scale).to_bounds(bounds.origin), 4.0 * zoom_scale, accent.opacity(if has_type_filter { 0.16 } else { 0.08 }));
                        }
                        paint_theme_icon(window, cx, bounds.origin, Slot::new(slot.x + 4.0 * zoom_scale, (header_h - 14.0 * zoom_scale) * 0.5, 14.0 * zoom_scale, 14.0 * zoom_scale), AtlasIcon::Funnel, false, 14.0 * zoom_scale, if has_type_filter { accent } else { self.colors.muted });
                    }
                }
                if self.content_lod.paints_metadata() && col.w > 28.0 * zoom_scale {
                    paint_sort_glyph(
                        window,
                        cx,
                        active,
                        &sort_dir,
                        f32::from(col_x) + col.w - if col.column == DetailsColumn::Type && filter_slot.is_some() { 32.0 * zoom_scale } else { 8.0 * zoom_scale },
                        f32::from(bounds.origin.y) + header_h * 0.5,
                    );
                }
            }

            let paint_grip = |window: &mut Window, edge_x: f32, hovered: bool| {
                window.paint_quad(gpui::fill(
                    Bounds {
                        origin: point(
                            bounds.origin.x + px(edge_x - if hovered { zoom_scale } else { zoom_scale * 0.5 }),
                            bounds.origin.y + px(if hovered { 4.0 } else { 8.0 } * zoom_scale),
                        ),
                        size: size(px(if hovered { 2.0 * zoom_scale } else { zoom_scale }), px((header_h - if hovered { 8.0 } else { 16.0 } * zoom_scale).max(1.0))),
                    },
                    if hovered { accent.opacity(0.75) } else { self.colors.muted.opacity(0.22) },
                ));
            };
            if !layout_snap.columns.is_empty() {
                paint_grip(window, layout_snap.name_right, matches!(hover_hit, Some(HeaderHit::ResizeName)));
            }
            let last_col = layout_snap.columns.len().saturating_sub(1);
            for (i, col) in layout_snap.columns.iter().enumerate() {
                if i != last_col && col.w >= 16.0 {
                    paint_grip(window, col.x + col.w,
                        matches!(hover_hit, Some(HeaderHit::ResizeMeta(column)) if column == col.column));
                }
            }

            let Some(app_entity) = app.upgrade() else {
                return;
            };

            // Left down: resize or begin click/drag
            let press_dn = press.clone();
            let layout_dn = layout_snap.clone();
            let down_view = app_entity.clone();
            let down_hitbox = prepaint.hitbox.clone();
            let down_targets = prepaint.targets.clone();
            let visible = self.visible.clone();
            let labels_down = type_labels.clone();
            window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if !down_hitbox.is_hovered(window) {
                    return;
                }
                let pos = ev.position;
                if !bounds.contains(&pos) {
                    return;
                }
                let local_x = f32::from(pos.x - bounds.origin.x);
                let local_y = f32::from(pos.y - bounds.origin.y);

                if ev.button == MouseButton::Right {
                    if matches!(
                        hit_test(&down_targets, local_x, local_y),
                        Some(HeaderHit::SortMeta(DetailsColumn::Type))
                    ) {
                        // Opened in a separate handler that has `window`.
                    }
                    cx.stop_propagation();
                    return;
                }
                if ev.button != MouseButton::Left {
                    return;
                }
                let Some(hit) = hit_test(&down_targets, local_x, local_y) else {
                    cx.stop_propagation();
                    return;
                };
                down_view.update(cx, |this, cx| {
                    if this.state.focused_tile != pane_id {
                        this.activate_tab(pane_id);
                        cx.notify();
                    }
                });
                match hit {
                    HeaderHit::SelectAll => down_view.update(cx, |this, cx| {
                        if let Some(tab) = this.state.tab_states.get_mut(&pane_id) { tab.selection.toggle_all_visible(&visible); }
                        this.notify_pane(pane_id, cx);
                    }),
                    HeaderHit::FilterType => down_view.update(cx, |this, cx| {
                        this.open_type_filter_menu(pane_id, point(pos.x, bounds.bottom()), labels_down.clone(), window, cx);
                    }),
                    HeaderHit::ResizeName => {
                        let start_width = layout_dn.name_right;
                        down_view.update(cx, move |this, _| {
                            this.col_resize = Some(ColResizeState {
                                col: "name",
                                pane: pane_id,
                                start_x: f32::from(pos.x),
                                start_width,
                            });
                        });
                    }
                    HeaderHit::ResizeMeta(column) => {
                        let start_width = layout_dn
                            .columns
                            .iter()
                            .find(|col| col.column == column)
                            .map(|col| col.w)
                            .unwrap_or(0.0);
                        down_view.update(cx, move |this, _| {
                            this.col_resize = Some(ColResizeState {
                                col: column.key(),
                                pane: pane_id,
                                start_x: f32::from(pos.x),
                                start_width,
                            });
                        });
                    }
                    HeaderHit::SortName | HeaderHit::SortMeta(_) => {
                        *press_dn.borrow_mut() = Some(HeaderPress {
                            hit,
                            start_screen_x: f32::from(pos.x),
                            moved: false,
                            start_order: down_view.read(cx).details_prefs_for(pane_id).order,
                            grab_x: layout_dn.columns.iter().find(|c| matches!(hit, HeaderHit::SortMeta(column) if column == c.column))
                                .map_or(0.0, |c| local_x - c.x),
                        });
                    }
                }
                cx.stop_propagation();
            });

            // Right-click Type filter (needs Window)
            let right_targets = prepaint.targets.clone();
            let right_view = app_entity.clone();
            let right_hitbox = prepaint.hitbox.clone();
            window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble || ev.button != MouseButton::Right {
                    return;
                }
                if !right_hitbox.is_hovered(window) {
                    return;
                }
                let pos = ev.position;
                if !bounds.contains(&pos) {
                    return;
                }
                let local_x = f32::from(pos.x - bounds.origin.x);
                if !matches!(
                    hit_test(&right_targets, local_x, f32::from(pos.y - bounds.origin.y)),
                    Some(HeaderHit::SortMeta(DetailsColumn::Type) | HeaderHit::FilterType)
                ) {
                    return;
                }
                let labels = type_labels.clone();
                right_view.update(cx, |this, cx| {
                    this.open_type_filter_menu(pane_id, pos, labels, window, cx);
                });
                cx.stop_propagation();
            });

            // Move: live column reorder
            let press_mv = press.clone();
            let move_view = app_entity.clone();
            let move_targets = prepaint.targets.clone();
            let move_hitbox = prepaint.hitbox.clone();
            window.on_mouse_event::<MouseMoveEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let hover = if move_hitbox.is_hovered(window) { hit_test(&move_targets, f32::from(ev.position.x - bounds.left()), f32::from(ev.position.y - bounds.top())) } else { None };
                if last_hover.replace(hover) != hover { window.refresh(); }
                let mut press = press_mv.borrow_mut();
                if let Some(p) = press.as_mut() {
                    let dx = (f32::from(ev.position.x) - p.start_screen_x).abs();
                    if dx > 4.0 {
                        p.moved = true;
                        if let HeaderHit::SortMeta(column) = p.hit {
                            let local_x = f32::from(ev.position.x - bounds.origin.x);
                            let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
                            move_view.update(cx, |this, cx| {
                                this.col_drag_grab_x = p.grab_x;
                                this.col_drag_pane = Some(pane_id);
                                this.reorder_details_live(pane_id, column, local_x, zoom);
                                this.notify_pane(pane_id, cx);
                            });
                        }
                    }
                }
            });

            // Up: sort click or finish reorder
            let press_up = press.clone();
            let up_view = app_entity.clone();
            let sort_col_up = sort_col.clone();
            let sort_dir_up = sort_dir.clone();
            window.on_mouse_event::<MouseUpEvent>(move |ev, phase, _window, cx| {
                if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                    return;
                }
                let Some(p) = press_up.borrow_mut().take() else {
                    return;
                };
                if p.moved {
                    up_view.update(cx, |this, cx| {
                        if this.col_dragging.is_some() {
                            this.finish_details_reorder();
                            this.notify_all_panes(cx);
                        }
                    });
                    return;
                }
                let (col, next_dir) = match p.hit {
                    HeaderHit::SortName => {
                        let next = if sort_col_up == SortColumn::Name && sort_dir_up == SortDir::Asc
                        {
                            SortDir::Desc
                        } else {
                            SortDir::Asc
                        };
                        (SortColumn::Name, next)
                    }
                    HeaderHit::SortMeta(column) => {
                        let sort = sort_for_column(column);
                        let next = if sort_col_up == sort && sort_dir_up == SortDir::Asc {
                            SortDir::Desc
                        } else {
                            SortDir::Asc
                        };
                        (sort, next)
                    }
                    _ => return,
                };
                up_view.update(cx, |this, cx| {
                    this.activate_tab(pane_id);
                    this.process_actions(vec![Action::SetSort { col, dir: next_dir }]);
                    cx.notify();
                });
            });

            window.on_key_event::<KeyDownEvent>(move |ev, phase, _window, cx| {
                if phase != DispatchPhase::Bubble || ev.keystroke.key != "escape" {
                    return;
                }
                let mut gesture = press.borrow_mut();
                if !gesture.as_ref().is_some_and(|p| p.moved) {
                    return;
                }
                let Some(gesture) = gesture.take() else {
                    return;
                };
                app_entity.update(cx, |this, cx| {
                    let mut prefs = this.details_prefs_for(pane_id);
                    prefs.order = gesture.start_order;
                    this.set_details_prefs(pane_id, prefs);
                    this.col_dragging = None;
                    this.col_drag_pane = None;
                    this.notify_all_panes(cx);
                });
                cx.stop_propagation();
            });
        });
    }
}
