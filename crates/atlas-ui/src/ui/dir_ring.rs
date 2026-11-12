//! Folder preview "ring": the real thumbnails (images, videos) of a folder's
//! files orbiting on a carousel. The pointer steers it — the closer to the
//! left/right edge (or a corner), the faster the ring spins that way; with the
//! pointer away it eases to the nearest card so one always sits in front.
//!
//! The ring owns its animation state in its own `Entity`: `request_animation_frame`
//! notifies the view being painted, so a per-frame spin only re-renders this
//! view and never `App` or the panes (see `palette_host.rs`).

use atlas_model::clamp::ClampLenient;
use gpui::prelude::*;
use gpui::{
    canvas, div, img, px, Bounds, Context, CursorStyle, Entity, MouseMoveEvent, ObjectFit, Pixels,
    RenderImage, Window,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

const WIDTH: f32 = 236.;
const HEIGHT: f32 = 236.;
/// Box each picture is fitted into; cards behind shrink toward `BACK_SCALE` of it.
const BOX_W: f32 = 130.;
const BOX_H: f32 = 178.;
const BACK_SCALE: f32 = 0.42;
/// Horizontal orbit radius and how far the front card sits below the back ones.
const RADIUS_X: f32 = 104.;
const DEPTH_DROP: f32 = 18.;
/// Cards whose depth (cos of their angle) is below this are behind the ring and
/// not drawn at all; the ring only shows its front arc.
const FRONT_CUTOFF: f32 = -0.2;
/// Pointer offset (of the half-width) that still counts as "centre".
const DEAD_ZONE: f32 = 0.14;
/// Spin speed (rad/s) with the pointer on the very edge.
const MAX_SPEED: f32 = 4.2;

pub(crate) struct DirRing {
    key: PathBuf,
    items: Vec<Arc<RenderImage>>,
    angle: f32,
    velocity: f32,
    /// Pointer offset in `[-1, 1]` from the ring's centre while hovering.
    pointer: Option<f32>,
    bounds: Option<Bounds<Pixels>>,
    last_frame: Option<Instant>,
}

impl DirRing {
    pub(crate) fn new() -> Self {
        Self {
            key: PathBuf::new(),
            items: Vec::new(),
            angle: 0.,
            velocity: 0.,
            pointer: None,
            bounds: None,
            last_frame: None,
        }
    }

    /// Replace the cards; a different folder restarts the ring at its first card.
    pub(crate) fn set_items(&mut self, key: &PathBuf, items: Vec<Arc<RenderImage>>) {
        if self.key != *key {
            self.key = key.clone();
            self.angle = 0.;
            self.velocity = 0.;
        }
        self.items = items;
    }

    fn on_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(bounds) = self.bounds else { return };
        let half = f32::from(bounds.size.width) * 0.5;
        if half <= 0. {
            return;
        }
        let center = f32::from(bounds.origin.x) + half;
        self.pointer = Some(((f32::from(event.position.x) - center) / half).clamp(-1., 1.));
        cx.notify();
    }

    /// Advance the physics by the time since the last frame; returns whether
    /// another frame is needed.
    fn step(&mut self) -> bool {
        let now = Instant::now();
        // First frame after idle has no previous timestamp: assume one 60 Hz tick so
        // the ring starts moving instead of parking with dt = 0.
        let dt = self
            .last_frame
            .map_or(1. / 60., |t| now.duration_since(t).as_secs_f32())
            .clamp_lenient(1. / 240., 0.05);
        self.last_frame = Some(now);
        let n = self.items.len();
        if n < 2 {
            self.angle = 0.;
            self.velocity = 0.;
            return false;
        }
        let step = std::f32::consts::TAU / n as f32;
        let ease = 1. - (-7. * dt).exp();
        match self.pointer {
            Some(x) => {
                let t = ((x.abs() - DEAD_ZONE).max(0.) / (1. - DEAD_ZONE)).powf(1.6);
                let target = x.signum() * t * MAX_SPEED;
                self.velocity += (target - self.velocity) * ease;
            }
            None => {
                // Spring to the nearest card so one always rests in front.
                let snap = (self.angle / step).round() * step;
                let target = ((snap - self.angle) * 7.).clamp_lenient(-MAX_SPEED, MAX_SPEED);
                self.velocity += (target - self.velocity) * ease;
                if (snap - self.angle).abs() < 0.002 && self.velocity.abs() < 0.02 {
                    self.angle = snap;
                    self.velocity = 0.;
                    self.last_frame = None;
                    return false;
                }
            }
        }
        self.angle += self.velocity * dt;
        if self.pointer.is_some() && self.velocity.abs() < 0.005 {
            // Parked in the dead zone: nothing to animate until the pointer moves.
            self.last_frame = None;
            return false;
        }
        true
    }
}

