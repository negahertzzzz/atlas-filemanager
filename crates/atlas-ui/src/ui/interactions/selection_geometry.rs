use crate::app::GridHitModel;
use std::collections::HashSet;

/// Rows intersecting a positive-area content span; work is proportional to hits.
pub(super) fn row_span(top: f32, bottom: f32, height: f32, count: usize) -> std::ops::Range<usize> {
    if height <= 0.0 || bottom <= top {
        return 0..0;
    }
    let start = ((top / height).floor().max(0.0) as usize).min(count);
    let end = ((bottom / height).ceil().max(0.0) as usize)
        .min(count)
        .max(start);
    start..end
}

/// Full content-space hit testing, independent of the currently painted slice.
pub(super) fn grid_hits(
    model: &GridHitModel,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
) -> HashSet<usize> {
    let mut hits = HashSet::new();
    if right <= left || bottom <= top || model.pitch_x <= 0.0 || model.pitch_y <= 0.0 {
        return hits;
    }
    let (rows, cols) = if model.major_rows > 0 {
        (
            model.major_rows,
            model.total_count.div_ceil(model.major_rows),
        )
    } else {
        (
            model.total_count.div_ceil(model.cols.max(1)),
            model.cols.max(1),
        )
    };
    let first_col =
        (((left - model.pad_x - model.cell_w) / model.pitch_x).floor() + 1.0).max(0.0) as usize;
    let last_col = (((right - model.pad_x) / model.pitch_x).ceil().max(0.0) as usize).min(cols);
    let first_row = model
        .bands
        .rows_through(top - model.pad_y - model.cell_h, model.pitch_y);
    let last_row = model
        .bands
        .rows_through(bottom - model.pad_y - 0.001, model.pitch_y)
        .min(rows);
    for col in first_col..last_col {
        for row in first_row..last_row {
            let pos = if model.major_rows > 0 {
                col * model.major_rows + row
            } else {
                row * model.cols.max(1) + col
            };
            if let Some(index) = model.visible.get(pos).filter(|index| {
                pos < model.total_count && !atlas_model::grouping::is_virtual_cell(**index)
            }) {
                hits.insert(*index);
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, Bounds};
    use std::sync::Arc;

    fn model(major_rows: usize) -> GridHitModel {
        GridHitModel {
            presented: Vec::new(),
            bounds: Bounds::from_corners(point(px(0.0), px(0.0)), point(px(100.0), px(100.0))),
            pad_x: 8.0,
            pad_y: 4.0,
            scroll_x: 226.0,
            scroll_y: 0.0,
            cell_w: 220.0,
            cell_h: 24.0,
            pitch_x: 226.0,
            pitch_y: 28.0,
            cols: 3,
            major_rows,
            visible: Arc::new((100..112).collect()),
            start_idx: 4,
            end_idx: 8,
            total_count: 12,
            bands: Default::default(),
        }
    }

    #[test]
    fn row_ranges_clamp_and_exclude_edge_only_contact() {
        assert_eq!(row_span(24.0, 48.0, 24.0, 100), 1..2);
        assert_eq!(row_span(-48.0, 1.0, 24.0, 100), 0..1);
        assert_eq!(row_span(2400.0, 4800.0, 24.0, 100), 100..100);
        assert!(row_span(24.0, 24.0, 24.0, 100).is_empty());
    }

    #[test]
    fn column_major_marquee_keeps_rows_outside_painted_slice() {
        assert_eq!(
            grid_hits(&model(4), 8.0, 680.0, 32.0, 84.0),
            HashSet::from([101, 102, 105, 106, 109, 110])
        );
    }

    #[test]
    fn gaps_and_edge_contact_do_not_select_neighbors() {
        assert!(grid_hits(&model(4), 228.0, 234.0, 4.0, 28.0).is_empty());
        assert!(grid_hits(&model(4), 8.0, 228.0, 28.0, 32.0).is_empty());
    }

    #[test]
    fn row_major_uses_filter_order_and_clamps_partial_last_row() {
        let mut grid = model(0);
        grid.total_count = 10;
        assert_eq!(
            grid_hits(&grid, -10.0, 9999.0, 88.0, 200.0),
            HashSet::from([109])
        );
    }
}
