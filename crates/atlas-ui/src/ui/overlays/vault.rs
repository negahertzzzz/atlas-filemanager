//! Private-folder surface. Uses Atlas theme/input/button primitives, never HTML.
use crate::{
    app::{
        vault::{Phase, PrivatePreview},
        App,
    },
    ui::{
        icons::{icon, AtlasIcon},
        rem_px as s,
    },
};
use gpui::{div, prelude::*, AnyElement, Context, KeyDownEvent, MouseButton, SharedString, Window};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::Input,
    v_flex, ActiveTheme, Disableable, Sizable,
};
use std::{sync::atomic::Ordering, time::Instant};

impl App {
    #[inline(never)]
    pub(crate) fn render_private_session_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        #[cfg(windows)]
        {
            let mount = self.vault_mount.as_ref()?;
            Some(
                h_flex()
                    .flex_shrink_0()
                    .gap(s(10.))
                    .px(s(14.))
                    .py(s(8.))
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .text_size(s(12.))
                    .child(icon(AtlasIcon::Lock, 14., vault_muted(cx)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(mount.name().to_owned()),
                    )
                    .child(
                        Button::new("private-session-manage")
                            .small()
                            .ghost()
                            .icon(gpui_component::IconName::Settings)
                            .tooltip(rust_i18n::t!("vault_manage").to_string())
                            .on_click(
                                cx.listener(|this, _, w, cx| this.manage_native_vault(w, cx)),
                            ),
                    )
                    .child(
                        Button::new("private-session-lock")
                            .small()
                            .primary()
                            .label(rust_i18n::t!("vault_lock").to_string())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.close_native_vault(false);
                                cx.notify();
                            })),
                    )
                    .into_any_element(),
            )
        }
        #[cfg(not(windows))]
        {
            let _ = cx;
            None
        }
    }

    #[inline(never)]
    pub(crate) fn render_locked_vault_preview(
        &self,
        path: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = vault_muted(cx);
        v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap(s(20.))
            .px(s(20.))
            .text_center()
            .child(
                gpui::img(crate::assets::VAULT_FOLDER_ART)
                    .w(s(160.))
                    .h(s(160.))
                    .max_w_full()
                    .object_fit(gpui::ObjectFit::Contain),
            )
            .child(
                v_flex()
                    .gap(s(8.))
                    .child(
                        div()
                            .text_size(s(18.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(rust_i18n::t!("vault_locked_heading").to_string()),
                    )
                    .child(
                        div()
                            .text_size(s(13.))
                            .text_color(muted)
                            .child(rust_i18n::t!("vault_locked_hint").to_string()),
                    ),
            )
            .child(
                Button::new("preview-unlock-vault")
                    .primary()
                    .large()
                    .rounded_full()
                    .bg(gpui::rgb(0xf25c35))
                    .text_color(gpui::white())
                    .label(rust_i18n::t!("vault_unlock_action").to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.try_open_vault(&path);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    pub(crate) fn render_vault(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.ensure_vault_inputs(window, cx);
        let Some(d) = self.vault.as_ref() else {
            return div().into_any_element();
        };
        let phase = d.phase;
        let busy = d.busy;
        let error = d.error.clone();
        let validation_field = error.as_deref().and_then(|e| {
            if e == rust_i18n::t!("vault_password_mismatch").as_ref() {
                Some(if d.phase == Phase::Manage { 2 } else { 1 })
            } else if e == rust_i18n::t!("vault_password_length").as_ref() {
                Some(if d.phase == Phase::Manage { 1 } else { 0 })
            } else {
                None
            }
        });
        let notice = d.notice.clone();
        let name = d
            .session
            .as_ref()
            .map(|s| s.name().to_owned())
            .unwrap_or_else(|| d.name.clone());
        let progress = d.control.completed.load(Ordering::Relaxed);
        let total = d.control.total.load(Ordering::Relaxed);
        let (surface, fg, muted, border, danger) = {
            let t = cx.theme();
            (
                t.background,
                t.popover_foreground,
                vault_muted(cx),
                t.border,
                t.danger,
            )
        };
        let focus = self.overlay_focus(cx);
        let mut body = v_flex().gap(s(20.)).p(s(32.));
        body = self.render_vault_header(body, phase, name, muted);

        if matches!(phase, Phase::Create | Phase::Unlock | Phase::Restore) {
            body = body.child(
                div().w_full().flex().justify_center().child(
                    gpui::img(crate::assets::VAULT_FOLDER_ART)
                        .size(s(if phase == Phase::Unlock { 188. } else { 100. }))
                        .object_fit(gpui::ObjectFit::Contain),
                ),
            );
        }
        if busy {
            body = body.child(
                h_flex()
                    .gap(s(10.))
                    .py(s(30.))
                    .text_color(muted)
                    .text_size(s(13.))
                    .child(crate::ui::loading_spinner::loading_spinner(
                        muted,
                        !self.state.settings.disable_animations,
                    ))
                    .child(if total > 0 && progress > 0 {
                        rust_i18n::t!(
                            "vault_progress",
                            done = atlas_core::util::display::format_size(progress),
                            total = atlas_core::util::display::format_size(total)
                        )
                        .to_string()
                    } else {
                        rust_i18n::t!("vault_working").to_string()
                    }),
            );
        } else if phase == Phase::Browse {
            body = self.render_vault_browser(body, cx);
        } else {
            body = self.render_vault_form(body, cx);
        }
        if let Some(error) = error.filter(|_| validation_field.is_none()) {
            body = body.child(div().text_size(s(12.)).text_color(danger).child(error));
        }
        if let Some(notice) = notice {
            body = body.child(div().text_size(s(12.)).text_color(muted).child(notice));
        }

        let footer = self.render_vault_footer(phase, busy, border, cx);
        let panel = v_flex()
            .id("vault-dialog")
            .w(s(if phase == Phase::Browse { 760. } else { 520. }))
            .max_w_full()
            .max_h_full()
            .bg(surface)
            .text_color(fg)
            .border_1()
            .border_color(border)
            .rounded(s(28.))
            .shadow_lg()
            .occlude()
            .track_focus(&focus)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(cx.listener(|this, _, _, _| {
                if let Some(d) = &mut this.vault {
                    d.last_activity = Instant::now();
                }
            }))
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                if let Some(d) = &mut this.vault {
                    d.last_activity = Instant::now();
                }
                match ev.keystroke.key.as_str() {
                    "escape" => {
                        this.close_vault(w, cx);
                        cx.stop_propagation();
                    }
                    "enter"
                        if this.vault.as_ref().is_some_and(|d| {
                            d.inputs.iter().any(|i| {
                                use gpui::Focusable;
                                i.read(cx).focus_handle(cx).is_focused(w)
                            })
                        }) =>
                    {
                        this.submit_vault(w, cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .id("vault-dialog-body")
                    .min_h_0()
                    .max_h(gpui::px(
                        (f32::from(window.viewport_size().height)
                            - f32::from(window.rem_size()) * 4.5
                            - 32.)
                            .max(120.),
                    ))
                    .overflow_y_scroll()
                    .child(body),
            )
            .child(footer)
            .into_any_element();
        self.overlay_scrim_custom("scrim-vault", cx.entity(), |_| {}, focus, panel, cx)
    }
    // Keep native builders in bounded frames on Windows' default debug stack.
    #[inline(never)]
    fn render_vault_header(
        &self,
        mut body: gpui::Div,
        phase: Phase,
        name: String,
        muted: gpui::Hsla,
    ) -> gpui::Div {
        let title_key = match phase {
            Phase::Create => "vault_protect_title",
            Phase::Unlock => "vault_unlock_title",
            Phase::Manage => "vault_manage_title",
            Phase::Restore => "vault_restore_title",
            Phase::Browse => "",
        };
        body = body.child(
            v_flex()
                .gap(s(14.))
                .child(
                    h_flex()
                        .gap(s(8.))
                        .text_color(muted)
                        .text_size(s(12.))
                        .child(icon(
                            if phase == Phase::Browse {
                                AtlasIcon::Lock
                            } else {
                                AtlasIcon::Folder
                            },
                            15.,
                            muted,
                        ))
                        .child(div().truncate().child(if phase == Phase::Browse {
                            rust_i18n::t!("vault_private_session").to_string()
                        } else {
                            name.clone()
                        })),
                )
                .child(
                    v_flex()
                        .gap(s(7.))
                        .child(
                            div()
                                .text_size(s(27.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(if phase == Phase::Browse {
                                    name.clone()
                                } else {
                                    rust_i18n::t!(title_key).to_string()
                                }),
                        )
                        .when(phase != Phase::Browse, |v| {
                            v.child(
                                div().text_size(s(14.)).text_color(muted).child(
                                    rust_i18n::t!(match phase {
                                        Phase::Create => "vault_protect_description",
                                        Phase::Unlock => "vault_unlock_description",
                                        Phase::Manage => "vault_manage_description",
                                        _ => "vault_restore_description",
                                    })
                                    .to_string(),
                                ),
                            )
                        }),
                ),
        );

        body
    }

    #[inline(never)]
    fn render_vault_footer(
        &self,
        phase: Phase,
        busy: bool,
        border: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let mut footer = h_flex()
            .flex_shrink_0()
            .justify_between()
            .gap(s(12.))
            .border_t_1()
            .border_color(border)
            .px(s(36.))
            .py(s(16.));
        if phase == Phase::Browse && !busy {
            footer = footer.child(
                Button::new("vault-restore")
                    .small()
                    .ghost()
                    .label(rust_i18n::t!("vault_remove_protection").to_string())
                    .on_click(
                        cx.listener(|this, _, w, cx| this.vault_phase(Phase::Restore, w, cx)),
                    ),
            );
        } else {
            footer = footer.child(div().flex_1());
        }
        let mut actions = h_flex().gap(s(8.));
        if phase == Phase::Browse && !busy {
            let d = self.vault.as_ref().unwrap();
            let selected = d.selected.as_ref();
            let directory = selected
                .and_then(|id| d.session.as_ref()?.entry(id))
                .is_some_and(|e| e.directory);
            actions = actions
                .child(
                    Button::new("vault-preview")
                        .small()
                        .ghost()
                        .label(
                            rust_i18n::t!(if directory {
                                "vault_open_folder"
                            } else {
                                "vault_preview"
                            })
                            .to_string(),
                        )
                        .disabled(selected.is_none() || d.preview.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.vault_preview_selected(cx))),
                )
                .child(
                    Button::new("vault-export")
                        .small()
                        .ghost()
                        .label(rust_i18n::t!("vault_export").to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.vault_export_picker(cx))),
                );
        }
        if matches!(phase, Phase::Manage | Phase::Restore) && !busy {
            actions = actions.child(
                Button::new("vault-cancel-manage")
                    .small()
                    .ghost()
                    .label(rust_i18n::t!("overlay_cancel").to_string())
                    .on_click(cx.listener(|this, _, w, cx| this.vault_phase(Phase::Browse, w, cx))),
            );
        } else {
            actions = actions.child(
                Button::new("vault-close")
                    .small()
                    .ghost()
                    .label(
                        rust_i18n::t!(if phase == Phase::Browse {
                            "vault_lock"
                        } else {
                            "overlay_cancel"
                        })
                        .to_string(),
                    )
                    .on_click(cx.listener(|this, _, w, cx| this.close_vault(w, cx))),
            );
        }
        if phase != Phase::Browse && (phase != Phase::Unlock || busy) {
            actions = actions.child(
                Button::new("vault-submit")
                    .primary()
                    .large()
                    .rounded_full()
                    .bg(gpui::rgb(0xf25c35))
                    .text_color(gpui::white())
                    .disabled(busy)
                    .label(
                        rust_i18n::t!(match phase {
                            Phase::Create => "vault_protect_action",
                            Phase::Unlock => "vault_unlock_action",
                            Phase::Manage => "vault_save_action",
                            _ => "vault_restore_action",
                        })
                        .to_string(),
                    )
                    .on_click(cx.listener(|this, _, w, cx| this.submit_vault(w, cx))),
            );
        }
        footer = footer.child(actions);
        footer
    }

    #[inline(never)]
    fn render_vault_browser(&mut self, mut body: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let d = self.vault.as_ref().unwrap();
        let preview = d.preview.as_ref().and_then(|preview| match preview {
            PrivatePreview::Text(text) => Some(SharedString::from(
                text.chars().take(20_000).collect::<String>(),
            )),
            _ => None,
        });
        let preview_image = d.preview.as_ref().and_then(|preview| match preview {
            PrivatePreview::Image(image) => Some(image.clone()),
            _ => None,
        });
        let has_preview = d.preview.is_some();
        let has_parent = !d.parents.is_empty();
        let name = d
            .session
            .as_ref()
            .map(|s| s.name().to_owned())
            .unwrap_or_default();
        let current_name = if has_preview {
            d.selected
                .as_ref()
                .and_then(|id| d.session.as_ref()?.entry(id))
                .map_or(name.clone(), |entry| entry.name.clone())
        } else {
            d.parents
                .last()
                .and_then(|id| d.session.as_ref()?.entry(id))
                .map_or(name, |e| e.name.clone())
        };
        let selected = d.selected.clone();
        let entries = d
            .session
            .as_ref()
            .map(|s| s.children(d.parents.last().map(String::as_str)))
            .unwrap_or_default();
        let (muted, secondary) = {
            let t = cx.theme();
            (vault_muted(cx), t.secondary)
        };
        #[cfg(windows)]
        if d.error.as_ref().is_some_and(|error| {
            error == rust_i18n::t!("vault_driver_missing").as_ref()
                || error == rust_i18n::t!("vault_driver_install_failed").as_ref()
                || error.starts_with("WinFsp: ")
        }) {
            body = body.child(
                div()
                    .text_size(s(12.))
                    .text_color(muted)
                    .child(rust_i18n::t!("vault_driver_external_instructions").to_string()),
            );
            body = body.child(
                Button::new("vault-winfsp-license")
                    .ghost()
                    .label("WinFsp — license and source")
                    .on_click(cx.listener(|_this, _, _, cx| {
                        cx.open_url("https://github.com/winfsp/winfsp/blob/v2.1/License.txt")
                    })),
            );
            body = body.child(
                Button::new("vault-install-driver")
                    .primary()
                    .large()
                    .label(rust_i18n::t!("vault_install_driver").to_string())
                    .on_click(cx.listener(|this, _, _, cx| this.install_vault_driver(cx))),
            );
        }
        body = body.child(
            h_flex()
                .justify_between()
                .gap(s(8.))
                .child(
                    Button::new("vault-import")
                        .small()
                        .ghost()
                        .label(rust_i18n::t!("vault_import").to_string())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.vault_import_picker(false, cx)),
                        ),
                )
                .child(
                    Button::new("vault-import-folder")
                        .small()
                        .ghost()
                        .label(rust_i18n::t!("vault_import_folder").to_string())
                        .on_click(cx.listener(|this, _, _, cx| this.vault_import_picker(true, cx))),
                )
                .child(
                    Button::new("vault-manage")
                        .small()
                        .ghost()
                        .label(rust_i18n::t!("vault_manage").to_string())
                        .on_click(
                            cx.listener(|this, _, w, cx| this.vault_phase(Phase::Manage, w, cx)),
                        ),
                ),
        );
        body = body.child(
            div()
                .text_size(s(12.))
                .text_color(muted)
                .child(rust_i18n::t!("vault_browse_hint").to_string()),
        );
        body = body.child(
            h_flex()
                .gap(s(8.))
                .text_size(s(12.))
                .text_color(muted)
                .child(
                    Button::new("vault-back")
                        .small()
                        .ghost()
                        .label(rust_i18n::t!("vault_back").to_string())
                        .disabled(!has_parent && !has_preview)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(d) = &mut this.vault {
                                if d.preview.is_some() {
                                    d.preview = None;
                                } else {
                                    d.parents.pop();
                                    d.selected = None;
                                }
                                d.last_activity = Instant::now();
                                d.error = None;
                            }
                            cx.notify();
                        })),
                )
                .child(div().truncate().child(current_name)),
        );
        if let Some(image) = preview_image {
            body = body.child(
                div()
                    .w_full()
                    .h(s(300.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(secondary)
                    .rounded(s(12.))
                    .overflow_hidden()
                    .child(
                        gpui::img(image)
                            .w_full()
                            .h_full()
                            .object_fit(gpui::ObjectFit::Contain),
                    ),
            );
        } else if let Some(text) = preview {
            body = body.child(
                div()
                    .id("vault-text-preview")
                    .h(s(270.))
                    .overflow_y_scroll()
                    .text_size(s(12.))
                    .font_family("Geist Mono")
                    .p(s(12.))
                    .bg(secondary)
                    .rounded(s(7.))
                    .child(text),
            );
        } else if entries.is_empty() {
            body = body.child(
                v_flex()
                    .h(s(240.))
                    .justify_center()
                    .items_center()
                    .gap(s(6.))
                    .text_color(muted)
                    .child(
                        div()
                            .text_size(s(14.))
                            .child(rust_i18n::t!("vault_empty").to_string()),
                    )
                    .child(
                        div()
                            .text_size(s(12.))
                            .child(rust_i18n::t!("vault_empty_hint").to_string()),
                    ),
            );
        } else {
            body =
                body.child(
                    v_flex()
                        .id("vault-entries")
                        .h(s(300.))
                        .overflow_y_scroll()
                        .children(entries.into_iter().map(|entry| {
                            let id = entry.id.clone();
                            let directory = entry.directory;
                            let is_selected = selected.as_deref() == Some(&id);
                            h_flex()
                                .id(SharedString::from(format!("vault-row-{id}")))
                                .w_full()
                                .h(s(44.))
                                .px(s(10.))
                                .gap(s(10.))
                                .rounded(s(5.))
                                .cursor_pointer()
                                .when(is_selected, |row| row.bg(secondary))
                                .hover(|row| row.bg(secondary))
                                .child(icon(
                                    if directory {
                                        AtlasIcon::Folder
                                    } else {
                                        AtlasIcon::File
                                    },
                                    16.,
                                    muted,
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(s(13.))
                                        .child(entry.name.clone()),
                                )
                                .child(div().text_size(s(11.)).text_color(muted).child(
                                    if directory {
                                        String::new()
                                    } else {
                                        atlas_core::util::display::format_size(entry.size)
                                    },
                                ))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                                        if let Some(d) = &mut this.vault {
                                            d.selected = Some(id.clone());
                                            d.last_activity = Instant::now();
                                            d.preview = None;
                                            d.error = None;
                                            if directory && ev.click_count >= 2 {
                                                d.parents.push(id.clone());
                                                d.selected = None;
                                            }
                                        }
                                        if !directory && ev.click_count >= 2 {
                                            this.vault_preview_selected(cx);
                                        }
                                        cx.notify();
                                    }),
                                )
                        })),
                );
        }
        body
    }

    #[inline(never)]
    fn render_vault_form(&mut self, mut body: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        let d = self.vault.as_ref().unwrap();
        let phase = d.phase;
        let inputs = d.inputs.clone();
        let profiles_open = d.profiles_open;
        let selected_profiles = d.profiles.clone();
        let reveal = d.reveal;
        let error = d.error.clone();
        let validation_field = error.as_deref().and_then(|e| {
            if e == rust_i18n::t!("vault_password_mismatch").as_ref() {
                Some(if phase == Phase::Manage { 2 } else { 1 })
            } else if e == rust_i18n::t!("vault_password_length").as_ref() {
                Some(if phase == Phase::Manage { 1 } else { 0 })
            } else {
                None
            }
        });
        let (muted, danger) = {
            let t = cx.theme();
            (vault_muted(cx), t.danger)
        };
        if !inputs.is_empty() {
            let labels = match phase {
                Phase::Create => vec!["vault_password", "vault_confirm_password"],
                Phase::Manage => vec![
                    "vault_current_password",
                    "vault_new_password",
                    "vault_confirm_password",
                ],
                _ => vec!["vault_password"],
            };
            let last = inputs.len() - 1;
            body = body.child(
                v_flex()
                    .gap(s(16.))
                    .children(inputs.iter().zip(labels).enumerate().map(
                        |(index, (input, key))| {
                            let field = Input::new(input)
                                .large()
                                .w_full()
                                .h(s(54.))
                                .rounded_full()
                                .text_size(s(16.));
                            let field = if index == 0 {
                                field.suffix(
                                    h_flex()
                                        .gap(s(8.))
                                        .child(
                                            Button::new("vault-password-visibility")
                                                .ghost()
                                                .xsmall()
                                                .label(
                                                    rust_i18n::t!(if reveal {
                                                        "vault_hide"
                                                    } else {
                                                        "vault_show"
                                                    })
                                                    .to_string(),
                                                )
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    if let Some(d) = &mut this.vault {
                                                        d.reveal = !d.reveal;
                                                        for input in &d.inputs {
                                                            input.update(cx, |i, cx| {
                                                                i.set_masked(!d.reveal, window, cx)
                                                            });
                                                        }
                                                    }
                                                    cx.notify();
                                                })),
                                        )
                                        .when(phase == Phase::Unlock, |row| {
                                            row.child(
                                                Button::new("vault-unlock-arrow")
                                                    .primary()
                                                    .icon(gpui_component::IconName::ArrowRight)
                                                    .w(s(40.))
                                                    .h(s(40.))
                                                    .rounded_full()
                                                    .bg(gpui::rgb(0xf25c35))
                                                    .text_color(gpui::white())
                                                    .tooltip(
                                                        rust_i18n::t!("vault_unlock_action")
                                                            .to_string(),
                                                    )
                                                    .on_click(cx.listener(|this, _, w, cx| {
                                                        this.submit_vault(w, cx)
                                                    })),
                                            )
                                        }),
                                )
                            } else {
                                field
                            };
                            v_flex()
                                .gap(s(7.))
                                .child(
                                    div()
                                        .text_size(s(12.))
                                        .child(rust_i18n::t!(key).to_string()),
                                )
                                .child(field)
                                .when(validation_field == Some(index), |v| {
                                    v.child(
                                        div()
                                            .text_size(s(12.))
                                            .text_color(danger)
                                            .child(error.clone().unwrap_or_default()),
                                    )
                                })
                                .when(index == last && phase == Phase::Create, |v| {
                                    v.child(
                                        div().text_size(s(11.)).text_color(muted).child(
                                            rust_i18n::t!("vault_password_hint").to_string(),
                                        ),
                                    )
                                })
                        },
                    )),
            );
        }
        if matches!(phase, Phase::Create | Phase::Manage) {
            let names = self
                .state
                .profiles
                .profiles
                .iter()
                .filter(|p| selected_profiles.contains(&p.id))
                .map(|p| p.name.clone())
                .collect::<Vec<_>>()
                .join(", ");
            let active = self.state.profiles.active().id.clone();
            body = body.child(
                v_flex()
                    .gap(s(8.))
                    .child(
                        div()
                            .text_size(s(12.))
                            .child(rust_i18n::t!("vault_visible_to").to_string()),
                    )
                    .child(
                        Button::new("vault-profiles-toggle")
                            .large()
                            .rounded(gpui::px(12.))
                            .w_full()
                            .label(names)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(d) = &mut this.vault {
                                    d.profiles_open = !d.profiles_open;
                                }
                                cx.notify();
                            })),
                    )
                    .when(profiles_open, |v| {
                        v.child(
                            v_flex()
                                .id("vault-profile-list")
                                .max_h(s(130.))
                                .overflow_y_scroll()
                                .gap(s(10.))
                                .p(s(10.))
                                .children(self.state.profiles.profiles.iter().map(|profile| {
                                    let id = profile.id.clone();
                                    let current = id == active;
                                    Checkbox::new(SharedString::from(format!("vault-profile-{id}")))
                                        .small()
                                        .label(profile.name.clone())
                                        .checked(selected_profiles.contains(&id))
                                        .disabled(current)
                                        .on_click(cx.listener(
                                            move |this, checked: &bool, _, cx| {
                                                if let Some(d) = &mut this.vault {
                                                    if *checked {
                                                        d.profiles.insert(id.clone());
                                                    } else {
                                                        d.profiles.remove(&id);
                                                    }
                                                    d.last_activity = Instant::now();
                                                }
                                                cx.notify();
                                            },
                                        ))
                                })),
                        )
                    })
                    .child(
                        div()
                            .text_size(s(11.))
                            .text_color(muted)
                            .child(rust_i18n::t!("vault_visibility_hint").to_string()),
                    ),
            );
        }
        if phase == Phase::Create {
            body = body.child(
                h_flex()
                    .gap(s(7.))
                    .text_color(muted)
                    .child(icon(AtlasIcon::Lock, 13., muted))
                    .child(
                        div()
                            .text_size(s(11.))
                            .child(rust_i18n::t!("vault_recovery_hint").to_string()),
                    ),
            );
        }
        body
    }
}

fn vault_muted(cx: &Context<App>) -> gpui::Hsla {
    let theme = cx.theme();
    if theme.background.lightness < 0.3 {
        let mut color = theme.popover_foreground;
        color.alpha *= 0.68;
        color
    } else {
        theme.muted_foreground
    }
}
