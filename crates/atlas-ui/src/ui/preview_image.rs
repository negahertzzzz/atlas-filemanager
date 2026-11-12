//! Image geometry and gestures for the retained preview. The canvas receives its
//! real flex bounds, so fit, zoom, panning and the navigator share one viewport.
use crate::app::{App, PreviewView};
use atlas_model::clamp::ClampLenient;
use atlas_model::layout_tree::TileId;
use gpui::ColorExt as _;
use gpui::{
    canvas, point, px, size, Bounds, Context, Corners, CursorStyle, DispatchPhase, Entity,
    HitboxBehavior, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    RenderImage, ScrollDelta, ScrollWheelEvent, Styled,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ImageGeometry {
    pub(crate) viewport: Bounds<Pixels>,
    pub(crate) image: Bounds<Pixels>,
    pub(crate) scale: f32,
    pub(crate) fit_scale: f32,
    pub(crate) pan_x: f32,
    pub(crate) pan_y: f32,
    pub(crate) minimap: Option<Minimap>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Minimap {
    pub(crate) frame: Bounds<Pixels>,
    pub(crate) image: Bounds<Pixels>,
    pub(crate) visible: Bounds<Pixels>,
}

impl ImageGeometry {
    pub(crate) fn new(viewport: Bounds<Pixels>, source: (usize, usize), view: PreviewView) -> Self {
        let vw = f32::from(viewport.size.width).max(1.0);
        let vh = f32::from(viewport.size.height).max(1.0);
        let sw = source.0.max(1) as f32;
        let sh = source.1.max(1) as f32;
        let fit_scale = ((vw - 24.0).max(1.0) / sw)
            .min((vh - 24.0).max(1.0) / sh)
            .min(1.0);
        let scale = fit_scale * view.zoom;
        let (iw, ih) = (sw * scale, sh * scale);
        // Zoomed in: the image may slide until its edge meets the viewport.
        // Zoomed out: it may be repositioned anywhere inside the viewport.
        let limit_x = ((iw - vw) * 0.5).abs();
        let limit_y = ((ih - vh) * 0.5).abs();
        let pan_x = view.pan_x.clamp_lenient(-limit_x, limit_x);
        let pan_y = view.pan_y.clamp_lenient(-limit_y, limit_y);
        let image = Bounds {
            origin: point(
                viewport.origin.x + px((vw - iw) * 0.5 + pan_x),
                viewport.origin.y + px((vh - ih) * 0.5 + pan_y),
            ),
            size: size(px(iw), px(ih)),
        };
        let minimap = (iw > vw + 1.0 || ih > vh + 1.0).then(|| {
            // The frame hugs the thumbnail's aspect ratio (no letterboxing) and
            // sits top-right, clear of the floating zoom controls at the bottom.
            const PAD: f32 = 3.0;
            let max_w = (vw * 0.40).clamp(56.0, 148.0).min((vw - 24.0).max(1.0));
            let max_h = (vh * 0.30).clamp(42.0, 112.0).min((vh - 24.0).max(1.0));
            let mini_scale =
                ((max_w - PAD * 2.0).max(1.0) / sw).min((max_h - PAD * 2.0).max(1.0) / sh);
            let (tw, th) = (sw * mini_scale, sh * mini_scale);
            let frame = Bounds {
                origin: point(
                    viewport.right() - px(tw + PAD * 2.0 + 10.0),
                    viewport.top() + px(10.0),
                ),
                size: size(px(tw + PAD * 2.0), px(th + PAD * 2.0)),
            };
            let thumb = Bounds {
                origin: point(frame.origin.x + px(PAD), frame.origin.y + px(PAD)),
                size: size(px(tw), px(th)),
            };
            let left = (f32::from(viewport.left() - image.left()) / iw).clamp(0.0, 1.0);
            let top = (f32::from(viewport.top() - image.top()) / ih).clamp(0.0, 1.0);
            let right = (f32::from(viewport.right() - image.left()) / iw).clamp(0.0, 1.0);
            let bottom = (f32::from(viewport.bottom() - image.top()) / ih).clamp(0.0, 1.0);
            // Keep the viewport marker grabbable even at extreme zoom.
            let (mut vx, mut vw_mark) = (left * tw, ((right - left) * tw).max(8.0).min(tw));
            let (mut vy, mut vh_mark) = (top * th, ((bottom - top) * th).max(8.0).min(th));
            vx = vx.min(tw - vw_mark);
            vy = vy.min(th - vh_mark);
            vw_mark = vw_mark.max(1.0);
            vh_mark = vh_mark.max(1.0);
            Minimap {
                frame,
                image: thumb,
                visible: Bounds {
                    origin: point(thumb.origin.x + px(vx), thumb.origin.y + px(vy)),
                    size: size(px(vw_mark), px(vh_mark)),
                },
            }
        });
        Self {
            viewport,
            image,
            scale,
            fit_scale,
            pan_x,
            pan_y,
            minimap,
        }
    }

    pub(crate) fn zoom_at(self, view: &mut PreviewView, zoom: f32, anchor: gpui::Point<Pixels>) {
        let ratio = zoom / view.zoom.max(0.001);
        let center = self.viewport.center();
        let x = f32::from(anchor.x - center.x);
        let y = f32::from(anchor.y - center.y);
        view.pan_x = (self.pan_x - x) * ratio + x;
        view.pan_y = (self.pan_y - y) * ratio + y;
        view.zoom = zoom;
    }

    pub(crate) fn navigate(self, view: &mut PreviewView, position: gpui::Point<Pixels>) {
        if let Some(minimap) = self.minimap {
            let x = (f32::from(position.x - minimap.image.origin.x)
                / f32::from(minimap.image.size.width))
            .clamp(0.0, 1.0);
            let y = (f32::from(position.y - minimap.image.origin.y)
                / f32::from(minimap.image.size.height))
            .clamp(0.0, 1.0);
            view.pan_x = (0.5 - x) * f32::from(self.image.size.width);
            view.pan_y = (0.5 - y) * f32::from(self.image.size.height);
        }
    }
}

pub(crate) fn image_canvas(
    app: Entity<App>,
    tab: TileId,
    texture: Arc<RenderImage>,
    source: (usize, usize),
    view: PreviewView,
    cx: &Context<App>,
) -> impl IntoElement {
    use gpui_component::ActiveTheme as _;
    let colors = (cx.theme().background, cx.theme().border, cx.theme().accent);
    let prepaint_app = app.clone();
    canvas(
        move |bounds, window, cx| {
            let geometry = ImageGeometry::new(bounds, source, view);
            prepaint_app.update(cx, |this, cx| {
                let state = this.preview_view.entry(tab).or_default();
                let size_changed = state.viewport.map(|b| b.size) != Some(bounds.size);
                state.viewport = Some(bounds);
                state.pan_x = geometry.pan_x;
                state.pan_y = geometry.pan_y;
                if size_changed {
                    cx.notify();
                }
            });
            (
                geometry,
                window.insert_hitbox(bounds, HitboxBehavior::Normal),
            )
        },
        move |_, (geometry, hitbox), window, _cx| {
            // Every primitive inside one `paint_layer` shares a draw order and
            // gpui then draws sprites after quads, so each visual tier of the
            // navigator gets its own layer (otherwise images cover the marker).
            window.paint_layer(geometry.viewport, |window| {
                let _ = window.paint_image(
                    geometry.image,
                    geometry.image,
                    Corners::default(),
                    texture.clone(),
                    0,
                    false,
                );
            });
            if let Some(minimap) = geometry.minimap {
                paint_minimap(window, minimap, &texture, colors);
            }
            if hitbox.is_hovered(window) {
                let cursor = if geometry
                    .minimap
                    .is_some_and(|m| m.frame.contains(&window.mouse_position()))
                {
                    CursorStyle::PointingHand
                } else {
                    CursorStyle::OpenHand
                };
                window.set_cursor_style(cursor, &hitbox);
            }
            let scroll_app = app.clone();
            let scroll_hitbox = hitbox.clone();
            window.on_mouse_event::<ScrollWheelEvent>(move |event, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !scroll_hitbox.is_hovered(window) {
                    return;
                }
                let delta = match event.delta {
                    ScrollDelta::Pixels(p) => f32::from(p.y) * 0.012,
                    ScrollDelta::Lines(p) | ScrollDelta::PreciseLines(p) => p.y * 0.14,
                };
                if delta != 0.0 {
                    scroll_app.update(cx, |this, cx| {
                        if this.state.focused_tile != tab {
                            return;
                        }
                        let state = this.preview_view.entry(tab).or_default();
                        let zoom = (state.zoom * delta.clamp(-0.4, 0.4).exp()).clamp(0.1, 48.0);
                        ImageGeometry::new(geometry.viewport, source, *state).zoom_at(
                            state,
                            zoom,
                            event.position,
                        );
                        cx.notify();
                    });
                }
                cx.stop_propagation();
            });
            let down_app = app.clone();
            let down_hitbox = hitbox.clone();
            window.on_mouse_event::<MouseDownEvent>(move |event, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !down_hitbox.is_hovered(window)
                {
                    return;
                }
                down_app.update(cx, |this, cx| {
                    if this.state.focused_tile != tab {
                        return;
                    }
                    let state = this.preview_view.entry(tab).or_default();
                    if geometry
                        .minimap
                        .is_some_and(|m| m.frame.contains(&event.position))
                    {
                        geometry.navigate(state, event.position);
                        state.minimap_drag = true;
                    } else if event.click_count >= 2 {
                        state.zoom = if (state.zoom - 1.0).abs() < 0.01 {
                            2.0
                        } else {
                            1.0
                        };
                        state.pan_x = 0.0;
                        state.pan_y = 0.0;
                        state.minimap_drag = false;
                    } else {
                        state.minimap_drag = false;
                    }
                    this.preview_drag = Some(event.position);
                    cx.notify();
                });
                cx.stop_propagation();
            });
            let move_app = app.clone();
            window.on_mouse_event::<MouseMoveEvent>(move |event, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                move_app.update(cx, |this, cx| {
                    if this.state.focused_tile != tab {
                        return;
                    }
                    if event.pressed_button != Some(MouseButton::Left) {
                        this.preview_drag = None;
                        return;
                    }
                    if let Some(previous) = this.preview_drag {
                        let state = this.preview_view.entry(tab).or_default();
                        if state.minimap_drag {
                            geometry.navigate(state, event.position);
                        } else {
                            state.pan_x += f32::from(event.position.x - previous.x);
                            state.pan_y += f32::from(event.position.y - previous.y);
                        }
                        this.preview_drag = Some(event.position);
                        cx.notify();
                    }
                });
            });
            let up_app = app.clone();
            window.on_mouse_event::<MouseUpEvent>(move |event, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    up_app.update(cx, |this, _| {
                        this.preview_drag = None;
                        if let Some(state) = this.preview_view.get_mut(&tab) {
                            state.minimap_drag = false;
                        }
                    });
                }
            });
        },
    )
    .size_full()
}

