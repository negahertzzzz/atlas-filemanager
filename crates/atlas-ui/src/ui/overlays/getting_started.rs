use super::brand;
use crate::app::App;
use crate::input::action::Action;
use crate::ui::icons::{atlas_logo, icon, AtlasIcon};
use atlas_model::settings::ViewMode;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    deferred, div, hsla, linear_color_stop, linear_gradient, px, AnyElement, Context, Entity,
    FocusHandle, Focusable, KeyDownEvent, MouseButton, SharedString, Window,
};
use gpui_component::input::InputState;
use gpui_component::{h_flex, scroll::ScrollableElement, v_flex};

const LOAD_MS: u128 = 5000;
const WELCOME_MS: u128 = 2600;
// First-run setup wears the same brand surface as the feature guide.
fn setup_bg() -> gpui::Hsla {
    hsla(0., 0., 0.02, 1.0)
}
fn setup_tile() -> gpui::Hsla {
    brand::wash()
}
fn setup_fg() -> gpui::Hsla {
    brand::ink()
}
fn setup_muted() -> gpui::Hsla {
    brand::muted()
}
fn setup_border() -> gpui::Hsla {
    brand::rim()
}

impl App {
    pub(crate) fn render_getting_started(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let loading_since = self.state.overlay.getting_started_loading_since;
        if let Some(started) = loading_since {
            if started.elapsed().as_millis() >= LOAD_MS {
                self.finish_getting_started(cx);
                return div().into_any_element();
            }
            window.request_animation_frame();
        }

        let view = cx.entity();
        let focus = self.overlay_focus(cx);
        let step = self.state.overlay.getting_started_step.min(2);
        let loading = loading_since.is_some();
        let reveal = loading_since
            .map(|started| (started.elapsed().as_millis() as f32 / LOAD_MS as f32).clamp(0.0, 1.0))
            .unwrap_or(0.0);
        let (fg, muted, border) = (setup_fg(), setup_muted(), setup_border());

        let content = match step {
            0 => self.setup_welcome_page(window, cx, fg, muted, border),
            1 => self.setup_theme_page(cx, fg, muted, border),
            _ => self.setup_workspace_page(fg, muted, border, cx),
        };

        let close_view = view.clone();
        let primary_view = view.clone();
        let back_view = view.clone();
        let guide_view = view.clone();
        // Last step: the filled button continues into the tutorial; the outlined
        // one just finishes the setup.
        let primary_label = if step == 2 {
            rust_i18n::t!("getting_started_guide_btn").to_string()
        } else {
            rust_i18n::t!("getting_started_next").to_string()
        };

        let loader = loading.then(|| {
            let pulse = (reveal * std::f32::consts::TAU * 5.0).sin() * 0.04;
            div()
                .absolute()
                .inset_0()
                .bg(hsla(0., 0., 0.04, (0.88 * (1.0 - reveal)).max(0.0)))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    v_flex()
                        .items_center()
                        .gap_4()
                        .opacity(0.72 + 0.22 * (1.0 - reveal))
                        .child(
                            div()
                                .scale(0.98 + pulse)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(atlas_logo(74.0)),
                        )
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(rust_i18n::t!("getting_started_loading").to_string()),
                        ),
                )
        });

        let page = div()
            .id("getting-started-fullscreen")
            .absolute()
            .inset_0()
            .size_full()
            .bg(setup_bg())
            .text_color(fg)
            .track_focus(&focus)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                if ev.keystroke.key == "escape" {
                    this.submit_getting_started_profile_name(cx);
                    this.process_actions([Action::CloseGettingStarted]);
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(brand::hot().opacity(0.0), 0.45),
                linear_color_stop(brand::hot().opacity(0.10), 1.0),
            )))
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(hsla(0., 0., 1., 0.035), 0.0),
                linear_color_stop(hsla(0., 0., 1., 0.0), 0.5),
            )))
            .child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .px_6()
                    .children((!loading).then(|| {
                        brand::panel()
                            .id("getting-started-card")
                            .flex()
                            .flex_col()
                            .w(px(760.))
                            .max_w(px(760.))
                            .min_h(px(520.))
                            .p(px(28.))
                            .gap_5()
                            .occlude()
                            .child(brand::top_rim())
                            .child(brand::warm_edge())
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .child(step_dots(step))
                                    .child(
                                        div()
                                            .id("getting-started-close")
                                            .size(px(30.))
                                            .rounded_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(muted)
                                            .cursor_pointer()
                                            .hover(move |h| h.bg(setup_tile()).text_color(fg))
                                            .child(icon(AtlasIcon::X, 13., muted))
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                move |_ev, _w, cx| {
                                                    close_view.update(cx, |this, cx| {
                                                        this.submit_getting_started_profile_name(cx);
                                                        this.process_actions([
                                                            Action::CloseGettingStarted,
                                                        ]);
                                                        cx.notify();
                                                    });
                                                },
                                            ),
                                    ),
                            )
                            .child(content)
                            .child(
                                h_flex()
                                    .justify_between()
                                    .items_center()
                                    .child(
                                        div()
                                            .id("getting-started-back")
                                            .px_4()
                                            .py_2()
                                            .rounded_full()
                                            .text_sm()
                                            .text_color(muted)
                                            .cursor_pointer()
                                            .when(step == 0, |d| d.opacity(0.0))
                                            .hover(move |h| h.bg(setup_tile()))
                                            .child(rust_i18n::t!("overlay_back").to_string())
                                            .on_mouse_down(
                                                MouseButton::Left,
                                                move |_ev, _w, cx| {
                                                    back_view.update(cx, |this, cx| {
                                                        this.state.overlay.getting_started_step =
                                                            this.state
                                                                .overlay
                                                                .getting_started_step
                                                                .saturating_sub(1);
                                                        cx.notify();
                                                    });
                                                },
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .id("getting-started-skip")
                                                    .px_4()
                                                    .py_2()
                                                    .rounded_full()
                                                    .text_sm()
                                                    .text_color(muted)
                                                    .cursor_pointer()
                                                    .hover(move |h| h.text_color(fg))
                                                    .child(
                                                        rust_i18n::t!("getting_started_secondary")
                                                            .to_string(),
                                                    )
                                                    .on_mouse_down(MouseButton::Left, {
                                                        let view = view.clone();
                                                        move |_ev, _w, cx| {
                                                            view.update(cx, |this, cx| {
                                                                this.submit_getting_started_profile_name(cx);
                                                                this.process_actions([
                                                                    Action::CloseGettingStarted,
                                                                ]);
                                                                cx.notify();
                                                            });
                                                        }
                                                    }),
                                            )
                                            .children((step == 2).then(|| {
                                                brand::ghost(
                                                    "getting-started-guide",
                                                    rust_i18n::t!("getting_started_primary")
                                                        .to_string(),
                                                )
                                                    .on_mouse_down(MouseButton::Left, {
                                                        let guide_view = guide_view.clone();
                                                        move |_ev, _w, cx| {
                                                            guide_view.update(cx, |this, cx| {
                                                                this.submit_getting_started_profile_name(cx);
                                                                this.process_actions([
                                                                    Action::StartGettingStarted,
                                                                ]);
                                                                cx.notify();
                                                            });
                                                        }
                                                    })
                                            }))
                                            .child(
                                                brand::pill("getting-started-primary", primary_label, None)
                                                    .when(loading, |d| d.opacity(0.72))
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        move |_ev, _w, cx| {
                                                            primary_view.update(cx, |this, cx| {
                                                                if this
                                                                    .state
                                                                    .overlay
                                                                    .getting_started_step
                                                                    >= 2
                                                                {
                                                                    this.submit_getting_started_profile_name(cx);
                                                                    this.process_actions([
                                                                        Action::CloseGettingStarted,
                                                                        Action::OpenGuide(0),
                                                                    ]);
                                                                } else {
                                                                    if this
                                                                        .state
                                                                        .overlay
                                                                        .getting_started_step
                                                                        == 0
                                                                    {
                                                                        this.submit_getting_started_profile_name(cx);
                                                                    }
                                                                    this.state
                                                                        .overlay
                                                                        .getting_started_step += 1;
                                                                }
                                                                cx.notify();
                                                            });
                                                        },
                                                    ),
                                            ),
                                    ),
                            )
                    })),
            )
            .children(loader);

        deferred(page).into_any_element()
    }

    fn finish_getting_started(&mut self, cx: &mut Context<Self>) {
        self.submit_getting_started_profile_name(cx);
        self.mark_getting_started_seen();
        self.state.overlay.show_getting_started = false;
        self.state.overlay.getting_started_loading_since = None;
        self.state.overlay.getting_started_welcome_since = Some(std::time::Instant::now());
        self.getting_started_profile_input = None;
    }

    pub(crate) fn mark_getting_started_seen(&mut self) {
        if !self.state.settings.getting_started_seen {
            self.state.settings.getting_started_seen = true;
            atlas_core::util::config::save_settings(&self.state.settings);
        }
    }

    fn submit_getting_started_profile_name(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.getting_started_profile_input.clone() else {
            return;
        };
        let value = input.read(cx).value().trim().to_string();
        if !value.is_empty() && value != self.state.profiles.active().name {
            self.rename_active_profile(value);
        }
    }

    fn getting_started_profile_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let name = self.state.profiles.active().name.clone();
        self.getting_started_profile_input
            .get_or_insert_with(|| {
                crate::ui::text_input::single_line(
                    window,
                    cx,
                    rust_i18n::t!("getting_started_profile_placeholder").to_string(),
                    name,
                )
            })
            .clone()
    }

    pub(crate) fn getting_started_profile_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FocusHandle {
        self.getting_started_profile_input(window, cx)
            .read(cx)
            .focus_handle(cx)
    }

    pub(crate) fn render_getting_started_welcome(
        &mut self,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(started) = self.state.overlay.getting_started_welcome_since else {
            return div().into_any_element();
        };
        let elapsed = started.elapsed().as_millis();
        if elapsed >= WELCOME_MS {
            self.state.overlay.getting_started_welcome_since = None;
            return div().into_any_element();
        }
        window.request_animation_frame();

        let t = (elapsed as f32 / WELCOME_MS as f32).clamp(0.0, 1.0);
        let fade = 1.0 - t;
        let fg = setup_fg();

        deferred(
            div()
                .id("getting-started-welcome")
                .absolute()
                .inset_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0.04, 0.62 * fade))
                .child(
                    v_flex()
                        .items_center()
                        .gap_4()
                        .opacity(fade)
                        .child(atlas_logo(82.0))
                        .child(
                            div()
                                .text_3xl()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(fg)
                                .child(rust_i18n::t!("getting_started_welcome").to_string()),
                        ),
                ),
        )
        .into_any_element()
    }

    fn setup_welcome_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        fg: gpui::Hsla,
        muted: gpui::Hsla,
        border: gpui::Hsla,
    ) -> AnyElement {
        let profile_input = self.getting_started_profile_input(window, cx);
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_2()
                    .items_center()
                    .child(atlas_logo(72.0))
                    .child(
                        div()
                            .text_size(px(28.))
                            .line_height(px(34.))
                            .font_weight(gpui::FontWeight::LIGHT)
                            .child(rust_i18n::t!("getting_started_title").to_string()),
                    )
                    .child(
                        div()
                            .max_w(px(520.))
                            .text_align(gpui::TextAlign::Center)
                            .text_sm()
                            .line_height(px(20.))
                            .text_color(muted)
                            .child(rust_i18n::t!("getting_started_subtitle").to_string()),
                    ),
            )
            .child(profile_field(&profile_input, fg, muted, border))
            .child(welcome_bento(fg, muted, border))
            .into_any_element()
    }

    fn setup_theme_page(
        &self,
        cx: &mut Context<Self>,
        fg: gpui::Hsla,
        muted: gpui::Hsla,
        border: gpui::Hsla,
    ) -> AnyElement {
        let active = self.state.profiles.active().theme.clone();
        let view = cx.entity();
        let themes = gpui_component::ThemeRegistry::global(cx)
            .sorted_themes()
            .iter()
            .filter(|cfg| crate::app::theme::theme_is_available(cfg.name.as_ref()))
            .map(|cfg| {
                let name = cfg.name.to_string();
                let selected = name == active;
                let pick = name.clone();
                let view = view.clone();
                div()
                    .id(SharedString::from(format!("setup-theme-{name}")))
                    .px_3()
                    .py_2()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(if selected {
                        brand::soft().opacity(0.55)
                    } else {
                        border
                    })
                    .bg(if selected {
                        brand::wash_hover()
                    } else {
                        setup_tile()
                    })
                    .text_color(if selected { fg } else { muted })
                    .text_sm()
                    .cursor_pointer()
                    .hover(move |h| h.bg(brand::wash_hover()).text_color(fg))
                    .child(name)
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                        let pick = pick.clone();
                        view.update(cx, |this, cx| {
                            this.process_actions([Action::SetTheme(pick)]);
                            cx.notify();
                        });
                    })
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let theme_list = div()
            .relative()
            .h(px(172.))
            .child(
                v_flex()
                    .id("getting-started-theme-list")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.getting_started_theme_scroll)
                    .pr(px(10.))
                    .gap_2()
                    .children(themes),
            )
            .vertical_scrollbar(&self.getting_started_theme_scroll)
            .into_any_element();

        let icons = v_flex()
            .gap_2()
            .child(category_label("getting_started_icons_category", muted))
            .child(super::guide::icon_theme_pills(
                &view,
                fg,
                muted,
                border,
                setup_tile(),
            ));

        setup_page(
            "getting_started_theme_title",
            "getting_started_theme_desc",
            v_flex()
                .gap_4()
                .child(theme_list)
                .child(icons)
                .into_any_element(),
            muted,
        )
    }

    fn setup_workspace_page(
        &self,
        fg: gpui::Hsla,
        muted: gpui::Hsla,
        border: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let view_mode = self
            .state
            .active_tab_data()
            .map(|tab| tab.view_mode.clone())
            .unwrap_or_else(|| self.state.settings.view_mode.clone());
        let mode_tiles = [
            (
                ViewMode::Details,
                "settings_view_details_btn",
                AtlasIcon::Details,
            ),
            (ViewMode::Grid, "settings_view_grid_btn", AtlasIcon::Grid),
            (ViewMode::List, "settings_view_list_btn", AtlasIcon::List),
            (
                ViewMode::Miller,
                "settings_view_miller_btn",
                AtlasIcon::Columns,
            ),
        ]
        .into_iter()
        .map(|(mode, label, icon_kind)| {
            let selected = view_mode == mode;
            let view = view.clone();
            option_tile(label, icon_kind, selected, fg, muted, border, move |cx| {
                let mode = mode.clone();
                view.update(cx, |this, cx| {
                    this.process_actions([Action::SetViewMode(mode)]);
                    cx.notify();
                });
            })
        })
        .collect::<Vec<_>>();

        let sidebar_on = self.state.settings.show_sidebar;
        let inspector_on = self.state.settings.show_inspector;
        let hidden_on = self.state.settings.show_hidden;
        let sidebar_view = view.clone();
        let inspector_view = view.clone();
        let hidden_view = view.clone();
        let sidebar = option_tile(
            "getting_started_show_sidebar",
            AtlasIcon::SidebarSimple,
            sidebar_on,
            fg,
            muted,
            border,
            move |cx| {
                sidebar_view.update(cx, |this, cx| {
                    this.process_actions([Action::ToggleSidebar]);
                    cx.notify();
                });
            },
        );
        let inspector = option_tile(
            "getting_started_show_inspector",
            AtlasIcon::Eye,
            inspector_on,
            fg,
            muted,
            border,
            move |cx| {
                inspector_view.update(cx, |this, cx| {
                    this.state.settings.show_inspector = !this.state.settings.show_inspector;
                    atlas_core::util::config::save_settings(&this.state.settings);
                    cx.notify();
                });
            },
        );
        let hidden = option_tile(
            "getting_started_show_hidden",
            AtlasIcon::File,
            hidden_on,
            fg,
            muted,
            border,
            move |cx| {
                hidden_view.update(cx, |this, cx| {
                    this.process_actions([Action::ToggleHidden]);
                    cx.notify();
                });
            },
        );

        let mut mode_tiles = mode_tiles.into_iter();
        let layout = v_flex()
            .gap_4()
            .child(
                v_flex()
                    .gap_2()
                    .child(category_label("getting_started_view_category", muted))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                mode_tiles
                                    .next()
                                    .unwrap_or_else(|| div().into_any_element()),
                            )
                            .child(
                                mode_tiles
                                    .next()
                                    .unwrap_or_else(|| div().into_any_element()),
                            )
                            .child(
                                mode_tiles
                                    .next()
                                    .unwrap_or_else(|| div().into_any_element()),
                            )
                            .child(
                                mode_tiles
                                    .next()
                                    .unwrap_or_else(|| div().into_any_element()),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(category_label("getting_started_workspace_category", muted))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(sidebar)
                            .child(inspector)
                            .child(hidden),
                    ),
            );

        setup_page(
            "getting_started_workspace_title",
            "getting_started_workspace_desc",
            layout.into_any_element(),
            muted,
        )
    }
}

