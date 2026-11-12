//! Scroll arithmetic shared by the canvas file views.
//!
//! The DG surfaces are not `uniform_list`s, so gpui never consumes the deferred
//! `UniformListScrollHandle::scroll_to_item` request for them. Each surface calls
//! [`apply_deferred`] once per frame with its own geometry instead.
use atlas_model::clamp::ClampLenient;
use gpui::{px, ScrollStrategy, UniformListScrollHandle};

/// Items `[start, end)` intersecting the clipped span `[visible_start, visible_end)`
/// of the scrolled content, widened by `overscan` items on both sides.
///
/// Called from prepaint: the parent scroller has already applied this frame's
/// offset, so the painted range never lags one frame behind smooth scrolling.
pub(crate) fn live_span(
    visible_start: f32,
    visible_end: f32,
    pitch: f32,
    overscan: usize,
    count: usize,
) -> (usize, usize) {
    if count == 0 || pitch <= 0.0 || visible_end <= visible_start {
        return (0, 0);
    }
    let first = (visible_start.max(0.0) / pitch).floor() as usize;
    let last = (visible_end.max(0.0) / pitch).ceil() as usize;
    let start = first.saturating_sub(overscan).min(count);
    let end = last.saturating_add(overscan).min(count).max(start);
    (start, end)
}

/// One-axis scroll target for an item spanning `[item_start, item_start + item_len)`.
pub(crate) fn axis_target(
    item_start: f32,
    item_len: f32,
    viewport: f32,
    current: f32,
    max: f32,
    strategy: ScrollStrategy,
) -> f32 {
    let item_end = item_start + item_len;
    match strategy {
        ScrollStrategy::Top => item_start,
        ScrollStrategy::Center => item_start - (viewport - item_len) * 0.5,
        ScrollStrategy::Bottom => item_end - viewport,
        ScrollStrategy::Nearest => {
            if item_start < current {
                item_start
            } else if item_end > current + viewport {
                item_end - viewport
            } else {
                current
            }
        }
    }
    .clamp_lenient(0.0, max.max(0.0))
}

/// Keyboard reveal: scroll only when the cursor would leave the viewport, keeping
/// one item of context past the edge (Finder/Explorer feel). Never recenters.
pub(crate) fn keyboard_target(
    item_start: f32,
    item_len: f32,
    viewport: f32,
    current: f32,
    max: f32,
) -> f32 {
    let margin = item_len.min(((viewport - item_len) * 0.5).max(0.0));
    let item_end = item_start + item_len;
    let target = if item_start - margin < current {
        item_start - margin
    } else if item_end + margin > current + viewport {
        item_end + margin - viewport
    } else {
        current
    };
    target.clamp_lenient(0.0, max.max(0.0))
}

/// Consume a pending `scroll_to_item` request. `item_extent(index)` maps the
/// requested index to `(start, len)` along the scroll axis; `vertical` selects the
/// axis. Returns true when the offset changed.
pub(crate) fn apply_deferred(
    handle: &UniformListScrollHandle,
    vertical: bool,
    viewport: f32,
    max: f32,
    item_extent: impl Fn(usize) -> Option<(f32, f32)>,
) -> bool {
    let (deferred, base) = {
        let mut state = handle.0.borrow_mut();
        let Some(deferred) = state.deferred_scroll_to_item.take() else {
            return false;
        };
        (deferred, state.base_handle.clone())
    };
    if viewport <= 1.0 {
        // Not laid out yet; retry next frame.
        handle.0.borrow_mut().deferred_scroll_to_item = Some(deferred);
        return false;
    }
    let Some((start, len)) = item_extent(deferred.item_index) else {
        return false;
    };
    let offset = base.offset();
    let current = -f32::from(if vertical { offset.y } else { offset.x });
    let target = axis_target(
        start,
        len,
        viewport,
        current.max(0.0),
        max,
        deferred.strategy,
    );
    if (target - current).abs() < 0.5 {
        return false;
    }
    base.set_offset(if vertical {
        gpui::point(offset.x, px(-target))
    } else {
        gpui::point(px(-target), offset.y)
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_span_overscans_both_edges_and_clamps() {
        assert_eq!(live_span(100.0, 300.0, 20.0, 2, 1000), (3, 17));
        assert_eq!(live_span(0.0, 300.0, 20.0, 2, 1000), (0, 17));
        assert_eq!(live_span(19_900.0, 20_100.0, 20.0, 2, 1000), (993, 1000));
        assert_eq!(live_span(0.0, 0.0, 20.0, 2, 10), (0, 0));
        assert_eq!(live_span(0.0, 100.0, 0.0, 2, 10), (0, 0));
    }

    #[test]
    fn live_span_covers_large_wheel_jumps_in_both_directions() {
        // Offsets can jump much farther than overscan between render and
        // prepaint. Every row intersecting the new viewport must be present.
        for offset in [0.0, 24_005.0, 9_217.0, 95_100.0, 52.0] {
            let (start, end) = live_span(offset, offset + 620.0, 27.0, 2, 10_000);
            let first_visible = (offset / 27.0).floor() as usize;
            let end_visible = ((offset + 620.0) / 27.0).ceil() as usize;
            assert!(start <= first_visible);
            assert!(end >= end_visible);
            assert!(end - start <= 28);
        }
    }

    #[test]
    fn keyboard_target_scrolls_minimally_with_one_item_of_context() {
        // Inside the viewport: no movement at all (no recentering).
        assert_eq!(keyboard_target(140.0, 20.0, 300.0, 0.0, 1000.0), 0.0);
        assert_eq!(keyboard_target(240.0, 20.0, 300.0, 0.0, 1000.0), 0.0);
        // Next row would touch the bottom edge: scroll just enough for one more row.
        assert_eq!(keyboard_target(280.0, 20.0, 300.0, 0.0, 1000.0), 20.0);
        // Moving up past the top edge keeps one row above visible.
        assert_eq!(keyboard_target(100.0, 20.0, 300.0, 100.0, 1000.0), 80.0);
        // Clamped to the directory edges.
        assert_eq!(keyboard_target(0.0, 20.0, 300.0, 50.0, 1000.0), 0.0);
        assert_eq!(keyboard_target(1280.0, 20.0, 300.0, 0.0, 1000.0), 1000.0);
        // Huge items never produce a negative margin.
        assert_eq!(keyboard_target(400.0, 400.0, 300.0, 0.0, 1000.0), 500.0);
    }

    #[test]
    fn axis_target_matches_scroll_strategies() {
        assert_eq!(
            axis_target(500.0, 20.0, 300.0, 0.0, 1000.0, ScrollStrategy::Top),
            500.0
        );
        assert_eq!(
            axis_target(500.0, 20.0, 300.0, 0.0, 1000.0, ScrollStrategy::Center),
            360.0
        );
        assert_eq!(
            axis_target(500.0, 20.0, 300.0, 0.0, 1000.0, ScrollStrategy::Bottom),
            220.0
        );
        assert_eq!(
            axis_target(500.0, 20.0, 300.0, 0.0, 1000.0, ScrollStrategy::Nearest),
            220.0
        );
        assert_eq!(
            axis_target(50.0, 20.0, 300.0, 100.0, 1000.0, ScrollStrategy::Nearest),
            50.0
        );
        assert_eq!(
            axis_target(150.0, 20.0, 300.0, 100.0, 1000.0, ScrollStrategy::Nearest),
            100.0
        );
    }
}
