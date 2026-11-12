//! Digital Grove Details policy, sharing Atlas's proven leaf canvas renderer.
//! The model owns preferences; header, rows, rename and input share this projection.
//! Resize follows the pointer directly. Reordering uses retained native springs.

use crate::app::{App, ColWidths, DetailsColumn};
use crate::ui::dg::table::{fit_columns, ColumnSpec};
use crate::ui::views::details::{DetailsColumnLayout, DetailsRowLayout, CELL_PAD, ROW_NAME_X};
use atlas_model::clamp::ClampLenient;
use atlas_model::pane::TabId;
use gpui::{AnyElement, Context, SharedString, Window};
use std::time::Instant;

fn column_index(column: DetailsColumn) -> usize {
    match column {
        DetailsColumn::Size => 0,
        DetailsColumn::Modified => 1,
        DetailsColumn::Type => 2,
    }
}

pub(crate) fn responsive_layout(
    available: f32,
    zoom: f32,
    preferred: ColWidths,
    order: &[DetailsColumn; 3],
) -> DetailsRowLayout {
    let zoom = if zoom.is_finite() && zoom > 0.0 {
        zoom
    } else {
        1.0
    };
    let preferred = preferred.projected(available);
    let [name, size, modified, type_w] = fit_columns(
        available,
        [
            ColumnSpec {
                preferred: preferred.name,
                minimum: 120.0 * zoom,
                priority: 3,
            },
            ColumnSpec {
                preferred: preferred.size,
                minimum: 46.0 * zoom,
                priority: 2,
            },
            ColumnSpec {
                preferred: preferred.modified,
                minimum: 92.0 * zoom,
                priority: 0,
            },
            ColumnSpec {
                preferred: preferred.type_w,
                minimum: 56.0 * zoom,
                priority: 1,
            },
            // A wide pane preserves the user's anchored widths, as the original did.
            // Spare room is trailing space, so dragging Name's divider stays effective.
        ],
        4,
    );
    let mut x = name;
    let mut columns = Vec::with_capacity(3);
    for &column in order {
        let w = match column {
            DetailsColumn::Size => size,
            DetailsColumn::Modified => modified,
            DetailsColumn::Type => type_w,
        };
        if w > 0.0 {
            columns.push(DetailsColumnLayout { column, x, w });
        }
        x += w;
    }
    let name_x = ROW_NAME_X * zoom;
    DetailsRowLayout {
        name_x,
        name_col_w: (name - name_x - CELL_PAD * zoom).max(0.0),
        name_right: name,
        columns,
    }
}

struct ColumnMotion {
    positions: [crate::ui::dg::motion::RetainedSpring; 3],
    available: f32,
}

pub(crate) fn presented_layout(
    app: &App,
    pane: TabId,
    available: f32,
    zoom: f32,
    window: &mut Window,
    cx: &mut gpui::App,
) -> (DetailsRowLayout, DetailsRowLayout) {
    let _perf = crate::perf::span("details.layout.columns");
    let prefs = app.details_prefs_for(pane);
    let mut layout = responsive_layout(available, zoom, prefs.widths, &prefs.order);
    let state = window.use_keyed_state(
        SharedString::from(format!("details-column-motion-{}", pane.0)),
        cx,
        |_, _| ColumnMotion {
            positions: std::array::from_fn(|i| {
                let x = layout
                    .columns
                    .iter()
                    .find(|c| column_index(c.column) == i)
                    .map_or(layout.name_right, |c| c.x);
                crate::ui::dg::motion::RetainedSpring::new(x, Instant::now())
            }),
            available,
        },
    );
    // The header follows the pointer directly; the rows chase it on the spring.
    let mut header = layout.clone();
    let active = state.update(cx, |state, _| {
        let now = Instant::now();
        let direct = app.state.settings.disable_animations
            || app.col_resize.is_some()
            || (state.available - available).abs() > 0.01;
        let mut active = false;
        for i in 0..3 {
            let mut target = layout
                .columns
                .iter()
                .find(|c| column_index(c.column) == i)
                .map_or(layout.name_right, |c| c.x);
            let position = &mut state.positions[i];
            let attached = app.col_drag_pane == Some(pane)
                && app.col_dragging.is_some_and(|c| column_index(c) == i);
            if attached {
                let width = layout
                    .columns
                    .iter()
                    .find(|c| column_index(c.column) == i)
                    .map_or(0.0, |c| c.w);
                target = (app.col_drag_x - app.col_drag_grab_x).clamp_lenient(
                    layout.name_right,
                    (available - width).max(layout.name_right),
                );
            }
            let (value, running) = position.sample(target, direct, now);
            active |= running;
            if let Some(column) = layout
                .columns
                .iter_mut()
                .find(|c| column_index(c.column) == i)
            {
                column.x = value;
            }
            if let Some(column) = header
                .columns
                .iter_mut()
                .find(|c| column_index(c.column) == i)
            {
                column.x = if attached { target } else { value };
            }
        }
        state.available = available;
        active
    });
    if active {
        window.request_animation_frame();
    }
    (layout, header)
}

impl App {
    pub(crate) fn render_details_view_dg(
        &mut self,
        pane_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_details_canvas(pane_id, window, cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proportions_scale_with_pane_and_survive_saved_preferences() {
        let mut widths = ColWidths::default();
        widths.resize("name", 400.0, 1300.0);
        let restored: ColWidths =
            serde_json::from_str(&serde_json::to_string(&widths).unwrap()).unwrap();
        let full = restored.projected(1300.0);
        let half = restored.projected(650.0);
        assert!((full.name - 400.0).abs() < 0.001);
        assert!((half.name - 200.0).abs() < 0.001);
        assert!((full.name + full.size + full.modified + full.type_w - 1300.0).abs() < 0.001);
        let legacy: ColWidths =
            serde_json::from_str(r#"{"name":300,"size":90,"modified":150,"type_w":110}"#).unwrap();
        assert_eq!(legacy.projected(1300.0).name, 300.0);
    }

    #[test]
    fn collapse_preserves_custom_order_and_primary_gutter() {
        let layout = responsive_layout(
            222.0,
            1.0,
            ColWidths::default(),
            &[
                DetailsColumn::Type,
                DetailsColumn::Modified,
                DetailsColumn::Size,
            ],
        );
        assert_eq!(layout.name_right, 120.0);
        assert_eq!(layout.columns.len(), 2);
        assert_eq!(layout.columns[0].column, DetailsColumn::Type);
        assert_eq!(layout.columns[1].column, DetailsColumn::Size);
        assert_eq!(layout.columns[1].x + layout.columns[1].w, 222.0);
        assert_eq!(
            responsive_layout(
                10.0,
                1.0,
                ColWidths::default(),
                &DetailsColumn::DEFAULT_ORDER
            )
            .name_col_w,
            0.0
        );
    }
}
