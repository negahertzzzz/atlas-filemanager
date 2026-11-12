//! Flat breadcrumb / nav surface: measured text slots + hit map (1 layout node).

use crate::app::App;
use crate::input::action::Action;
use crate::ui::icons::AtlasIcon;
use crate::ui::surface::{
    chrome_font_size_crumbs, hit_first, line_width, paint_fill, paint_rounded, paint_theme_icon,
    shape_text, text_origin_y, Slot,
};
use atlas_model::clamp::ClampLenient;
use atlas_model::location::Location;
use atlas_model::pane::TabId;
use gpui::ColorExt as _;
use gpui::{
    point, px, size, App as GpuiApp, Bounds, ContentMask, DispatchPhase, Element, ElementId, Font,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    Pixels, Refineable, ShapedLine, SharedString, Style, StyleRefinement, Styled, TextAlign,
    WeakEntity, Window,
};
use std::path::{Component, Path};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct CrumbSeg {
    pub label: String,
    pub location: Location,
}

#[derive(Clone, Copy, Debug)]
enum CrumbHit {
    Back,
    Forward,
    Up,
    Bookmark,
    Segment(usize),
    Finder,
}

pub(crate) struct CrumbStripElement {
    id: ElementId,
    pane_id: TabId,
    app: WeakEntity<App>,
    segments: Arc<Vec<CrumbSeg>>,
    can_back: bool,
    can_forward: bool,
    can_up: bool,
    bookmarked: bool,
    font: Font,
    font_size: Pixels,
    windows_theme: bool,
    fg: Hsla,
    muted: Hsla,
    bg: Hsla,
    border: Hsla,
    hover: Hsla,
    accent: Hsla,
    style: StyleRefinement,
}

struct NavBtn {
    slot: Slot,
    icon: AtlasIcon,
    enabled: bool,
    hit: CrumbHit,
}

pub(crate) struct CrumbPrepaint {
    seg_lines: Vec<ShapedLine>,
    seg_slots: Vec<Slot>,
    nav: Vec<NavBtn>,
    strip_h: f32,
    radius: f32,
}

impl CrumbStripElement {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        pane_id: TabId,
        app: WeakEntity<App>,
        segments: Vec<CrumbSeg>,
        can_back: bool,
        can_forward: bool,
        can_up: bool,
        bookmarked: bool,
        font: Font,
        font_size: Pixels,
        windows_theme: bool,
        fg: Hsla,
        muted: Hsla,
        bg: Hsla,
        border: Hsla,
        _hover: Hsla,
        accent: Hsla,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from(format!("crumb-surface-{}", pane_id.0))),
            pane_id,
            app,
            segments: Arc::new(segments),
            can_back,
            can_forward,
            can_up,
            bookmarked,
            font,
            font_size,
            windows_theme,
            fg,
            muted,
            bg,
            border,
            hover: crate::ui::glass::control_hover(fg),
            accent,
            style: StyleRefinement::default(),
        }
    }
}

/// Build breadcrumb segments from a filesystem path (Windows-friendly).
pub(crate) fn path_segments(path: &Path) -> Vec<CrumbSeg> {
    let mut segments = Vec::new();
    let mut cumulative = std::path::PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => {
                cumulative.push(p.as_os_str());
                let raw = atlas_core::util::display::display_name(Path::new(p.as_os_str()));
                // "C:" looks better than "C:\" for the drive crumb.
                let label = raw.trim_end_matches(['\\', '/']).to_string();
                segments.push(CrumbSeg {
                    label: if label.is_empty() { raw } else { label },
                    location: Location::Directory(cumulative.clone()),
                });
            }
            Component::RootDir => {
                // Skip lone "\" after drive prefix; path already rooted.
                if cumulative.as_os_str().is_empty() {
                    cumulative.push(comp.as_os_str());
                    segments.push(CrumbSeg {
                        label: "/".into(),
                        location: Location::Directory(cumulative.clone()),
                    });
                } else {
                    // Ensure cumulative is absolute without a visible "\" crumb.
                    let s = cumulative.as_os_str().to_string_lossy();
                    if !s.ends_with('\\') && !s.ends_with('/') {
                        cumulative.push(comp.as_os_str());
                    }
                }
            }
            Component::Normal(s) => {
                cumulative.push(s);
                let label = s.to_string_lossy().into_owned();
                segments.push(CrumbSeg {
                    label,
                    location: Location::Directory(cumulative.clone()),
                });
            }
            Component::CurDir | Component::ParentDir => {
                cumulative.push(comp.as_os_str());
            }
        }
    }
    segments
}

