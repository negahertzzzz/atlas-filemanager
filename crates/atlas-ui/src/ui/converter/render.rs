use super::{geometry, wheel_view, Mode, Phase, RingAction};
use crate::app::App;
use crate::ui::icons::{icon, AtlasIcon};
use atlas_core::convert::actions::Action;
use atlas_core::convert::{Backend, ConversionOption, Tool, Tools};
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    deferred, div, px, AnyElement, Context, KeyDownEvent, MouseButton, SharedString, Window,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex, ActiveTheme};
use std::path::Path;

/// Entry animation clock, 0..=1. Labs capture single frames, so they skip it.
pub(super) fn entry_progress(opened: std::time::Instant) -> f32 {
    if cfg!(any(feature = "conversion-lab", feature = "ui-lab")) {
        return 1.0;
    }
    (opened.elapsed().as_secs_f32() / 0.45).min(1.0)
}
pub(super) fn smooth(t: f32, start: f32, len: f32) -> f32 {
    ((t - start) / len).clamp(0.0, 1.0)
}
pub(super) fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}
/// Slight overshoot; computed here, never passed through gpui's easing assert.
pub(super) fn ease_out_back(t: f32) -> f32 {
    let (c1, c3) = (1.2_f32, 2.2_f32);
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}
/// Opacity of the `order`-th sector (clockwise from the pinned one) at `reveal`.
pub(super) fn sector_alpha(reveal: f32, order: usize, count: usize) -> f32 {
    let start = 0.55 * order as f32 / count as f32;
    ease_out_cubic(smooth(reveal, start, 0.45))
}

