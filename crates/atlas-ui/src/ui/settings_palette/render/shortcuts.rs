//! Keyboard-shortcut editor: a searchable settings page listing every
//! remappable command with its current binding.
//!
//! Enter / click starts capture (the next key combination becomes the
//! binding), Backspace during capture removes it, Esc cancels, Alt+Backspace
//! restores the built-in default. Capture itself is the shared engine in
//! `input/shortcuts.rs` (conflicts, reserved keys, per-profile storage); while
//! it runs, keyboard focus leaves the search field so keys reach the root
//! capture handler instead of being typed.

use super::{highlighted, hint, Colors};
use crate::app::App;
use crate::ui::icons::{icon, AtlasIcon};
use crate::ui::palette::ShortcutCommand;
use crate::ui::rem_px;
use atlas_core::search::fuzzy::FuzzyScorer;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, px, uniform_list, AnyElement, Context, FontWeight, Keystroke, MouseButton, ScrollStrategy,
    SharedString, Window,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex};
use std::ops::Range;
use std::rc::Rc;

const SHORTCUT_ROW_H: f32 = 42.0;

struct ShortcutHit {
    command: ShortcutCommand,
    highlight: Option<Vec<Range<usize>>>,
    keys: Option<String>,
    customized: bool,
}

/// Key caps for a stored binding ("ctrl-shift-t" → [Ctrl] [Shift] [T]).
fn key_caps(keys: &str, colors: Colors, strong: bool) -> AnyElement {
    let label = Keystroke::parse(keys).map_or_else(
        |_| keys.to_string(),
        |k| gpui_component::kbd::Kbd::format(&k),
    );
    let parts: Vec<String> = if label.len() > 1 && label.contains('+') {
        label
            .split('+')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect()
    } else {
        vec![label]
    };
    h_flex()
        .gap(rem_px(3.))
        .flex_shrink_0()
        .children(parts.into_iter().map(|part| {
            div()
                .min_w(rem_px(20.))
                .h(rem_px(20.))
                .px(rem_px(6.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(rem_px(5.))
                .bg(colors.fg.opacity(if strong { 0.10 } else { 0.07 }))
                .border_1()
                .border_color(colors.fg.opacity(0.10))
                .text_xs()
                .text_color(if strong { colors.fg } else { colors.muted })
                .font_weight(FontWeight::MEDIUM)
                .child(part)
        }))
        .into_any_element()
}

impl App {
    fn shortcut_hits(&mut self, query: &str, cx: &gpui::App) -> Vec<ShortcutHit> {
        let prefs = self.command_shortcut_prefs();
        let commands = self.shortcut_commands(cx);
        let pattern = FuzzyScorer::parse_pattern(query.trim());
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut scored: Vec<(ShortcutHit, u32)> = commands
            .into_iter()
            .filter_map(|command| {
                let keys = prefs.shortcut_string(command.id);
                let customized = prefs.customized(command.id);
                let Some(pattern) = &pattern else {
                    return Some((
                        ShortcutHit {
                            command,
                            highlight: None,
                            keys,
                            customized,
                        },
                        0,
                    ));
                };
                let score = self
                    .fuzzy_scorer
                    .score_pattern(pattern, &command.title)
                    .map(|s| s + 20)
                    .or_else(|| {
                        let haystack = format!(
                            "{} {} {} {} {}",
                            command.title,
                            command.group,
                            command.keywords,
                            command.id,
                            keys.as_deref().unwrap_or_default()
                        )
                        .to_lowercase();
                        words
                            .iter()
                            .all(|w| haystack.contains(w.as_str()))
                            .then_some(1)
                    })?;
                let highlight = self
                    .fuzzy_scorer
                    .match_ranges_pattern(pattern, &command.title);
                Some((
                    ShortcutHit {
                        command,
                        highlight,
                        keys,
                        customized,
                    },
                    score,
                ))
            })
            .collect();
        if pattern.is_some() {
            scored.sort_by(|a, b| b.1.cmp(&a.1));
        }
        scored.into_iter().map(|(hit, _)| hit).collect()
    }

    pub(super) fn settings_shortcuts_body(
        &mut self,
        query: &str,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Vec<AnyElement>) {
        let hits = self.shortcut_hits(query, cx);
        let count = hits.len();
        let capturing = self.command_shortcut_capture.clone();
        let conflict = self.shortcut_conflict_message(cx);
        let Some(drill) = self.settings_ui.drill.as_mut() else {
            return (div().into_any_element(), Vec::new());
        };
        if query != drill.last_query {
            drill.last_query = query.to_string();
            drill.selected = 0;
            self.settings_ui
                .options_scroll
                .scroll_to_item(0, ScrollStrategy::Top);
        }
        drill.selected = drill.selected.min(count.saturating_sub(1));
        drill.visible = hits
            .iter()
            .map(|hit| SharedString::from(hit.command.id))
            .collect();
        let selected = drill.selected;
        let selected_customized = hits.get(selected).is_some_and(|hit| hit.customized);

        let hits = Rc::new(hits);
        let view = cx.entity();
        let row_capture = capturing.clone();
        let list = uniform_list(
            "settings-shortcuts",
            count,
            cx.processor(
                move |this: &mut App, range: Range<usize>, window: &mut Window, _cx| {
                    let selected = this.settings_ui.drill.as_ref().map_or(0, |d| d.selected);
                    range
                        .filter_map(|i| hits.get(i).map(|hit| (i, hit)))
                        .map(|(i, hit)| {
                            let capturing_row = row_capture.as_deref() == Some(hit.command.id);
                            let conflict = conflict.clone().filter(|_| capturing_row);
                            shortcut_row(
                                hit,
                                i,
                                i == selected,
                                capturing_row,
                                conflict,
                                colors,
                                view.clone(),
                                window,
                            )
                        })
                        .collect::<Vec<_>>()
                },
            ),
        )
        .size_full()
        .px(rem_px(8.))
        .py(rem_px(6.))
        .track_scroll(&self.settings_ui.options_scroll);

        // Capture moves focus here (off the search field) — see module docs.
        let focus = self.overlay_focus(cx);
        let body = if count == 0 {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(colors.muted)
                .child(rust_i18n::t!("sp_empty", query = query.trim()).to_string())
                .into_any_element()
        } else {
            div()
                .size_full()
                .relative()
                .track_focus(&focus)
                .child(list)
                .vertical_scrollbar(&self.settings_ui.options_scroll)
                .into_any_element()
        };

        let footer = if capturing.is_some() {
            vec![
                div()
                    .flex_1()
                    .text_color(colors.fg)
                    .child(rust_i18n::t!("sp_shortcut_press").to_string())
                    .into_any_element(),
                hint(colors, &["backspace"], "sp_hint_remove"),
                hint(colors, &["escape"], "sp_hint_cancel"),
            ]
        } else {
            let mut footer = vec![
                div()
                    .flex_1()
                    .child(rust_i18n::t!("sp_shortcut_count", count = count).to_string())
                    .into_any_element(),
                hint(colors, &["up", "down"], "sp_hint_move"),
                hint(colors, &["enter"], "sp_hint_change"),
            ];
            if selected_customized {
                footer.push(hint(colors, &["alt-backspace"], "sp_hint_reset"));
            }
            footer.push(hint(colors, &["escape"], "sp_hint_back"));
            footer
        };
        (body, footer)
    }

    /// Enter on the shortcut page: (re)bind the highlighted command.
    pub(super) fn settings_shortcut_capture_selected(&mut self, cx: &mut Context<Self>) {
        let id = self
            .settings_ui
            .drill
            .as_ref()
            .and_then(|d| d.visible.get(d.selected).cloned());
        if let Some(id) = id {
            self.start_command_shortcut_capture(id.to_string());
            cx.notify();
        }
    }

    /// Alt+Backspace on the shortcut page: back to the built-in binding.
    pub(super) fn settings_shortcut_reset_selected(&mut self, cx: &mut Context<Self>) {
        let id = self
            .settings_ui
            .drill
            .as_ref()
            .and_then(|d| d.visible.get(d.selected).cloned());
        if let Some(id) = id {
            self.reset_command_shortcut(&id);
            cx.notify();
        }
    }
}

#[allow(clippy::too_many_arguments)] // reason: one row needs all of its render inputs
fn shortcut_row(
    hit: &ShortcutHit,
    index: usize,
    selected: bool,
    capturing: bool,
    conflict: Option<String>,
    colors: Colors,
    view: gpui::Entity<App>,
    window: &Window,
) -> AnyElement {
    let command = &hit.command;
    let id = command.id;
    let title = highlighted(
        command.title.clone(),
        hit.highlight.as_ref(),
        colors.fg,
        if selected { colors.fg } else { colors.primary },
        window,
    );
    let group = SharedString::from(format!("sp-sc-{id}"));
    let keys: AnyElement = if capturing {
        h_flex()
            .h(rem_px(24.))
            .px(rem_px(10.))
            .gap(rem_px(6.))
            .items_center()
            .rounded(rem_px(6.))
            .border_1()
            .border_color(colors.primary)
            .bg(colors.primary.opacity(0.12))
            .text_xs()
            .text_color(colors.fg)
            .child(icon(AtlasIcon::Keyboard, 12.0, colors.primary))
            .child(rust_i18n::t!("sp_shortcut_listening").to_string())
            .into_any_element()
    } else if let Some(keys) = hit.keys.as_deref() {
        key_caps(keys, colors, selected)
    } else {
        div()
            .text_xs()
            .text_color(colors.muted.opacity(0.7))
            .child(rust_i18n::t!("sp_shortcut_none").to_string())
            .into_any_element()
    };
    let reset_view = view.clone();
    let reset = hit.customized.then(|| {
        div()
            .id(SharedString::from(format!("sp-sc-reset-{id}")))
            .size(rem_px(22.))
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .rounded(rem_px(6.))
            .cursor_pointer()
            .when(!selected, |d| {
                d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
            })
            .hover(|h| h.bg(colors.fg.opacity(0.10)))
            .tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new(rust_i18n::t!("sp_reset_tooltip").to_string())
                    .build(window, cx)
            })
            .child(icon(AtlasIcon::ArrowCounterClockwise, 12.0, colors.muted))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                reset_view.update(cx, |app, cx| {
                    app.reset_command_shortcut(id);
                    cx.notify();
                });
            })
    });
    let hover_view = view.clone();
    div()
        .id(SharedString::from(format!("sp-sc-{index}")))
        .accessibility_id(SharedString::from(format!("sp-sc-{id}")))
        .role(gpui::Role::Button)
        .aria_label(command.title.clone())
        .aria_selected(selected)
        .group(group)
        .w_full()
        .h(rem_px(SHORTCUT_ROW_H))
        .flex()
        .items_center()
        .gap(rem_px(10.))
        .px(rem_px(10.))
        .rounded(rem_px(8.))
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .when(selected, |d| {
            d.bg(colors.accent.opacity(0.14))
                .border_color(colors.accent.opacity(0.45))
        })
        .when(!selected, |d| d.hover(|h| h.bg(colors.fg.opacity(0.045))))
        .child(
            div()
                .size(rem_px(26.))
                .flex()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .rounded(rem_px(7.))
                .bg(colors.fg.opacity(if selected { 0.10 } else { 0.06 }))
                .child(icon(
                    command.icon,
                    13.0,
                    if selected { colors.fg } else { colors.muted },
                )),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w(px(0.))
                .child(
                    h_flex()
                        .gap(rem_px(6.))
                        .items_center()
                        .child(div().min_w(px(0.)).truncate().child(title))
                        .when(hit.customized, |d| {
                            d.child(
                                div()
                                    .size(rem_px(6.))
                                    .flex_shrink_0()
                                    .rounded_full()
                                    .bg(colors.primary),
                            )
                        }),
                )
                .when_some(conflict, |col, conflict| {
                    col.child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(gpui::red().opacity(0.85))
                            .child(conflict),
                    )
                }),
        )
        .child(
            div()
                .flex_shrink_0()
                .px(rem_px(6.))
                .py(px(1.))
                .rounded(rem_px(5.))
                .bg(colors.fg.opacity(0.06))
                .text_xs()
                .text_color(colors.muted)
                .child(command.group.clone()),
        )
        .children(reset)
        .child(
            div()
                .flex_shrink_0()
                .min_w(rem_px(96.))
                .flex()
                .justify_end()
                .child(keys),
        )
        .on_hover(move |hovered, _, cx| {
            if *hovered {
                hover_view.update(cx, |app, cx| {
                    if app.command_shortcut_capture.is_some() {
                        return;
                    }
                    if let Some(drill) = app.settings_ui.drill.as_mut() {
                        if drill.selected != index {
                            drill.selected = index;
                            cx.notify();
                        }
                    }
                });
            }
        })
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            cx.stop_propagation();
            view.update(cx, |app, cx| {
                if let Some(drill) = app.settings_ui.drill.as_mut() {
                    drill.selected = index;
                }
                app.start_command_shortcut_capture(id.to_string());
                cx.notify();
            });
        })
        .into_any_element()
}