/// Profile name as a floating-label field drawn in the brand surface; the input
/// itself is borderless so it inherits the field instead of the app theme.
fn profile_field(
    input: &Entity<InputState>,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
) -> AnyElement {
    h_flex()
        .h(px(60.))
        .pl(px(18.))
        .pr(px(16.))
        .gap_3()
        .items_center()
        .rounded(px(18.))
        .border_1()
        .border_color(border)
        .bg(setup_tile())
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(brand::faint())
                        .child(rust_i18n::t!("getting_started_profile_name").to_string()),
                )
                .child(
                    div().text_color(fg).child(
                        crate::ui::text_input::flat(input)
                            .text_size(px(16.))
                            .text_color(fg)
                            .px_0(),
                    ),
                ),
        )
        .child(icon(AtlasIcon::PencilSimple, 16., muted))
        .into_any_element()
}

/// Bento shell: quiet wash, clipped so oversized decorative icons bleed off the edge.
fn bento_tile(id: &'static str, border: gpui::Hsla) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .relative()
        .overflow_hidden()
        .rounded(px(22.))
        .border_1()
        .border_color(border)
        .bg(setup_tile())
        .p(px(18.))
}

/// Giant low-contrast glyph parked half outside a tile.
fn bento_ghost(icon_kind: AtlasIcon, size: f32, right: f32, top: f32) -> AnyElement {
    div()
        .absolute()
        .right(px(right))
        .top(px(top))
        .child(icon(icon_kind, size, hsla(0., 0., 1., 0.035)))
        .into_any_element()
}

