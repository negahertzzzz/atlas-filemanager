//! Feature guide: a short, interactive help overlay.
//!
//! The overlay lists topics phrased as what the user wants to do. "Start" turns
//! a topic into a *coach*: a small card that stays out of the way while the user
//! really does each step. Progress is detected from the `Action`s that flow
//! through `process_actions`, so the shortcut and the manual route (menus,
//! palette, clicks) both count.

use super::brand;
use crate::app::App;
use crate::input::action::Action;
use crate::ui::icons::{icon, AtlasIcon};
use atlas_model::overlay::GuideCoach;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{deferred, div, px, AnyElement, Context, MouseButton, SharedString};
use gpui_component::kbd::Kbd;
use gpui_component::{h_flex, v_flex};

struct Step {
    /// Command id whose (remappable) shortcut is shown next to the step.
    command: Option<&'static str>,
    /// Detects the step being done, by shortcut or by any manual route.
    hit: Option<fn(&Action) -> bool>,
    /// Fingerprint of app state; any change means the user did the step, even
    /// through a path that never goes through `process_actions` (bottom bar,
    /// settings page, ...).
    probe: Option<fn(&App) -> u64>,
    /// i18n key of the palette command that does the same, shown as a second route.
    palette: Option<&'static str>,
    /// Offer the optional icon-theme downloads under this step.
    icons: bool,
}

struct Topic {
    /// i18n key prefix: `guide_<id>_title|lead|s1..s3`.
    id: &'static str,
    steps: &'static [Step],
}

const fn step(command: Option<&'static str>, hit: fn(&Action) -> bool) -> Step {
    Step {
        command,
        hit: Some(hit),
        probe: None,
        palette: None,
        icons: false,
    }
}

const fn read(command: Option<&'static str>) -> Step {
    Step {
        command,
        hit: None,
        probe: None,
        palette: None,
        icons: false,
    }
}

impl Step {
    const fn watch(mut self, probe: fn(&App) -> u64) -> Self {
        self.probe = Some(probe);
        self
    }

    const fn icons(mut self) -> Self {
        self.icons = true;
        self
    }

    const fn via(mut self, palette_title_key: &'static str) -> Self {
        self.palette = Some(palette_title_key);
        self
    }

    fn detectable(&self) -> bool {
        self.hit.is_some() || self.probe.is_some()
    }
}

