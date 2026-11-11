//! Add Cloud Drive modal — service picker, form fields, rclone download progress.
//!
//! Keyboard: name input gets focus via `overlay_focus_target` → `cloud_add_focus`.
//! On submit, input values are synced into `AddRemoteForm` then handed to the cloud
//! worker through `CloudAction::SubmitAddRemote`.

use crate::app::cloud::CloudAction;
use crate::app::App;
use atlas_core::cloud::AccessKind;
use atlas_model::cloud_state::AddRemoteKind;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, px, AnyElement, Context, Entity, FocusHandle, Focusable, KeyDownEvent, MouseButton,
    SharedString, Window,
};
use gpui_component::input::InputState;
use gpui_component::{h_flex, v_flex, ActiveTheme};

/// Native text inputs owned by the add-remote dialog for the lifetime of its open state.
pub(crate) struct CloudAddInputs {
    pub name: Entity<InputState>,
    pub host: Entity<InputState>,
    pub port: Entity<InputState>,
    pub user: Entity<InputState>,
    pub password: Entity<InputState>,
}

impl App {
    /// Ensure the dialog inputs exist and return the name input (primary focus target).
    pub(crate) fn cloud_add_name_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if self.cloud_add_inputs.is_none() {
            self.cloud_add_inputs = Some(CloudAddInputs {
                name: crate::ui::text_input::single_line(
                    window,
                    cx,
                    rust_i18n::t!("cloud_field_name").as_ref(),
                    "",
                ),
                host: crate::ui::text_input::single_line(
                    window,
                    cx,
                    rust_i18n::t!("cloud_field_host").as_ref(),
                    "",
                ),
                port: crate::ui::text_input::single_line(window, cx, "22", ""),
                user: crate::ui::text_input::single_line(
                    window,
                    cx,
                    rust_i18n::t!("cloud_field_user").as_ref(),
                    "",
                ),
                password: crate::ui::text_input::single_line(
                    window,
                    cx,
                    rust_i18n::t!("cloud_field_password").as_ref(),
                    "",
                ),
            });
        }
        self.cloud_add_inputs.as_ref().unwrap().name.clone()
    }

    /// Focus handle for `overlay_focus_target` — focuses the name field.
    pub(crate) fn cloud_add_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        let name = self.cloud_add_name_input(window, cx);
        // Keep whichever field the user clicked: sync_overlay_focus runs every
        // frame, so always answering "name" would steal focus from the others.
        if let Some(inputs) = &self.cloud_add_inputs {
            for input in [&inputs.host, &inputs.port, &inputs.user, &inputs.password] {
                let handle = input.read(cx).focus_handle(cx);
                if handle.is_focused(window) {
                    return handle;
                }
            }
        }
        name.read(cx).focus_handle(cx)
    }

    fn close_add_cloud(&mut self) {
        self.state.cloud.show_add_dialog = false;
        self.cloud_add_inputs = None;
        self.state.cloud.last_error = None;
    }

    fn submit_add_cloud(&mut self, cx: &mut Context<Self>) {
        if let Some(ref inputs) = self.cloud_add_inputs {
            self.state.cloud.add_form.name = inputs.name.read(cx).value().to_string();
            self.state.cloud.add_form.host = inputs.host.read(cx).value().to_string();
            self.state.cloud.add_form.port = inputs.port.read(cx).value().to_string();
            self.state.cloud.add_form.user = inputs.user.read(cx).value().to_string();
            self.state.cloud.add_form.password = inputs.password.read(cx).value().to_string();
        }
        self.cloud_add_inputs = None;
        self.handle_cloud_action(CloudAction::SubmitAddRemote);
    }

    pub(crate) fn render_add_cloud(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.theme();
        let (popover, fg, muted, border, accent, accent_fg, danger) = (
            t.popover,
            t.popover_foreground,
            t.muted_foreground,
            t.border,
            t.accent,
            t.accent_foreground,
            t.danger,
        );
        let view = cx.entity();
        let focus = self.overlay_focus(cx);

        let name_input = self.cloud_add_name_input(window, cx);
        let inputs = self
            .cloud_add_inputs
            .as_ref()
            .expect("initialized by cloud_add_name_input");
        let host_input = inputs.host.clone();
        let port_input = inputs.port.clone();
        let user_input = inputs.user.clone();
        let pass_input = inputs.password.clone();

        let kind = self.state.cloud.add_form.kind;
        let is_oauth = kind.is_oauth();
        let access = self.state.cloud.add_form.access;
        let rclone_progress = self.state.cloud.rclone_progress;
        let last_error = self.state.cloud.last_error.clone();

        // ── Service picker ─────────────────────────────────────────────────────
        let kinds: &[(&str, AddRemoteKind)] = &[
            ("Google Drive", AddRemoteKind::GoogleDrive),
            ("OneDrive", AddRemoteKind::OneDrive),
            ("SFTP", AddRemoteKind::Sftp),
            ("FTP", AddRemoteKind::Ftp),
            ("WebDAV", AddRemoteKind::WebDav),
            ("SMB", AddRemoteKind::Smb),
        ];
        let service_picker = {
            let mut row = h_flex().w_full().gap_1().flex_wrap();
            for &(label, k) in kinds {
                let selected = kind == k;
                let view2 = view.clone();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("cloud-kind-{label}")))
                        .px_2()
                        .py_0p5()
                        .rounded(px(crate::ui::RADIUS))
                        .border_1()
                        .text_xs()
                        .cursor_pointer()
                        .when(selected, |d| {
                            d.border_color(accent).bg(accent).text_color(accent_fg)
                        })
                        .when(!selected, |d| {
                            d.border_color(border)
                                .text_color(muted)
                                .hover(|h| h.border_color(accent.opacity(0.5)).text_color(fg))
                        })
                        .child(label)
                        .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                            view2.update(cx, |this, cx| {
                                this.state.cloud.add_form.kind = k;
                                cx.notify();
                            });
                        }),
                );
            }
            row
        };

        // ── Download progress (shown while rclone binary is being fetched) ─────
        let progress_bar: Option<AnyElement> = rclone_progress.map(|(done, total)| {
            let frac = if total == 0 {
                0.0_f32
            } else {
                (done as f32 / total as f32).clamp(0.0, 1.0)
            };
            let pct = (frac * 100.0) as u32;
            v_flex()
                .gap_1()
                .child(div().text_xs().text_color(muted).child(format!(
                    "{} ({}%)",
                    rust_i18n::t!("cloud_downloading_rclone"),
                    pct
                )))
                .child(
                    div().w_full().h(px(4.)).rounded(px(2.)).bg(border).child(
                        div()
                            .h_full()
                            .w(gpui::relative(frac))
                            .rounded(px(2.))
                            .bg(accent),
                    ),
                )
                .into_any_element()
        });

        let native_hint = div()
            .px_3()
            .py_2()
            .rounded(px(crate::ui::RADIUS))
            .bg(border.opacity(0.18))
            .border_1()
            .border_color(border)
            .text_xs()
            .text_color(muted)
            .child(rust_i18n::t!("cloud_native_hint").to_string());

        // ── Error banner ───────────────────────────────────────────────────────
        let error_banner: Option<AnyElement> = last_error.as_deref().map(|msg| {
            div()
                .px_3()
                .py_2()
                .rounded(px(crate::ui::RADIUS))
                .bg(danger.opacity(0.1))
                .border_1()
                .border_color(danger.opacity(0.4))
                .text_xs()
                .text_color(danger)
                .child(msg.to_string())
                .into_any_element()
        });

        // ── Form fields (hidden for OAuth) ─────────────────────────────────────
        let fields: Option<AnyElement> =
            if is_oauth {
                Some(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .px_1()
                        .child(rust_i18n::t!("cloud_oauth_hint").to_string())
                        .into_any_element(),
                )
            } else {
                // Field row helper — label above bordered input
                let field = |label: String, input: &Entity<InputState>| {
                    v_flex()
                        .gap_0p5()
                        .child(div().text_xs().text_color(muted).child(label))
                        .child(crate::ui::text_input::bordered(input))
                };
                // Host + Port side by side, then user, then password
                Some(
                    v_flex()
                        .gap_2()
                        .child(field(
                            rust_i18n::t!("cloud_field_name").to_string(),
                            &name_input,
                        ))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .gap_0p5()
                                        .child(
                                            div().text_xs().text_color(muted).child(
                                                rust_i18n::t!("cloud_field_host").to_string(),
                                            ),
                                        )
                                        .child(crate::ui::text_input::bordered(&host_input)),
                                )
                                .child(
                                    v_flex()
                                        .w(px(72.))
                                        .gap_0p5()
                                        .child(
                                            div().text_xs().text_color(muted).child(
                                                rust_i18n::t!("cloud_field_port").to_string(),
                                            ),
                                        )
                                        .child(crate::ui::text_input::bordered(&port_input)),
                                ),
                        )
                        .child(field(
                            if kind == AddRemoteKind::Smb {
                                rust_i18n::t!("cloud_field_user_domain").to_string()
                            } else {
                                rust_i18n::t!("cloud_field_user").to_string()
                            },
                            &user_input,
                        ))
                        .child(field(
                            rust_i18n::t!("cloud_field_password").to_string(),
                            &pass_input,
                        ))
                        .into_any_element(),
                )
            };

        // ── For OAuth, still show a name field at the top ──────────────────────
        let oauth_name: Option<AnyElement> = if is_oauth {
            Some(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(rust_i18n::t!("cloud_field_name").to_string()),
                    )
                    .child(crate::ui::text_input::bordered(&name_input))
                    .into_any_element(),
            )
        } else {
            None
        };

        // ── Access mode toggle (only for non-OAuth) ────────────────────────────
        let access_toggle: Option<AnyElement> = if !is_oauth {
            let view_br = view.clone();
            let view_mt = view.clone();
            let browse_sel = access == AccessKind::Access;
            Some(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(rust_i18n::t!("cloud_access_label").to_string()),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                div()
                                    .id("cloud-access-browse")
                                    .px_3()
                                    .py_0p5()
                                    .rounded(px(crate::ui::RADIUS))
                                    .border_1()
                                    .text_xs()
                                    .cursor_pointer()
                                    .when(browse_sel, |d| {
                                        d.border_color(accent).bg(accent).text_color(accent_fg)
                                    })
                                    .when(!browse_sel, |d| {
                                        d.border_color(border)
                                            .text_color(muted)
                                            .hover(|h| h.border_color(accent.opacity(0.5)))
                                    })
                                    .child(rust_i18n::t!("cloud_access_browse").to_string())
                                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                                        view_br.update(cx, |this, cx| {
                                            this.state.cloud.add_form.access = AccessKind::Access;
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                div()
                                    .id("cloud-access-mount")
                                    .px_3()
                                    .py_0p5()
                                    .rounded(px(crate::ui::RADIUS))
                                    .border_1()
                                    .text_xs()
                                    .cursor_pointer()
                                    .when(!browse_sel, |d| {
                                        d.border_color(accent).bg(accent).text_color(accent_fg)
                                    })
                                    .when(browse_sel, |d| {
                                        d.border_color(border)
                                            .text_color(muted)
                                            .hover(|h| h.border_color(accent.opacity(0.5)))
                                    })
                                    .child(rust_i18n::t!("cloud_access_mount").to_string())
                                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                                        view_mt.update(cx, |this, cx| {
                                            this.state.cloud.add_form.access = AccessKind::Mount;
                                            cx.notify();
                                        });
                                    }),
                            ),
                    )
                    .into_any_element(),
            )
        } else {
            None
        };

        // ── Buttons ────────────────────────────────────────────────────────────
        let cancel_view = view.clone();
        let submit_view = view.clone();
        let busy = rclone_progress.is_some();
        let buttons = h_flex()
            .justify_end()
            .gap_2()
            .child(
                div()
                    .id("cloud-add-cancel")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .cursor_pointer()
                    .text_sm()
                    .hover(|h| h.bg(border))
                    .child(rust_i18n::t!("cloud_cancel").to_string())
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                        cancel_view.update(cx, |this, cx| {
                            this.close_add_cloud();
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .id("cloud-add-connect")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(accent)
                    .bg(accent)
                    .text_color(accent_fg)
                    .cursor_pointer()
                    .text_sm()
                    .when(busy, |d| d.opacity(0.5))
                    .when(!busy, |d| d.hover(|h| h.opacity(0.85)))
                    .child(rust_i18n::t!("cloud_connect").to_string())
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                        if !busy {
                            submit_view.update(cx, |this, cx| {
                                this.submit_add_cloud(cx);
                                cx.notify();
                            });
                        }
                    }),
            );

        // ── Panel ──────────────────────────────────────────────────────────────
        let panel = v_flex()
            .w(px(420.))
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
                match ev.keystroke.key.as_str() {
                    "escape" => {
                        this.close_add_cloud();
                        cx.stop_propagation();
                        cx.notify();
                    }
                    "enter" => {
                        this.submit_add_cloud(cx);
                        cx.stop_propagation();
                        cx.notify();
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .text_base()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(rust_i18n::t!("cloud_add_title").to_string()),
            )
            .child(service_picker)
            .child(native_hint)
            .children(oauth_name)
            .children(fields)
            .children(access_toggle)
            .children(progress_bar)
            .children(error_banner)
            .child(buttons);

        self.overlay_scrim_custom(
            "scrim-add-cloud",
            view,
            |this| this.close_add_cloud(),
            focus,
            panel.into_any_element(),
            cx,
        )
    }
}
