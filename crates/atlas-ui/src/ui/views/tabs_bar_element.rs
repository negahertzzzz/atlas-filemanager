//! Flat TabsBar surface: math layout + measured labels + hit slots (1 layout node).

use crate::app::App;
use crate::input::action::Action;
use crate::ui::dg::cut::RectCut;
use crate::ui::drag_hover::{DragHoverCommand, DragHoverId, DragHoverKind, DragHoverTarget};
use crate::ui::icons::AtlasIcon;
use crate::ui::surface::{
    chrome_font_size_tabs, hit_first, line_width, paint_fill, paint_rounded, paint_rounded_corners,
    paint_theme_icon, shape_text, shape_text_truncated, text_origin_y, Slot,
};
use atlas_model::clamp::ClampLenient;
use atlas_model::pane::TabId;
use gpui::ColorExt as _;
use gpui::{
    point, px, size, App as GpuiApp, Bounds, ContentMask, Corners, DispatchPhase, Element,
    ElementId, Font, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Refineable, ShapedLine, SharedString,
    Style, StyleRefinement, Styled, TextAlign, WeakEntity, Window,
};
use std::sync::Arc;
use std::{cell::RefCell, rc::Rc};

/// Resting slots from the final compressed/visible projection, without FLIP.
/// Insertion uses these slots so animated neighbors cannot move the target.
#[derive(Default)]
pub(crate) struct TabDropGeometry {
    slots: Vec<(usize, Slot)>,
    count: usize,
}

impl TabDropGeometry {
    pub(crate) fn boundary_at(&self, x: f32) -> usize {
        self.slots
            .iter()
            .find(|(_, slot)| x < slot.x + slot.w * 0.5)
            .map_or(self.count, |(index, _)| *index)
    }
}

#[derive(Clone)]
pub(crate) struct TabBarItem {
    pub pane_id: TabId,
    pub label: String,
    pub is_active: bool,
    pub tag_rgb: Option<[u8; 3]>,
    pub closing: Option<std::time::Instant>,
    /// Location glyph drawn before the label (folder, recycle bin, ...).
    pub icon: AtlasIcon,
    /// Icon-pack texture (Shell / icon theme folder); wins over `icon`.
    pub icon_tex: Option<std::sync::Arc<gpui::RenderImage>>,
}

#[derive(Clone, Copy, Debug)]
enum TabHit {
    Activate(usize),
    Close(usize),
    NewTab,
}

pub(crate) struct TabsBarElement {
    id: ElementId,
    app: WeakEntity<App>,
    tabs: Arc<Vec<TabBarItem>>,
    drop_geometry: Rc<RefCell<TabDropGeometry>>,
    reorder: Option<crate::ui::tab_reorder::TabReorderPreview>,
    instant: bool,
    active: Option<TabId>,
    font: Font,
    font_size: Pixels,
    windows_theme: bool,
    bar_bg: Hsla,
    active_bg: Hsla,
    hover_bg: Hsla,
    active_fg: Hsla,
    tab_fg: Hsla,
    muted: Hsla,
    border: Hsla,
    style: StyleRefinement,
}

struct TabGeom {
    tab: Slot,
    label: Slot,
    close: Slot,
    tag: Option<Slot>,
    icon: Option<Slot>,
    visible: bool,
    opacity: f32,
}

pub(crate) struct TabsBarPrepaint {
    labels: Vec<ShapedLine>,
    geoms: Vec<TabGeom>,
    new_btn: Slot,
    strip_h: f32,
    radius: f32,
    hitboxes: Arc<Vec<gpui::Hitbox>>,
}