fn fp<T: std::hash::Hash>(v: &T) -> u64 {
    use std::hash::Hasher as _;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn watch_view(app: &App) -> u64 {
    fp(&format!(
        "{:?}",
        app.state.active_tab_data().map(|t| t.view_mode.clone())
    ))
}
fn watch_group(app: &App) -> u64 {
    fp(&format!(
        "{:?}",
        app.state.active_tab_data().map(|t| t.group_by)
    ))
}
fn watch_inspector(app: &App) -> u64 {
    fp(&app.state.settings.show_inspector)
}
fn watch_panes(app: &App) -> u64 {
    fp(&app.state.all_pane_tile_ids().len())
}
fn watch_goto(app: &App) -> u64 {
    fp(&app.state.overlay.show_goto_bar)
}
fn watch_palette(app: &App) -> u64 {
    fp(&app.state.overlay.show_palette)
}
fn watch_settings(app: &App) -> u64 {
    fp(&app.state.overlay.show_settings)
}
fn watch_theme(app: &App) -> u64 {
    fp(&app.state.profiles.active().theme)
}
fn watch_converter(app: &App) -> u64 {
    fp(&app.converter.is_some())
}
fn watch_extensions(app: &App) -> u64 {
    fp(&app.state.ext.show_manager)
}

fn is_noise(a: &Action) -> bool {
    matches!(
        a,
        Action::OpenPalette
            | Action::ClosePalette
            | Action::CloseOverlays
            | Action::OpenGuide(_)
            | Action::CloseGuide
            | Action::SetGuideTopic(_)
            | Action::StartGuideCoach(_)
            | Action::StopGuideCoach
            | Action::DownloadIconTheme(_)
    )
}

static TOPICS: [Topic; 11] = [
    Topic {
        id: "goto",
        steps: &[
            step(Some("nav.goto"), |a| matches!(a, Action::OpenGotoBar)).watch(watch_goto),
            step(None, |a| {
                matches!(a, Action::Navigate { .. } | Action::NavigateLocation { .. })
            }),
        ],
    },
    Topic {
        id: "commands",
        steps: &[
            step(Some("sys.palette"), |a| matches!(a, Action::OpenPalette)).watch(watch_palette),
            step(None, |a| !is_noise(a)),
        ],
    },
    Topic {
        id: "views",
        steps: &[
            step(None, |a| matches!(a, Action::SetViewMode(_)))
                .watch(watch_view)
                .via("cmd_view_grid_title"),
            step(None, |a| matches!(a, Action::SetGroupBy(_))).watch(watch_group),
            step(Some("view.inspector"), |a| {
                matches!(a, Action::ToggleInspector)
            })
            .watch(watch_inspector)
            .via("cmd_view_inspector_title"),
        ],
    },
    Topic {
        id: "panes",
        steps: &[
            step(Some("pane.split-h"), |a| {
                matches!(
                    a,
                    Action::SplitHorizontalFocused | Action::SplitVerticalFocused
                )
            })
            .watch(watch_panes)
            .via("cmd_pane_split_h_title"),
            step(Some("tab.new"), |a| {
                matches!(a, Action::NewTabFocused | Action::NewTab(_))
            })
            .watch(watch_panes)
            .via("cmd_tab_new_title"),
            step(Some("pane.close"), |a| {
                matches!(a, Action::CloseFocusedPane)
            })
            .watch(watch_panes)
            .via("cmd_pane_close_title"),
        ],
    },
    Topic {
        id: "convert",
        steps: &[
            read(None),
            step(Some("files.convert"), |a| {
                matches!(a, Action::OpenConverter)
            })
            .watch(watch_converter),
        ],
    },
    Topic {
        id: "analyze",
        steps: &[
            read(None),
            step(None, |a| matches!(a, Action::OpenSpaceLens { .. })),
        ],
    },
    Topic {
        id: "vault",
        steps: &[read(None), read(None), read(None)],
    },
    Topic {
        id: "tags",
        steps: &[
            step(None, |a| {
                matches!(
                    a,
                    Action::OpenCreateTagOverlay { .. } | Action::CreateTag { .. }
                )
            }),
            step(None, |a| matches!(a, Action::AssignTagToFiles { .. })),
            step(None, |a| matches!(a, Action::OpenTagResults { .. })),
        ],
    },
    Topic {
        id: "cloud",
        steps: &[read(None), read(None), read(None)],
    },
    Topic {
        id: "themes",
        steps: &[
            step(Some("sys.settings"), |a| matches!(a, Action::OpenSettings)).watch(watch_settings),
            step(None, |a| matches!(a, Action::SetTheme(_))).watch(watch_theme),
            step(None, |a| matches!(a, Action::DownloadIconTheme(_))).icons(),
        ],
    },
    Topic {
        id: "extensions",
        steps: &[
            step(Some("ext.manager"), |a| matches!(a, Action::OpenExtManager))
                .watch(watch_extensions)
                .via("ext_manager_title"),
            read(None),
            read(None),
        ],
    },
];

fn tr(id: &str, part: &str) -> String {
    let key = format!("guide_{id}_{part}");
    rust_i18n::t!(key.as_str()).to_string()
}

fn extension_manager_button(view: &gpui::Entity<App>) -> AnyElement {
    let view = view.clone();
    brand::ghost(
        "guide-open-extensions",
        rust_i18n::t!("ext_manager_title").to_string(),
    )
    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
        view.update(cx, |this, cx| {
            this.process_actions([Action::CloseGuide, Action::OpenExtManager]);
            cx.notify();
        });
    })
    .into_any_element()
}

fn practicable(topic: &Topic) -> bool {
    topic.steps.iter().any(|s| s.hit.is_some())
}

/// Optional icon-theme downloads: (`icon-manager` command id, label).
pub(crate) const ICON_THEMES: [(&str, &str); 3] = [
    ("icon-manager.download-material", "Material"),
    ("icon-manager.download-catppuccin", "Catppuccin"),
    ("icon-manager.download-flow", "Flow"),
];