impl Render for DirRing {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.step() {
            window.request_animation_frame();
        }
        let this = cx.entity();
        let n = self.items.len();
        let step = if n == 0 {
            0.
        } else {
            std::f32::consts::TAU / n as f32
        };

        // (depth, card) — painted back to front. Bare pictures: each keeps its own
        // aspect ratio inside the box, with no frame or tile behind it.
        let mut cards: Vec<(f32, gpui::Div)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, texture)| {
                let theta = self.angle + step * i as f32;
                let depth = theta.cos();
                if depth < FRONT_CUTOFF {
                    return None;
                }
                let near = (depth + 1.) * 0.5;
                // Fade in over the first stretch of the front arc.
                let edge_fade = ((depth - FRONT_CUTOFF) / 0.45).clamp(0., 1.);
                let scale = BACK_SCALE + (1. - BACK_SCALE) * near;
                let natural = texture.size(0);
                let (nw, nh) = (
                    i32::from(natural.width).max(1) as f32,
                    i32::from(natural.height).max(1) as f32,
                );
                let fit = (BOX_W / nw).min(BOX_H / nh) * scale;
                let (w, h) = ((nw * fit).round().max(1.), (nh * fit).round().max(1.));
                let x = WIDTH * 0.5 + theta.sin() * RADIUS_X - w * 0.5;
                let y = HEIGHT * 0.5 + (depth - 0.3) * DEPTH_DROP - h * 0.5;
                let card = div()
                    .absolute()
                    .left(crate::ui::rem_px(x))
                    .top(crate::ui::rem_px(y))
                    .w(crate::ui::rem_px(w))
                    .h(crate::ui::rem_px(h))
                    .rounded(px(8.))
                    .overflow_hidden()
                    .shadow_md()
                    .opacity((0.3 + 0.7 * near * near) * edge_fade)
                    .child(
                        img(texture.clone())
                            .size_full()
                            .object_fit(ObjectFit::Contain),
                    );
                Some((depth, card))
            })
            .collect();
        cards.sort_by(|a, b| a.0.total_cmp(&b.0));

        div()
            .id("dir-ring")
            .relative()
            .flex_shrink_0()
            .w(crate::ui::rem_px(WIDTH))
            .h(crate::ui::rem_px(HEIGHT))
            .cursor(CursorStyle::PointingHand)
            .child(
                canvas(
                    {
                        let this = this.clone();
                        move |bounds, _, cx| {
                            this.update(cx, |ring: &mut DirRing, _| ring.bounds = Some(bounds));
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(cards.into_iter().map(|(_, card)| card))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                this.on_move(event, cx);
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.pointer = None;
                    cx.notify();
                }
            }))
    }
}

/// Convenience for the inspector: build the entity once, keep it afterwards.
pub(crate) fn ensure(slot: &mut Option<Entity<DirRing>>, cx: &mut gpui::App) -> Entity<DirRing> {
    slot.get_or_insert_with(|| cx.new(|_| DirRing::new()))
        .clone()
}
