//! The same polar geometry owns paint and hit testing; no invisible box targets.
use super::super::converter::{Phase, Session};
use crate::app::App;
use gpui::prelude::*;
use gpui::{
    canvas, point, px, Bounds, DispatchPhase, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    PathBuilder, Pixels, Point, WeakEntity, Window,
};
use std::f32::consts::TAU;

use super::geometry::{hit, Ring, GAP};

fn polar(center: Point<Pixels>, radius: f32, angle: f32) -> Point<Pixels> {
    point(
        center.x + px(radius * angle.cos()),
        center.y + px(radius * angle.sin()),
    )
}

/// Point `distance` px from `a` toward `b`.
fn toward(a: Point<Pixels>, b: Point<Pixels>, distance: f32) -> Point<Pixels> {
    let (dx, dy) = (f32::from(b.x - a.x), f32::from(b.y - a.y));
    let t = distance / dx.hypot(dy).max(f32::EPSILON);
    point(a.x + px(dx * t), a.y + px(dy * t))
}

/// One annular sector with a constant pixel gutter and four rounded corners.
fn sector(
    center: Point<Pixels>,
    inner: f32,
    outer: f32,
    mid: f32,
    step: f32,
) -> Option<gpui::Path<Pixels>> {
    let half = step * 0.5;
    let (os, oe) = (
        mid - half + GAP * 0.5 / outer,
        mid + half - GAP * 0.5 / outer,
    );
    let (is, ie) = (
        mid - half + GAP * 0.5 / inner,
        mid + half - GAP * 0.5 / inner,
    );
    let corner = ((outer - inner) * 0.22).min(inner * (ie - is).max(0.0) * 0.3);
    let (oa, ob) = (os + corner / outer, oe - corner / outer);
    let (ia, ib) = (is + corner / inner, ie - corner / inner);
    let (o_end, i_end) = (polar(center, outer, oe), polar(center, inner, ie));
    let (o_start, i_start) = (polar(center, outer, os), polar(center, inner, is));
    // Exact circular arcs; quadratic-equivalent cubics round the corners.
    // A constant-size command buffer, no per-frame polygon allocations.
    let mut path = PathBuilder::fill();
    let curve = |path: &mut PathBuilder, a: Point<Pixels>, p: Point<Pixels>, b: Point<Pixels>| {
        let c1 = point(
            a.x + (p.x - a.x) * (2.0 / 3.0),
            a.y + (p.y - a.y) * (2.0 / 3.0),
        );
        let c2 = point(
            b.x + (p.x - b.x) * (2.0 / 3.0),
            b.y + (p.y - b.y) * (2.0 / 3.0),
        );
        path.cubic_bezier_to(b, c1, c2);
    };
    path.move_to(polar(center, outer, oa));
    path.arc_to(
        point(px(outer), px(outer)),
        px(0.),
        ob - oa > std::f32::consts::PI,
        true,
        polar(center, outer, ob),
    );
    curve(
        &mut path,
        polar(center, outer, ob),
        o_end,
        toward(o_end, i_end, corner),
    );
    path.line_to(toward(i_end, o_end, corner));
    curve(
        &mut path,
        toward(i_end, o_end, corner),
        i_end,
        polar(center, inner, ib),
    );
    path.arc_to(
        point(px(inner), px(inner)),
        px(0.),
        ib - ia > std::f32::consts::PI,
        false,
        polar(center, inner, ia),
    );
    curve(
        &mut path,
        polar(center, inner, ia),
        i_start,
        toward(i_start, o_start, corner),
    );
    path.line_to(toward(o_start, i_start, corner));
    curve(
        &mut path,
        toward(o_start, i_start, corner),
        o_start,
        polar(center, outer, oa),
    );
    path.close();
    path.build().ok()
}

