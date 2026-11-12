//! Digital Grove Experimental Chrome Element for Atlas.
//!
//! Inspired by `pexer` (principles 1, 2, 3, 4, 8 of Digital Grove):
//! - Stateless procedural generation (every frame derived from upstream App state).
//! - Rect Slicing layout (single-cycle O(1) arithmetic instead of Taffy Flexbox).
//! - Direct primitive painting (`paint_quad`, `paint_fill`, `paint_theme_icon`, shaped glyphs).
//! - Native Hitbox registration (`insert_hitbox`) for precise Z-order occlusion.
//! - OLE Drag & Drop target registration for seamless file transfers.
//! - Read-only breadcrumbs and a shared Ctrl+L location finder.

use crate::app::App;
use crate::input::action::Action;
use crate::ui::dg::cut::RectCut;
use crate::ui::drag_hover::{
    DragHoverCommand, DragHoverDropCommand, DragHoverId, DragHoverKind, DragHoverTarget,
};
use crate::ui::icons::AtlasIcon;
use crate::ui::surface::{
    chrome_font_size_crumbs, hit_first, line_width, paint_fill, paint_rounded, paint_theme_icon,
    shape_text_truncated, text_origin_y, Slot,
};
use atlas_model::clamp::ClampLenient;
use atlas_model::location::Location;
use atlas_model::pane::TabId;
use gpui::ColorExt as _;
use gpui::{
    point, px, size, App as GpuiApp, Bounds, DispatchPhase, Element, ElementId, GlobalElementId,
    Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, Pixels, Refineable, ShapedLine, SharedString, Style, StyleRefinement, Styled,
    TextAlign, WeakEntity, Window,
};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DgNavAction {
    #[default]
    Back,
    Forward,
    Up,
    Bookmark,
    Segment(usize),
    Chevron(usize),
    OpenLocation,
}

#[derive(Clone)]
pub(crate) struct DgCrumbSeg {
    pub is_ghost: bool,
    pub label: String,
    pub location: Location,
    pub parent_dir: Option<PathBuf>,
}

pub(crate) struct DgChromeElement {
    id: ElementId,
    pane_id: TabId,
    app: WeakEntity<App>,
    segments: Arc<Vec<DgCrumbSeg>>,
    can_back: bool,
    can_forward: bool,
    can_up: bool,
    bookmarked: bool,
    windows_theme: bool,
    is_glass: bool,
    fg: Hsla,
    muted: Hsla,
    bg: Hsla,
    border: Hsla,
    hover: Hsla,
    accent: Hsla,
    style: StyleRefinement,
}

struct DgNavButtonSlot {
    slot: Slot,
    icon: AtlasIcon,
    enabled: bool,
    action: DgNavAction,
}

struct DgChevronSlot {
    slot: Slot,
    dir: PathBuf,
    idx: usize,
}

pub(crate) struct DgChromePrepaint {
    hitbox: Hitbox,
    nav_buttons: Vec<DgNavButtonSlot>,
    seg_slots: Vec<(Slot, usize)>,
    seg_lines: Vec<ShapedLine>,
    chevron_slots: Vec<DgChevronSlot>,
    /// Filled "address field" behind the breadcrumb segments.
    field: Option<Slot>,
    strip_h: f32,
    radius: f32,
    all_hits: Vec<(Slot, DgNavAction)>,
}

impl DgChromeElement {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        pane_id: TabId,
        app: WeakEntity<App>,
        segments: Arc<Vec<DgCrumbSeg>>,
        can_back: bool,
        can_forward: bool,
        can_up: bool,
        bookmarked: bool,
        windows_theme: bool,
        is_glass: bool,
        fg: Hsla,
        muted: Hsla,
        bg: Hsla,
        border: Hsla,
        hover: Hsla,
        accent: Hsla,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from(format!("dg-chrome-{}", pane_id.0))),
            pane_id,
            app,
            segments,
            can_back,
            can_forward,
            can_up,
            bookmarked,
            windows_theme,
            is_glass,
            fg,
            muted,
            bg,
            border,
            hover,
            accent,
            style: StyleRefinement::default(),
        }
    }
}