fn bento_title(key: &'static str, size: f32, fg: gpui::Hsla) -> AnyElement {
    div()
        .text_size(px(size))
        .line_height(px(size + 4.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(fg)
        .child(rust_i18n::t!(key).to_string())
        .into_any_element()
}

fn bento_desc(key: &'static str, muted: gpui::Hsla) -> AnyElement {
    div()
        .text_xs()
        .line_height(px(17.))
        .text_color(muted)
        .child(rust_i18n::t!(key).to_string())
        .into_any_element()
}

/// Asymmetric feature bento: one tall hero tile, one wide tile, two odd-sized
/// small ones. Sizes are deliberately uneven.
fn welcome_bento(fg: gpui::Hsla, muted: gpui::Hsla, border: gpui::Hsla) -> AnyElement {
    let profile = bento_tile("getting-started-profile", border)
        .w(px(264.))
        .h_full()
        .flex()
        .flex_col()
        .justify_between()
        .bg(linear_gradient(
            160.,
            linear_color_stop(brand::hot().opacity(0.16), 0.0),
            linear_color_stop(setup_tile(), 0.75),
        ))
        .child(bento_ghost(AtlasIcon::UserSwitch, 176., -58., 46.))
        .child(icon(AtlasIcon::UserSwitch, 44., fg))
        .child(
            v_flex()
                .gap_1()
                .child(bento_title("getting_started_profile", 19., fg))
                .child(bento_desc("getting_started_profile_desc", muted)),
        );

    let palette = bento_tile("getting-started-palette", border)
        .h(px(116.))
        .flex()
        .items_center()
        .gap_4()
        .child(
            v_flex()
                .flex_1()
                .gap_1()
                .child(bento_title("getting_started_palette", 16., fg))
                .child(bento_desc("getting_started_palette_desc", muted)),
        )
        .child(
            div()
                .mr(px(-6.))
                .child(icon(AtlasIcon::Lightning, 60., brand::soft())),
        );

    let extensions = bento_tile("getting-started-ext", border)
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .justify_between()
        .child(bento_ghost(AtlasIcon::PuzzlePiece, 110., -40., 8.))
        .child(icon(AtlasIcon::PuzzlePiece, 30., fg))
        .child(bento_title("getting_started_extensions", 14., fg));

    let integrations = bento_tile("getting-started-integrations", border)
        .w(px(156.))
        .h_full()
        .flex()
        .flex_col()
        .justify_between()
        .child(icon(AtlasIcon::TerminalWindow, 30., brand::soft()))
        .child(bento_title("getting_started_integrations", 14., fg));

    h_flex()
        .h(px(244.))
        .gap_3()
        .child(profile)
        .child(
            v_flex().flex_1().h_full().gap_3().child(palette).child(
                h_flex()
                    .flex_1()
                    .gap_3()
                    .child(extensions)
                    .child(integrations),
            ),
        )
        .into_any_element()
}

fn option_tile(
    label: &'static str,
    icon_kind: AtlasIcon,
    selected: bool,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
    on_click: impl Fn(&mut gpui::App) + 'static,
) -> AnyElement {
    v_flex()
        .flex_1()
        .h(px(92.))
        .items_center()
        .justify_center()
        .gap_2()
        .rounded(px(16.))
        .border_1()
        .border_color(if selected {
            brand::soft().opacity(0.55)
        } else {
            border
        })
        .bg(if selected {
            brand::wash_hover()
        } else {
            setup_tile()
        })
        .cursor_pointer()
        .hover(|h| h.bg(brand::wash_hover()))
        .child(icon(icon_kind, 20., if selected { fg } else { muted }))
        .child(
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(if selected { fg } else { muted })
                .child(rust_i18n::t!(label).to_string()),
        )
        .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
            on_click(cx);
        })
        .into_any_element()
}

