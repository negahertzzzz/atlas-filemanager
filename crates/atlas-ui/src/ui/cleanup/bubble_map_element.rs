use super::{packing, CleanupPanel};
use crate::ui::{
    file_icons::FileIcon,
    icons::AtlasIcon,
    surface::{chrome_ui_font, paint_theme_icon, shape_text_truncated, Slot},
};
use atlas_model::clamp::ClampLenient;
use gpui::*;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Node {
    pub path: Option<PathBuf>,
    pub label: String,
    pub size: String,
    pub bytes: u64,
    pub directory: bool,
}
#[derive(Default)]
pub(super) struct Cache {
    pub(super) key: String,
    pub(super) circles: Vec<packing::Circle>,
    pub(super) bounds: Bounds<Pixels>,
    pub(super) rebuilds: u64,
    revision: u64,
    started: Option<std::time::Instant>,
    /// Per-bubble hover amount and velocity of its spring (0 = rest, 1 = hovered).
    hover: Vec<f32>,
    vel: Vec<f32>,
    tick: Option<std::time::Instant>,
    labels: Vec<(ShapedLine, ShapedLine)>,
}

impl Cache {
    /// Advance every bubble's hover spring; true while anything still moves.
    fn step_hover(&mut self, hovered: Option<usize>) -> bool {
        let n = self.circles.len();
        if self.hover.len() != n {
            self.hover = vec![0.; n];
            self.vel = vec![0.; n];
        }
        let now = std::time::Instant::now();
        let dt = self
            .tick
            .map_or(1. / 60., |t| now.duration_since(t).as_secs_f32())
            .clamp_lenient(0.001, 1. / 30.);
        let mut moving = false;
        for i in 0..n {
            let target = f32::from(u8::from(hovered == Some(self.circles[i].index)));
            if cfg!(feature = "ui-lab") {
                self.hover[i] = target;
                continue;
            }
            // Lightly underdamped spring: a small, soft overshoot.
            let accel = 300. * (target - self.hover[i]) - 24. * self.vel[i];
            self.vel[i] += accel * dt;
            self.hover[i] += self.vel[i] * dt;
            if (target - self.hover[i]).abs() < 0.002 && self.vel[i].abs() < 0.01 {
                self.hover[i] = target;
                self.vel[i] = 0.;
            } else {
                moving = true;
            }
        }
        self.tick = moving.then_some(now);
        moving
    }
}

/// Overshoots slightly; evaluated here, outside gpui's easing assertion.
fn ease_out_back(t: f32) -> f32 {
    1. + 2.2 * (t - 1.).powi(3) + 1.2 * (t - 1.).powi(2)
}