impl IntoElement for CrumbStripElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for CrumbStripElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Element for CrumbStripElement {
    type RequestLayoutState = Style;
    type PrepaintState = CrumbPrepaint;

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
        crate::perf::count("details.crumbs.layout");
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let strip_h = if self.windows_theme { 44.0 } else { 36.0 } * zoom;
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
        _cx: &mut GpuiApp,
    ) -> Self::PrepaintState {
        let _perf = crate::perf::span("details.crumbs.prepaint");
        crate::perf::count("details.crumbs.prepaint");

        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        self.font_size = chrome_font_size_crumbs(window, self.windows_theme);
        let strip_h = if self.windows_theme { 44.0 } else { 36.0 } * zoom;
        let btn = if self.windows_theme { 34.0 } else { 26.0 } * zoom;
        let gap = if self.windows_theme { 9.0 } else { 6.0 } * zoom;
        let pad_x = 12.0 * zoom;
        let radius = if self.windows_theme { 8.0 } else { 7.0 } * zoom;
        let seg_h = if self.windows_theme { 32.0 } else { 26.0 } * zoom;
        let seg_pad = if self.windows_theme { 10.0 } else { 8.0 } * zoom;
        let chevron_w = 14.0 * zoom;
        let max_seg = if self.windows_theme { 260.0 } else { 220.0 } * zoom;

        let y_btn = (strip_h - btn) * 0.5;
        let mut x = pad_x;
        let mut nav = Vec::new();
        for (icon, enabled, hit) in [
            (AtlasIcon::ArrowLeft, self.can_back, CrumbHit::Back),
            (AtlasIcon::ArrowRight, self.can_forward, CrumbHit::Forward),
            (AtlasIcon::ArrowUp, self.can_up, CrumbHit::Up),
            (
                if self.bookmarked {
                    AtlasIcon::StarFill
                } else {
                    AtlasIcon::Star
                },
                true,
                CrumbHit::Bookmark,
            ),
        ] {
            nav.push(NavBtn {
                slot: Slot::new(x, y_btn, btn, btn),
                icon,
                enabled,
                hit,
            });
            x += btn + gap;
        }
        // divider + gap
        x += 4.0 * zoom + gap;

        let mut seg_lines = Vec::with_capacity(self.segments.len());
        let mut seg_slots = Vec::with_capacity(self.segments.len());
        let y_seg = (strip_h - seg_h) * 0.5;
        let avail_right = f32::from(bounds.size.width) - pad_x;

        for (i, seg) in self.segments.iter().enumerate() {
            if i > 0 {
                x += chevron_w;
            }
            let line = shape_text(&seg.label, &self.font, self.font_size, self.fg, window);
            let text_w = line_width(&line);
            let w = (text_w + seg_pad * 2.0).clamp_lenient(28.0 * zoom, max_seg);
            // Clip trailing crumbs if overflow
            if x + w > avail_right && i + 1 < self.segments.len() {
                // keep going with smaller; last ones matter most — simple: shrink
            }
            let slot = Slot::new(x, y_seg, w.min((avail_right - x).max(0.0)), seg_h);
            x += slot.w + 4.0 * zoom;
            seg_lines.push(line);
            seg_slots.push(slot);
        }

        if let Some(app) = self.app.upgrade() {
            let pane = self.pane_id;
            let mut targets = Vec::new();
            for (i, seg) in self.segments.iter().enumerate() {
                let slot = seg_slots[i];
                let seg_bounds = slot.to_bounds(bounds.origin);
                if let Location::Directory(path) = &seg.location {
                    targets.push(crate::ui::drag_hover::DragHoverTarget {
                        id: crate::ui::drag_hover::DragHoverId::Breadcrumb(pane, path.clone()),
                        bounds: seg_bounds.intersect(&bounds),
                        kind: crate::ui::drag_hover::DragHoverKind::Folder,
                        command: crate::ui::drag_hover::DragHoverCommand::Process(
                            crate::input::action::Action::NavigateLocation {
                                tab: pane,
                                location: seg.location.clone(),
                            },
                        ),
                        drop_command: Some(
                            crate::ui::drag_hover::DragHoverDropCommand::TransferTo(path.clone()),
                        ),
                    });
                }
            }
            if !targets.is_empty() {
                app.update(_cx, |this, _| {
                    for target in targets {
                        this.register_drag_hover_target(target);
                    }
                });
            }
        }

        CrumbPrepaint {
            seg_lines,
            seg_slots,
            nav,
            strip_h,
            radius,
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
        let _perf = crate::perf::span("details.crumbs.paint");
        crate::perf::count("details.crumbs.paint");

        let pane_id = self.pane_id;
        let app = self.app.clone();
        let segments = self.segments.clone();
        let can_back = self.can_back;
        let can_forward = self.can_forward;
        let can_up = self.can_up;
        let fg = self.fg;
        let muted = self.muted;
        let bg = self.bg;
        let border = self.border;
        let hover = self.hover;
        let accent = self.accent;
        let strip_h = prepaint.strip_h;
        let radius = prepaint.radius;
        let origin = bounds.origin;

        let mouse = window.mouse_position();
        let mlx = f32::from(mouse.x - origin.x);
        let mly = f32::from(mouse.y - origin.y);
        let windows_theme = self.windows_theme;
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        // Old theme_icon sizes for nav (15–18) and chevron (~10–11).
        let nav_icon_px = if windows_theme { 18.0 } else { 15.0 } * zoom;
        let chevron_px = 11.0 * zoom;
        let seg_pad_x = if windows_theme { 10.0 } else { 8.0 } * zoom;

        style.paint(bounds, window, cx, |window, cx| {
            paint_fill(window, bounds, bg);
            paint_fill(
                window,
                Bounds {
                    origin: point(origin.x, origin.y + px(strip_h - 1.0)),
                    size: size(bounds.size.width, px(1.0)),
                },
                border.opacity(0.45),
            );

            // Nav buttons
            for nav in &prepaint.nav {
                let b = nav.slot.to_bounds(origin);
                if nav.slot.contains(mlx, mly) && nav.enabled {
                    paint_rounded(window, b, radius, hover);
                }
                let color = if nav.enabled {
                    fg.opacity(0.88)
                } else {
                    muted.opacity(0.35)
                };
                let icon_color = if matches!(nav.hit, CrumbHit::Bookmark) && self.bookmarked {
                    accent
                } else {
                    color
                };
                paint_theme_icon(
                    window,
                    cx,
                    origin,
                    nav.slot,
                    nav.icon,
                    windows_theme,
                    nav_icon_px,
                    icon_color,
                );
            }

            // Divider after nav
            if let Some(last_nav) = prepaint.nav.last() {
                let dx = last_nav.slot.x + last_nav.slot.w + 4.0;
                paint_fill(
                    window,
                    Bounds {
                        origin: point(origin.x + px(dx), origin.y + px(strip_h * 0.28)),
                        size: size(px(1.0), px(strip_h * 0.44)),
                    },
                    border.opacity(0.35),
                );
            }

            // Segments + chevrons (chevron centered in the gap between slots)
            for (i, (line, slot)) in prepaint
                .seg_lines
                .iter()
                .zip(prepaint.seg_slots.iter())
                .enumerate()
            {
                if i > 0 {
                    let prev = &prepaint.seg_slots[i - 1];
                    let gap = (slot.x - (prev.x + prev.w)).max(chevron_px);
                    let cx0 = prev.x + prev.w + (gap - chevron_px) * 0.5;
                    let cy0 = slot.center_y() - chevron_px * 0.5;
                    paint_theme_icon(
                        window,
                        cx,
                        origin,
                        Slot::new(cx0, cy0, chevron_px, chevron_px),
                        AtlasIcon::CaretRight,
                        windows_theme,
                        chevron_px,
                        muted,
                    );
                }
                let b = slot.to_bounds(origin);
                if slot.contains(mlx, mly) {
                    paint_rounded(window, b, radius * 0.85, hover);
                }
                let text_x = b.origin.x + px(seg_pad_x);
                let text_y = origin.y + px(text_origin_y(slot.y, zoom));
                window.with_content_mask(
                    Some(ContentMask {
                        bounds: b.intersect(&bounds),
                        fade_out: Default::default(),
                    }),
                    |window| {
                        // line_height = slot height → GPUI centers glyphs (same as Details rows).
                        let _ = line.paint(
                            point(text_x, text_y),
                            px(slot.h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    },
                );
            }

            // Hit map: priority close-ish zones first isn't needed; list order
            let mut hits: Vec<(Slot, CrumbHit)> = Vec::new();
            for nav in &prepaint.nav {
                hits.push((nav.slot, nav.hit));
            }
            for (i, slot) in prepaint.seg_slots.iter().enumerate() {
                hits.push((*slot, CrumbHit::Segment(i)));
            }
            // Empty strip area → finder
            hits.push((
                Slot::new(0.0, 0.0, f32::from(bounds.size.width), strip_h),
                CrumbHit::Finder,
            ));

            let Some(app_entity) = app.upgrade() else {
                return;
            };
            let segs = segments.clone();
            let hits = Arc::new(hits);
            let hit_view = app_entity;
            window.on_mouse_event::<MouseDownEvent>(move |ev, phase, _window, cx| {
                if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                    return;
                }
                if !bounds.contains(&ev.position) {
                    return;
                }
                let lx = f32::from(ev.position.x - bounds.origin.x);
                let ly = f32::from(ev.position.y - bounds.origin.y);
                let Some(hit) = hit_first(&hits, lx, ly) else {
                    return;
                };
                hit_view.update(cx, |this, cx| {
                    match hit {
                        CrumbHit::Back if can_back => {
                            this.process_actions(vec![Action::NavigateBack(pane_id)]);
                        }
                        CrumbHit::Forward if can_forward => {
                            this.process_actions(vec![Action::NavigateForward(pane_id)]);
                        }
                        CrumbHit::Up if can_up => {
                            this.process_actions(vec![Action::NavigateUp(pane_id)]);
                        }
                        CrumbHit::Bookmark => {
                            this.activate_tab(pane_id);
                            if let Some(path) = this
                                .tab_data_for(pane_id)
                                .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()))
                            {
                                let name = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| path.display().to_string());
                                let idx = this
                                    .state
                                    .profiles
                                    .active()
                                    .bookmarks
                                    .iter()
                                    .position(|b| b.path == path);
                                if let Some(idx) = idx {
                                    this.process_actions(vec![Action::RemoveBookmark(idx)]);
                                } else {
                                    this.process_actions(vec![Action::AddBookmark { name, path }]);
                                }
                            }
                        }
                        CrumbHit::Segment(i) => {
                            if let Some(seg) = segs.get(i) {
                                this.process_actions(vec![Action::NavigateLocation {
                                    tab: pane_id,
                                    location: seg.location.clone(),
                                }]);
                            }
                        }
                        CrumbHit::Finder => {
                            this.activate_tab(pane_id);
                            this.open_location_finder_or_editor();
                        }
                        _ => {}
                    }
                    cx.notify();
                });
                cx.stop_propagation();
            });
        });
    }
}
