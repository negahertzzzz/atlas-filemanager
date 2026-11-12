use gpui::{canvas, point, px, AnyElement, Hsla, IntoElement, PathBuilder, Styled};
use std::{sync::OnceLock, time::Instant};

fn angle_degrees(seconds: f64) -> f32 {
    (seconds * 350.0).rem_euclid(360.0) as f32
}

/// A monotonic clock determines orientation; no per-frame angle is accumulated.
pub(crate) fn loading_spinner(color: Hsla, animate: bool) -> AnyElement {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = *EPOCH.get_or_init(Instant::now);
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _cx| {
            let angle = if animate {
                angle_degrees(epoch.elapsed().as_secs_f64())
            } else {
                0.0
            }
            .to_radians();
            let radius = f32::from(bounds.size.width.min(bounds.size.height)) * 0.38;
            let center = bounds.center();
            let mut arc = PathBuilder::stroke(px(1.5));
            for segment in 0..=32 {
                let t = angle + segment as f32 / 32.0 * std::f32::consts::TAU * 0.75;
                let point = point(
                    center.x + px(radius * t.cos()),
                    center.y + px(radius * t.sin()),
                );
                if segment == 0 {
                    arc.move_to(point);
                } else {
                    arc.line_to(point);
                }
            }
            if let Ok(path) = arc.build() {
                window.paint_path(path, color);
            }
            if animate {
                window.request_animation_frame();
            }
        },
    )
    .size(super::rem_px(18.0))
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_rotation_is_350_degrees_per_second_and_wraps_after_stalls() {
        assert_eq!(angle_degrees(0.0), 0.0);
        assert_eq!(angle_degrees(1.0), 350.0);
        assert_eq!(angle_degrees(2.0), 340.0);
        assert_eq!(angle_degrees(360.0), 0.0);
        assert!((angle_degrees(10.25) - 347.5).abs() < 0.001);
    }
}
