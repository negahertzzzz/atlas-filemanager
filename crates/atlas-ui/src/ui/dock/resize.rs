use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{div, px, AnyElement, Entity, MouseButton};
use gpui_component::ActiveTheme as _;

use crate::app::{App, DockResizeState};
use atlas_model::layout_tree::LinearDir;
use atlas_model::layout_tree::TileId;

/// Render a splitter at a caller-selected thickness. Pane close-out uses this
/// to collapse one of the two adjacent seams together with the disappearing
/// pane, so tree removal does not expose a transient background gap.
pub(crate) fn splitter(
    container: TileId,
    divider: usize,
    dir: LinearDir,
    view: &Entity<App>,
    cx: &mut gpui::App,
    thickness: f32,
) -> AnyElement {
    // The seam is empty space; its hairline only appears while hovered so the
    // resize affordance is discoverable without drawing lines between islands.
    let border = cx.theme().border.opacity(0.9);
    let view = view.clone();
    let thickness = thickness.max(0.0);
    let line = thickness.min(1.0);
    let margin = (thickness - line) * 0.5;
    let el = match dir {
        LinearDir::Horizontal => div()
            .group("dock-splitter")
            .w(px(thickness))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .child(
                div()
                    .w(px(line))
                    .h_full()
                    .mx(px(margin))
                    .group_hover("dock-splitter", |s| s.bg(border)),
            ),
        LinearDir::Vertical => div()
            .group("dock-splitter")
            .h(px(thickness))
            .w_full()
            .flex_none()
            .cursor_row_resize()
            .child(
                div()
                    .h(px(line))
                    .w_full()
                    .my(px(margin))
                    .group_hover("dock-splitter", |s| s.bg(border)),
            ),
    };
    el.on_mouse_down(MouseButton::Left, move |ev, _window, cx| {
        view.update(cx, |this, _cx| {
            let Some(bounds) = this.dock_linear_bounds.get(&container).copied() else {
                return;
            };
            let Some(atlas_model::layout_tree::Tile::Container(
                atlas_model::layout_tree::Container::Linear(linear),
            )) = this.state.pane_tree.tiles.get(container)
            else {
                return;
            };
            let span_px = match dir {
                LinearDir::Horizontal => f32::from(bounds.size.width),
                LinearDir::Vertical => f32::from(bounds.size.height),
            };
            this.dock_resize = Some(DockResizeState {
                container,
                divider,
                dir,
                start_pos: match dir {
                    LinearDir::Horizontal => f32::from(ev.position.x),
                    LinearDir::Vertical => f32::from(ev.position.y),
                },
                span_px,
                start_shares: linear.shares.clone(),
            });
        });
        cx.stop_propagation();
    })
    .into_any_element()
}