/// Row of pills that download an icon theme. Shared by the guide and the
/// first-run setup; colours are passed in because they use different palettes.
pub(crate) fn icon_theme_pills(
    view: &gpui::Entity<App>,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
    fill: gpui::Hsla,
) -> AnyElement {
    ICON_THEMES
        .iter()
        .fold(h_flex().gap_2().flex_wrap(), |row, (cmd, label)| {
            let v = view.clone();
            row.child(
                h_flex()
                    .id(SharedString::from(format!("icon-theme-{cmd}")))
                    .h(px(32.))
                    .px_4()
                    .gap_2()
                    .items_center()
                    .rounded_full()
                    .border_1()
                    .border_color(border)
                    .bg(fill)
                    .text_xs()
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(move |h| h.text_color(fg).border_color(fg.opacity(0.5)))
                    .child(icon(AtlasIcon::Download, 12., muted))
                    .child(*label)
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                        v.update(cx, |this, cx| {
                            this.process_actions([Action::DownloadIconTheme((*cmd).to_string())]);
                            cx.notify();
                        });
                    }),
            )
        })
        .into_any_element()
}

impl App {
    /// Derive names, descriptions and totals from the same live manifests as
    /// the manager, including optional tool providers in their own count.
    fn guide_extension_inventory(&self, fg: gpui::Hsla, muted: gpui::Hsla) -> AnyElement {
        use atlas_model::ext_model::ExtKind;
        let providers = self
            .ext_registry
            .extensions
            .iter()
            .filter(|e| e.manifest.kind == ExtKind::ToolProvider)
            .count();
        let count = self
            .ext_registry
            .extensions
            .iter()
            .filter(|e| !e.manifest.hidden && e.manifest.kind != ExtKind::ToolProvider)
            .count();
        let mut entries: Vec<_> = self
            .ext_registry
            .extensions
            .iter()
            .filter(|e| !e.manifest.hidden)
            .collect();
        entries.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
        v_flex()
            .gap_3()
            .child(
                div().text_sm().text_color(muted).child(
                    rust_i18n::t!("guide_extensions_count", count = count, tools = providers)
                        .to_string(),
                ),
            )
            .children(entries.into_iter().map(|e| {
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_color(fg)
                            .child(format!("{} · {}", e.manifest.name, e.manifest.version)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(e.manifest.description.clone()),
                    )
            }))
            .into_any_element()
    }

    /// "Or: [palette key] “Command name”" — the manual route to the same step.
    fn guide_via(&self, key: Option<&'static str>, muted: gpui::Hsla) -> Option<AnyElement> {
        let key = key?;
        let title = rust_i18n::t!(key).to_string();
        let palette = self.command_shortcut_keystroke("sys.palette");
        Some(
            h_flex()
                .items_center()
                .gap_2()
                .text_xs()
                .text_color(muted)
                .child(rust_i18n::t!("guide_or").to_string())
                .when_some(palette, |row, ks| row.child(Kbd::new(ks)))
                .child(format!("“{title}”"))
                .into_any_element(),
        )
    }

    /// Advance the coach when `action` completes its current step.
    pub(crate) fn observe_guide(&mut self, action: &Action) {
        let Some(GuideCoach { topic, step, .. }) = self.state.overlay.guide_coach else {
            return;
        };
        let Some(t) = TOPICS.get(topic) else {
            self.state.overlay.guide_coach = None;
            return;
        };
        let Some(current) = t.steps.get(step) else {
            return;
        };
        // Reading-only steps have no detector; the card's "Next" button moves on.
        if current.hit.is_some_and(|hit| hit(action)) {
            self.state.overlay.guide_coach = Some(GuideCoach {
                topic,
                step: step + 1,
                base: u64::MAX,
            });
        }
    }

    /// Advance the coach when the state its current step watches changes.
    fn sync_guide_probe(&mut self) {
        let Some(c) = self.state.overlay.guide_coach else {
            return;
        };
        let Some(probe) = TOPICS
            .get(c.topic)
            .and_then(|t| t.steps.get(c.step))
            .and_then(|s| s.probe)
        else {
            return;
        };
        let now = probe(self);
        if c.base == u64::MAX {
            self.state.overlay.guide_coach = Some(GuideCoach { base: now, ..c });
        } else if now != c.base {
            self.state.overlay.guide_coach = Some(GuideCoach {
                step: c.step + 1,
                base: u64::MAX,
                ..c
            });
        }
    }

    pub(crate) fn render_guide(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        let active = self.state.overlay.guide_topic.min(TOPICS.len() - 1);
        let topic = &TOPICS[active];
        let total = TOPICS.len();

        let go = |target: usize| {
            let v = view.clone();
            move |_: &gpui::MouseDownEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
                v.update(cx, |this, cx| {
                    this.process_actions([Action::SetGuideTopic(target)]);
                    cx.notify();
                });
            }
        };