fn tab_hover_targets(
    tabs: &[TabBarItem],
    geoms: &[TabGeom],
    bounds: Bounds<Pixels>,
    mask: Bounds<Pixels>,
) -> Vec<DragHoverTarget> {
    tabs.iter()
        .zip(geoms)
        .filter(|(tab, geom)| geom.visible && tab.closing.is_none())
        .filter_map(|(tab, geom)| {
            let bounds = geom
                .tab
                .to_bounds(bounds.origin)
                .intersect(&bounds)
                .intersect(&mask);
            (bounds.size.width > px(0.0) && bounds.size.height > px(0.0)).then(|| DragHoverTarget {
                id: DragHoverId::Tab(tab.pane_id),
                bounds,
                kind: DragHoverKind::Nav,
                command: DragHoverCommand::Process(Action::FocusTab(tab.pane_id)),
                // The destination is resolved from the live tab at drop time.
                drop_command: None,
            })
        })
        .collect()
}

impl TabsBarElement {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        app: WeakEntity<App>,
        tabs: Vec<TabBarItem>,
        active: Option<TabId>,
        font: Font,
        font_size: Pixels,
        windows_theme: bool,
        bar_bg: Hsla,
        active_bg: Hsla,
        hover_bg: Hsla,
        active_fg: Hsla,
        tab_fg: Hsla,
        muted: Hsla,
        border: Hsla,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from("tabs-bar-surface")),
            app,
            tabs: Arc::new(tabs),
            drop_geometry: Rc::default(),
            reorder: None,
            instant: false,
            active,
            font,
            font_size,
            windows_theme,
            bar_bg,
            active_bg,
            hover_bg,
            active_fg,
            tab_fg,
            muted,
            border,
            style: StyleRefinement::default(),
        }
    }

    pub(crate) fn interaction(
        mut self,
        geometry: Rc<RefCell<TabDropGeometry>>,
        reorder: Option<crate::ui::tab_reorder::TabReorderPreview>,
        instant: bool,
    ) -> Self {
        self.drop_geometry = geometry;
        self.reorder = reorder;
        self.instant = instant;
        self
    }
}