impl IntoElement for DgChromeElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for DgChromeElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Element for DgChromeElement {
    type RequestLayoutState = Style;
    type PrepaintState = DgChromePrepaint;

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
        cx: &mut GpuiApp,
    ) -> Self::PrepaintState {
        // Insert opaque hitbox to respect Z-order of popovers and menus above us
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        // Slit/sub-pixel culling: if pane chrome is collapsing/narrower than 12px, bypass layout & processing
        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return DgChromePrepaint {
                hitbox,
                nav_buttons: Vec::new(),
                seg_slots: Vec::new(),
                seg_lines: Vec::new(),
                chevron_slots: Vec::new(),
                field: None,
                strip_h: 0.0,
                radius: 0.0,
                all_hits: Vec::new(),
            };
        }

        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let font = window.text_style().font();
        let font_size = chrome_font_size_crumbs(window, self.windows_theme);
        let strip_h = if self.windows_theme { 44.0 } else { 36.0 } * zoom;
        let btn_sz = if self.windows_theme { 34.0 } else { 26.0 } * zoom;
        let gap = if self.windows_theme { 9.0 } else { 6.0 } * zoom;
        let total_w = f32::from(bounds.size.width);

        let pad_x = if total_w < 180.0 * zoom {
            4.0 * zoom
        } else {
            12.0 * zoom
        };
        let radius = if self.windows_theme { 8.0 } else { 7.0 } * zoom;
        let seg_h = if self.windows_theme { 32.0 } else { 26.0 } * zoom;
        let seg_pad = if self.windows_theme { 10.0 } else { 8.0 } * zoom;
        let chevron_w = 14.0 * zoom;
        let max_seg = if self.windows_theme { 260.0 } else { 220.0 } * zoom;

        let mut slicer = RectCut::new(pad_x, 0.0, (total_w - pad_x * 2.0).max(0.0), strip_h);

        // 1. Cut navigation buttons on the left with graceful responsive degradation
        let mut nav_buttons = Vec::with_capacity(4);
        let y_btn = (strip_h - btn_sz) * 0.5;
        let min_crumbs_w = 36.0 * zoom;

        // Degradation order preserving natural left-to-right order:
        // Buttons: [Back] [Forward] [Up] [Star]
        // Star drops first (< 220px) -> Up drops second (< 160px) -> Forward drops third (< 110px) -> Back drops last (< 65px)
        // Keeps [Back] and [Forward] paired together without odd middle gaps!
        let show_back = total_w >= 65.0 * zoom;
        let show_forward = total_w >= 110.0 * zoom;
        let show_up = total_w >= 160.0 * zoom;
        let show_bookmark = total_w >= 220.0 * zoom;

        let mut candidate_buttons = Vec::with_capacity(4);
        if show_back {
            candidate_buttons.push((AtlasIcon::ArrowLeft, self.can_back, DgNavAction::Back));
        }
        if show_forward {
            candidate_buttons.push((
                AtlasIcon::ArrowRight,
                self.can_forward,
                DgNavAction::Forward,
            ));
        }
        if show_up {
            candidate_buttons.push((AtlasIcon::ArrowUp, self.can_up, DgNavAction::Up));
        }
        if show_bookmark {
            candidate_buttons.push((
                if self.bookmarked {
                    AtlasIcon::StarFill
                } else {
                    AtlasIcon::Star
                },
                true,
                DgNavAction::Bookmark,
            ));
        }

        for (icon, enabled, action) in candidate_buttons {
            if slicer.w < btn_sz + min_crumbs_w {
                break;
            }
            let mut slot = slicer.cut_left(btn_sz);
            if slot.w < btn_sz * 0.75 {
                break;
            }
            slot.y = y_btn;
            slot.h = btn_sz;
            nav_buttons.push(DgNavButtonSlot {
                slot,
                icon,
                enabled,
                action,
            });
            let _ = slicer.cut_left(gap);
        }

        // Gap between the nav buttons and the address field.
        if !nav_buttons.is_empty() && slicer.w >= min_crumbs_w + 10.0 * zoom {
            let _ = slicer.cut_left(6.0 * zoom);
        }

        let address_slot = slicer.remaining();
        let field_inset = 3.0 * zoom;
        let field_h = (seg_h + 2.0 * field_inset).min(strip_h);
        let field = (address_slot.w >= 16.0 * zoom).then(|| {
            Slot::new(
                address_slot.x,
                (strip_h - field_h) * 0.5,
                address_slot.w,
                field_h,
            )
        });
        // Segments sit inside the field, not flush against its rounded edge.
        if field.is_some() {
            let _ = slicer.cut_left(field_inset);
        }
        let mut seg_lines = Vec::new();
        let mut seg_slots = Vec::new();
        let mut chevron_slots = Vec::new();

        if !self.segments.is_empty() {
            let y_seg = (strip_h - seg_h) * 0.5;

            // When space is constrained (< 130px), degrade to showing ONLY the leaf folder
            if slicer.w < 130.0 * zoom {
                let leaf_idx = self
                    .segments
                    .iter()
                    .rposition(|seg| !seg.is_ghost)
                    .unwrap_or(self.segments.len() - 1);
                let seg = &self.segments[leaf_idx];
                let avail_w = (slicer.w - seg_pad * 2.0).max(0.0);
                let line =
                    shape_text_truncated(&seg.label, &font, font_size, self.fg, avail_w, window);
                let text_w = line_width(&line);
                let target_w = (text_w + seg_pad * 2.0).clamp_lenient(16.0 * zoom, slicer.w);
                if target_w >= 16.0 * zoom {
                    let mut slot = slicer.cut_left(target_w);
                    if slot.w >= 16.0 * zoom {
                        slot.y = y_seg;
                        slot.h = seg_h;
                        seg_slots.push((slot, leaf_idx));
                        seg_lines.push(line);
                    }
                }
            } else {
                // Standard mode: cut all segments that fit from left to right
                for (i, seg) in self.segments.iter().enumerate() {
                    if i > 0 {
                        if slicer.w < chevron_w + 20.0 * zoom {
                            break;
                        }
                        let mut ch_slot = slicer.cut_left(chevron_w);
                        ch_slot.y = y_seg;
                        ch_slot.h = seg_h;
                        if let Some(parent) = &seg.parent_dir {
                            chevron_slots.push(DgChevronSlot {
                                slot: ch_slot,
                                dir: parent.clone(),
                                idx: i,
                            });
                        }
                    }
                    if slicer.w < 16.0 * zoom {
                        break;
                    }
                    let avail_w = (slicer.w - seg_pad * 2.0)
                        .min(max_seg - seg_pad * 2.0)
                        .max(0.0);
                    let line = shape_text_truncated(
                        &seg.label,
                        &font,
                        font_size,
                        if seg.is_ghost {
                            self.fg.opacity(0.40)
                        } else {
                            self.fg
                        },
                        avail_w,
                        window,
                    );
                    let text_w = line_width(&line);
                    let target_w = (text_w + seg_pad * 2.0)
                        .clamp_lenient(24.0 * zoom, max_seg)
                        .min(slicer.w);

                    let mut slot = slicer.cut_left(target_w);
                    if slot.w < 16.0 * zoom {
                        break;
                    }
                    slot.y = y_seg;
                    slot.h = seg_h;
                    seg_slots.push((slot, i));
                    seg_lines.push(line);
                    let _ = slicer.cut_left(4.0 * zoom);
                }
            }
        }

        // Build flat hit map
        let mut all_hits = Vec::new();
        for btn in &nav_buttons {
            if btn.enabled && btn.slot.w >= 8.0 * zoom {
                all_hits.push((btn.slot, btn.action));
            }
        }
        for ch in &chevron_slots {
            if ch.slot.w >= 8.0 * zoom {
                all_hits.push((ch.slot, DgNavAction::Chevron(ch.idx)));
            }
        }
        for (slot, seg_idx) in &seg_slots {
            if slot.w >= 8.0 * zoom {
                all_hits.push((*slot, DgNavAction::Segment(*seg_idx)));
            }
        }
        // The address background opens the same location finder as Ctrl+L
        if address_slot.w >= 16.0 * zoom {
            all_hits.push((address_slot, DgNavAction::OpenLocation));
        }

        // Register OLE Drop Targets so dragging files over breadcrumbs or buttons works seamlessly
        let origin = bounds.origin;
        let pane_id = self.pane_id;
        let has_gpui_drag = cx.has_active_drag();
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |this, _| {
                let is_drag_active = this.is_drag_active(has_gpui_drag);
                if is_drag_active {
                    // Clear previous chrome and breadcrumb targets for this pane to prevent stale accumulation
                    this.pending_drag_hover_targets.retain(|target| {
                        !matches!(
                            target.id,
                            DragHoverId::Chrome(_, p) | DragHoverId::Breadcrumb(p, _) if p == pane_id
                        )
                    });

                    // Register nav buttons as drop targets with distinct IDs and dimension checks
                    for btn in &nav_buttons {
                        if btn.slot.w < btn_sz * 0.75 {
                            continue;
                        }
                        let (cmd, id_str) = match btn.action {
                            DgNavAction::Back => (
                                DragHoverCommand::NavigateBack(pane_id),
                                "pane-back",
                            ),
                            DgNavAction::Forward => (
                                DragHoverCommand::NavigateForward(pane_id),
                                "pane-forward",
                            ),
                            DgNavAction::Up => {
                                (DragHoverCommand::NavigateUp(pane_id), "pane-up")
                            }
                            _ => continue,
                        };
                        this.register_drag_hover_target(DragHoverTarget {
                            id: DragHoverId::Chrome(id_str, pane_id),
                            bounds: btn.slot.to_bounds(origin),
                            kind: DragHoverKind::Nav,
                            command: cmd,
                            drop_command: None,
                        });
                    }

                    // Register directory breadcrumb segments as drop targets
                    for (slot, seg_idx) in &seg_slots {
                        if slot.w < 16.0 * zoom {
                            continue;
                        }
                        if let Some(seg) = self.segments.get(*seg_idx) {
                            if let Location::Directory(ref p) = seg.location {
                                let seg_bounds = slot.to_bounds(origin);
                                this.register_drag_hover_target(DragHoverTarget {
                                    id: DragHoverId::Breadcrumb(pane_id, p.clone()),
                                    bounds: seg_bounds,
                                    kind: DragHoverKind::Folder,
                                    command: DragHoverCommand::NavigateLocation(seg.location.clone()),
                                    drop_command: Some(DragHoverDropCommand::TransferTo(p.clone())),
                                });
                            }
                        }
                    }
                } else if !this.pending_drag_hover_targets.is_empty() {
                    this.pending_drag_hover_targets.retain(|target| {
                        !matches!(
                            target.id,
                            DragHoverId::Chrome(_, p) | DragHoverId::Breadcrumb(p, _) if p == pane_id
                        )
                    });
                }
            });
        }

        DgChromePrepaint {
            hitbox,
            nav_buttons,
            seg_slots,
            seg_lines,
            chevron_slots,
            field,
            strip_h,
            radius,
            all_hits,
        }
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
        crate::perf::count("chrome_dg.paint");

        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return;
        }

        let origin = bounds.origin;
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let nav_icon_px = if self.windows_theme { 16.0 } else { 15.0 } * zoom;
        let chevron_px = 11.0 * zoom;
        let seg_pad_x = if self.windows_theme { 10.0 } else { 8.0 } * zoom;

        let feedback = window.use_keyed_state(
            SharedString::from(format!("chrome-action-feedback-{}", self.pane_id.0)),
            cx,
            |_, _| crate::ui::dg::motion::ActionPulse::<DgNavAction>::default(),
        );

        // Check hover coordinates safely through hitbox
        let all_hits = Arc::new(prepaint.all_hits.clone());
        let active_hit = crate::ui::surface::track_hover(
            SharedString::from(format!("chrome-hover-{}", self.pane_id.0)),
            &prepaint.hitbox,
            origin,
            all_hits.clone(),
            window,
            cx,
        );
        if active_hit.is_some() {
            window.set_cursor_style(gpui::CursorStyle::PointingHand, &prepaint.hitbox);
        }

        // Render everything enclosed within hardware clipping layer
        window.paint_layer(bounds, |window| {
            // Background & bottom border
            paint_fill(window, bounds, self.bg);
            if !self.is_glass {
                paint_fill(
                    window,
                    Bounds {
                        origin: point(origin.x, origin.y + px(prepaint.strip_h - 1.0)),
                        size: size(bounds.size.width, px(1.0)),
                    },
                    self.border.opacity(0.45),
                );
            }

            // Paint nav buttons
            for btn in &prepaint.nav_buttons {
                if btn.slot.w < 8.0 * zoom {
                    continue;
                }
                let b = btn.slot.to_bounds(origin);
                let (pulse, active) = feedback.read(cx).sample(
                    &btn.action,
                    std::time::Instant::now(),
                    cx.reduce_motion(),
                );
                if active {
                    window.request_animation_frame();
                }
                if pulse > 0.0 {
                    paint_rounded(
                        window,
                        b,
                        prepaint.radius,
                        self.accent.opacity(0.16 * pulse),
                    );
                }
                if active_hit == Some(btn.action) && btn.enabled {
                    paint_rounded(window, b, prepaint.radius, self.hover);
                }
                let color = if btn.enabled {
                    self.fg.opacity(0.88)
                } else {
                    self.muted.opacity(0.35)
                };
                let icon_color = if matches!(btn.action, DgNavAction::Bookmark) && self.bookmarked {
                    self.accent
                } else {
                    color
                };
                paint_theme_icon(
                    window,
                    cx,
                    origin,
                    btn.slot,
                    btn.icon,
                    self.windows_theme,
                    nav_icon_px * (1.0 - 0.12 * pulse),
                    icon_color,
                );
            }

            // Address field: a filled, rounded well behind the path segments.
            if let Some(field) = prepaint.field {
                let fill = self
                    .fg
                    .opacity(if active_hit == Some(DgNavAction::OpenLocation) {
                        0.075
                    } else {
                        0.05
                    });
                paint_rounded(
                    window,
                    field.to_bounds(origin),
                    prepaint.radius + 2.0 * zoom,
                    fill,
                );
            }

            // The location strip is read-only; editing belongs to the location finder.
            {
                // Paint chevrons with hover highlight
                for ch in &prepaint.chevron_slots {
                    if ch.slot.w < 8.0 * zoom {
                        continue;
                    }
                    let b = ch.slot.to_bounds(origin);
                    if active_hit == Some(DgNavAction::Chevron(ch.idx)) {
                        paint_rounded(window, b, prepaint.radius * 0.75, self.hover);
                    }
                    paint_theme_icon(
                        window,
                        cx,
                        origin,
                        ch.slot,
                        AtlasIcon::CaretRight,
                        self.windows_theme,
                        chevron_px,
                        self.muted.opacity(0.55),
                    );
                }

                // Paint breadcrumb segments
                for (line, (slot, seg_idx)) in prepaint.seg_lines.iter().zip(&prepaint.seg_slots) {
                    if slot.w < 8.0 * zoom {
                        continue;
                    }
                    let b = slot.to_bounds(origin);
                    if active_hit == Some(DgNavAction::Segment(*seg_idx)) {
                        paint_rounded(window, b, prepaint.radius * 0.85, self.hover);
                    }

                    let text_x = b.origin.x + px(seg_pad_x);
                    let text_y = origin.y + px(text_origin_y(slot.y, zoom));
                    window.paint_layer(b, |window| {
                        let _ = line.paint(
                            point(text_x, text_y),
                            px(slot.h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    });
                }
            }
        });

        // Handle Clicks via window mouse down listener
        if let Some(app_entity) = self.app.upgrade() {
            let hit_view = app_entity.clone();
            let pane_id = self.pane_id;
            let segs = self.segments.clone();
            let click_hitbox = prepaint.hitbox.clone();
            let chevron_dirs: Vec<(usize, PathBuf)> = prepaint
                .chevron_slots
                .iter()
                .map(|ch| (ch.idx, ch.dir.clone()))
                .collect();
            let can_back = self.can_back;
            let can_forward = self.can_forward;
            let can_up = self.can_up;

            window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                    return;
                }
                if !click_hitbox.is_hovered(window) {
                    return;
                }
                let lx = f32::from(ev.position.x - origin.x);
                let ly = f32::from(ev.position.y - origin.y);
                let Some(hit) = hit_first(&all_hits, lx, ly) else {
                    return;
                };

                let enabled = match hit {
                    DgNavAction::Back => can_back,
                    DgNavAction::Forward => can_forward,
                    DgNavAction::Up => can_up,
                    DgNavAction::Bookmark => true,
                    _ => false,
                };
                if enabled {
                    feedback.update(cx, |state, _| state.trigger(hit, std::time::Instant::now()));
                    window.refresh();
                }

                hit_view.update(cx, |this, cx| {
                    match hit {
                        DgNavAction::Back if can_back => {
                            this.process_actions(vec![Action::NavigateBack(pane_id)]);
                        }
                        DgNavAction::Forward if can_forward => {
                            this.process_actions(vec![Action::NavigateForward(pane_id)]);
                        }
                        DgNavAction::Up if can_up => {
                            this.process_actions(vec![Action::NavigateUp(pane_id)]);
                        }
                        DgNavAction::Bookmark => {
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
                        DgNavAction::Segment(i) => {
                            if let Some(seg) = segs.get(i) {
                                this.process_actions(vec![Action::NavigateLocation {
                                    tab: pane_id,
                                    location: seg.location.clone(),
                                }]);
                            }
                        }
                        DgNavAction::Chevron(idx) => {
                            if let Some((_, dir)) = chevron_dirs.iter().find(|(i, _)| *i == idx) {
                                this.state.focused_tile = pane_id;
                                this.open_breadcrumb_menu(
                                    pane_id,
                                    dir.clone(),
                                    ev.position,
                                    window,
                                    cx,
                                );
                            }
                        }
                        DgNavAction::OpenLocation => {
                            this.activate_tab(pane_id);
                            this.open_location_finder_or_editor();
                            cx.notify();
                        }
                        _ => {}
                    }
                    this.notify_pane(pane_id, cx);
                });
                cx.stop_propagation();
            });
        }
    }
}