        // Chapter scrubber: one hairline per topic across the top, clickable.
        let scrubber = TOPICS.iter().enumerate().fold(
            h_flex().w_full().gap_1().items_center(),
            |row, (i, t)| {
                row.child(
                    div()
                        .id(SharedString::from(format!("guide-tick-{}", t.id)))
                        .flex_1()
                        .py(px(6.))
                        .cursor_pointer()
                        .child(div().h(px(2.)).rounded_full().bg(if i == active {
                            brand::soft()
                        } else if i < active {
                            brand::muted().opacity(0.55)
                        } else {
                            brand::rim()
                        }))
                        .on_mouse_down(MouseButton::Left, go(i)),
                )
            },
        );

        // Chapter index on the right: numeral + title, the open one in ink.
        let index = TOPICS
            .iter()
            .enumerate()
            .fold(v_flex().gap_0p5(), |col, (i, t)| {
                let selected = i == active;
                col.child(
                    h_flex()
                        .id(SharedString::from(format!("guide-topic-{}", t.id)))
                        .h(px(30.))
                        .px_3()
                        .gap_3()
                        .items_center()
                        .rounded(px(10.))
                        .cursor_pointer()
                        .when(selected, |r| r.bg(brand::wash()))
                        .hover(|h| h.bg(brand::wash()))
                        .child(brand::mono(
                            format!("{:02}", i + 1),
                            if selected {
                                brand::soft()
                            } else {
                                brand::faint()
                            },
                        ))
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(if selected {
                                    brand::ink()
                                } else {
                                    brand::muted()
                                })
                                .truncate()
                                .child(tr(t.id, "title")),
                        )
                        .on_mouse_down(MouseButton::Left, go(i)),
                )
            });

        let steps = topic
            .steps
            .iter()
            .enumerate()
            .fold(v_flex(), |col, (n, s)| {
                let keystroke = s.command.and_then(|c| self.command_shortcut_keystroke(c));
                let via = self.guide_via(s.palette, brand::muted());
                col.child(
                    h_flex()
                        .items_start()
                        .gap_5()
                        .py(px(14.))
                        .border_t_1()
                        .border_color(brand::rim())
                        .child(
                            div()
                                .pt(px(3.))
                                .child(brand::mono(format!("{:02}", n + 1), brand::faint())),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1p5()
                                .child(
                                    div()
                                        .w_full()
                                        .whitespace_normal()
                                        .text_size(px(15.))
                                        .line_height(px(22.))
                                        .text_color(brand::text())
                                        .child(tr(topic.id, &format!("s{}", n + 1))),
                                )
                                .when_some(via, |col, v| col.child(v))
                                .when(s.icons, |col| {
                                    col.child(icon_theme_pills(
                                        &view,
                                        brand::ink(),
                                        brand::muted(),
                                        brand::rim(),
                                        brand::wash(),
                                    ))
                                }),
                        )
                        .when_some(keystroke, |row, ks| row.child(Kbd::new(ks))),
                )
            });

        let start = practicable(topic).then(|| {
            let v = view.clone();
            brand::pill(
                "guide-start",
                rust_i18n::t!("guide_start").to_string(),
                Some(
                    icon(AtlasIcon::ArrowRight, 13., gpui::hsla(0., 0., 0.04, 1.))
                        .into_any_element(),
                ),
            )
            .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                v.update(cx, |this, cx| {
                    this.process_actions([Action::StartGuideCoach(active)]);
                    cx.notify();
                });
            })
        });

        let page = v_flex()
            .id("guide-page")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .pl(px(48.))
            .pr(px(36.))
            .pt(px(28.))
            .pb(px(40.))
            .gap_8()
            .child(
                v_flex()
                    .gap_3()
                    .child(brand::mono(
                        format!("{:02} — {:02}", active + 1, total),
                        brand::soft(),
                    ))
                    .child(
                        div()
                            .text_size(px(36.))
                            .line_height(px(42.))
                            .font_weight(gpui::FontWeight::LIGHT)
                            .text_color(brand::ink())
                            .child(tr(topic.id, "title")),
                    )
                    .child(
                        div()
                            .max_w(px(440.))
                            .text_size(px(17.))
                            .line_height(px(25.))
                            .text_color(brand::muted())
                            .child(tr(topic.id, "lead")),
                    )
                    .when(topic.id == "extensions", |col| {
                        col.child(extension_manager_button(&view))
                    }),
            )
            .child(steps)
            .when(topic.id == "extensions", |col| {
                col.child(self.guide_extension_inventory(brand::text(), brand::muted()))
            })
            .when_some(start, |col, b| {
                col.child(
                    h_flex().gap_4().items_center().child(b).child(
                        div()
                            .flex_1()
                            .text_xs()
                            .line_height(px(17.))
                            .text_color(brand::faint())
                            .child(rust_i18n::t!("guide_hint").to_string()),
                    ),
                )
            });

        let round_btn = |id: &'static str, glyph: AtlasIcon, enabled: bool, target: usize| {
            div()
                .id(id)
                .size(px(30.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(brand::rim())
                .when(!enabled, |d| d.opacity(0.35))
                .when(enabled, |d| {
                    d.cursor_pointer()
                        .hover(|h| h.bg(brand::wash_hover()))
                        .on_mouse_down(MouseButton::Left, go(target))
                })
                .child(icon(glyph, 12., brand::muted()))
        };

        let close_view = view.clone();
        let header = h_flex()
            .px(px(28.))
            .pt(px(22.))
            .gap_4()
            .items_center()
            .child(brand::mono(
                rust_i18n::t!("guide_title").to_string().to_uppercase(),
                brand::faint(),
            ))
            .child(div().flex_1().child(scrubber))
            .child(round_btn(
                "guide-prev",
                AtlasIcon::ArrowLeft,
                active > 0,
                active.saturating_sub(1),
            ))
            .child(round_btn(
                "guide-next",
                AtlasIcon::ArrowRight,
                active + 1 < total,
                (active + 1).min(total - 1),
            ))
            .child(
                div()
                    .id("guide-close")
                    .size(px(30.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|h| h.bg(brand::wash_hover()))
                    .child(icon(AtlasIcon::X, 12., brand::muted()))
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                        close_view.update(cx, |this, cx| {
                            this.process_actions([Action::CloseGuide]);
                            cx.notify();
                        });
                    }),
            );

        let panel = brand::panel()
            .id("guide-panel")
            .w(px(940.))
            .max_w_full()
            .h(px(590.))
            .max_h_full()
            .flex()
            .flex_col()
            .text_color(brand::ink())
            .occlude()
            .child(brand::top_rim())
            .child(brand::warm_edge())
            .child(header)
            .child(
                h_flex().flex_1().min_h_0().child(page).child(
                    div()
                        .id("guide-index")
                        .w(px(232.))
                        .flex_none()
                        .my(px(28.))
                        .mr(px(16.))
                        .pl(px(16.))
                        .border_l_1()
                        .border_color(brand::rim())
                        .overflow_y_scroll()
                        .child(index),
                ),
            );

        let close_view = view;
        let scrim = brand::scrim()
            .id("scrim-guide")
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                close_view.update(cx, |this, cx| {
                    this.process_actions([Action::CloseGuide]);
                    cx.notify();
                });
            })
            .child(panel);
        deferred(scrim).into_any_element()
    }

    /// Floating card that guides the user through one topic while they use the
    /// real app. It never blocks the window: only the card itself takes clicks.
    pub(crate) fn render_guide_coach(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.sync_guide_probe();
        let Some(GuideCoach { topic, step, .. }) = self.state.overlay.guide_coach else {
            return div().into_any_element();
        };
        let topic_ix = topic.min(TOPICS.len() - 1);
        let t = &TOPICS[topic_ix];
        let view = cx.entity();
        let total = t.steps.len();
        let finished = step >= total;
        let current = t.steps.get(step);
        let keystroke = current
            .and_then(|s| s.command)
            .and_then(|c| self.command_shortcut_keystroke(c));
        let via = current.and_then(|s| self.guide_via(s.palette, brand::muted()));

        let ticks = (0..total).fold(h_flex().flex_1().gap_1().items_center(), |row, i| {
            let color = if i < step || finished {
                brand::soft()
            } else if i == step {
                brand::ink().opacity(0.6)
            } else {
                brand::rim()
            };
            row.child(div().flex_1().h(px(2.)).rounded_full().bg(color))
        });

        let stop_view = view.clone();
        let label = if finished {
            tr(t.id, "title").to_uppercase()
        } else {
            format!(
                "{} · {:02}/{:02}",
                tr(t.id, "title").to_uppercase(),
                step + 1,
                total
            )
        };
        let header = h_flex()
            .items_center()
            .gap_4()
            .child(brand::mono(label, brand::faint()))
            .child(ticks)
            .child(
                div()
                    .id("guide-coach-close")
                    .size(px(26.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|h| h.bg(brand::wash_hover()))
                    .child(icon(AtlasIcon::X, 11., brand::muted()))
                    .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                        stop_view.update(cx, |this, cx| {
                            this.process_actions([Action::StopGuideCoach]);
                            cx.notify();
                        });
                    }),
            );

        let advance = {
            let v = view.clone();
            move |_: &gpui::MouseDownEvent, _: &mut gpui::Window, cx: &mut gpui::App| {
                v.update(cx, |this, cx| {
                    this.state.overlay.guide_coach = Some(GuideCoach {
                        topic: topic_ix,
                        step: step + 1,
                        base: u64::MAX,
                    });
                    cx.notify();
                });
            }
        };

        let detect = current.is_some_and(Step::detectable);
        let icons_row = current.is_some_and(|s| s.icons).then(|| {
            icon_theme_pills(
                &view,
                brand::ink(),
                brand::muted(),
                brand::rim(),
                brand::wash(),
            )
        });
        let body = if finished {
            let back_view = view.clone();
            let end_view = view;
            v_flex()
                .gap_5()
                .child(
                    div()
                        .text_size(px(24.))
                        .line_height(px(30.))
                        .font_weight(gpui::FontWeight::LIGHT)
                        .child(rust_i18n::t!("guide_done").to_string()),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            brand::pill(
                                "guide-coach-end",
                                rust_i18n::t!("guide_finish").to_string(),
                                None,
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_ev, _w, cx| {
                                    end_view.update(cx, |this, cx| {
                                        this.process_actions([Action::StopGuideCoach]);
                                        cx.notify();
                                    });
                                },
                            ),
                        )
                        .child(
                            brand::ghost(
                                "guide-coach-back",
                                rust_i18n::t!("guide_back").to_string(),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                move |_ev, _w, cx| {
                                    back_view.update(cx, |this, cx| {
                                        this.process_actions([
                                            Action::StopGuideCoach,
                                            Action::OpenGuide((topic_ix + 1) % TOPICS.len()),
                                        ]);
                                        cx.notify();
                                    });
                                },
                            ),
                        ),
                )
        } else {
            v_flex()
                .gap_4()
                .child(
                    h_flex()
                        .items_start()
                        .justify_between()
                        .gap_4()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    div()
                                        .w_full()
                                        .whitespace_normal()
                                        .text_size(px(20.))
                                        .line_height(px(27.))
                                        .font_weight(gpui::FontWeight::LIGHT)
                                        .text_color(brand::ink())
                                        .child(tr(t.id, &format!("s{}", step + 1))),
                                )
                                .when_some(via, |col, v| col.child(v)),
                        )
                        .when_some(keystroke, |row, ks| row.child(Kbd::new(ks))),
                )
                .when_some(icons_row, |col, r| col.child(r))
                .when(
                    current.is_some_and(|s| s.command == Some("ext.manager")),
                    |col| col.child(extension_manager_button(&view)),
                )
                .child(if detect {
                    h_flex()
                        .items_center()
                        .justify_between()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_2()
                                .child(div().size(px(5.)).rounded_full().bg(brand::soft()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(brand::muted())
                                        .child(rust_i18n::t!("guide_wait").to_string()),
                                ),
                        )
                        .child(
                            div()
                                .id("guide-coach-next")
                                .h(px(30.))
                                .px_3()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .text_xs()
                                .text_color(brand::muted())
                                .cursor_pointer()
                                .hover(|h| h.bg(brand::wash_hover()).text_color(brand::ink()))
                                .child(rust_i18n::t!("guide_skip").to_string())
                                .on_mouse_down(MouseButton::Left, advance),
                        )
                } else {
                    h_flex().child(
                        brand::pill(
                            "guide-coach-next",
                            rust_i18n::t!("guide_next").to_string(),
                            None,
                        )
                        .on_mouse_down(MouseButton::Left, advance),
                    )
                })
        };

        let card = brand::panel()
            .id("guide-coach")
            .w(px(480.))
            .max_w_full()
            .p_6()
            .flex()
            .flex_col()
            .gap_5()
            .text_color(brand::ink())
            .occlude()
            .child(brand::top_rim())
            .child(brand::warm_edge())
            .child(header)
            .child(body);

        deferred(
            div()
                .absolute()
                .bottom(px(32.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(card),
        )
        .into_any_element()
    }
}