pub(super) fn text(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

fn button(
    id: &'static str,
    label: String,
    cx: &mut Context<App>,
    action: impl Fn(&mut App) + 'static,
) -> AnyElement {
    let fg = cx.theme().foreground;
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_color(fg)
        .text_sm()
        .hover(move |s| s.bg(fg.opacity(0.08)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, _, _, cx| {
                action(app);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .child(label)
        .into_any_element()
}

/// Text-xs variant of [`button`] for value cells in the info column.
fn value_button(
    id: &'static str,
    label: String,
    cx: &mut Context<App>,
    action: impl Fn(&mut App) + 'static,
) -> AnyElement {
    let fg = cx.theme().foreground;
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .cursor_pointer()
        .text_color(fg)
        .text_xs()
        .font_weight(gpui::FontWeight::MEDIUM)
        .bg(fg.opacity(0.05))
        .hover(move |s| s.bg(fg.opacity(0.10)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, _, _, cx| {
                action(app);
                cx.stop_propagation();
                cx.notify();
            }),
        )
        .child(label)
        .into_any_element()
}

/// Full, localized action name (toasts, info column, apply button).
pub(super) fn action_label(action: Action) -> String {
    text(match action {
        Action::Resize => "ring_action_resize",
        Action::RotateRight => "ring_action_rotate_right",
        Action::RotateLeft => "ring_action_rotate_left",
        Action::StripMetadata => "ring_action_strip",
        Action::Compress => "ring_action_compress",
        Action::CompressPdf => "ring_action_compress_pdf",
    })
}

/// Sector label: a few characters that fit a ring cell.
fn action_short(action: &RingAction) -> SharedString {
    match action {
        RingAction::Native(action) => text(match action {
            Action::Resize => "ring_short_resize",
            Action::RotateRight => "ring_short_rotate_right",
            Action::RotateLeft => "ring_short_rotate_left",
            Action::StripMetadata => "ring_short_strip",
            Action::Compress => "ring_short_compress",
            Action::CompressPdf => "ring_short_compress_pdf",
        })
        .into(),
        RingAction::Extension { title, .. } => title.trim_end_matches('…').to_string().into(),
    }
}

fn action_title(action: &RingAction) -> String {
    match action {
        RingAction::Native(action) => action_label(*action),
        RingAction::Extension { title, .. } => title.trim_end_matches('…').to_string(),
    }
}

fn action_engine(action: &RingAction) -> String {
    match action {
        RingAction::Native(action) => action
            .tool()
            .map_or_else(|| "Atlas".to_string(), |tool| tool.name().to_string()),
        RingAction::Extension { extension, .. } => extension.clone(),
    }
}

fn short_label(option: &ConversionOption) -> &'static str {
    if option.target.extension == "md" {
        "MD"
    } else {
        option.target.label
    }
}

/// What the chosen route does with the settings, and the caveat worth reading.
struct RouteInfo {
    backend: Option<Backend>,
    quality: bool,
    edge: bool,
    note: Option<&'static str>,
}

fn route_info(source: &Path, option: &ConversionOption, tools: &Tools) -> RouteInfo {
    let backend = atlas_core::convert::route(source, option.target, tools);
    let dst = option.target.extension;
    let quality = match backend {
        Some(Backend::Image | Backend::Vector | Backend::PdfPages) => dst == "jpg",
        Some(Backend::External(Tool::Magick)) => {
            matches!(dst, "jpg" | "webp" | "avif" | "heic" | "jxl")
        }
        Some(Backend::SystemAudio) => matches!(dst, "mp3" | "aac" | "m4a"),
        Some(
            Backend::SystemVideo | Backend::External(Tool::Ffmpeg) | Backend::External(Tool::Opus),
        ) => true,
        _ => false,
    };
    let edge = matches!(
        backend,
        Some(
            Backend::Image
                | Backend::Vector
                | Backend::PdfPages
                | Backend::SystemVideo
                | Backend::External(Tool::Magick)
        )
    );
    let note = match backend {
        Some(Backend::PdfPages) => Some("convert_all_pages_note"),
        Some(Backend::SystemPdfText) => Some("convert_pdf_ocr_note"),
        Some(Backend::External(Tool::PdfText)) => Some("convert_pdf_text_note"),
        Some(Backend::OfficePackage) => Some("convert_word_package_note"),
        Some(Backend::Sheet) => Some("convert_first_sheet"),
        Some(Backend::Data) => Some("convert_data_note"),
        Some(Backend::Subtitles) if matches!(dst, "lrc" | "smi") => {
            Some("convert_subtitle_linear_note")
        }
        Some(Backend::Subtitles) => Some("convert_subtitle_note"),
        Some(Backend::Contacts) => Some("convert_contacts_note"),
        Some(Backend::Book) => Some("convert_book_note"),
        Some(Backend::Pcm) => Some("convert_pcm_note"),
        Some(Backend::Flac) => Some("convert_flac_note"),
        Some(Backend::SystemAudio) => Some("convert_windows_audio_note"),
        Some(Backend::SystemVideo) => Some("convert_windows_video_note"),
        Some(Backend::External(Tool::Opus)) => Some("convert_opus_note"),
        Some(Backend::Text) => Some("convert_doc_text_note"),
        Some(Backend::Vector) if dst == "pdf" => Some("convert_svg_pdf_note"),
        Some(Backend::Image | Backend::Vector | Backend::External(Tool::Magick))
            if dst == "jpg" =>
        {
            Some("convert_jpeg_note")
        }
        Some(Backend::Image) if dst == "webp" => Some("convert_webp_note"),
        _ => None,
    };
    RouteInfo {
        backend,
        quality,
        edge,
        note,
    }
}

/// Window placement for the anchored card, computed once per frame.
struct Placement {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    /// Base-px wheel geometry → window px.
    scale: f32,
    /// Info column below the wheel (narrow windows) instead of beside it.
    stack: bool,
    /// Info column to the left of the wheel (anchor near the right edge).
    info_left: bool,
    info_w: f32,
    info_h: f32,
    pad: f32,
    gap: f32,
}

impl Placement {
    const MARGIN: f32 = 8.0;

    /// The wheel is centered on `anchor` like a game radial; the info column
    /// goes on whichever side has room, and everything is clamped so the card
    /// never leaves the window. When even that cannot fit, the wheel shrinks.
    fn new(anchor: (f32, f32), window: (f32, f32), zoom: f32, wheel_d: f32) -> Self {
        let (ax, ay) = anchor;
        let (ww, wh) = window;
        let m = Self::MARGIN;
        let pad = 12.0 * zoom;
        let gap = 12.0 * zoom;
        let info_w = 264.0 * zoom;
        let min_info_h = 360.0 * zoom;
        let side_room = ww - 2.0 * m - 2.0 * pad - gap - info_w;
        let stack = side_room < wheel_d * zoom * 0.6;
        let stack_info_h = 230.0 * zoom;
        let scale = if stack {
            zoom.min((ww - 2.0 * m - 2.0 * pad) / wheel_d)
                .min((wh - 2.0 * m - 2.0 * pad - gap - stack_info_h) / wheel_d)
        } else {
            zoom.min(side_room / wheel_d)
                .min((wh - 2.0 * m - 2.0 * pad) / wheel_d)
        }
        .max(0.35);
        let d = wheel_d * scale;
        let (width, height, info_h, wheel_x, wheel_y, info_left) = if stack {
            let width = (2.0 * pad + d.max(info_w)).min(ww - 2.0 * m);
            let info_h = stack_info_h.min((wh - 2.0 * m - 2.0 * pad - gap - d).max(80.0));
            let height = 2.0 * pad + d + gap + info_h;
            (width, height, info_h, width * 0.5, pad + d * 0.5, false)
        } else {
            let width = 2.0 * pad + d + gap + info_w;
            let height = (2.0 * pad + d.max(min_info_h)).min(wh - 2.0 * m);
            let right_fits = ax + d * 0.5 + gap + info_w + pad + m <= ww;
            let left_fits = ax - d * 0.5 - gap - info_w - pad - m >= 0.0;
            let info_left = !right_fits && left_fits;
            let wheel_x = if info_left {
                width - pad - d * 0.5
            } else {
                pad + d * 0.5
            };
            (width, height, height - 2.0 * pad, wheel_x, height * 0.5, info_left)
        };
        let clamp = |v: f32, size: f32, room: f32| v.min(room - size - m).max(m);
        Self {
            left: clamp(ax - wheel_x, width, ww),
            top: clamp(ay - wheel_y, height, wh),
            width,
            height,
            scale,
            stack,
            info_left,
            info_w,
            info_h,
            pad,
            gap,
        }
    }
}

fn source_extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_uppercase()
}

impl App {
    /// Alt+Left / mouse Back: one step back in the flow (results → formats → closed).
    pub(crate) fn converter_back(&mut self) {
        let Some(session) = self.converter.as_mut() else {
            return;
        };
        match session.phase {
            Phase::Finished(_) => {
                session.phase = Phase::Choosing;
                session.hovered = None;
            }
            Phase::Running { .. } => {}
            _ => self.dismiss_converter(),
        }
    }
    /// Alt+Right / mouse Forward: advance, i.e. convert with the selected format.
    pub(crate) fn converter_forward(&mut self) {
        if self
            .converter
            .as_ref()
            .is_some_and(|s| matches!(s.phase, Phase::Choosing) && s.len() > 0)
        {
            self.start_conversion();
        }
    }
    pub(crate) fn handle_converter_key(&mut self, event: &KeyDownEvent) -> bool {
        let Some(session) = self.converter.as_ref().filter(|s| s.visible()) else {
            return false;
        };
        let choosing = matches!(session.phase, Phase::Choosing);
        let count = session.len();
        let key = event.keystroke.key.as_str();
        let plain = !event.keystroke.modifiers.alt
            && !event.keystroke.modifiers.control
            && !event.keystroke.modifiers.platform;
        // Keyboard takes over from a stale pointer preview.
        self.converter.as_mut().unwrap().hovered = None;
        match key {
            "escape" => self.dismiss_converter(),
            "left" if event.keystroke.modifiers.alt => self.converter_back(),
            "right" if event.keystroke.modifiers.alt => self.converter_forward(),
            "enter" if choosing => self.start_conversion(),
            "i" if choosing && event.keystroke.modifiers.alt => {
                self.activate_selected_conversion_engine()
            }
            "m" if choosing && plain => self.toggle_ring_mode(),
            _ if !choosing || count == 0 => {}
            "left" | "right" | "up" | "down" | "pageup" | "pagedown" | "tab" | "home"
            | "end" => {
                let rings = geometry::rings(count);
                let session = self.converter.as_mut().unwrap();
                let current = session.selected.min(count - 1);
                let outward = matches!(key, "up" | "pageup");
                session.selected = match key {
                    "left" => geometry::rotate(&rings, current, -1),
                    "right" => geometry::rotate(&rings, current, 1),
                    // A single ring has nowhere to hop: arrows keep rotating.
                    "up" | "down" if rings.len() == 1 => {
                        geometry::rotate(&rings, current, if outward { -1 } else { 1 })
                    }
                    "up" | "down" | "pageup" | "pagedown" => {
                        geometry::hop(&rings, current, if outward { 1 } else { -1 })
                    }
                    "home" => 0,
                    "end" => count - 1,
                    _ => {
                        let delta = if event.keystroke.modifiers.shift { -1 } else { 1 };
                        (current as isize + delta).rem_euclid(count as isize) as usize
                    }
                };
            }
            // 1-9 pick within the ring that holds the current selection.
            digit if matches!(digit.parse::<usize>(), Ok(1..=9)) => {
                let rings = geometry::rings(count);
                let n = digit.parse::<usize>().unwrap_or(1) - 1;
                let session = self.converter.as_mut().unwrap();
                if let Some(ring) = geometry::ring_of(&rings, session.selected.min(count - 1)) {
                    if n < ring.len {
                        session.selected = ring.start + n;
                    }
                }
            }
            _ => {}
        }
        true
    }

    pub(crate) fn render_converter(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(session) = self.converter.as_ref() else {
            return div().into_any_element();
        };
        let is_glass = self.state.profiles.active().theme.ends_with(" Glass");
        let fg = cx.theme().foreground;
        let muted = cx.theme().muted_foreground;
        let border = cx.theme().border;
        let accent = cx.theme().accent;
        let popover = cx.theme().popover;
        let dark = cx.theme().is_dark();
        let running = session.running();

        // If minimized, display as a floating bottom-right transfer status pill
        if session.minimized {
            let done_text = match &session.phase {
                Phase::Running { done, total, .. } => format!("{done} / {total}"),
                Phase::Finished(report) => {
                    text("convert_done_count").replace("%{count}", &report.outputs().len().to_string())
                }
                _ => text("convert_title"),
            };
            let fraction = match &session.phase {
                Phase::Running { done, total, .. } => *done as f32 / (*total).max(1) as f32,
                _ => 1.0,
            };
            return deferred(
                h_flex()
                    .absolute()
                    .right(px(24.))
                    .bottom(px(24.))
                    .gap_3()
                    .items_center()
                    .px_3()
                    .py_2()
                    .rounded_xl()
                    .bg(if is_glass { popover.opacity(0.70) } else { popover.opacity(0.92) })
                    .when(is_glass, |s| s.backdrop_blur(px(16.0)))
                    .border_1()
                    .border_color(if is_glass { border.opacity(0.35) } else { border.opacity(0.55) })
                    .shadow_xl()
                    .occlude()
                    .child(icon(AtlasIcon::ArrowClockwise, 15., accent))
                    .child(
                        v_flex()
                            .gap_0p5()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(fg)
                                    .child(done_text),
                            )
                            .when(running, |this| {
                                this.child(
                                    div()
                                        .w(px(70.))
                                        .h(px(2.))
                                        .rounded_full()
                                        .bg(fg.opacity(0.12))
                                        .child(
                                            div()
                                                .h_full()
                                                .w(px(70. * fraction))
                                                .rounded_full()
                                                .bg(accent),
                                        ),
                                )
                            }),
                    )
                    .child(button("convert-reopen", text("convert_show"), cx, |app| {
                        if let Some(s) = &mut app.converter {
                            s.minimized = false;
                        }
                    }))
                    .child(
                        div()
                            .id("convert-minimized-close")
                            .size(px(18.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .text_color(muted)
                            .hover(move |s| s.bg(fg.opacity(0.10)).text_color(fg))
                            .on_mouse_down(MouseButton::Left, cx.listener(|app, _, _, cx| {
                                if app.converter.as_ref().is_some_and(|s| s.running()) {
                                    app.cancel_conversion();
                                }
                                app.dismiss_converter();
                                cx.stop_propagation();
                                cx.notify();
                            }))
                            .child(icon(AtlasIcon::X, 11., muted)),
                    ),
            )
            .into_any_element();
        }

        // Every option gets a sector; rings grow outward and the card scales
        // down to the window, so nothing ever pages or spills off-screen.
        let count = match session.phase {
            Phase::Choosing => session.len(),
            _ => 0,
        };
        let rings = geometry::rings(count.max(1));
        let wheel_d = geometry::radius(&rings) * 2.0;
        let win = window.bounds().size;
        let (ww, wh) = (f32::from(win.width), f32::from(win.height));
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let anchor = *self
            .converter
            .as_mut()
            .unwrap()
            .anchor
            .get_or_insert_with(|| {
                let mouse = window.mouse_position();
                let (mx, my) = (f32::from(mouse.x), f32::from(mouse.y));
                if mx > 0.0 && my > 0.0 && mx < ww && my < wh {
                    mouse
                } else {
                    gpui::point(px(ww * 0.5), px(wh * 0.5))
                }
            });
        let place = Placement::new(
            (f32::from(anchor.x), f32::from(anchor.y)),
            (ww, wh),
            zoom,
            wheel_d,
        );
        let d = wheel_d * place.scale;
        let progress = self
            .converter
            .as_ref()
            .map_or(1.0, |s| entry_progress(s.opened));
        if progress < 1.0 {
            window.request_animation_frame();
        }

        // Explicit alpha: some themes ship a translucent popover, and without
        // the old full-window dim the card must stand on its own.
        let mut card_bg = popover;
        card_bg.alpha = if is_glass { 0.82 } else { 0.97 };
        let card_border = if is_glass {
            border.opacity(0.35)
        } else {
            border.opacity(0.55)
        };

        let wheel_slot = match &self.converter.as_ref().unwrap().phase {
            Phase::Discovering => v_flex()
                .size(px(d))
                .items_center()
                .justify_center()
                .gap_3()
                .child(crate::ui::loading_spinner::loading_spinner(
                    muted,
                    !self.state.settings.disable_animations,
                ))
                .child(
                    div()
                        .text_xs()
                        .text_center()
                        .text_color(muted)
                        .child(text("convert_discovering")),
                )
                .into_any_element(),
            Phase::Choosing => self.render_format_wheel(place.scale, rings, is_glass, dark, cx),
            Phase::Running {
                done,
                total,
                current,
                cancel,
            } => {
                let fraction = *done as f32 / (*total).max(1) as f32;
                let cancelling = cancel.load(std::sync::atomic::Ordering::Relaxed);
                let bar = (d * 0.6).max(80.0);
                v_flex()
                    .size(px(d))
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(crate::ui::loading_spinner::loading_spinner(
                        accent,
                        !self.state.settings.disable_animations,
                    ))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(fg)
                            .child(text(if cancelling {
                                "convert_cancelling"
                            } else {
                                "convert_running"
                            })),
                    )
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(accent)
                            .child(format!("{done} / {total}")),
                    )
                    .child(
                        div()
                            .w(px(bar))
                            .h(px(3.))
                            .rounded_full()
                            .bg(fg.opacity(0.12))
                            .child(div().h_full().w(px(bar * fraction)).rounded_full().bg(accent)),
                    )
                    .child(
                        div()
                            .max_w(px(d * 0.8))
                            .truncate()
                            .text_xs()
                            .text_color(muted)
                            .child(
                                current
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned(),
                            ),
                    )
                    .into_any_element()
            }
            Phase::Finished(report) => {
                let errors = report.errors();
                let summary = rust_i18n::t!(
                    "convert_summary",
                    created = report.outputs().len(),
                    failed = errors.len(),
                    cancelled = report.cancelled()
                )
                .into_owned();
                let mut slot = v_flex()
                    .size(px(d))
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(
                        div()
                            .text_base()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(if errors.is_empty() {
                                accent
                            } else {
                                cx.theme().danger
                            })
                            .child(text(if errors.is_empty() && report.cancelled() == 0 {
                                "convert_finished"
                            } else {
                                "convert_review"
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_center()
                            .text_color(muted)
                            .child(summary),
                    );
                if let Some(folder) = report
                    .outputs()
                    .first()
                    .and_then(|p| p.parent())
                    .map(Path::to_path_buf)
                {
                    slot = slot.child(button(
                        "convert-reveal",
                        text("convert_reveal"),
                        cx,
                        move |app| {
                            let folder = folder.clone();
                            app.rt.spawn_blocking(move || {
                                let _ = open::that(folder);
                            });
                        },
                    ));
                }
                slot.into_any_element()
            }
        };

        let header = self.render_conversion_header(cx);
        let choosing_now = matches!(
            self.converter.as_ref().map(|s| &s.phase),
            Some(Phase::Choosing)
        );
        // Filename row (+ mode switch while choosing) and the gap below it.
        let header_h = (if choosing_now { 64.0 } else { 30.0 }) * zoom;
        let (body, footer) = self.render_conversion_info(cx);
        // The scroll area gets an explicit height: a scrollable never shrinks
        // under flex, and the primary action must stay inside the card.
        let footer_h = if footer.is_some() { 58.0 * zoom } else { 0.0 };
        let inner_pad = 14.0 * zoom;
        // The panel follows the ring: later, and sliding away from it.
        let info_t = ease_out_cubic(smooth(progress, 0.18, 0.55));
        let info_alpha = info_t;
        let slide = (1.0 - info_t) * 14.0 * zoom;
        let info_dx = if place.info_left { -slide } else { slide };
        let info_dy = slide;
        let info = v_flex()
            .flex_none()
            .p(px(inner_pad))
            .rounded(px(18. * zoom))
            .bg(card_bg)
            .backdrop_blur(px(if is_glass { 24.0 } else { 12.0 }))
            .border_1()
            .border_color(card_border)
            .shadow(vec![gpui::BoxShadow {
                color: gpui::hsla(0., 0., 0., if dark { 0.30 } else { 0.14 }).into(),
                offset: gpui::point(px(0.0), px(6.0)),
                blur_radius: px(22.0 * zoom),
                spread_radius: px(-4.0),
                inset: false,
            }])
            .opacity(info_alpha)
            .relative()
            .left(px(if place.stack { 0.0 } else { info_dx }))
            .top(px(if place.stack { info_dy } else { 0.0 }))
            .when(place.stack, |s| s.w_full())
            .when(!place.stack, |s| s.w(px(place.info_w)))
            .h(px(place.info_h))
            .justify_between()
            .child(header)
            .child(
                div()
                    .id("convert-info")
                    .w_full()
                    .h(px((place.info_h - footer_h - header_h - 2.0 * inner_pad).max(40.0)))
                    .overflow_y_scrollbar()
                    // Gutter so an appearing scrollbar never covers the values.
                    .child(div().w_full().pr(px(10.0 * zoom)).child(body)),
            )
            .children(footer);

        let card = div()
            .id("convert-card")
            .absolute()
            .left(px(place.left))
            .top(px(place.top))
            .w(px(place.width))
            .h(px(place.height))
            .flex()
            .items_center()
            .when(place.stack, |s| s.flex_col())
            .when(!place.stack && place.info_left, |s| s.flex_row_reverse())
            .gap(px(place.gap))
            .p(px(place.pad))
            .text_color(fg)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .size(px(d))
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(!matches!(self.converter.as_ref().map(|s| &s.phase), Some(Phase::Choosing)), |s| {
                        s.rounded_full()
                            .bg(card_bg)
                            .border_1()
                            .border_color(card_border)
                    })
                    .child(wheel_slot),
            )
            .child(info);

        // Positional like the context menu: a transparent click-catcher for
        // outside clicks and focus, no full-window dim or blur.
        let focus = self.overlay_focus(cx);
        let layer = div()
            .id("convert-layer")
            .absolute()
            .inset_0()
            .size_full()
            .occlude()
            .track_focus(&focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|app, _, _, cx| {
                    app.dismiss_converter();
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|app, _, _, cx| {
                    app.dismiss_converter();
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Back),
                cx.listener(|app, _, _, cx| {
                    app.converter_back();
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Forward),
                cx.listener(|app, _, _, cx| {
                    app.converter_forward();
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .capture_key_down(cx.listener(|app, event, _, cx| {
                if app.handle_converter_key(event) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(card);
        deferred(layer).into_any_element()
    }

    fn render_format_wheel(
        &mut self,
        scale: f32,
        rings: Vec<geometry::Ring>,
        is_glass: bool,
        dark: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let diameter = geometry::radius(&rings) * 2.0 * scale;
        let session = self.converter.as_ref().unwrap();
        if session.mode == Mode::Actions && session.actions.is_empty() {
            return v_flex()
                .size(px(diameter))
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .max_w(px(diameter * 0.8))
                        .text_sm()
                        .text_center()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(text("ring_no_actions")),
                )
                .child(button("ring-to-convert", text("ring_mode_convert"), cx, App::toggle_ring_mode))
                .into_any_element();
        }
        let options = session.options();
        if session.mode == Mode::Convert && options.is_empty() {
            return v_flex()
                .size(px(diameter))
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    div()
                        .text_sm()
                        .text_center()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(text("convert_no_formats")),
                )
                .child(button(
                    "convert-zip-selection",
                    text("convert_pack_zip"),
                    cx,
                    |app| {
                        let paths = app
                            .converter
                            .as_ref()
                            .map(|s| s.sources.clone())
                            .unwrap_or_default();
                        app.dismiss_converter();
                        app.process_actions(vec![crate::input::action::Action::CompressToZip(
                            paths,
                        )]);
                    },
                ))
                .into_any_element();
        }
        let entries: Vec<wheel_view::WheelOption> = match session.mode {
            Mode::Convert => options
                .iter()
                .map(|o| wheel_view::WheelOption {
                    label: short_label(o).into(),
                    available: o.available(),
                    engine: o.engines.first().copied().unwrap_or("").to_string(),
                })
                .collect(),
            Mode::Actions => session
                .actions
                .iter()
                .map(|a| wheel_view::WheelOption {
                    label: action_short(a),
                    available: true,
                    engine: action_engine(a),
                })
                .collect(),
        };
        let selected = session.selected.min(entries.len() - 1);
        let props = wheel_view::WheelProps {
            scale,
            options: entries,
            selected,
            hovered: session.hovered,
            glass: is_glass,
            dark,
            engine_needed: text("convert_engine_needed"),
        };
        let opened = session.opened;
        let app = cx.entity().downgrade();
        let session = self.converter.as_mut().unwrap();
        let view = session
            .wheel
            .get_or_insert_with(|| cx.new(|_| wheel_view::WheelView::new(app, opened, props.clone())))
            .clone();
        view.update(cx, |v, cx| {
            if v.props != props {
                v.props = props;
                cx.notify();
            }
        });
        view.into_any_element()
    }

    /// Pinned top of the info column: source, close button and the
    /// Convert / Actions switch. Kept out of the scroll area so the scrollbar
    /// never covers (or steals clicks from) the close button.
    fn render_conversion_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let session = self.converter.as_ref().unwrap();
        let (fg, muted, accent) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.accent)
        };
        let filename = session
            .sources
            .first()
            .and_then(|p| p.file_name())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let filename = if session.sources.len() > 1 {
            format!("{filename}  (+{})", session.sources.len() - 1)
        } else {
            filename
        };
        let mut header = v_flex().w_full().gap_2().child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .min_w_0()
                        .child(icon(AtlasIcon::File, 14., accent))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(muted)
                                .child(SharedString::from(filename)),
                        ),
                )
                .child(
                    div()
                        .id("convert-dismiss")
                        .flex_none()
                        .size(px(20.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(move |s| s.bg(fg.opacity(0.10)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|app, _, _, cx| {
                                app.dismiss_converter();
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )
                        .child(icon(AtlasIcon::X, 12., muted)),
                ),
        );

        if matches!(session.phase, Phase::Choosing) {
            header = header.child(self.render_ring_mode_switch(cx));
        }
        header.into_any_element()
    }

    /// Always-visible side column: what is converted, how, where it lands and
    /// the settings that matter for this route. The footer (primary action)
    /// is returned apart so it stays pinned outside the scroll area.
    fn render_conversion_info(
        &mut self,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Option<AnyElement>) {
        let session = self.converter.as_ref().unwrap();
        let (fg, muted, accent, accent_fg, danger) = {
            let t = cx.theme();
            (
                t.foreground,
                t.muted_foreground,
                t.accent,
                t.accent_foreground,
                t.danger,
            )
        };
        let mut column = v_flex().w_full().gap_2();
        let choosing = matches!(session.phase, Phase::Choosing);
        let session = self.converter.as_ref().unwrap();
        if choosing && session.mode == Mode::Actions {
            return self.render_action_info(column, cx);
        }
        let options = session.options();
        let shown = session.hovered.unwrap_or(session.selected);
        let Some(option) = options
            .get(shown.min(options.len().saturating_sub(1)))
            .filter(|_| choosing)
        else {
            let hint = match session.phase {
                Phase::Running { .. } => Some("convert_background"),
                Phase::Discovering => Some("convert_keep_originals"),
                _ => None,
            };
            if let Some(hint) = hint {
                column = column.child(div().text_xs().text_color(muted).child(text(hint)));
            }
            let running = session.running();
            if choosing {
                if let Some(locked) = self.render_locked_engines(cx) {
                    column = column.child(locked);
                }
            }
            if running {
                column = column
                    .child(button("convert-minimize", text("convert_minimize"), cx, |app| {
                        if let Some(s) = &mut app.converter {
                            s.minimized = true;
                        }
                    }))
                    .child(button(
                        "convert-cancel",
                        text("convert_cancel"),
                        cx,
                        App::cancel_conversion,
                    ));
            }
            return (column.into_any_element(), None);
        };

        let source = &session.sources[0];
        let info = route_info(source, option, &session.tools);
        let enabled = option.available();
        let extension = option.target.extension;

        // Source formats → target: the one-glance summary of the conversion.
        let mut sources: Vec<String> = Vec::new();
        for path in &session.sources {
            let ext = source_extension(path);
            if !ext.is_empty() && !sources.contains(&ext) {
                sources.push(ext);
            }
        }
        let source_label = match sources.len() {
            0 => "—".to_string(),
            1..=3 => sources.join(", "),
            n => format!("{}, +{}", sources[..2].join(", "), n - 2),
        };
        let chip = |label: String, tint: gpui::Hsla, strong: bool| {
            div()
                .min_w_0()
                .px_2()
                .py_0p5()
                .rounded_md()
                .bg(tint.opacity(if strong { 0.16 } else { 0.08 }))
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if strong { tint } else { fg })
                .truncate()
                .child(label)
        };
        column = column.child(
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .py_1()
                .child(chip(source_label, fg, false))
                .child(icon(AtlasIcon::ArrowRight, 13., muted))
                .child(chip(
                    option.target.label.to_string(),
                    if enabled { accent } else { danger },
                    true,
                )),
        );

        let row = |label: String, value: String| {
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .justify_between()
                .text_xs()
                .child(div().flex_none().text_color(muted).child(label))
                .child(div().min_w_0().truncate().text_color(fg).child(value))
        };
        let engines = if option.engines.is_empty() {
            info.backend.map_or("Atlas", Backend::name).to_string()
        } else {
            option.engines.join(" + ")
        };
        column = column.child(row(text("convert_engine_label"), engines));
        let output = if session.sources.len() == 1 {
            let stem = source
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if info.backend == Some(Backend::PdfPages) {
                format!("{stem}/*.{extension}")
            } else {
                format!("{stem}.{extension}")
            }
        } else {
            rust_i18n::t!(
                "convert_output_multi",
                count = session.sources.len(),
                ext = extension
            )
            .into_owned()
        };
        column = column.child(row(text("convert_output_label"), output));
        let (quality, edge, note) = (info.quality, info.edge, info.note);
        column = column.child(self.render_conversion_settings(quality, edge, cx));

        if let Some(note) = note {
            column = column.child(
                div()
                    .w_full()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .bg(fg.opacity(0.05))
                    .text_xs()
                    .text_color(muted)
                    .child(text(note)),
            );
        }
        let session = self.converter.as_ref().unwrap();
        if !enabled || session.package.is_some() || session.package_error.is_some() {
            column = column.child(self.render_conversion_detail(cx));
        }
        if let Some(locked) = self.render_locked_engines(cx) {
            column = column.child(locked);
        }

        let session = self.converter.as_ref().unwrap();
        let multi_ring = geometry::rings(session.len()).len() > 1;
        let footer = v_flex()
            .w_full()
            .gap_1()
            .child(
                div()
                    .id("convert-apply")
                    .w_full()
                    .py_1p5()
                    .rounded_lg()
                    .text_center()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .when(enabled, |s| {
                        s.bg(accent)
                            .text_color(accent_fg)
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.9))
                    })
                    .when(!enabled, |s| s.bg(fg.opacity(0.06)).text_color(muted))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |app, _, _, cx| {
                            if enabled {
                                app.start_conversion();
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(format!("{} · .{extension}", text("convert_apply"))),
            )
            .child(
                div()
                    .w_full()
                    .text_center()
                    .text_size(px(10.))
                    .text_color(muted.opacity(0.8))
                    .child(text(if multi_ring {
                        "convert_keys_rings"
                    } else {
                        "convert_keys"
                    })),
            )
            .into_any_element();
        (column.into_any_element(), Some(footer))
    }

    /// Convert / Actions switch at the top of the info column (key: M).
    fn render_ring_mode_switch(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let session = self.converter.as_ref().unwrap();
        let (fg, muted, accent) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.accent)
        };
        let current = session.mode;
        let counts = [session.catalog.len(), session.actions.len()];
        let mut row = h_flex().w_full().gap_1().p_0p5().rounded_lg().bg(fg.opacity(0.05));
        for (index, (mode, id, key)) in [
            (Mode::Convert, "ring-mode-convert", "ring_mode_convert"),
            (Mode::Actions, "ring-mode-actions", "ring_mode_actions"),
        ]
        .into_iter()
        .enumerate()
        {
            let active = current == mode;
            row = row.child(
                div()
                    .id(id)
                    .flex_1()
                    .py_0p5()
                    .rounded_md()
                    .text_center()
                    .text_xs()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .cursor_pointer()
                    .when(active, |s| s.bg(accent.opacity(0.16)).text_color(accent))
                    .when(!active, |s| s.text_color(muted).hover(move |s| s.bg(fg.opacity(0.06))))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |app, _, _, cx| {
                            if let Some(session) = app.converter.as_mut() {
                                session.set_mode(mode);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(format!("{} · {}", text(key), counts[index])),
            );
        }
        row.into_any_element()
    }

    /// Installable provider engines that would add more formats for this
    /// selection. Their routes stay hidden from the wheel until installed.
    fn render_locked_engines(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.converter.as_ref()?;
        if session.package.is_some() || session.locked.is_empty() {
            return None;
        }
        let muted = cx.theme().muted_foreground;
        let mut column = v_flex().w_full().gap_1().pt_1();
        let mut any = false;
        for tool in session.locked.clone() {
            let Some(request) = session.installers.get(&tool) else {
                continue;
            };
            any = true;
            let size = format!("{:.1} MiB", request.tool.download_bytes as f64 / 1048576.0);
            column = column
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(rust_i18n::t!("convert_more_formats", engine = tool.name()).into_owned()),
                )
                .child(button(
                    "convert-unlock-engine",
                    rust_i18n::t!("convert_unlock_package", engine = tool.name(), size = size)
                        .into_owned(),
                    cx,
                    move |app| app.install_conversion_engine(tool),
                ));
        }
        any.then(|| column.into_any_element())
    }

    /// Actions side of the info column: what the action does, where the
    /// result lands and the knobs it honours.
    fn render_action_info(
        &mut self,
        mut column: gpui::Div,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Option<AnyElement>) {
        let session = self.converter.as_ref().unwrap();
        let (fg, muted, accent, accent_fg) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground, t.accent, t.accent_foreground)
        };
        let shown = session.hovered.unwrap_or(session.selected);
        let Some(action) = session
            .actions
            .get(shown.min(session.actions.len().saturating_sub(1)))
            .cloned()
        else {
            column = column.child(div().text_xs().text_color(muted).child(text("ring_no_actions")));
            return (column.into_any_element(), None);
        };
        let title = action_title(&action);
        column = column.child(
            div()
                .w_full()
                .px_2()
                .py_0p5()
                .rounded_md()
                .bg(accent.opacity(0.16))
                .text_sm()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(accent)
                .truncate()
                .child(title.clone()),
        );
        let description = match &action {
            RingAction::Native(native) => text(match native {
                Action::Resize => "ring_desc_resize",
                Action::RotateRight | Action::RotateLeft => "ring_desc_rotate",
                Action::StripMetadata => "ring_desc_strip",
                Action::Compress => "ring_desc_compress",
                Action::CompressPdf => "ring_desc_compress_pdf",
            }),
            RingAction::Extension { .. } => text("ring_desc_extension"),
        };
        column = column.child(div().text_xs().text_color(muted).child(description));
        let row = |label: String, value: String| {
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .justify_between()
                .text_xs()
                .child(div().flex_none().text_color(muted).child(label))
                .child(div().min_w_0().truncate().text_color(fg).child(value))
        };
        column = column.child(row(text("convert_engine_label"), action_engine(&action)));
        if let RingAction::Native(native) = action {
            let settings = &session.action_settings;
            let suffix = match native {
                Action::Resize => format!("-{}px", settings.edge),
                Action::RotateRight | Action::RotateLeft => "-rotated".to_string(),
                Action::StripMetadata => "-clean".to_string(),
                Action::Compress | Action::CompressPdf => "-compressed".to_string(),
            };
            let output = if session.sources.len() == 1 {
                let source = &session.sources[0];
                let stem = source.file_stem().unwrap_or_default().to_string_lossy();
                let ext = source.extension().unwrap_or_default().to_string_lossy();
                format!("{stem}{suffix}.{ext}")
            } else {
                rust_i18n::t!("ring_output_multi", count = session.sources.len(), suffix = suffix)
                    .into_owned()
            };
            column = column.child(row(text("convert_output_label"), output));
            let edge = settings.edge;
            let quality = settings.quality;
            let knob = |label: String, control: AnyElement| {
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .child(div().text_xs().text_color(muted).child(label))
                    .child(control)
            };
            match native {
                Action::Resize => {
                    column = column.child(knob(
                        text("ring_max_edge"),
                        value_button("ring-edge", format!("{edge} px"), cx, |app| {
                            if let Some(s) = &mut app.converter {
                                const EDGES: [u32; 5] = [800, 1280, 1920, 2560, 3840];
                                let next = EDGES
                                    .iter()
                                    .position(|e| *e == s.action_settings.edge)
                                    .map_or(0, |i| (i + 1) % EDGES.len());
                                s.action_settings.edge = EDGES[next];
                            }
                        }),
                    ));
                }
                // Quality only matters for JPEG; PNG compression is lossless.
                Action::Compress
                    if session.sources.iter().any(|p| {
                        p.extension().is_some_and(|e| {
                            e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg")
                        })
                    }) =>
                {
                    column = column.child(knob(
                        text("convert_quality"),
                        value_button("ring-quality", format!("{quality}%"), cx, |app| {
                            if let Some(s) = &mut app.converter {
                                s.action_settings.quality = match s.action_settings.quality {
                                    90 => 80,
                                    80 => 60,
                                    _ => 90,
                                };
                            }
                        }),
                    ));
                }
                _ => {}
            }
            column = column.child(self.render_conversion_settings(false, false, cx));
        }
        let multi_ring = geometry::rings(self.converter.as_ref().map_or(1, |s| s.len())).len() > 1;
        let footer = v_flex()
            .w_full()
            .gap_1()
            .child(
                div()
                    .id("ring-apply")
                    .w_full()
                    .py_1p5()
                    .rounded_lg()
                    .text_center()
                    .text_sm()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .bg(accent)
                    .text_color(accent_fg)
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.9))
                    .truncate()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|app, _, _, cx| {
                            app.start_conversion();
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(title),
            )
            .child(
                div()
                    .w_full()
                    .text_center()
                    .text_size(px(10.))
                    .text_color(muted.opacity(0.8))
                    .child(text(if multi_ring { "ring_keys_rings" } else { "ring_keys" })),
            )
            .into_any_element();
        (column.into_any_element(), Some(footer))
    }

    fn render_conversion_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let session = self.converter.as_ref().unwrap();
        let options = session.options();
        let Some(option) = options.get(session.selected) else {
            return div().into_any_element();
        };
        if option.available() && session.package.is_none() && session.package_error.is_none() {
            return div().into_any_element();
        }
        let mut detail = v_flex()
            .w_full()
            .gap_1p5()
            .items_center()
            .text_xs()
            .text_color(cx.theme().muted_foreground);
        if !option.available()
            && session
                .package
                .as_ref()
                .is_none_or(|package| !option.missing.contains(&package.tool))
        {
            let names = option
                .missing
                .iter()
                .map(|t| t.name())
                .collect::<Vec<_>>()
                .join(" + ");
            detail = detail.child(
                div()
                    .text_center()
                    .text_color(cx.theme().danger)
                    .child(rust_i18n::t!("convert_needs", engine = names).into_owned()),
            );
            for tool in &option.missing {
                let engine = *tool;
                if let Some(provider) = session.disabled_providers.get(tool) {
                    let id = provider.clone();
                    detail = detail.child(button(
                        "convert-enable-package",
                        rust_i18n::t!("convert_enable_package", engine = tool.name()).into_owned(),
                        cx,
                        move |app| {
                            app.handle_ext_action(crate::app::ext::ExtAction::SetEnabled {
                                id: id.clone(),
                                enabled: true,
                            })
                        },
                    ));
                } else if let Some(request) = session
                    .installers
                    .get(tool)
                    .filter(|_| session.package.is_none())
                {
                    let size = format!("{:.1} MiB", request.tool.download_bytes as f64 / 1048576.0);
                    detail = detail.child(button(
                        "convert-install-package",
                        rust_i18n::t!("convert_install_package", engine = tool.name(), size = size)
                            .into_owned(),
                        cx,
                        move |app| app.install_conversion_engine(engine),
                    ));
                    if request.tool.installed_bytes > 0 {
                        detail = detail.child(
                            div().text_xs().text_center().child(
                                rust_i18n::t!(
                                    "convert_package_disk",
                                    size = format!(
                                        "{:.1} MiB",
                                        request.tool.installed_bytes as f64 / 1048576.0
                                    )
                                )
                                .into_owned(),
                            ),
                        );
                    }
                    if !request.tool.license.is_empty() {
                        let license = request.tool.license.clone();
                        let url = request.tool.license_url.clone();
                        detail = detail.child(button(
                            "convert-package-license",
                            license,
                            cx,
                            move |app| {
                                // The URL comes from an extension manifest: only
                                // web links, never a local program or URI handler.
                                let lower = url.trim_start().to_ascii_lowercase();
                                let is_web = lower.starts_with("https://") || lower.starts_with("http://");
                                if !is_web {
                                    tracing::warn!(%url, "refusing to open a non-web license link");
                                    return;
                                }
                                let url = url.clone();
                                app.rt.spawn_blocking(move || {
                                    let _ = open::that(url);
                                });
                            },
                        ));
                    }
                    continue;
                }
                let url = tool.url();
                detail = detail.child(button(
                    tool.executable(),
                    rust_i18n::t!("convert_get_engine", engine = tool.name()).into_owned(),
                    cx,
                    move |app| {
                        app.rt.spawn_blocking(move || {
                            let _ = open::that(url);
                        });
                    },
                ));
            }
            detail = detail.child(button(
                "convert-rescan",
                text("convert_rescan"),
                cx,
                App::refresh_conversion_tools,
            ));
        }
        if let Some(package) = &session.package {
            let label = match package.progress {
                atlas_core::ext::tool_package::Progress::Download { bytes, total } => {
                    let amount = total.map_or_else(
                        || format!("{:.1} MiB", bytes as f64 / 1048576.0),
                        |total| {
                            format!(
                                "{:.1} / {:.1} MiB",
                                bytes as f64 / 1048576.0,
                                total as f64 / 1048576.0
                            )
                        },
                    );
                    rust_i18n::t!(
                        "convert_package_downloading",
                        engine = package.tool.name(),
                        amount = amount
                    )
                    .into_owned()
                }
                atlas_core::ext::tool_package::Progress::Extracting => {
                    rust_i18n::t!("convert_package_extracting", engine = package.tool.name())
                        .into_owned()
                }
            };
            detail = detail.child(div().text_xs().text_center().child(label));
            if package.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                detail = detail.child(div().text_xs().child(text("convert_package_cancelling")));
            } else {
                detail = detail.child(button(
                    "convert-package-cancel",
                    text(
                        if matches!(
                            package.progress,
                            atlas_core::ext::tool_package::Progress::Download { .. }
                        ) {
                            "convert_package_cancel_download"
                        } else {
                            "convert_package_cancel_install"
                        },
                    ),
                    cx,
                    App::cancel_conversion_package,
                ));
            }
        }
        if let Some(error) = &session.package_error {
            detail = detail.child(
                div()
                    .text_xs()
                    .text_center()
                    .text_color(cx.theme().danger)
                    .child(error.clone()),
            );
        }
        detail.into_any_element()
    }

    /// Only the knobs the selected route honours; the destination always shows.
    fn render_conversion_settings(
        &mut self,
        quality_applies: bool,
        edge_applies: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let session = self.converter.as_ref().unwrap();
        let quality = session.settings.quality;
        let edge = session.settings.max_edge;
        let custom_dir = session.settings.output_dir.is_some();
        let destination = session
            .settings
            .output_dir
            .as_ref()
            .map_or_else(|| text("convert_beside"), |p| p.display().to_string());
        let muted = cx.theme().muted_foreground;
        let row = |label: String, control: AnyElement| {
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(div().text_xs().text_color(muted).child(label))
                .child(control)
        };
        let mut settings = v_flex().w_full().gap_1p5();
        if quality_applies {
            settings = settings.child(row(
                text("convert_quality"),
                value_button("convert-quality", format!("{quality}%"), cx, |app| {
                    if let Some(s) = &mut app.converter {
                        s.settings.quality = match s.settings.quality {
                            90 => 75,
                            75 => 100,
                            _ => 90,
                        };
                    }
                }),
            ));
        }
        if edge_applies {
            settings = settings.child(row(
                text("convert_dimensions"),
                value_button(
                    "convert-size",
                    edge.map_or_else(|| text("convert_original"), |n| format!("{n} px")),
                    cx,
                    |app| {
                        if let Some(s) = &mut app.converter {
                            s.settings.max_edge = match s.settings.max_edge {
                                None => Some(1920),
                                Some(1920) => Some(1280),
                                _ => None,
                            };
                        }
                    },
                ),
            ));
        }
        settings
            .child(row(
                text("convert_destination_label"),
                h_flex()
                    .gap_1()
                    .child(value_button(
                        "convert-folder",
                        text("convert_folder"),
                        cx,
                        App::choose_conversion_directory,
                    ))
                    .when(custom_dir, |s| {
                        s.child(value_button(
                            "convert-reset-folder",
                            text("convert_reset"),
                            cx,
                            |app| {
                                if let Some(s) = &mut app.converter {
                                    s.settings.output_dir = None;
                                }
                            },
                        ))
                    })
                    .into_any_element(),
            ))
            .child(
                div()
                    .w_full()
                    .truncate()
                    .text_xs()
                    .text_color(muted)
                    .child(destination),
            )
            .into_any_element()
    }
}