/// Helper function to parse path segments for DgChromeElement with parent directory tracking
pub(crate) fn path_to_dg_segments(path: &Path) -> Vec<DgCrumbSeg> {
    if let Some(root) = atlas_core::vault::live::mount_root(path) {
        let mut current = root.clone();
        let mut segments = vec![DgCrumbSeg {
            is_ghost: false,
            label: atlas_core::util::display::display_name(&root),
            location: Location::Directory(root.clone()),
            parent_dir: None,
        }];
        if let Ok(relative) = path.strip_prefix(&root) {
            for part in relative.components() {
                let parent = current.clone();
                current.push(part.as_os_str());
                segments.push(DgCrumbSeg {
                    is_ghost: false,
                    label: part.as_os_str().to_string_lossy().into_owned(),
                    location: Location::Directory(current.clone()),
                    parent_dir: Some(parent),
                });
            }
        }
        return segments;
    }
    let mut segments = Vec::new();
    let mut current = PathBuf::new();
    for comp in path.components() {
        let parent = if current.as_os_str().is_empty() {
            None
        } else {
            Some(current.clone())
        };
        match comp {
            Component::Prefix(p) => {
                current.push(p.as_os_str());
                segments.push(DgCrumbSeg {
                    is_ghost: false,
                    label: prefix_label(p),
                    location: Location::Directory(current.clone()),
                    parent_dir: parent,
                });
            }
            Component::RootDir => {
                current.push(comp.as_os_str());
                if let Some(segment) = segments.last_mut() {
                    segment.location = Location::Directory(current.clone());
                } else {
                    segments.push(DgCrumbSeg {
                        is_ghost: false,
                        label: std::path::MAIN_SEPARATOR.to_string(),
                        location: Location::Directory(current.clone()),
                        parent_dir: None,
                    });
                }
            }
            Component::Normal(name) => {
                current.push(name);
                segments.push(DgCrumbSeg {
                    is_ghost: false,
                    label: name.to_string_lossy().into_owned(),
                    location: Location::Directory(current.clone()),
                    parent_dir: parent,
                });
            }
            _ => {}
        }
    }
    segments
}