impl IntoElement for TabsBarElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for TabsBarElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Element for TabsBarElement {
    type RequestLayoutState = Style;
    type PrepaintState = TabsBarPrepaint;

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
        crate::perf::count("tabs.bar.layout");
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let strip_h = if self.windows_theme { 46.0 } else { 34.0 } * zoom;
        let mut style = Style::default();
        style.refine(&self.style);
        style.size.width =
            gpui::Length::Definite(gpui::DefiniteLength::Relative(gpui::relative(1.0)));
        style.size.height = gpui::Length::Definite(gpui::DefiniteLength::Absolute(
            gpui::AbsoluteLength::Pixels(px(strip_h)),
        ));
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
        let _perf = crate::perf::span("tabs.bar.prepaint");
        crate::perf::count("tabs.bar.prepaint");
        let mut hitboxes = vec![window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal)];

        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        // Force chrome type scale (ignore body font_size from caller).
        self.font_size = chrome_font_size_tabs(window, self.windows_theme);
        let strip_h = if self.windows_theme { 46.0 } else { 40.0 } * zoom;
        let tab_h = if self.windows_theme { 38.0 } else { 30.0 } * zoom;
        let tab_min = if self.windows_theme { 118.0 } else { 72.0 } * zoom;
        let tab_max = if self.windows_theme { 210.0 } else { 220.0 } * zoom;
        let gap = if self.windows_theme { 8.0 } else { 6.0 } * zoom;
        let close_sz = if self.windows_theme { 22.0 } else { 18.0 } * zoom;
        let pad_x = 8.0 * zoom;
        let pad_in = 10.0 * zoom;
        let radius = if self.windows_theme { 8.0 } else { 8.0 } * zoom;
        let new_sz = if self.windows_theme { 32.0 } else { 26.0 } * zoom;
        let tag_dot = 9.0 * zoom;
        let icon_w = if self.windows_theme { 18.0 } else { 16.0 } * zoom;

        // Shape labels with final colors (active vs idle).
        let mut labels = Vec::with_capacity(self.tabs.len());
        let mut natural_ws = Vec::with_capacity(self.tabs.len());
        for tab in self.tabs.iter() {
            let color = if tab.is_active {
                self.active_fg
            } else {
                self.tab_fg
            };
            let line = shape_text(&tab.label, &self.font, self.font_size, color, window);
            let text_w = line_width(&line);
            let tag_w = if tab.tag_rgb.is_some() {
                tag_dot + 6.0 * zoom
            } else {
                icon_w + 6.0 * zoom
            };
            // pad + tag + text + gap + close + pad
            let natural = (pad_in + tag_w + text_w + 6.0 * zoom + close_sz + pad_in)
                .clamp_lenient(tab_min, tab_max);
            natural_ws.push(natural);
            labels.push(line);
        }

        let avail = (f32::from(bounds.size.width) - pad_x * 2.0 - new_sz - gap).max(0.0);

        // Progressive Disclosure: if the space is too small for all tabs,
        // start hiding inactive ones (just like Filepilot).
        let min_required = self.tabs.len() as f32 * (tab_min * 0.55 + gap);
        let mut visible_tabs: Vec<bool> = vec![true; self.tabs.len()];
        let mut sum_visible = 0.0;

        if min_required > avail && self.tabs.len() > 1 {
            // We need to hide tabs. Always keep the active tab visible.
            for (i, tab) in self.tabs.iter().enumerate() {
                if !tab.is_active {
                    visible_tabs[i] = false;
                } else {
                    sum_visible += natural_ws[i];
                }
            }
        } else {
            sum_visible = natural_ws.iter().sum();
        }

        let visible_count = visible_tabs.iter().filter(|visible| **visible).count();
        let tab_space = (avail - gap * visible_count.saturating_sub(1) as f32).max(0.0);
        let scale = if sum_visible > tab_space && sum_visible > 0.0 {
            tab_space / sum_visible
        } else {
            1.0
        };

        let y0 = (strip_h - tab_h) * 0.5;
        let mut slicer = RectCut::new(pad_x, y0, avail, tab_h);
        let mut geoms = Vec::with_capacity(self.tabs.len());
        for (i, tab) in self.tabs.iter().enumerate() {
            if !visible_tabs[i] {
                // Invisible tabs get a zero-width slot
                geoms.push(TabGeom {
                    tab: Slot::new(slicer.x, y0, 0.0, 0.0),
                    label: Slot::new(slicer.x, y0, 0.0, 0.0),
                    close: Slot::new(slicer.x, y0, 0.0, 0.0),
                    tag: None,
                    icon: None,
                    visible: false,
                    opacity: 0.0,
                });
                continue;
            }

            let openness = tab.closing.map_or(1.0, |started| {
                if self.instant {
                    return 0.0;
                }
                let t = started.elapsed().as_secs_f32() * 1000.0 / crate::ui::motion::TAB_MS as f32;
                if t < 1.0 {
                    window.request_animation_frame();
                }
                1.0 - crate::ui::motion::ease_out_quart(t)
            });
            let tw = (natural_ws[i] * scale).max(tab_min * 0.55) * openness;
            let tab_slot = slicer.cut_left(tw.min(slicer.w.max(0.0)));
            slicer.cut_left(gap * openness);

            let mut tab_cut = RectCut::new(tab_slot.x, tab_slot.y, tab_slot.w, tab_slot.h);
            tab_cut.pad(pad_in * scale.max(0.5), 0.0);

            let tag = if tab.tag_rgb.is_some() {
                let tag_cut = tab_cut.cut_left(tag_dot);
                tab_cut.cut_left(6.0 * zoom);
                Some(Slot::new(
                    tag_cut.x,
                    y0 + (tab_h - tag_dot) * 0.5,
                    tag_dot,
                    tag_dot,
                ))
            } else {
                None
            };
            let icon = if tag.is_none() && tab_cut.w > icon_w + close_sz + 24.0 * zoom {
                let icon_cut = tab_cut.cut_left(icon_w);
                tab_cut.cut_left(6.0 * zoom);
                Some(Slot::new(
                    icon_cut.x,
                    y0 + (tab_h - icon_w) * 0.5,
                    icon_w,
                    icon_w,
                ))
            } else {
                None
            };
            let close_raw = tab_cut.cut_right(close_sz);
            let close = Slot::new(
                close_raw.x,
                y0 + (tab_h - close_sz) * 0.5,
                close_sz,
                close_sz,
            );
            tab_cut.cut_right(4.0 * zoom);
            let label_rem = tab_cut.remaining();
            let label = Slot::new(label_rem.x, y0, label_rem.w, tab_h);
            geoms.push(TabGeom {
                tab: tab_slot,
                label,
                close,
                tag,
                icon,
                visible: true,
                opacity: openness,
            });
            let color = if tab.is_active {
                self.active_fg
            } else {
                self.tab_fg
            };
            labels[i] = shape_text_truncated(
                &tab.label,
                &self.font,
                self.font_size,
                color,
                label.w.max(0.0),
                window,
            );
        }

        // Paint, drag starts, and reorder boundaries share this exact projection.
        self.drop_geometry.replace(TabDropGeometry {
            slots: geoms
                .iter()
                .enumerate()
                .filter(|(i, g)| g.visible && self.tabs[*i].closing.is_none())
                .map(|(i, g)| (i, g.tab))
                .collect(),
            count: self.tabs.len(),
        });
        let viewport = [f32::from(bounds.size.width), tab_max, zoom];
        let now = std::time::Instant::now();
        let motion = window.use_keyed_state("tabs-neighbor-motion", cx, |_, _| {
            crate::ui::dg::motion::RowMotion::<TabId>::new(0, viewport)
        });
        let mut targets: Vec<_> = geoms
            .iter()
            .enumerate()
            .filter(|(_, g)| g.visible)
            .map(|(i, g)| (self.tabs[i].pane_id, i, g.tab.x))
            .collect();
        if let Some(preview) = self.reorder.as_ref().filter(|preview| !preview.copying) {
            let ids: Vec<_> = self.tabs.iter().map(|tab| tab.pane_id).collect();
            let mut x = pad_x;
            for pane in
                crate::ui::tab_reorder::projected_order(&ids, preview.dragged, preview.boundary)
            {
                if let Some(target) = targets.iter_mut().find(|(id, _, _)| *id == pane) {
                    target.2 = x;
                    x += geoms[target.1].tab.w + gap;
                }
            }
        }
        // Closing slots already release their width on their own timeline.
        let instant = self.instant || self.tabs.iter().any(|tab| tab.closing.is_some());
        let (positions, animating) = motion.update(cx, |motion, _| {
            motion.sample(&targets, 0, viewport, instant, now)
        });
        for (i, x, opacity) in positions {
            let g = &mut geoms[i];
            let offset = x - g.tab.x;
            g.tab.x += offset;
            g.label.x += offset;
            g.close.x += offset;
            if let Some(tag) = &mut g.tag {
                tag.x += offset;
            }
            g.opacity *= opacity;
        }
        if animating {
            window.request_animation_frame();
        }

        let new_btn = Slot::new(slicer.x, y0 + (tab_h - new_sz) * 0.5, new_sz, new_sz);

        let targets = tab_hover_targets(&self.tabs, &geoms, bounds, window.content_mask().bounds);
        // Native caption hit testing must stop at real controls, just as it did
        // for the old tab chips. During DnD the strip's parent owns the drop, so
        // these foreground hitboxes must not hide that parent's hitbox.
        if !cx.has_active_drag() {
            for control in targets
                .iter()
                .map(|target| target.bounds)
                .chain(std::iter::once(
                    new_btn.to_bounds(bounds.origin).intersect(&bounds),
                ))
            {
                hitboxes.push(
                    window.insert_hitbox(control, gpui::HitboxBehavior::BlockMouseExceptScroll),
                );
            }
        }
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |this, _| {
                // Replace this strip's projection: hidden/closing tabs must not
                // keep old drop rectangles while the other panes remain cached.
                this.pending_drag_hover_targets.retain(|target| {
                    !matches!(target.id, DragHoverId::Tab(id) if self.tabs.iter().any(|tab| tab.pane_id == id))
                });
                for target in targets {
                    this.register_drag_hover_target(target);
                }
            });
        }

        TabsBarPrepaint {
            labels,
            geoms,
            new_btn,
            strip_h,
            radius,
            hitboxes: Arc::new(hitboxes),
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
        let _perf = crate::perf::span("tabs.bar.paint");
        crate::perf::count("tabs.bar.paint");

        let app = self.app.clone();
        let tabs = self.tabs.clone();
        let active_bg = self.active_bg;
        let hover_bg = self.hover_bg;
        let muted = self.muted;
        let bar_bg = self.bar_bg;
        let border = self.border;
        let radius = prepaint.radius;
        let strip_h = prepaint.strip_h;
        let origin = bounds.origin;

        // Hover from live mouse (no extra state).
        let mouse = window.mouse_position();
        let mlx = f32::from(mouse.x - origin.x);
        let mly = f32::from(mouse.y - origin.y);
        let hover_i = prepaint.geoms.iter().position(|g| g.tab.contains(mlx, mly));
        if prepaint
            .hitboxes
            .iter()
            .any(|hitbox| hitbox.is_hovered(window))
            && (hover_i.is_some() || prepaint.new_btn.contains(mlx, mly))
        {
            if let Some(hitbox) = prepaint
                .hitboxes
                .iter()
                .find(|hitbox| hitbox.is_hovered(window))
            {
                window.set_cursor_style(gpui::CursorStyle::PointingHand, hitbox);
            }
        }

        let windows_theme = self.windows_theme;
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        // Match old theme_icon sizes (px @ 16rem, then zoom).
        let close_icon_px = if windows_theme { 11.0 } else { 10.0 } * zoom;
        let plus_icon_px = if windows_theme { 15.0 } else { 13.0 } * zoom;

        style.paint(bounds, window, cx, |window, cx| {
            // Tab bar: rounded top corners like old strip (12px).
            let bar_r = 12.0 * zoom;
            paint_rounded_corners(
                window,
                bounds,
                Corners {
                    top_left: px(bar_r),
                    top_right: px(bar_r),
                    bottom_left: px(0.),
                    bottom_right: px(0.),
                },
                bar_bg,
            );
            paint_fill(
                window,
                Bounds {
                    origin: point(origin.x, origin.y + px(strip_h - 1.0)),
                    size: size(bounds.size.width, px(1.0)),
                },
                border.opacity(0.42),
            );

            for (i, tab) in tabs.iter().enumerate() {
                let g = &prepaint.geoms[i];
                if !g.visible {
                    continue;
                }
                let tab_b = g.tab.to_bounds(origin);
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: tab_b.intersect(&bounds),
                        fade_out: Default::default(),
                    }),
                    |window| {
                        window.with_element_opacity(Some(g.opacity), |window| {
                            if tab.is_active {
                                // The fill alone marks the active tab (no accent underline).
                                paint_rounded(window, tab_b, radius, active_bg);
                            } else if hover_i == Some(i) {
                                paint_rounded(window, tab_b, radius, hover_bg);
                            }

                            if let (Some(tag_slot), Some(rgb)) = (g.tag, tab.tag_rgb) {
                                let c = gpui::rgb(
                                    (u32::from(rgb[0]) << 16)
                                        | (u32::from(rgb[1]) << 8)
                                        | u32::from(rgb[2]),
                                );
                                paint_rounded(
                                    window,
                                    tag_slot.to_bounds(origin),
                                    tag_slot.h * 0.5,
                                    gpui::rgb_to_hsla(c),
                                );
                            }

                            if let (Some(icon_slot), Some(tex)) = (g.icon, tab.icon_tex.clone()) {
                                let side = icon_slot.w.min(icon_slot.h);
                                let image = Slot::new(
                                    icon_slot.x + (icon_slot.w - side) * 0.5,
                                    icon_slot.y + (icon_slot.h - side) * 0.5,
                                    side,
                                    side,
                                );
                                let image = crate::ui::thumbs::contain_bounds(
                                    &tex,
                                    image.to_bounds(origin),
                                );
                                let _ = window.paint_image(
                                    image,
                                    image,
                                    Corners::default(),
                                    tex,
                                    0,
                                    false,
                                );
                            } else if let Some(icon_slot) = g.icon {
                                let icon_color = if tab.is_active { self.active_fg } else { muted };
                                paint_theme_icon(
                                    window,
                                    cx,
                                    origin,
                                    icon_slot,
                                    tab.icon,
                                    windows_theme,
                                    14.0 * zoom,
                                    icon_color,
                                );
                            }

                            if let Some(line) = prepaint.labels.get(i) {
                                let lb = g.label.to_bounds(origin);
                                window.with_content_mask(
                                    Some(ContentMask {
                                        bounds: lb.intersect(&bounds),
                                        fade_out: gpui::Edges {
                                            right: if line.width() > lb.size.width {
                                                px(8.0 * zoom)
                                            } else {
                                                px(0.0)
                                            },
                                            ..Default::default()
                                        },
                                    }),
                                    |window| {
                                        // origin = top of label slot; line_height = slot h → GPUI centers glyphs.
                                        let ty = origin.y + px(text_origin_y(g.label.y, zoom));
                                        let _ = line.paint(
                                            point(lb.origin.x, ty),
                                            px(g.label.h),
                                            TextAlign::Left,
                                            None,
                                            window,
                                            cx,
                                        );
                                    },
                                );
                            }

                            if g.close.contains(mlx, mly) {
                                paint_rounded(
                                    window,
                                    g.close.to_bounds(origin),
                                    radius * 0.6,
                                    border.opacity(0.45),
                                );
                            }
                            paint_theme_icon(
                                window,
                                cx,
                                origin,
                                g.close,
                                AtlasIcon::X,
                                windows_theme,
                                close_icon_px,
                                muted,
                            );
                        });
                    },
                );
            }

            if prepaint.new_btn.contains(mlx, mly) {
                paint_rounded(window, prepaint.new_btn.to_bounds(origin), radius, hover_bg);
            }
            paint_theme_icon(
                window,
                cx,
                origin,
                prepaint.new_btn,
                AtlasIcon::Plus,
                windows_theme,
                plus_icon_px,
                muted,
            );

            // Hit table for input
            let mut hits: Vec<(Slot, TabHit)> = Vec::with_capacity(tabs.len() * 2 + 1);
            for (i, g) in prepaint.geoms.iter().enumerate() {
                if !g.visible || tabs[i].closing.is_some() {
                    continue;
                }
                let close = g
                    .close
                    .to_bounds(origin)
                    .intersect(&g.tab.to_bounds(origin));
                hits.push((
                    Slot::new(
                        f32::from(close.origin.x - origin.x),
                        f32::from(close.origin.y - origin.y),
                        f32::from(close.size.width),
                        f32::from(close.size.height),
                    ),
                    TabHit::Close(i),
                ));
                hits.push((g.tab, TabHit::Activate(i)));
            }
            hits.push((prepaint.new_btn, TabHit::NewTab));

            let Some(app_entity) = app.upgrade() else {
                return;
            };
            let tabs_hit = tabs.clone();
            let active_for_new = self.active;
            let hits = Arc::new(hits);
            let hit_view = app_entity.clone();
            let press = window.use_keyed_state("tab-drag-press", cx, |_, _| {
                None::<(gpui::Point<Pixels>, TabId, gpui::Point<Pixels>)>
            });
            let down_press = press.clone();
            let hitboxes = prepaint.hitboxes.clone();
            let geoms: Vec<_> = prepaint.geoms.iter().map(|g| g.tab).collect();
            window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                    return;
                }
                if !bounds.contains(&ev.position)
                    || !hitboxes.iter().any(|hitbox| hitbox.is_hovered(window))
                {
                    return;
                }
                let lx = f32::from(ev.position.x - bounds.origin.x);
                let ly = f32::from(ev.position.y - bounds.origin.y);
                let Some(hit) = hit_first(&hits, lx, ly) else {
                    return;
                };
                match hit {
                    TabHit::Close(i) => {
                        if let Some(tab) = tabs_hit.get(i) {
                            hit_view.update(cx, |this, cx| {
                                this.process_actions(vec![Action::CloseTab(tab.pane_id)]);
                                cx.notify();
                            });
                        }
                    }
                    TabHit::Activate(i) => {
                        if let Some(tab) = tabs_hit.get(i) {
                            let tab_origin = origin + point(px(geoms[i].x), px(geoms[i].y));
                            down_press.update(cx, |press, _| {
                                *press = Some((ev.position, tab.pane_id, tab_origin))
                            });
                            tracing::debug!(pane = tab.pane_id.0, "dnd tab press");
                        }
                    }
                    TabHit::NewTab => {
                        if let Some(anchor) = active_for_new {
                            hit_view.update(cx, |this, cx| {
                                this.process_actions(vec![Action::NewTab(anchor)]);
                                cx.notify();
                            });
                        }
                    }
                }
                cx.stop_propagation();
            });
            let move_press = press.clone();
            let drag_view = app_entity.clone();
            let hover_hitboxes = prepaint.hitboxes.clone();
            window.on_mouse_event::<MouseMoveEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if bounds.contains(&ev.position)
                    && hover_hitboxes
                        .iter()
                        .any(|hitbox| hitbox.is_hovered(window))
                {
                    window.refresh();
                }
                let pending = move_press.update(cx, |press, _| {
                    if ev.pressed_button != Some(MouseButton::Left) {
                        *press = None;
                        return None;
                    }
                    if press
                        .as_ref()
                        // A deliberate move, not click jitter, starts a tab drag.
                        .is_some_and(|(at, _, _)| (ev.position - *at).magnitude() > 6.0)
                    {
                        press.take()
                    } else {
                        None
                    }
                });
                if cx.has_active_drag() {
                    return;
                }
                let Some((_, pane, tab_origin)) = pending else {
                    return;
                };
                let payload = drag_view.update(cx, |this, cx| {
                    if this.tab_closing.contains_key(&pane) || this.pane_closing.contains_key(&pane)
                    {
                        tracing::debug!(pane = pane.0, "dnd tab start blocked by close");
                        return None;
                    }
                    if !window.modifiers().control {
                        this.animate_pane_drag(pane, true);
                    }
                    this.state.dragging_pane = Some(pane);
                    this.state.drag_pending = false;
                    let content = this.get_or_make_file_panel(pane, cx).clone().into();
                    let container = this.state.find_tabs_container(pane);
                    let size = this
                        .pane_view_bounds
                        .get(&pane)
                        .map(|b| b.size)
                        .or_else(|| {
                            active_for_new
                                .and_then(|a| this.pane_view_bounds.get(&a).map(|b| b.size))
                        })
                        .or_else(|| {
                            container.and_then(|c| this.pane_layout_bounds.get(&c).map(|b| b.size))
                        })
                        .or_else(|| {
                            active_for_new
                                .and_then(|a| this.pane_layout_bounds.get(&a).map(|b| b.size))
                        })
                        .or_else(|| this.pane_layout_bounds.get(&pane).map(|b| b.size))
                        .or_else(|| {
                            this.pane_view_bounds
                                .get(&this.state.focused_tile)
                                .map(|b| b.size)
                        });
                    cx.notify();
                    Some(crate::ui::dock::drag::TabDrag {
                        pane_id: pane,
                        content,
                        size,
                    })
                });
                let Some(payload) = payload else {
                    return;
                };
                let view = crate::ui::dock::drag::tab_drag_entity(
                    pane,
                    payload.content.clone(),
                    payload.size,
                    cx,
                );
                cx.start_drag(gpui::AnyDrag::new(payload, view).offset(ev.position - tab_origin));
                tracing::debug!(pane = pane.0, "dnd tab started");
                window.refresh();
                cx.stop_propagation();
            });
            let up_press = press.clone();
            let up_view = app_entity;
            window.on_mouse_event::<MouseUpEvent>(move |ev, _, _, cx| {
                if ev.button == MouseButton::Left {
                    let clicked = up_press.update(cx, |press, _| press.take());
                    if let Some((_, pane_id, _)) = clicked {
                        up_view.update(cx, |this, cx| {
                            this.process_actions(vec![Action::FocusTab(pane_id)]);
                            cx.notify();
                        });
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_targets_follow_painted_tabs_and_exclude_hidden_closing_and_clipped_tabs() {
        use atlas_model::layout_tree::TileId;

        let mut tabs: Vec<_> = (1..=5)
            .map(|id| TabBarItem {
                pane_id: TileId(id),
                label: String::new(),
                is_active: id == 1,
                tag_rgb: None,
                closing: None,
                icon: AtlasIcon::Folder,
                icon_tex: None,
            })
            .collect();
        tabs[2].closing = Some(std::time::Instant::now());
        let geom = |x, w, visible| TabGeom {
            tab: Slot::new(x, 4.0, w, 26.0),
            label: Slot::default(),
            close: Slot::default(),
            tag: None,
            icon: None,
            visible,
            opacity: 1.0,
        };
        // Final animated slots in a moved pane, with an ancestor clipping both
        // edges. These exact rectangles also protect tabs from native caption
        // dragging and give them priority over the root split zones.
        let geoms = vec![
            geom(20.0, 90.0, true),
            geom(10.0, 70.0, false),
            geom(60.0, 60.0, true),
            geom(160.0, 40.0, true),
            geom(120.0, 80.0, true),
        ];
        let bounds = Bounds::new(point(px(400.0), px(120.0)), size(px(200.0), px(34.0)));
        let mask = Bounds::new(point(px(450.0), px(120.0)), size(px(100.0), px(34.0)));
        let targets = tab_hover_targets(&tabs, &geoms, bounds, mask);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].id, DragHoverId::Tab(TileId(1)));
        assert_eq!(
            targets[0].bounds,
            Bounds::new(point(px(450.0), px(124.0)), size(px(60.0), px(26.0)))
        );
        assert_eq!(targets[1].id, DragHoverId::Tab(TileId(5)));
        assert_eq!(
            targets[1].bounds,
            Bounds::new(point(px(520.0), px(124.0)), size(px(30.0), px(26.0)))
        );
        assert!(matches!(
            targets[1].command,
            DragHoverCommand::Process(Action::FocusTab(TileId(5)))
        ));
        assert!(targets.iter().all(|target| target.drop_command.is_none()));
    }

    #[test]
    fn drop_boundaries_use_compressed_slots_and_preserve_hidden_tab_indices() {
        // A narrow pane paints only children 0 and 3, each compressed to 60 px.
        // Reordering must return tree indices, rather than natural-width slots
        // or indices in the visible subset.
        let geometry = TabDropGeometry {
            count: 4,
            slots: vec![
                (0, Slot::new(8.0, 0.0, 60.0, 26.0)),
                (3, Slot::new(74.0, 0.0, 60.0, 26.0)),
            ],
        };
        assert_eq!(geometry.boundary_at(20.0), 0);
        assert_eq!(geometry.boundary_at(80.0), 3);
        assert_eq!(geometry.boundary_at(120.0), 4);
    }
}
