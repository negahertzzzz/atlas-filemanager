//! Digital Grove result list for the unified palettes (`PaletteListElement`).
//!
//! The palette shell (search row, input, footer, key model, popover/glass
//! material) is shared with the legacy renderer, and every row is the very same
//! gpui element the legacy list builds (`palette_item_row` / `palette_section_header`),
//! so icons, badges, tooltips, hover and focus behave identically. What this element
//! changes is *where* rows are laid out:
//!
//! 1. It is the single child of a tracked `overflow_y_scroll` div, so wheel input,
//!    smooth scrolling and the scrollbar are the global ones (`palette_scroll` is a
//!    `ScrollHandle` fed by `scroll_smooth_params`). The element only reports its
//!    full content height.
//! 2. Row positions are plain arithmetic over the known row heights; only the rows
//!    intersecting the live viewport (plus overscan) are prepainted and painted.
//!    The span is computed in prepaint from the live offset, so smooth scrolling
//!    never shows an empty strip (see `dg::scroll::live_span`).

use gpui::{
    point, px, relative, size, AnyElement, App as GpuiApp, AvailableSpace, Bounds, Element,
    ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, ScrollHandle,
    Style, Window,
};
use std::ops::Range;

/// One result row: its element and the exact height (px) the element is built with.
pub(crate) struct PaletteListRow {
    pub height: f32,
    pub element: AnyElement,
}

pub(crate) struct PaletteListElement {
    id: ElementId,
    scroll: ScrollHandle,
    rows: Vec<PaletteListRow>,
    /// Top of each row relative to the element origin (includes `pad`).
    tops: Vec<f32>,
    pad: f32,
    total: f32,
    overscan: f32,
    fallback_viewport: f32,
}

pub(crate) struct PaletteListPrepaint {
    visible: Range<usize>,
}

/// Cumulative row tops for `heights` after `pad`, and the total content height
/// (padding on both ends included).
pub(crate) fn row_tops(heights: &[f32], pad: f32) -> (Vec<f32>, f32) {
    let mut y = pad;
    let mut tops = Vec::with_capacity(heights.len());
    for h in heights {
        tops.push(y);
        y += h;
    }
    (tops, y + pad)
}

/// Rows intersecting `[from, to)`. Rows are contiguous, so both edges bisect.
pub(crate) fn visible_rows(tops: &[f32], heights: &[f32], from: f32, to: f32) -> Range<usize> {
    let (mut lo, mut hi) = (0, tops.len());
    while lo < hi {
        let mid = (lo + hi) / 2;
        if tops[mid] + heights[mid] <= from {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let end = tops.partition_point(|top| *top < to).max(lo);
    lo..end
}

impl PaletteListElement {
    /// `pad` is the inset (px) above the first row, below the last one and left of
    /// every row; `overscan` extends the realised span past the viewport (px).
    pub(crate) fn new(
        id: impl Into<ElementId>,
        scroll: ScrollHandle,
        rows: Vec<PaletteListRow>,
        pad: f32,
        overscan: f32,
        fallback_viewport: f32,
    ) -> Self {
        let heights: Vec<f32> = rows.iter().map(|row| row.height).collect();
        let (tops, total) = row_tops(&heights, pad);
        Self {
            id: id.into(),
            scroll,
            rows,
            tops,
            pad,
            total,
            overscan,
            fallback_viewport,
        }
    }

    pub(crate) fn content_height(&self) -> f32 {
        self.total
    }
}

impl IntoElement for PaletteListElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PaletteListElement {
    type RequestLayoutState = ();
    type PrepaintState = PaletteListPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) -> (LayoutId, Self::RequestLayoutState) {
        crate::perf::count("palette_dg.request_layout");
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = px(self.total).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) -> Self::PrepaintState {
        crate::perf::count("palette_dg.prepaint");
        let tracked = f32::from(self.scroll.bounds().size.height);
        let viewport = if tracked > 1.0 {
            tracked
        } else {
            self.fallback_viewport
        };
        let scroll_y = (-f32::from(self.scroll.offset().y)).max(0.0);
        let heights: Vec<f32> = self.rows.iter().map(|row| row.height).collect();
        let visible = visible_rows(
            &self.tops,
            &heights,
            scroll_y - self.overscan,
            scroll_y + viewport + self.overscan,
        );
        let width = (f32::from(bounds.size.width) - self.pad).max(1.0);
        for index in visible.clone() {
            let row = &mut self.rows[index];
            row.element.prepaint_as_root(
                point(
                    bounds.origin.x + px(self.pad),
                    bounds.origin.y + px(self.tops[index]),
                ),
                size(
                    AvailableSpace::Definite(px(width)),
                    AvailableSpace::Definite(px(row.height)),
                ),
                window,
                cx,
            );
        }
        PaletteListPrepaint { visible }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) {
        crate::perf::count("palette_dg.paint");
        for index in prepaint.visible.clone() {
            self.rows[index].element.paint(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_tops_accumulate_variable_heights_with_padding() {
        let (tops, total) = row_tops(&[32.0, 36.0, 56.0], 4.0);
        assert_eq!(tops, vec![4.0, 36.0, 72.0]);
        assert_eq!(total, 132.0);
        assert_eq!(row_tops(&[], 4.0), (vec![], 8.0));
    }

    #[test]
    fn visible_rows_cover_the_viewport_for_variable_heights() {
        let heights = [32.0, 36.0, 36.0, 56.0, 36.0, 36.0];
        let (tops, _) = row_tops(&heights, 4.0);
        // Viewport [0, 40): header + the row it overlaps.
        assert_eq!(visible_rows(&tops, &heights, 0.0, 40.0), 0..2);
        // A row ending exactly at `from` is excluded; one starting at `to` too.
        assert_eq!(visible_rows(&tops, &heights, 36.0, 72.0), 1..2);
        // Deep scroll keeps only the intersecting rows.
        assert_eq!(visible_rows(&tops, &heights, 100.0, 150.0), 2..4);
        // Past the end / empty.
        assert_eq!(visible_rows(&tops, &heights, 900.0, 1000.0), 6..6);
        assert_eq!(visible_rows(&[], &[], 0.0, 100.0), 0..0);
    }

    #[test]
    fn visible_rows_never_skip_a_row_that_intersects() {
        let heights: Vec<f32> = (0..200)
            .map(|i| if i % 7 == 0 { 32.0 } else { 36.0 })
            .collect();
        let (tops, _) = row_tops(&heights, 4.0);
        for from in [0.0, 13.0, 777.0, 3_001.0, 7_000.0] {
            let to = from + 600.0;
            let range = visible_rows(&tops, &heights, from, to);
            for (i, (top, h)) in tops.iter().zip(&heights).enumerate() {
                let intersects = *top < to && top + h > from;
                assert_eq!(range.contains(&i), intersects, "row {i} at viewport {from}");
            }
        }
    }
}