fn prefix_label(prefix: std::path::PrefixComponent<'_>) -> String {
    #[cfg(windows)]
    if let Some(label) = atlas_core::util::display::unc_share_label(prefix.kind()) {
        return label;
    }
    prefix.as_os_str().to_string_lossy().into_owned()
}

pub(crate) fn path_with_ghost_segments(path: &Path, trail: Option<&Path>) -> Vec<DgCrumbSeg> {
    let target = trail
        .filter(|trail| *trail != path && trail.starts_with(path))
        .unwrap_or(path);
    let mut segments = path_to_dg_segments(target);
    for segment in &mut segments {
        segment.is_ghost = segment
            .location
            .fs_dir()
            .is_some_and(|dir| dir != path && dir.starts_with(path));
    }
    segments
}

#[cfg(test)]
mod ghost_tests {
    use super::*;
    #[test]
    fn ghost_suffix_keeps_clickable_full_paths_and_ignores_other_branches() {
        let current = Path::new("/users/me");
        let trail = current.join("developer/atlas");
        let segments = path_with_ghost_segments(current, Some(&trail));
        assert_eq!(
            segments
                .iter()
                .filter(|s| s.is_ghost)
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            ["developer", "atlas"]
        );
        assert_eq!(
            segments.last().unwrap().location,
            Location::Directory(trail)
        );
        assert!(
            path_with_ghost_segments(current, Some(Path::new("/other/atlas")))
                .iter()
                .all(|s| !s.is_ghost)
        );
    }
    #[cfg(windows)]
    #[test]
    fn windows_crumbs_are_absolute_including_the_volume_root() {
        let segments = path_to_dg_segments(Path::new(r"C:\Users\me"));
        assert_eq!(
            segments[0].location,
            Location::Directory(PathBuf::from(r"C:\"))
        );
        assert!(segments
            .iter()
            .all(|segment| segment.location.fs_dir().unwrap().is_absolute()));
    }

    #[cfg(windows)]
    #[test]
    fn wsl_crumbs_show_the_distro_and_keep_unc_navigation() {
        let root = Path::new(r"\\wsl.localhost\Arch\");
        let path = root.join("home").join("me");
        let segments = path_to_dg_segments(&path);
        assert_eq!(
            segments
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            ["Arch", "home", "me"]
        );
        assert_eq!(segments[0].location, Location::Directory(root.into()));
        assert_eq!(segments[2].location, Location::Directory(path));
    }
}
