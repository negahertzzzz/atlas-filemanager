//! Group divider shared by every DG view: sidebar chevron, label, count, hairline.
//! Headers are few, so they are plain absolutely-placed elements over the canvas;
//! the canvas only reserves their cells.
use crate::app::App;
use crate::input::action::Action;
use atlas_model::{grouping::GroupHeader, pane::TabId};
use gpui::{
    div, prelude::*, px, AnyElement, ColorExt, Entity, FontWeight, Hsla, MouseButton, Pixels,
    SharedString,
};
use gpui_component::h_flex;

/// Height of a header band in the wrapped Grid, before zoom.
pub(crate) const GROUP_BAND_H: f32 = 30.0;

#[derive(Clone, Copy)]
pub(crate) struct GroupHeaderColors {
    pub fg: Hsla,
    pub muted: Hsla,
    pub border: Hsla,
}

/// `left/top/width/height` are content-space pixels already scaled by zoom;
/// no width means the header runs to the right edge.
pub(crate) fn group_header(
    pane: TabId,
    header: &GroupHeader,
    view: &Entity<App>,
    colors: GroupHeaderColors,
    (left, top, width, height): (Pixels, Pixels, Option<Pixels>, Pixels),
) -> AnyElement {
    let id = header.id.clone();
    let toggle = view.clone();
    let dim = colors.muted;
    let label_text = if header.label.starts_with("group_") || header.label.starts_with("file_kind_")
    {
        rust_i18n::t!(&header.label).into_owned()
    } else {
        header.label.clone()
    };
    h_flex()
        .id(SharedString::from(format!(
            "group-header-{}-{}",
            pane.0, header.id
        )))
        .absolute()
        .left(left)
        .top(top)
        .h(height)
        .map(|el| match width {
            Some(width) => el.w(width),
            None => el.right_0(),
        })
        .px(crate::ui::rem_px(10.0))
        .gap(crate::ui::rem_px(6.0))
        .items_center()
        .cursor_pointer()
        // Blocks clicks to the canvas below (no select/rubber-band) but lets the wheel through.
        .block_mouse_except_scroll()
        .text_color(dim)
        .hover(|el| el.text_color(colors.fg))
        .child(crate::ui::reveal::chevron(
            SharedString::from(format!("group-chevron-{}-{}", pane.0, header.id)),
            !header.collapsed,
            dim,
        ))
        .child(
            div()
                .flex_none()
                .font_weight(FontWeight::MEDIUM)
                .child(SharedString::from(label_text)),
        )
        .child(
            // Quiet pill: no parentheses, a whisper of tint, smaller type.
            div()
                .flex_none()
                .px(crate::ui::rem_px(6.0))
                .h(crate::ui::rem_px(16.0))
                .flex()
                .items_center()
                .rounded_full()
                .bg(colors.fg.opacity(0.07))
                .text_size(crate::ui::rem_px(11.0))
                .line_height(crate::ui::rem_px(16.0))
                .child(SharedString::from(header.count.to_string())),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(8.0))
                .h(px(1.0))
                .bg(colors.border.opacity(0.55)),
        )
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            toggle.update(cx, |app, cx| {
                app.activate_tab(pane);
                app.process_actions(vec![Action::ToggleGroup {
                    tab: pane,
                    id: id.clone(),
                }]);
                app.flush_dirty_panes(cx);
            });
        })
        .into_any_element()
}
