//! Size-filter overlay for the Details view.

use crate::app::App;
use crate::ui::icons::{icon, AtlasIcon};
use atlas_model::file_entry::{FileEntry, FileKind};
use atlas_model::pane::TabId;
use gpui::prelude::*;
use gpui::AnimationExt as _;
use gpui::{div, px, AnyElement, Context, MouseButton, Pixels, Point, SharedString, Window};
use gpui_component::StyledExt as _;
use gpui_component::{v_flex, ActiveTheme, Sizable as _};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub(crate) enum SizeFilter {
    Folders,
    Unknown,
    Empty,
    Tiny,
    Small,
    Medium,
    Large,
    Huge,
}

impl SizeFilter {
    pub(crate) const ALL: [Self; 8] = [
        Self::Folders,
        Self::Unknown,
        Self::Empty,
        Self::Tiny,
        Self::Small,
        Self::Medium,
        Self::Large,
        Self::Huge,
    ];

    pub(crate) fn label(self) -> String {
        match self {
            Self::Folders => rust_i18n::t!("size_folders").into_owned(),
            Self::Unknown => rust_i18n::t!("size_unknown").into_owned(),
            Self::Empty => rust_i18n::t!("size_empty").into_owned(),
            Self::Tiny => "< 10 KB".to_string(),
            Self::Small => "10 KB - 1 MB".to_string(),
            Self::Medium => "1 MB - 100 MB".to_string(),
            Self::Large => "100 MB - 1 GB".to_string(),
            Self::Huge => "> 1 GB".to_string(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn matches(self, entry: &FileEntry) -> bool {
        if matches!(entry.kind(), FileKind::Dir) {
            return self == Self::Folders;
        }
        match (self, entry.size()) {
            (Self::Unknown, None) => true,
            (Self::Empty, Some(0)) => true,
            (Self::Tiny, Some(1..=10_239)) => true,
            (Self::Small, Some(10_240..=1_048_575)) => true,
            (Self::Medium, Some(1_048_576..=104_857_599)) => true,
            (Self::Large, Some(104_857_600..=1_073_741_823)) => true,
            (Self::Huge, Some(1_073_741_824..)) => true,
            _ => false,
        }
    }
}

pub(crate) struct SizeFilterMenuState {
    pub pane: TabId,
    pub anchor: Point<Pixels>,
}

impl App {
    #[allow(dead_code)]
    pub(crate) fn open_size_filter_menu(
        &mut self,
        pane: TabId,
        anchor: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.size_filter_menu = Some(SizeFilterMenuState { pane, anchor });
        cx.notify();
    }

    pub(crate) fn close_size_filter_menu(&mut self, cx: &mut Context<Self>) {
        self.size_filter_menu = None;
        cx.notify();
    }

    pub(crate) fn render_size_filter_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(state) = self.size_filter_menu.as_ref() else {
            return div().into_any_element();
        };
        let pane = state.pane;
        let anchor = state.anchor;

        let (fg, muted, border, accent, accent_fg) = {
            let t = cx.theme();
            (
                t.popover_foreground,
                t.muted_foreground,
                t.border,
                t.accent,
                t.accent_foreground,
            )
        };

        let active = self.size_filters.get(&pane).cloned().unwrap_or_default();
        let win = window.bounds().size;
        const W: f32 = 220.;
        let panel_h = 306.0_f32;
        let ax = f32::from(anchor.x);
        let ay = f32::from(anchor.y);
        let ww = f32::from(win.width);
        let wh = f32::from(win.height);
        let left = ax.min((ww - W - 6.0).max(6.0)).max(6.0);
        let top = ay.min((wh - panel_h - 6.0).max(6.0)).max(6.0);
        let view = cx.entity();

        let mut rows: Vec<AnyElement> = Vec::new();
        let all_checked = active.is_empty();
        let v_all = view.clone();
        rows.push(
            size_filter_row(
                SharedString::from("sf-all"),
                &rust_i18n::t!("all_sizes"),
                all_checked,
                fg,
                muted,
                accent,
                accent_fg,
            )
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                v_all.update(cx, |this, cx| {
                    this.size_filters.remove(&pane);
                    this.close_size_filter_menu(cx);
                });
            })
            .into_any_element(),
        );

        rows.push(
            div()
                .h(px(1.))
                .w_full()
                .my(px(2.))
                .bg(border)
                .into_any_element(),
        );

        for bucket in SizeFilter::ALL {
            let checked = active.contains(&bucket);
            let v = view.clone();
            rows.push(
                size_filter_row(
                    SharedString::from(format!("sf-{:?}", bucket)),
                    &bucket.label(),
                    checked,
                    fg,
                    muted,
                    accent,
                    accent_fg,
                )
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    v.update(cx, |this, cx| {
                        let set = this.size_filters.entry(pane).or_default();
                        if !set.insert(bucket) {
                            set.remove(&bucket);
                            if set.is_empty() {
                                this.size_filters.remove(&pane);
                            }
                        }
                        cx.notify();
                    });
                })
                .into_any_element(),
            );
        }

        let close_view = view;
        div()
            .absolute()
            .inset_0()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                close_view.update(cx, |this, cx| {
                    this.close_size_filter_menu(cx);
                });
            })
            .child(
                v_flex()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(W))
                    .h(px(panel_h))
                    .popover_style(cx)
                    .py_1()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .children(rows)
                    .with_animation(
                        "size-filter-appear",
                        gpui_component::animation::dropdown_animation(),
                        gpui_component::animation::dropdown_appear,
                    ),
            )
            .into_any_element()
    }
}

fn size_filter_row(
    id: SharedString,
    label: &str,
    checked: bool,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    accent_fg: gpui::Hsla,
) -> gpui::Stateful<gpui::Div> {
    let (bg, text, check_color) = if checked {
        (accent, accent_fg, accent_fg)
    } else {
        (gpui::transparent_black(), fg, muted)
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .h(px(28.))
        .bg(bg)
        .text_color(text)
        .text_sm()
        .rounded(px(4.))
        .hover(|s| s.opacity(0.85))
        .cursor_pointer()
        .child(icon(
            AtlasIcon::HardDrives,
            14.0,
            if checked { check_color } else { muted },
        ))
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .child(SharedString::from(label.to_string())),
        )
        .when(checked, |el| {
            el.child(
                gpui_component::Icon::new(gpui_component::IconName::Check)
                    .with_size(gpui_component::Size::XSmall)
                    .text_color(check_color),
            )
        })
}
