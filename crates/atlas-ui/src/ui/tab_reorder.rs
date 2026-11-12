//! Provisional tab order; only a drop commits the layout tree.
use atlas_model::layout_tree::TileId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelDirection {
    Left,
    Right,
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootDropEdge {
    RootTop,
    RootBottom,
    RootLeft,
    RootRight,
}

impl RootDropEdge {
    pub fn at(x: f32, y: f32, width: f32, height: f32, band: f32) -> Option<Self> {
        if x < 0.0 || y < 0.0 || x > width || y > height {
            return None;
        }
        if y < band {
            Some(Self::RootTop)
        } else if y > height - band {
            Some(Self::RootBottom)
        } else if x < band {
            Some(Self::RootLeft)
        } else if x > width - band {
            Some(Self::RootRight)
        } else {
            None
        }
    }
    pub fn vertical(self) -> bool {
        matches!(self, Self::RootTop | Self::RootBottom)
    }
    pub fn before(self) -> bool {
        matches!(self, Self::RootTop | Self::RootLeft)
    }
    pub fn preview(self, width: f32, height: f32) -> [f32; 4] {
        let w = ((width - 5.0) / 2.0).max(0.0);
        let h = ((height - 5.0) / 2.0).max(0.0);
        match self {
            Self::RootTop => [0.0, 0.0, width, h],
            Self::RootBottom => [0.0, height - h, width, h],
            Self::RootLeft => [0.0, 0.0, w, height],
            Self::RootRight => [width - w, 0.0, w, height],
        }
    }
}

/// Choose the nearest centroid in the requested half-plane. Resolve ties by ID
/// so hash-map iteration cannot make keyboard focus jump unpredictably.
pub(crate) fn spatial_neighbor(
    origin: (f32, f32),
    panes: impl IntoIterator<Item = (TileId, (f32, f32))>,
    direction: PanelDirection,
) -> Option<TileId> {
    panes
        .into_iter()
        .filter_map(|(id, (x, y))| {
            let (dx, dy) = (x - origin.0, y - origin.1);
            let forward = match direction {
                PanelDirection::Left => -dx,
                PanelDirection::Right => dx,
                PanelDirection::Above => -dy,
                PanelDirection::Below => dy,
            };
            (forward > 0.5 && dx.is_finite() && dy.is_finite()).then_some((id, dx * dx + dy * dy))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
        .map(|(id, _)| id)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TabReorderPreview {
    pub dragged: TileId,
    pub children: Vec<TileId>,
    pub boundary: usize,
    pub copying: bool,
}

pub(crate) fn projected_order(
    children: &[TileId],
    dragged: TileId,
    boundary: usize,
) -> Vec<TileId> {
    let boundary = boundary.min(children.len());
    let before = children
        .iter()
        .take(boundary)
        .filter(|id| **id == dragged)
        .count();
    let mut order: Vec<_> = children
        .iter()
        .copied()
        .filter(|id| *id != dragged)
        .collect();
    order.insert((boundary - before).min(order.len()), dragged);
    order
}

/// Fixed resting slots prevent a moving neighbour from changing the drop target
/// under a stationary pointer and oscillating the preview.
pub(crate) fn boundary_at(x: f32, widths: &[f32], pad: f32, gap: f32) -> usize {
    let mut left = pad;
    for (index, width) in widths.iter().enumerate() {
        if x < left + width * 0.5 {
            return index;
        }
        left += width + gap;
    }
    widths.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_predictions_reserve_the_actual_splitter_and_corners_have_one_target() {
        assert_eq!(
            RootDropEdge::RootLeft.preview(1005.0, 605.0),
            [0.0, 0.0, 500.0, 605.0]
        );
        assert_eq!(
            RootDropEdge::RootRight.preview(1005.0, 605.0),
            [505.0, 0.0, 500.0, 605.0]
        );
        assert_eq!(
            RootDropEdge::RootBottom.preview(1005.0, 605.0),
            [0.0, 305.0, 1005.0, 300.0]
        );
        assert_eq!(
            RootDropEdge::at(2.0, 2.0, 1005.0, 605.0, 20.0),
            Some(RootDropEdge::RootTop)
        );
        assert_eq!(RootDropEdge::at(500.0, 300.0, 1005.0, 605.0, 20.0), None);
        assert_eq!(RootDropEdge::at(-2.0, 2.0, 1005.0, 605.0, 20.0), None);
    }
    #[test]
    fn spatial_focus_uses_distance_in_the_requested_half_plane_and_stable_ties() {
        let panes = [
            (TileId(4), (100.0, -50.0)),
            (TileId(2), (100.0, 50.0)),
            (TileId(1), (-10.0, 0.0)),
        ];
        assert_eq!(
            spatial_neighbor((0.0, 0.0), panes, PanelDirection::Right),
            Some(TileId(2))
        );
        assert_eq!(
            spatial_neighbor((0.0, 0.0), panes, PanelDirection::Above),
            Some(TileId(4))
        );
        assert_eq!(
            spatial_neighbor((0.0, 0.0), panes, PanelDirection::Left),
            Some(TileId(1))
        );
        assert_eq!(
            spatial_neighbor((-20.0, 0.0), panes, PanelDirection::Left),
            None
        );
    }
    #[test]
    fn preview_supports_both_directions_and_append_without_mutating_the_source() {
        let ids = [TileId(1), TileId(2), TileId(3)];
        assert_eq!(
            projected_order(&ids, ids[0], 3),
            vec![ids[1], ids[2], ids[0]]
        );
        assert_eq!(
            projected_order(&ids, ids[2], 0),
            vec![ids[2], ids[0], ids[1]]
        );
        assert_eq!(projected_order(&ids, ids[1], 2), ids);
        assert_eq!(ids, [TileId(1), TileId(2), TileId(3)]);
        assert_eq!(
            projected_order(&ids, TileId(4), 1),
            vec![ids[0], TileId(4), ids[1], ids[2]]
        );
        assert_eq!(
            projected_order(&ids, ids[0], usize::MAX),
            vec![ids[1], ids[2], ids[0]]
        );
        assert_eq!(projected_order(&[ids[0]], ids[0], 0), vec![ids[0]]);
        assert_eq!(boundary_at(130.0, &[100.0, 160.0, 80.0], 8.0, 6.0), 1);
        assert_eq!(boundary_at(-10.0, &[100.0], 8.0, 6.0), 0);
        assert_eq!(boundary_at(999.0, &[100.0], 8.0, 6.0), 1);
    }
}
