//! Atlas brand surface for first-run setup and the feature guide.
//!
//! Mirrors atlasnative.com: near-black ink, neutral greys, orange only as a
//! small accent. Light lives on the edges, never over content: a neutral rim
//! that is brighter along the top, a warm hairline on the bottom edge and a
//! warm bloom *behind* the panel. Primary actions are white pills.

use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{div, linear_color_stop, linear_gradient, point, px, AnyElement, BoxShadow, Div, Hsla};

fn hex(v: &str) -> Hsla {
    <Hsla as gpui_component::Colorize>::parse_hex(v).unwrap_or_default()
}

/// Headline / primary text (`--ink`).
pub(crate) fn ink() -> Hsla {
    hex("#f2f2f2")
}
/// Body text (`--text`).
pub(crate) fn text() -> Hsla {
    hex("#cfcfcf")
}
/// Secondary text (`--muted`).
pub(crate) fn muted() -> Hsla {
    hex("#8c8c8c")
}
/// Labels, numerals, second tone of headlines (`--faint`).
pub(crate) fn faint() -> Hsla {
    hex("#5a5a5a")
}
/// The one warm accent, used as text/hairline, never as a big fill (`--soft`).
pub(crate) fn soft() -> Hsla {
    hex("#ff9a5c")
}
/// Hot core of the warm hairline (`--hot`).
pub(crate) fn hot() -> Hsla {
    hex("#ff5a1f")
}
/// Translucent glass over the blurred backdrop.
pub(crate) fn glass() -> Hsla {
    gpui::hsla(240. / 360., 0.04, 0.05, 0.72)
}
/// Neutral rim and separators.
pub(crate) fn rim() -> Hsla {
    gpui::hsla(0., 0., 1., 0.08)
}
/// Quiet fill for tiles and ghost buttons (`rgba(255,255,255,.04)`).
pub(crate) fn wash() -> Hsla {
    gpui::hsla(0., 0., 1., 0.04)
}
/// Hover fill.
pub(crate) fn wash_hover() -> Hsla {
    gpui::hsla(0., 0., 1., 0.08)
}

pub(crate) const RADIUS: f32 = 28.;

/// Floating brand panel: rounded squircle, frosted, neutral rim, deep shadow
/// plus a faint warm bloom spreading behind it.
pub(crate) fn panel() -> Div {
    div()
        .relative()
        .rounded(px(RADIUS))
        .rounded_smoothing_ios()
        .backdrop_blur(px(28.))
        .bg(glass())
        .border_1()
        .border_color(rim())
        .shadow(vec![
            BoxShadow {
                color: gpui::hsla(0., 0., 0., 0.55).into(),
                offset: point(px(0.), px(30.)),
                blur_radius: px(60.),
                spread_radius: px(-30.),
                inset: false,
            },
            BoxShadow {
                color: hot().opacity(0.10).into(),
                offset: point(px(0.), px(70.)),
                blur_radius: px(130.),
                spread_radius: px(-60.),
                inset: false,
            },
        ])
}

/// Brighter specular along the top edge, kept clear of the rounded corners.
pub(crate) fn top_rim() -> AnyElement {
    div()
        .absolute()
        .top_0()
        .left(px(RADIUS))
        .right(px(RADIUS))
        .h(px(1.))
        .child(
            gpui_component::h_flex()
                .size_full()
                .child(div().flex_1().h_full().bg(linear_gradient(
                    90.,
                    linear_color_stop(gpui::hsla(0., 0., 1., 0.0), 0.0),
                    linear_color_stop(gpui::hsla(0., 0., 1., 0.20), 1.0),
                )))
                .child(div().flex_1().h_full().bg(linear_gradient(
                    90.,
                    linear_color_stop(gpui::hsla(0., 0., 1., 0.20), 0.0),
                    linear_color_stop(gpui::hsla(0., 0., 1., 0.0), 1.0),
                ))),
        )
        .into_any_element()
}

/// Warm hairline on the bottom edge: transparent → orange → cream → orange → transparent.
pub(crate) fn warm_edge() -> AnyElement {
    let cream = hex("#ffd6aa");
    div()
        .absolute()
        .bottom_0()
        .left(px(RADIUS * 2.))
        .right(px(RADIUS * 2.))
        .h(px(1.))
        .child(
            gpui_component::h_flex()
                .size_full()
                .child(div().flex_1().h_full().bg(linear_gradient(
                    90.,
                    linear_color_stop(soft().opacity(0.0), 0.0),
                    linear_color_stop(cream.opacity(0.75), 1.0),
                )))
                .child(div().flex_1().h_full().bg(linear_gradient(
                    90.,
                    linear_color_stop(cream.opacity(0.75), 0.0),
                    linear_color_stop(soft().opacity(0.0), 1.0),
                ))),
        )
        .into_any_element()
}

/// Full-window backdrop: dim + blur the app, with a warm bloom low in the frame.
pub(crate) fn scrim() -> Div {
    div()
        .absolute()
        .inset_0()
        .size_full()
        .backdrop_blur(px(10.))
        .bg(linear_gradient(
            180.,
            linear_color_stop(gpui::hsla(0., 0., 0., 0.52), 0.0),
            linear_color_stop(gpui::hsla(0., 0., 0., 0.74), 1.0),
        ))
}

/// White pill (`.btn.light`): the one primary action on a surface.
pub(crate) fn pill(
    id: &'static str,
    label: String,
    trailing: Option<AnyElement>,
) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .relative()
        .h(px(40.))
        .px(px(22.))
        .flex()
        .items_center()
        .gap_2()
        .rounded_full()
        .bg(linear_gradient(
            180.,
            linear_color_stop(hex("#ffffff"), 0.0),
            linear_color_stop(hex("#d9d9d9"), 1.0),
        ))
        .text_color(hex("#0a0a0a"))
        .text_sm()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|h| h.opacity(0.92))
        .child(label)
        .children(trailing)
}

/// Ghost pill (`.btn.ghost`): secondary action, a quiet wash with a thin rim.
pub(crate) fn ghost(id: &'static str, label: String) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .h(px(40.))
        .px(px(20.))
        .flex()
        .items_center()
        .rounded_full()
        .bg(wash())
        .border_1()
        .border_color(gpui::hsla(0., 0., 1., 0.14))
        .text_color(ink())
        .text_sm()
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|h| {
            h.bg(wash_hover())
                .border_color(gpui::hsla(0., 0., 1., 0.22))
        })
        .child(label)
}

/// Small mono label (eyebrows, numerals, counters).
pub(crate) fn mono(text: String, color: Hsla) -> Div {
    div()
        .font_family("Geist Mono")
        .text_size(px(11.))
        .text_color(color)
        .child(text)
}
