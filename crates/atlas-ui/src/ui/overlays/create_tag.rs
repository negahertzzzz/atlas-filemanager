//! Create-tag modal (name + colour) for the active profile or global.
#![allow(clippy::too_many_lines)] // reason: cohesive overlay kept whole

use crate::app::App;
use crate::input::action::Action;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, px, AnyElement, Context, Entity, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    SharedString, Window,
};
use gpui_component::input::InputState;
use gpui_component::{h_flex, v_flex, ActiveTheme};

impl App {
    pub(crate) fn create_tag_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let initial = self.state.profile_ui.new_tag_name.clone();
        self.create_tag_input
            .get_or_insert_with(|| {
                crate::ui::text_input::single_line(
                    window,
                    cx,
                    rust_i18n::t!("tag_name_placeholder").to_string(),
                    initial,
                )
            })
            .clone()
    }

    pub(crate) fn create_tag_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        self.create_tag_input(window, cx).read(cx).focus_handle(cx)
    }

    pub(crate) fn render_create_tag(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (popover, fg, muted, border, accent, accent_fg, danger) = {
            let t = cx.theme();
            (
                t.popover,
                t.popover_foreground,
                t.muted_foreground,
                t.border,
                t.accent,
                t.accent_foreground,
                t.danger,
            )
        };
        let view = cx.entity();
        let focus = self.overlay_focus(cx);
        let global = self.state.profile_ui.new_tag_global;
        let editing = self.state.profile_ui.editing_tag.is_some();
        let input = self.create_tag_input(window, cx);
        let name = input.read(cx).value().to_string();
        let selected_color = self.state.profile_ui.new_tag_color;
        let can_create = !name.trim().is_empty();
        let title = if global {
            rust_i18n::t!("tag_title_global").to_string()
        } else {
            rust_i18n::t!("tag_title_profile").to_string()
        };
        let title = if editing {
            rust_i18n::t!("tag_rename_title").to_string()
        } else {
            title
        };
        let scope = if editing {
            rust_i18n::t!("tag_rename_scope").to_string()
        } else if global {
            rust_i18n::t!("tag_scope_global").to_string()
        } else {
            rust_i18n::t!("tag_scope_profile").to_string()
        };

        let color_buttons = atlas_model::profiles::PRESET_COLORS
            .iter()
            .enumerate()
            .map(|(idx, color)| {
                let color_value = *color;
                let rgb = gpui::rgb(
                    (u32::from(color[0]) << 16) | (u32::from(color[1]) << 8) | u32::from(color[2]),
                );
                let selected = color_value == selected_color;
                let color_view = view.clone();
                div()
                    .id(SharedString::from(format!("tag-color-{idx}")))
                    .size(px(22.))
                    .rounded_full()
                    .border_2()
                    .border_color(if selected { accent } else { border })
                    .bg(rgb)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                        color_view.update(cx, |this, cx| {
                            this.state.profile_ui.new_tag_color = color_value;
                            cx.notify();
                        });
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let cancel_view = view.clone();
        let create_view = view.clone();
        let panel = v_flex()
            .w(px(360.))
            .bg(popover)
            .text_color(fg)
            .border_1()
            .border_color(border)
            .rounded_lg()
            .shadow_lg()
            .occlude()
            .p_4()
            .gap_3()
            .track_focus(&focus)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                this.handle_create_tag_key(ev, cx);
                cx.notify();
            }))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(div().text_sm().text_color(muted).child(scope))
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(rust_i18n::t!("tag_name").to_string()),
                    )
                    .child(crate::ui::text_input::bordered(&input)),
            )
            .when(!editing, |panel| {
                panel.child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted)
                                .child(rust_i18n::t!("tag_color").to_string()),
                        )
                        .child(h_flex().gap_2().children(color_buttons)),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .items_center()
                    .child(h_flex().when(editing, |row| {
                        let delete_view = view.clone();
                        let tag_id = self.state.profile_ui.editing_tag;
                        row.child(
                            div()
                                .id("create-tag-delete")
                                .px_3()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(danger.opacity(0.4))
                                .text_color(danger)
                                .cursor_pointer()
                                .hover(move |h| h.bg(danger.opacity(0.12)))
                                .child(rust_i18n::t!("tag_delete").to_string())
                                .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                                    if let Some(tag) = tag_id {
                                        delete_view.update(cx, |this, cx| {
                                            this.process_actions(vec![
                                                Action::DeleteTag(tag),
                                                Action::CloseCreateTagOverlay,
                                            ]);
                                            cx.notify();
                                        });
                                    }
                                }),
                        )
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .id("create-tag-cancel")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(border)
                                    .cursor_pointer()
                                    .hover(|h| h.bg(border))
                                    .child(rust_i18n::t!("tag_cancel").to_string())
                                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                                        cancel_view.update(cx, |this, cx| {
                                            this.process_actions(vec![
                                                Action::CloseCreateTagOverlay,
                                            ]);
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .id("create-tag-confirm")
                                    .px_3()
                                    .py_1()
                                    .rounded_md()
                                    .border_1()
                                    .border_color(if can_create { accent } else { border })
                                    .when(can_create, |s| s.bg(accent).text_color(accent_fg))
                                    .when(!can_create, |s| s.text_color(muted))
                                    .cursor_pointer()
                                    .child(if editing {
                                        rust_i18n::t!("tag_save").to_string()
                                    } else {
                                        rust_i18n::t!("tag_create").to_string()
                                    })
                                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                                        create_view.update(cx, |this, cx| {
                                            this.submit_create_tag(cx);
                                            cx.notify();
                                        });
                                    }),
                            ),
                    ),
            );

        self.overlay_scrim_custom(
            "scrim-create-tag",
            view,
            |this| this.process_actions(vec![Action::CloseCreateTagOverlay]),
            focus,
            panel.into_any_element(),
            cx,
        )
    }

    fn handle_create_tag_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "escape" => {
                self.process_actions(vec![Action::CloseCreateTagOverlay]);
                cx.stop_propagation();
            }
            "enter" => {
                self.submit_create_tag(cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn submit_create_tag(&mut self, cx: &mut Context<Self>) {
        let name = self
            .create_tag_input
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_else(|| self.state.profile_ui.new_tag_name.trim().to_string());
        if name.is_empty() {
            return;
        }
        let color = self.state.profile_ui.new_tag_color;
        if let Some(tag) = self.state.profile_ui.editing_tag {
            self.process_actions(vec![
                Action::RenameTag { tag, name },
                Action::CloseCreateTagOverlay,
            ]);
            return;
        }
        let profile = if self.state.profile_ui.new_tag_global {
            None
        } else {
            Some(self.state.profiles.active().name.clone())
        };
        self.process_actions(vec![
            Action::CreateTag {
                name,
                color,
                profile,
            },
            Action::CloseCreateTagOverlay,
        ]);
    }
}