fn category_label(label: &'static str, muted: gpui::Hsla) -> AnyElement {
    div()
        .text_xs()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(muted)
        .child(rust_i18n::t!(label).to_string())
        .into_any_element()
}

fn setup_page(
    title: &'static str,
    desc: &'static str,
    content: AnyElement,
    muted: gpui::Hsla,
) -> AnyElement {
    v_flex()
        .gap_4()
        .child(
            v_flex()
                .gap_2()
                .items_center()
                .child(atlas_logo(72.0))
                .child(
                    div()
                        .text_size(px(28.))
                        .line_height(px(34.))
                        .font_weight(gpui::FontWeight::LIGHT)
                        .child(rust_i18n::t!(title).to_string()),
                )
                .child(
                    div()
                        .max_w(px(520.))
                        .text_align(gpui::TextAlign::Center)
                        .text_sm()
                        .line_height(px(20.))
                        .text_color(muted)
                        .child(rust_i18n::t!(desc).to_string()),
                ),
        )
        .child(content)
        .into_any_element()
}

fn step_dots(step: usize) -> AnyElement {
    h_flex()
        .items_center()
        .child(h_flex().gap_1().children((0..3).map(|i| {
            div()
                .w(px(if i == step { 40. } else { 20. }))
                .h(px(2.))
                .rounded_full()
                .bg(if i == step {
                    brand::soft()
                } else if i < step {
                    brand::muted().opacity(0.55)
                } else {
                    brand::rim()
                })
                .into_any_element()
        })))
        .into_any_element()
}