pub(super) struct BubbleMapElement {
    pub panel: WeakEntity<CleanupPanel>,
    pub nodes: Arc<Vec<Node>>,
    pub revision: u64,
    pub enter: f32,
    pub selected: Option<PathBuf>,
    pub hovered: Option<usize>,
    pub cache: Rc<RefCell<Cache>>,
    pub fg: Hsla,
    pub muted: Hsla,
    pub dark: bool,
    pub windows: bool,
    pub colors: [Hsla; 5],
    pub icons: Arc<Vec<Option<FileIcon>>>,
}
pub(super) struct MapPrepaint {
    hitbox: Hitbox,
}
impl IntoElement for BubbleMapElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for BubbleMapElement {
    type RequestLayoutState = ();
    type PrepaintState = MapPrepaint;
    fn id(&self) -> Option<ElementId> {
        Some("cleanup-bubbles".into())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> MapPrepaint {
        let _span = crate::perf::span("cleanup.map.prepaint");
        let font = chrome_ui_font(window);
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let key = format!(
            "{}:{:?}:{:?}:{:?}:{:?}:{:?}:{:?}:{}",
            self.revision, bounds.size, font, self.fg, self.muted, zoom, self.colors, self.windows
        );
        let mut cache = self.cache.borrow_mut();
        cache.bounds = bounds;
        if cache.key != key {
            cache.rebuilds += 1;
            crate::perf::count("cleanup.map.rebuild");
            cache.circles = packing::pack(
                &self.nodes.iter().map(|n| n.bytes).collect::<Vec<_>>(),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            );
            cache.labels = cache
                .circles
                .iter()
                .map(|circle| {
                    let node = &self.nodes[circle.index];
                    let width = (circle.radius * 1.55).max(0.);
                    let large = circle.radius > 100. * zoom;
                    let mut number_font = font.clone();
                    number_font.weight = FontWeight::SEMIBOLD;
                    (
                        shape_text_truncated(
                            &node.label,
                            &font,
                            px(if large { 13. * zoom } else { 11. * zoom }),
                            self.fg,
                            width,
                            window,
                        ),
                        shape_text_truncated(
                            &node.size,
                            &number_font,
                            px(if large { 20. * zoom } else { 13. * zoom }),
                            self.fg,
                            width,
                            window,
                        ),
                    )
                })
                .collect();
            // The hidden lab captures single frames; keep them deterministic.
            if !cfg!(feature = "ui-lab")
                && cache.revision != self.revision
                && !cache.circles.is_empty()
            {
                cache.started = Some(std::time::Instant::now());
            }
            cache.revision = self.revision;
            cache.hover.clear();
            cache.vel.clear();
            cache.key = key;
        }
        MapPrepaint {
            hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut MapPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let _span = crate::perf::span("cleanup.map.paint");
        let hover_moving = self.cache.borrow_mut().step_hover(self.hovered);
        let cache = self.cache.borrow();
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let elapsed = cache
            .started
            .map_or(f32::MAX, |s| s.elapsed().as_secs_f32());
        // Bubbles pop in one after another; the whole entrance is under 0.6 s.
        if elapsed < 0.6 || hover_moving {
            window.request_animation_frame();
        }
        let (mid_x, mid_y) = (
            f32::from(bounds.size.width) * 0.5,
            f32::from(bounds.size.height) * 0.5,
        );
        window.with_content_mask(
            Some(ContentMask {
                bounds,
                fade_out: Default::default(),
            }),
            |window| {
                for (i, circle) in cache.circles.iter().enumerate() {
                    let delay = (i as f32 * 0.018).min(0.22);
                    let t = ((elapsed - delay) / 0.32).clamp(0., 1.);
                    let scale = self.enter + (1. - self.enter) * ease_out_back(t);
                    let fade = 1. - (1. - t).powi(3);
                    let h = cache.hover.get(i).copied().unwrap_or(0.);
                    let circle = &packing::Circle {
                        x: mid_x + (circle.x - mid_x) * scale,
                        y: mid_y + (circle.y - mid_y) * scale,
                        radius: circle.radius * scale * (1. + 0.04 * h),
                        index: circle.index,
                    };
                    let node = &self.nodes[circle.index];
                    let selected = node.path.is_some() && node.path == self.selected;
                    let color = super::appearance::file_color(
                        node.path.as_deref(),
                        node.directory,
                        &self.colors,
                    );
                    let diameter = circle.radius * 2.;
                    let b = Bounds {
                        origin: point(
                            bounds.origin.x + px(circle.x - circle.radius),
                            bounds.origin.y + px(circle.y - circle.radius),
                        ),
                        size: size(px(diameter), px(diameter)),
                    };
                    window.paint_quad(quad(
                        b,
                        Corners::all(px(circle.radius)),
                        color.opacity(
                            fade * if selected {
                                0.22
                            } else if self.dark {
                                0.12 + 0.07 * h
                            } else {
                                0.065 + 0.05 * h
                            },
                        ),
                        Edges::all(px(if selected {
                            2. * zoom
                        } else {
                            (1. + 0.5 * h) * zoom
                        })),
                        color.opacity(fade * if selected { 0.9 } else { 0.35 + 0.55 * h }),
                        Default::default(),
                    ));
                    if selected {
                        window.paint_quad(quad(
                            b.dilate(px(4. * zoom)),
                            Corners::all(px(circle.radius + 4. * zoom)),
                            transparent_black(),
                            Edges::all(px(1.)),
                            color.opacity(0.22),
                            Default::default(),
                        ));
                    }
                    if circle.radius >= 23. * zoom && t > 0.7 {
                        let icon_size =
                            (circle.radius * 0.48).clamp_lenient(20. * zoom, 52. * zoom);
                        let icon_y = if circle.radius > 43. * zoom {
                            circle.y - icon_size - 6. * zoom
                        } else {
                            circle.y - icon_size * 0.5
                        };
                        let texture = self
                            .icons
                            .get(circle.index)
                            .and_then(|i| i.as_ref())
                            .and_then(|icon| {
                                icon.texture_for_size(icon_size * f32::from(window.scale_factor()))
                            });
                        if let Some(texture) = texture {
                            let natural = texture.size(0);
                            let natural_w = i32::from(natural.width).max(1) as f32;
                            let natural_h = i32::from(natural.height).max(1) as f32;
                            let fit = (icon_size / natural_w).min(icon_size / natural_h);
                            let image_bounds = Bounds {
                                origin: point(
                                    bounds.origin.x + px(circle.x - natural_w * fit * 0.5),
                                    bounds.origin.y
                                        + px(icon_y + (icon_size - natural_h * fit) * 0.5),
                                ),
                                size: size(px(natural_w * fit), px(natural_h * fit)),
                            };
                            if !node.directory && !self.dark {
                                window.paint_quad(quad(
                                    image_bounds.dilate(px(4. * zoom)),
                                    Corners::all(px(6. * zoom)),
                                    self.fg.opacity(0.55),
                                    Edges::default(),
                                    transparent_black(),
                                    Default::default(),
                                ));
                            }
                            let _ = window.paint_image(
                                image_bounds,
                                image_bounds,
                                Corners::default(),
                                texture,
                                0,
                                false,
                            );
                        } else {
                            paint_theme_icon(
                                window,
                                cx,
                                bounds.origin,
                                Slot::new(circle.x - icon_size * 0.5, icon_y, icon_size, icon_size),
                                if node.path.is_none() {
                                    AtlasIcon::Grid
                                } else if node.directory {
                                    AtlasIcon::Folder
                                } else {
                                    AtlasIcon::File
                                },
                                self.windows,
                                icon_size,
                                color,
                            );
                        }
                    }
                    if circle.radius > 43. * zoom && t > 0.7 {
                        let (label, size) = &cache.labels[i];
                        let center = bounds.origin.x + px(circle.x);
                        let _ = label.paint(
                            point(
                                center - label.width() * 0.5,
                                bounds.origin.y + px(circle.y + 5. * zoom),
                            ),
                            px(16. * zoom),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                        let _ = size.paint(
                            point(
                                center - size.width() * 0.5,
                                bounds.origin.y + px(circle.y + 20. * zoom),
                            ),
                            px(24. * zoom),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }
            },
        );
        let circles = Arc::new(cache.circles.clone());
        let hitbox = prepaint.hitbox.clone();
        let panel = self.panel.clone();
        let nodes = self.nodes.clone();
        let down_circles = circles.clone();
        window.on_mouse_event::<MouseDownEvent>(move |event, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            if let Some(index) = hit(&down_circles, bounds, event.position) {
                let _ = panel.update(cx, |panel, cx| {
                    panel.map_click(nodes[index].path.clone(), event.click_count, cx);
                });
                cx.stop_propagation();
            }
        });
        let hitbox = prepaint.hitbox.clone();
        let panel = self.panel.clone();
        window.on_mouse_event::<MouseMoveEvent>(move |event, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let hovered = if hitbox.is_hovered(window) {
                hit(&circles, bounds, event.position)
            } else {
                None
            };
            let _ = panel.update(cx, |panel, cx| {
                if panel.hovered != hovered {
                    panel.hovered = hovered;
                    cx.notify();
                }
            });
        });
    }
}
fn hit(
    circles: &[packing::Circle],
    bounds: Bounds<Pixels>,
    position: Point<Pixels>,
) -> Option<usize> {
    let x = f32::from(position.x - bounds.origin.x);
    let y = f32::from(position.y - bounds.origin.y);
    circles
        .iter()
        .find(|c| (x - c.x).powi(2) + (y - c.y).powi(2) <= c.radius.powi(2))
        .map(|c| c.index)
}
