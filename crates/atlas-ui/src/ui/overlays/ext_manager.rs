//! Extension Manager — right-side sheet panel.
//!
//! Two tabs:
//!  - **Installed**: lists local extensions; toggle enable/disable + trash-icon uninstall.
//!  - **Marketplace**: catalogue from the R2 registry.json; install/update remotely.
//!
//! Keyboard: focus managed by `overlay_focus_target` → `ext_manager_focus_handle`.
//! Scrollbar: same pattern as settings.rs (shared ScrollHandle between
//!   `.track_scroll()` inner and `.vertical_scrollbar()` outer).
#![allow(clippy::too_many_lines)]

use crate::app::ext::ExtAction;
use crate::app::App;
use crate::ui::icons::{icon, AtlasIcon};
use atlas_model::ext_state::ExtManagerTab;
use gpui::prelude::*;
use gpui::AnimationExt;
use gpui::ColorExt as _;
use gpui::{div, px, AnyElement, Context, Hsla, MouseButton, SharedString, Window};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex, ActiveTheme};

const PANEL_W: f32 = 400.;

impl App {
    pub(crate) fn render_ext_manager(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.theme();
        let popover = t.popover;
        let fg = t.popover_foreground;
        let muted = t.muted_foreground;
        let border = t.border;
        let accent = t.accent;
        let accent_fg = t.accent_foreground;
        let surface = t.background;
        let danger = t.danger;

        let view = cx.entity();
        let focus = self.overlay_focus(cx);
        let (smooth_ease, smooth_boost) =
            crate::app::scroll_smooth_params(self.state.settings.scroll_smoothness);
        self.ext_manager_scroll
            .set_smooth(smooth_ease, smooth_boost);

        // Ensure search input exists + capture query this frame.
        let input_entity = {
            if self.ext_manager_input.is_none() {
                self.ext_manager_input = Some(crate::ui::text_input::single_line(
                    window,
                    cx,
                    rust_i18n::t!("ext_manager_search").as_ref(),
                    "",
                ));
            }
            self.ext_manager_input.clone().unwrap()
        };
        let query: String = input_entity.read(cx).value().to_string();
        let query_lower = query.to_lowercase();

        let active_tab = self.state.ext.manager_tab;

        // ── Tab switcher ───────────────────────────────────────────────────────
        let tab_btn = move |label: &str,
                            tab: ExtManagerTab,
                            view: gpui::Entity<App>,
                            accent: gpui::Hsla,
                            muted: gpui::Hsla,
                            fg: gpui::Hsla| {
            let selected = active_tab == tab;
            div()
                .id(SharedString::from(format!("ext-tab-{label}")))
                .px_3()
                .py_1()
                .rounded(px(crate::ui::RADIUS))
                .cursor_pointer()
                .text_sm()
                .when(selected, |d| d.bg(accent.opacity(0.15)).text_color(accent))
                .when(!selected, |d| {
                    d.text_color(muted)
                        .hover(|h| h.bg(accent.opacity(0.07)).text_color(fg))
                })
                .child(label.to_string())
                .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                    view.update(cx, |this, cx| {
                        this.state.ext.manager_tab = tab;
                        if tab == ExtManagerTab::Marketplace
                            && this.state.ext.remote.is_none()
                            && !this.state.ext.remote_loading
                        {
                            this.handle_ext_action(ExtAction::FetchRegistry);
                        }
                        cx.notify();
                    });
                })
        };

        // ── Installed tab content ──────────────────────────────────────────────
        let installed_rows: Vec<AnyElement> = {
            let exts: Vec<_> = self
                .ext_registry
                .extensions
                .iter()
                .filter(|e| !e.manifest.hidden)
                .filter(|e| {
                    query_lower.is_empty()
                        || e.manifest.name.to_lowercase().contains(&query_lower)
                        || e.manifest.description.to_lowercase().contains(&query_lower)
                        || e.manifest.id.to_lowercase().contains(&query_lower)
                })
                .cloned()
                .collect();

            if exts.is_empty() {
                vec![div()
                    .py_4()
                    .text_center()
                    .text_sm()
                    .text_color(muted)
                    .child(rust_i18n::t!("ext_no_installed").to_string())
                    .into_any_element()]
            } else {
                exts.into_iter()
                    .map(|ext| {
                        let id = ext.manifest.id.clone();
                        let name = ext.manifest.name.clone();
                        let version = ext.manifest.version.clone();
                        let description = ext.manifest.description.clone();
                        let enabled = ext.enabled;
                        let builtin = ext.builtin;
                        let view2 = view.clone();
                        let view3 = view.clone();

                        h_flex()
                            .id(SharedString::from(format!("ext-row-{id}")))
                            .w_full()
                            .gap_3()
                            .py_2()
                            .px_1()
                            .border_b_1()
                            .border_color(border)
                            .items_center()
                            // Icon
                            .child({
                                let svg_path = ext.dir.join("icon.svg");
                                let icon_path = if svg_path.exists() {
                                    svg_path
                                } else {
                                    ext.dir.join("icon.png")
                                };
                                if icon_path.exists() {
                                    extension_icon(icon_path, 36., fg)
                                } else {
                                    div().size(px(36.)).flex_none().into_any_element()
                                }
                            })
                            // Left: info
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .items_center()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                                    .text_color(if enabled { fg } else { muted })
                                                    .child(name),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(format!("v{version}")),
                                            )
                                            .when(builtin, |r| {
                                                r.child(
                                                    div()
                                                        .px_1()
                                                        .py_0p5()
                                                        .rounded_sm()
                                                        .bg(border)
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .child(
                                                            rust_i18n::t!("ext_builtin_badge")
                                                                .to_string(),
                                                        ),
                                                )
                                            })
                                            .when(!enabled, |r| {
                                                r.child(
                                                    div()
                                                        .px_1()
                                                        .py_0p5()
                                                        .rounded_sm()
                                                        .bg(border)
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .child(
                                                            rust_i18n::t!("ext_disabled_badge")
                                                                .to_string(),
                                                        ),
                                                )
                                            }),
                                    )
                                    .when(!description.is_empty(), |r| {
                                        r.child(
                                            div().text_xs().text_color(muted).child(description),
                                        )
                                    })
                                    .when(enabled && !ext.manifest.commands.is_empty(), |r| {
                                        let id_for_cmds = id.clone();
                                        r.child(h_flex().flex_wrap().gap_1().mt_1().children(
                                            ext.manifest.commands.iter().map(|cmd| {
                                                let cmd_id = cmd.id.clone();
                                                let cmd_title = cmd.title.clone();
                                                let ext_id_clone = id_for_cmds.clone();
                                                let view_cmd = view.clone();
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "ext-cmd-{cmd_id}"
                                                    )))
                                                    .px_2()
                                                    .py_0p5()
                                                    .rounded(px(crate::ui::RADIUS))
                                                    .bg(accent.opacity(0.12))
                                                    .text_color(accent)
                                                    .text_xs()
                                                    .cursor_pointer()
                                                    .hover(|h| h.bg(accent.opacity(0.24)))
                                                    .child(cmd_title)
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        move |_ev, _w, cx| {
                                                            view_cmd.update(cx, |this, _cx| {
                                                                this.handle_ext_action(
                                                                    ExtAction::Run {
                                                                        ext_id: ext_id_clone
                                                                            .clone(),
                                                                        cmd_id: cmd_id.clone(),
                                                                    },
                                                                );
                                                            });
                                                        },
                                                    )
                                            }),
                                        ))
                                    }),
                            )
                            // Right: toggle + trash icon
                            .child(
                                h_flex()
                                    .flex_none()
                                    .gap_1()
                                    .items_center()
                                    .child({
                                        // Enable/disable toggle pill
                                        let id2 = id.clone();
                                        div()
                                            .id(SharedString::from(format!("ext-toggle-{id}")))
                                            .w(px(36.))
                                            .h(px(20.))
                                            .rounded_full()
                                            .p(px(2.))
                                            .cursor_pointer()
                                            .when(enabled, |d| d.bg(accent))
                                            .when(!enabled, |d| d.bg(border))
                                            .child(
                                                div()
                                                    .size(px(16.))
                                                    .rounded_full()
                                                    .bg(fg)
                                                    .when(enabled, |d| d.ml(px(16.))),
                                            )
                                            .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                                                view2.update(cx, |this, cx| {
                                                    this.handle_ext_action(ExtAction::SetEnabled {
                                                        id: id2.clone(),
                                                        enabled: !enabled,
                                                    });
                                                    cx.notify();
                                                });
                                            })
                                    })
                                    .when(!builtin, |r| {
                                        let id3 = id.clone();
                                        r.child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "ext-uninstall-{id}"
                                                )))
                                                .size(px(24.))
                                                .rounded(px(crate::ui::RADIUS))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .cursor_pointer()
                                                .text_color(muted)
                                                .hover(|d| {
                                                    d.text_color(danger).bg(danger.opacity(0.1))
                                                })
                                                .child(icon(AtlasIcon::Trash, 13., danger))
                                                .on_mouse_down(
                                                    MouseButton::Left,
                                                    move |_ev, _w, cx| {
                                                        view3.update(cx, |this, cx| {
                                                            this.handle_ext_action(
                                                                ExtAction::Uninstall(id3.clone()),
                                                            );
                                                            cx.notify();
                                                        });
                                                    },
                                                ),
                                        )
                                    }),
                            )
                            .into_any_element()
                    })
                    .collect()
            }
        };

        // ── Marketplace tab content ────────────────────────────────────────────
        let remote_loading = self.state.ext.remote_loading;
        let installed_versions: std::collections::HashMap<String, String> = self
            .ext_registry
            .extensions
            .iter()
            .map(|e| (e.manifest.id.clone(), e.manifest.version.clone()))
            .collect();
        let marketplace_visible = self.state.ext.marketplace_visible.max(40);
        let currently_installing = self.state.ext.installing.clone();

        let marketplace_rows: Vec<AnyElement> = if remote_loading {
            vec![div()
                .py_4()
                .text_center()
                .text_sm()
                .text_color(muted)
                .child(rust_i18n::t!("ext_loading").to_string())
                .into_any_element()]
        } else {
            match &self.state.ext.remote {
                None => {
                    let view2 = view.clone();
                    vec![div()
                        .py_4()
                        .text_center()
                        .text_sm()
                        .text_color(muted)
                        .child(rust_i18n::t!("ext_no_remote").to_string())
                        .child(
                            div()
                                .id("ext-reload-empty")
                                .mt_2()
                                .px_3()
                                .py_1()
                                .rounded(px(crate::ui::RADIUS))
                                .border_1()
                                .border_color(border)
                                .text_xs()
                                .cursor_pointer()
                                .hover(|d| d.bg(border))
                                .child(rust_i18n::t!("ext_reload").to_string())
                                .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                                    view2.update(cx, |this, cx| {
                                        this.handle_ext_action(ExtAction::FetchRegistry);
                                        cx.notify();
                                    });
                                }),
                        )
                        .into_any_element()]
                }
                Some(list) => {
                    let filtered: Vec<_> = list
                        .iter()
                        .filter(|e| {
                            query_lower.is_empty()
                                || e.name.to_lowercase().contains(&query_lower)
                                || e.description.to_lowercase().contains(&query_lower)
                                || e.id.to_lowercase().contains(&query_lower)
                        })
                        .cloned()
                        .collect();

                    if filtered.is_empty() {
                        vec![div()
                            .py_4()
                            .text_center()
                            .text_sm()
                            .text_color(muted)
                            .child(rust_i18n::t!("ext_no_remote").to_string())
                            .into_any_element()]
                    } else {
                        let has_more = filtered.len() > marketplace_visible;
                        let mut rows: Vec<AnyElement> = filtered
                            .into_iter()
                            .take(marketplace_visible)
                            .map(|remote| {
                                let id = remote.id.clone();
                                let name = remote.name.clone();
                                let version = remote.latest_version.clone();
                                let description = remote.description.clone();
                                let author = remote.author.clone();
                                let icon_url = remote.icon_url.clone();
                                let download_url = remote.download_url.clone();
                                let sha256 = remote.sha256.clone();
                                let installed_version = installed_versions.get(&id);
                                let is_installed = installed_version.is_some();
                                let has_update =
                                    installed_version.is_some_and(|installed| {
                                        match (
                                            semver::Version::parse(installed),
                                            semver::Version::parse(&version),
                                        ) {
                                            (Ok(installed), Ok(remote)) => remote > installed,
                                            _ => version != *installed,
                                        }
                                    });
                                let is_installing = currently_installing.contains(&id);
                                let view2 = view.clone();

                                h_flex()
                                    .id(SharedString::from(format!("mkt-row-{id}")))
                                    .w_full()
                                    .gap_3()
                                    .py_2()
                                    .px_1()
                                    .border_b_1()
                                    .border_color(border)
                                    .items_center()
                                    // Icon
                                    .child(if let Some(url) = icon_url.clone() {
                                        extension_icon(std::path::PathBuf::from(url), 36., fg)
                                    } else {
                                        div().size(px(36.)).flex_none().into_any_element()
                                    })
                                    // Info
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_0p5()
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .items_center()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .font_weight(gpui::FontWeight::SEMIBOLD)
                                                            .child(name),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_xs()
                                                            .text_color(muted)
                                                            .child(format!("v{version}")),
                                                    )
                                                    .when(!author.is_empty(), |r| {
                                                        r.child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(muted)
                                                                .child(rust_i18n::t!(
                                                                    "ext_author_by",
                                                                    author = author
                                                                )),
                                                        )
                                                    }),
                                            )
                                            .when(!description.is_empty(), |r| {
                                                r.child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .child(description),
                                                )
                                            }),
                                    )
                                    // Action button
                                    .child(if is_installing {
                                        div()
                                            .id(SharedString::from(format!("mkt-installing-{id}")))
                                            .px_2()
                                            .py_0p5()
                                            .rounded(px(crate::ui::RADIUS))
                                            .border_1()
                                            .border_color(border)
                                            .text_xs()
                                            .flex_none()
                                            .text_color(muted)
                                            .child(rust_i18n::t!("ext_installing").to_string())
                                            .into_any_element()
                                    } else if is_installed && !has_update {
                                        div()
                                            .id(SharedString::from(format!("mkt-installed-{id}")))
                                            .px_2()
                                            .py_0p5()
                                            .rounded(px(crate::ui::RADIUS))
                                            .border_1()
                                            .border_color(border)
                                            .text_xs()
                                            .flex_none()
                                            .text_color(muted)
                                            .child(rust_i18n::t!("ext_tab_installed").to_string())
                                            .into_any_element()
                                    } else {
                                        div()
                                            .id(SharedString::from(format!("mkt-install-{id}")))
                                            .px_2()
                                            .py_0p5()
                                            .rounded(px(crate::ui::RADIUS))
                                            .border_1()
                                            .border_color(accent)
                                            .bg(accent)
                                            .text_xs()
                                            .flex_none()
                                            .text_color(accent_fg)
                                            .cursor_pointer()
                                            .hover(|d| d.opacity(0.8))
                                            .child(
                                                rust_i18n::t!(if has_update {
                                                    "ext_update"
                                                } else {
                                                    "ext_install"
                                                })
                                                .to_string(),
                                            )
                                            .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                                                view2.update(cx, |this, cx| {
                                                    this.handle_ext_action(
                                                        ExtAction::InstallRemote {
                                                            id: id.clone(),
                                                            download_url: download_url.clone(),
                                                            sha256: sha256.clone(),
                                                        },
                                                    );
                                                    cx.notify();
                                                });
                                            })
                                            .into_any_element()
                                    })
                                    .into_any_element()
                            })
                            .collect();
                        if has_more {
                            let view_more = view.clone();
                            rows.push(
                                div()
                                    .id("ext-load-more")
                                    .my_3()
                                    .mx_auto()
                                    .px_3()
                                    .py_1()
                                    .rounded(px(crate::ui::RADIUS))
                                    .border_1()
                                    .border_color(border)
                                    .text_xs()
                                    .cursor_pointer()
                                    .hover(|d| d.bg(border))
                                    .child(rust_i18n::t!("ext_load_more").to_string())
                                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                                        view_more.update(cx, |this, cx| {
                                            this.state.ext.marketplace_visible += 40;
                                            cx.notify();
                                        });
                                    })
                                    .into_any_element(),
                            );
                        }
                        rows
                    }
                }
            }
        };

        // ── Error / feedback banner ─────────────────────────────────────────────
        let error_banner: Option<AnyElement> = self.state.ext.last_error.as_deref().map(|msg| {
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
        let feedback_banner: Option<AnyElement> = self.state.ext.feedback.as_deref().map(|msg| {
            div()
                .px_3()
                .py_2()
                .rounded(px(crate::ui::RADIUS))
                .bg(accent.opacity(0.1))
                .border_1()
                .border_color(accent.opacity(0.4))
                .text_xs()
                .text_color(accent)
                .child(msg.to_string())
                .into_any_element()
        });

        // ── Reload + install-from-file footer ──────────────────────────────────
        let view_reload = view.clone();
        let view_install_file = view.clone();

        let footer = h_flex()
            .gap_2()
            .pt_2()
            .border_t_1()
            .border_color(border)
            .child(
                div()
                    .id("ext-install-file")
                    .flex_1()
                    .px_2()
                    .py_1()
                    .rounded(px(crate::ui::RADIUS))
                    .border_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(fg)
                    .cursor_pointer()
                    .hover(|d| d.bg(border))
                    .child(rust_i18n::t!("ext_install_from_file").to_string())
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                        view_install_file.update(cx, |this, cx| {
                            this.handle_ext_action(ExtAction::Install);
                            cx.notify();
                        });
                    }),
            )
            .child(
                div()
                    .id("ext-reload-btn")
                    .px_2()
                    .py_1()
                    .rounded(px(crate::ui::RADIUS))
                    .border_1()
                    .border_color(border)
                    .text_xs()
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(|d| d.bg(border))
                    .child(rust_i18n::t!("ext_reload").to_string())
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                        view_reload.update(cx, |this, cx| {
                            this.handle_ext_action(ExtAction::FetchRegistry);
                            cx.notify();
                        });
                    }),
            );

        // ── Content panel (scrollable) ─────────────────────────────────────────
        let rows = if active_tab == ExtManagerTab::Installed {
            installed_rows
        } else {
            marketplace_rows
        };

        let content_panel = v_flex()
            .id("ext-mgr-content")
            .w_full()
            .flex_1()
            .overflow_y_scroll()
            .track_scroll(&self.ext_manager_scroll)
            .children(rows);

        // ── Full panel (header + tabs + search + content + footer) ─────────────
        let view_inst = view.clone();
        let view_mkt = view.clone();

        let panel = v_flex()
            .id("ext-mgr-panel")
            .h_full()
            .p_4()
            .gap_3()
            // Title
            .child(
                div()
                    .text_base()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child(rust_i18n::t!("ext_manager_title").to_string()),
            )
            // Tab switcher — VS Code–style plain text tabs with accent underline
            .child(
                h_flex()
                    .gap_1()
                    .border_b_1()
                    .border_color(border)
                    .pb_1()
                    .child(tab_btn(
                        &format!(
                            "{} ({})",
                            rust_i18n::t!("ext_tab_installed"),
                            self.ext_registry
                                .extensions
                                .iter()
                                .filter(|e| !e.manifest.hidden)
                                .count()
                        ),
                        ExtManagerTab::Installed,
                        view_inst,
                        accent,
                        muted,
                        fg,
                    ))
                    .child(tab_btn(
                        &self.state.ext.remote.as_ref().map_or_else(
                            || rust_i18n::t!("ext_tab_marketplace").to_string(),
                            |list| {
                                format!("{} ({})", rust_i18n::t!("ext_tab_marketplace"), list.len())
                            },
                        ),
                        ExtManagerTab::Marketplace,
                        view_mkt,
                        accent,
                        muted,
                        fg,
                    )),
            )
            // Search bar
            .child(
                div()
                    .w_full()
                    .border_1()
                    .border_color(border)
                    .rounded(px(crate::ui::RADIUS))
                    .bg(surface)
                    .px_2()
                    .py_1()
                    .child(crate::ui::text_input::flat(&input_entity)),
            )
            // Banners
            .children(error_banner)
            .children(feedback_banner)
            // Scrollable content
            .child(content_panel)
            // Footer
            .child(footer);

        // ── Sheet (right-anchored, full height, slide-in) ─────────────────────
        // The sheet panel is absolute-positioned relative to the backdrop div in
        // overlay_scrim_right_sheet. The animation drives `right` from -PANEL_W
        // (off-screen) to 0 (visible at right edge).
        let sheet_panel = div()
            .id("ext-mgr-sheet")
            .absolute()
            .right_0()
            .top_0()
            .h_full()
            .w(px(PANEL_W))
            .bg(popover)
            .text_color(fg)
            .border_l_1()
            .border_color(border)
            .rounded_tl(px(crate::ui::RADIUS * 2.0))
            .rounded_bl(px(crate::ui::RADIUS * 2.0))
            .shadow_xl()
            .overflow_hidden()
            .occlude()
            .child(panel)
            .vertical_scrollbar(&self.ext_manager_scroll)
            .with_animation(
                "ext-mgr-slide",
                gpui::Animation::new(std::time::Duration::from_millis(200)),
                |el, t| el.right(px(PANEL_W * (t - 1.0))),
            )
            .into_any_element();

        let view_close = cx.entity();
        self.overlay_scrim_right_sheet(
            "scrim-ext-manager",
            view_close,
            move |this| {
                this.state.ext.show_manager = false;
                this.ext_manager_input = None;
            },
            focus,
            sheet_panel,
            cx,
        )
    }
}

fn extension_icon(path: std::path::PathBuf, size: f32, color: Hsla) -> AnyElement {
    let path_text = path.to_string_lossy();
    if path_text.starts_with("http://") || path_text.starts_with("https://") {
        gpui::img(path_text.into_owned())
            .size(px(size))
            .flex_none()
            .into_any_element()
    } else if path.extension().is_some_and(|ext| ext == "svg") {
        gpui::svg()
            .external_path(path_text.into_owned())
            .size(px(size))
            .text_color(color)
            .flex_none()
            .into_any_element()
    } else {
        gpui::img(path)
            .size(px(size))
            .flex_none()
            .into_any_element()
    }
}