/// `rings` are in base px; `scale` fits them into the window (and zoom).
pub(super) fn paint_wheel(
    scale: f32,
    rings: Vec<Ring>,
    selected: usize,
    hovered: Option<usize>,
    available: Vec<bool>,
    colors: (Hsla, Hsla, Hsla, Hsla),
    reveal: f32,
    (hover, selection): (Vec<f32>, Vec<f32>),
    view: WeakEntity<App>,
) -> gpui::AnyElement {
    let diameter = super::geometry::radius(&rings) * 2.0 * scale;
    #[cfg(feature = "conversion-lab")]
    let geometry_view = view.clone();
    canvas(
        move |bounds, _, cx| {
            #[cfg(feature = "conversion-lab")]
            geometry_view.update(cx, |app, _| {
                if let Some(session) = app.converter.as_mut() {
                    session.wheel_bounds = Some(bounds);
                }
            });
            let _ = (bounds, cx);
        },
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _cx| {
            let center = bounds.center();
            let count = available.len().max(1);
            for ring in &rings {
                let step = TAU / ring.len as f32;
                for local in 0..ring.len {
                    let index = ring.start + local;
                    let mid = super::geometry::angle(local, ring.len);
                    let h = hover.get(index).copied().unwrap_or(0.);
                    let s = selection.get(index).copied().unwrap_or(0.);
                    // The hovered sector lifts outward a few pixels.
                    let lift = (h * 5.0 + s * 2.0) * scale;
                    let Some(path) = sector(
                        center,
                        ring.inner * scale,
                        ring.outer * scale + lift,
                        mid,
                        step,
                    ) else {
                        continue;
                    };
                    let base = if available.get(index).copied().unwrap_or(false) {
                        colors.0
                    } else {
                        colors.2
                    };
                    // Sectors fade in clockwise from the pinned one.
                    let order = (index + count - selected.min(count - 1)) % count;
                    let alpha = super::render::sector_alpha(reveal, order, count);
                    window.paint_path(path.clone(), gpui::ColorExt::opacity(&base, alpha));
                    if h > 0.003 && index != selected {
                        window.paint_path(
                            path.clone(),
                            gpui::ColorExt::opacity(&colors.3, alpha * h * (1. - s)),
                        );
                    }
                    if s > 0.003 {
                        window.paint_path(path, gpui::ColorExt::opacity(&colors.1, alpha * s));
                    }
                }
            }
            let locate = {
                let rings = rings.clone();
                move |position: Point<Pixels>| {
                    hit(
                        f32::from(position.x - center.x) / scale,
                        f32::from(position.y - center.y) / scale,
                        &rings,
                    )
                }
            };
            // Hover only previews: crossing sectors on the way to the info
            // column must not change the pinned choice.
            let hover_view = view.clone();
            let hover_locate = locate.clone();
            window.on_mouse_event::<MouseMoveEvent>(move |ev, phase, _window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let index = if bounds.contains(&ev.position) {
                    hover_locate(ev.position)
                } else {
                    None
                };
                hover_view.update(cx, |app, cx| {
                    if let Some(session) = app
                        .converter
                        .as_mut()
                        .filter(|s| matches!(s.phase, Phase::Choosing))
                    {
                        if session.hovered != index {
                            session.hovered = index;
                            cx.notify();
                        }
                    }
                });
            });
            // Click pins a format; a double click converts straight away.
            let click_view = view.clone();
            window.on_mouse_event::<MouseDownEvent>(move |ev, phase, _window, cx| {
                if phase != DispatchPhase::Bubble
                    || ev.button != MouseButton::Left
                    || !bounds.contains(&ev.position)
                {
                    return;
                }
                let Some(index) = locate(ev.position) else {
                    return;
                };
                click_view.update(cx, |app, cx| {
                    if let Some(Session {
                        phase: Phase::Choosing,
                        selected,
                        ..
                    }) = app.converter.as_mut()
                    {
                        *selected = index;
                        if ev.click_count >= 2 {
                            app.start_conversion();
                        }
                        cx.notify();
                    }
                });
                cx.stop_propagation();
            });
        },
    )
    .size(px(diameter))
    .into_any_element()
}