/// Navigator: translucent frame, the full image, a dimmed surround and a crisp
/// outline around the visible region.
fn paint_minimap(
    window: &mut gpui::Window,
    minimap: Minimap,
    texture: &Arc<RenderImage>,
    (background, border, accent): (gpui::Hsla, gpui::Hsla, gpui::Hsla),
) {
    window.paint_layer(minimap.frame, |window| {
        window.paint_quad(gpui::quad(
            minimap.frame,
            px(7.0),
            background.opacity(0.92),
            px(1.0),
            border.opacity(0.55),
            Default::default(),
        ));
    });
    window.paint_layer(minimap.frame, |window| {
        let _ = window.paint_image(
            minimap.image,
            minimap.image,
            Corners::all(px(3.0)),
            texture.clone(),
            0,
            false,
        );
    });
    window.paint_layer(minimap.frame, |window| {
        let (image, visible) = (minimap.image, minimap.visible);
        let shade = gpui::hsla(0.0, 0.0, 0.0, 0.58);
        let bands = [
            Bounds::from_corners(image.origin, point(image.right(), visible.top())),
            Bounds::from_corners(point(image.left(), visible.bottom()), image.bottom_right()),
            Bounds::from_corners(
                point(image.left(), visible.top()),
                point(visible.left(), visible.bottom()),
            ),
            Bounds::from_corners(
                point(visible.right(), visible.top()),
                point(image.right(), visible.bottom()),
            ),
        ];
        for band in bands {
            if band.size.width > px(0.0) && band.size.height > px(0.0) {
                window.paint_quad(gpui::fill(band, shade));
            }
        }
        // Dark halo + accent ring: legible on bright and dark images alike.
        window.paint_quad(gpui::quad(
            visible.dilate(px(1.0)),
            px(3.0),
            gpui::transparent_black(),
            px(1.0),
            gpui::hsla(0.0, 0.0, 0.0, 0.6),
            Default::default(),
        ));
        window.paint_quad(gpui::quad(
            visible,
            px(2.0),
            accent.opacity(0.14),
            px(2.0),
            accent,
            Default::default(),
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn viewport() -> Bounds<Pixels> {
        Bounds {
            origin: point(px(900.0), px(80.0)),
            size: size(px(320.0), px(400.0)),
        }
    }

    #[test]
    fn fit_and_pan_use_the_real_offset_viewport() {
        let view = PreviewView {
            pan_x: 900.0,
            pan_y: -500.0,
            ..Default::default()
        };
        let geometry = ImageGeometry::new(viewport(), (2048, 2048), view);
        // A fitted image can still be repositioned, but never leaves the viewport.
        assert!(geometry.pan_x > 0.0 && geometry.pan_y < 0.0);
        assert!(geometry.image.left() >= viewport().left());
        assert!(geometry.image.right() <= viewport().right());
        assert!(geometry.image.top() >= viewport().top());
        assert!(geometry.image.bottom() <= viewport().bottom());
        assert!(geometry.minimap.is_none());
    }

    #[test]
    fn navigator_projects_the_visible_area_and_clamps_navigation() {
        let mut view = PreviewView {
            zoom: 4.0,
            ..Default::default()
        };
        let geometry = ImageGeometry::new(viewport(), (2048, 2048), view);
        let minimap = geometry.minimap.unwrap();
        assert!(minimap.visible.size.width < minimap.image.size.width);
        geometry.navigate(&mut view, minimap.image.origin);
        let moved = ImageGeometry::new(viewport(), (2048, 2048), view);
        assert_eq!(moved.image.left(), viewport().left());
        assert_eq!(moved.image.top(), viewport().top());
        assert_eq!(moved.minimap.unwrap().visible.origin, minimap.image.origin);
    }

    #[test]
    fn wheel_zoom_keeps_the_image_point_under_the_pointer() {
        let mut view = PreviewView {
            zoom: 3.0,
            ..Default::default()
        };
        let before = ImageGeometry::new(viewport(), (2048, 2048), view);
        let pointer = point(px(1020.0), px(240.0));
        let image_x = f32::from(pointer.x - before.image.left()) / before.scale;
        before.zoom_at(&mut view, 4.5, pointer);
        let after = ImageGeometry::new(viewport(), (2048, 2048), view);
        assert!((image_x - f32::from(pointer.x - after.image.left()) / after.scale).abs() < 0.01);
    }
}
