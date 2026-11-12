//! Settings palette renderer + keyboard model.
//!
//! Visual language mirrors the command palette (same shell material, search
//! row, row highlight and footer), but rows carry live controls instead of
//! "run and close" actions:
//!   * ↑↓ move · ←→ adjust sliders / switches / segments, → opens pickers
//!   * Enter toggles or opens · Tab / Shift+Tab cycle categories
//!   * Alt+Backspace resets the selected setting · Esc backs out / closes
//!
//! Pickers drill in place: the search input is reused for the option list,
//! the highlighted option is previewed live (theme, icons, view, font,
//! language) and Esc reverts it.

mod shortcuts;

use super::{
    find, Category, ChoiceOption, Control, Drill, SettingDef, SettingsUi, Slider, SETTINGS,
};
use crate::app::App;
use crate::ui::icons::{icon, AtlasIcon};
use crate::ui::{motion, rem_px};
use atlas_core::search::fuzzy::FuzzyScorer;
use atlas_model::settings::Settings;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    canvas, div, px, relative, uniform_list, AnimationExt, AnyElement, Bounds, Context,
    DispatchPhase, Entity, FontWeight, HighlightStyle, Hsla, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, ScrollStrategy, SharedString, StyledText, Window,
};
use gpui_component::input::{
    Enter as InputEnter, Escape as InputEscape, IndentInline, InputState, MoveDown, MoveLeft,
    MoveRight, MoveUp, OutdentInline,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex, ActiveTheme, StyledExt as _};
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

const PANEL_W: f32 = 740.0;
const PANEL_MAX_H: f32 = 600.0;
const SEARCH_H: f32 = 52.0;
const PILLS_H: f32 = 44.0;
const FOOTER_H: f32 = 36.0;
const HEADER_H: f32 = 30.0;
const ROW_H: f32 = 46.0;
const ROW_DESC_H: f32 = 58.0;
const OPTION_H: f32 = 34.0;
const DROPDOWN_W: f32 = 320.0;
const DROPDOWN_ROWS: usize = 9;
const DROPDOWN_SEARCH_H: f32 = 40.0;
const SLIDER_W: f32 = 168.0;
const SWITCH_MS: u64 = 160;

#[derive(Clone, Copy)]
struct Colors {
    fg: Hsla,
    muted: Hsla,
    accent: Hsla,
    primary: Hsla,
    primary_fg: Hsla,
    border: Hsla,
    surface: Hsla,
}

impl Colors {
    fn from_cx(cx: &gpui::App) -> Self {
        let t = cx.theme();
        Self {
            fg: t.popover_foreground,
            muted: t.popover_foreground.opacity(0.62),
            accent: t.accent,
            primary: t.primary,
            primary_fg: t.primary_foreground,
            border: t.border,
            surface: t.popover,
        }
    }
}

/// One matched setting with its title highlight.
struct Hit {
    def: &'static SettingDef,
    title: SharedString,
    highlight: Option<Vec<Range<usize>>>,
}

/// One matched picker option.
struct OptionHit {
    option: ChoiceOption,
    highlight: Option<Vec<Range<usize>>>,
}

fn highlighted(
    text: SharedString,
    ranges: Option<&Vec<Range<usize>>>,
    base: Hsla,
    hit: Hsla,
    window: &Window,
) -> AnyElement {
    match ranges {
        Some(ranges) if !ranges.is_empty() => {
            let mut style = window.text_style();
            style.color = base;
            let highlights = ranges
                .iter()
                .filter(|r| r.end <= text.len())
                .map(|r| {
                    (
                        r.clone(),
                        HighlightStyle {
                            color: Some(hit),
                            font_weight: Some(FontWeight::SEMIBOLD),
                            ..HighlightStyle::default()
                        },
                    )
                })
                .collect::<Vec<_>>();
            StyledText::new(text)
                .with_default_highlights(&style, highlights)
                .into_any_element()
        }
        _ => text.into_any_element(),
    }
}

fn swatch(colors: [Hsla; 3], ring: Hsla) -> AnyElement {
    h_flex()
        .flex_shrink_0()
        .children(colors.into_iter().enumerate().map(|(i, color)| {
            div()
                .size(rem_px(12.))
                .rounded_full()
                .bg(color)
                .border_1()
                .border_color(ring)
                .when(i > 0, |d| d.ml(rem_px(-4.)))
        }))
        .into_any_element()
}

impl App {
    fn settings_search_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        self.settings_input
            .get_or_insert_with(|| {
                crate::ui::text_input::single_line(
                    window,
                    cx,
                    rust_i18n::t!("sp_search_placeholder"),
                    "",
                )
            })
            .clone()
    }

    /// Fuzzy-match settings (all categories; the caller filters by pill).
    fn settings_hits(&mut self, query: &str) -> Vec<(Hit, u32)> {
        let pattern = FuzzyScorer::parse_pattern(query.trim());
        let mut hits: Vec<(Hit, u32)> = SETTINGS
            .iter()
            .filter_map(|def| {
                let title = def.title();
                let Some(pattern) = &pattern else {
                    return Some((
                        Hit {
                            def,
                            title,
                            highlight: None,
                        },
                        0,
                    ));
                };
                let score = self
                    .fuzzy_scorer
                    .score_pattern(pattern, &title)
                    .map(|s| s + 20)
                    .or_else(|| def.words_match(query).then_some(1))?;
                let highlight = self.fuzzy_scorer.match_ranges_pattern(pattern, &title);
                Some((
                    Hit {
                        def,
                        title,
                        highlight,
                    },
                    score,
                ))
            })
            .collect();
        if pattern.is_some() {
            // Stable: equal scores keep the declaration order.
            hits.sort_by(|a, b| b.1.cmp(&a.1));
        }
        hits
    }

    fn settings_option_hits(&mut self, options: Vec<ChoiceOption>, query: &str) -> Vec<OptionHit> {
        let Some(pattern) = FuzzyScorer::parse_pattern(query.trim()) else {
            return options
                .into_iter()
                .map(|option| OptionHit {
                    option,
                    highlight: None,
                })
                .collect();
        };
        let mut scored: Vec<(OptionHit, u32)> = options
            .into_iter()
            .filter_map(|option| {
                let score = self
                    .fuzzy_scorer
                    .score_pattern(&pattern, &option.label)
                    .or_else(|| {
                        option
                            .detail
                            .as_ref()
                            .and_then(|detail| self.fuzzy_scorer.score_pattern(&pattern, detail))
                            .map(|s| s.saturating_sub(10).max(1))
                    })?;
                let highlight = self
                    .fuzzy_scorer
                    .match_ranges_pattern(&pattern, &option.label);
                Some((OptionHit { option, highlight }, score))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().map(|(hit, _)| hit).collect()
    }

    pub(crate) fn render_settings(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = self.settings_search_input(window, cx);
        if let Some(restore) = self.settings_ui.restore_query.take() {
            let placeholder = rust_i18n::t!("sp_search_placeholder").to_string();
            input.update(cx, |state, cx| {
                state.set_value(restore, window, cx);
                state.set_placeholder(placeholder, window, cx);
            });
            self.settings_ui.last_query = input.read(cx).value().to_string();
        }
        let query = input.read(cx).value().to_string();
        let colors = Colors::from_cx(cx);
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let panel_w = (PANEL_W * zoom).min(vw - 24.0 * zoom).max(1.0);
        let panel_h = (PANEL_MAX_H * zoom)
            .min(vh * 0.82)
            .min(vh - 24.0 * zoom)
            .max(1.0);
        let top = ((vh - panel_h) * 0.32).max(12.0 * zoom);
        let (ease, boost) = crate::app::scroll_smooth_params(self.state.settings.scroll_smoothness);
        self.settings_scroll.set_smooth(ease, boost);

        let drill_def = self
            .settings_ui
            .drill
            .as_ref()
            .and_then(|d| find(d.setting));
        let page = drill_def.is_some_and(|def| matches!(def.control, Control::Page { .. }));
        let searchable_page =
            drill_def.is_some_and(|def| matches!(def.control, Control::Page { searchable: true }));
        // Choices open as a dropdown over the (still visible) list.
        let choice = drill_def.is_some() && !page;
        let (body, mut footer, crumb) = match drill_def {
            Some(def) if searchable_page => {
                let (body, footer) = self.settings_shortcuts_body(&query, colors, cx);
                (body, footer, Some(self.settings_crumb(def, colors, cx)))
            }
            _ if page => self.settings_page_body(colors, cx),
            _ => self.settings_main_body(&query, colors, window, cx),
        };
        let dropdown = if choice {
            let fallback = gpui::point(
                px((vw - panel_w) / 2.0 + panel_w - 24.0 * zoom),
                px(top + (SEARCH_H + PILLS_H) * zoom),
            );
            let (dropdown, choice_footer) =
                self.settings_choice_dropdown(colors, zoom, (vw, vh), fallback, window, cx);
            footer = choice_footer;
            Some(dropdown)
        } else {
            None
        };

        let search_row = h_flex()
            .w_full()
            .h(rem_px(SEARCH_H))
            .flex_shrink_0()
            .px_4()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(colors.border)
            .children(crumb)
            .when(!page || searchable_page, |row| {
                row.child(icon(AtlasIcon::Search, 16.0, colors.muted))
            })
            .child(
                // Pages have nothing to search, but the input keeps keyboard focus.
                div()
                    .min_w(px(0.))
                    .flex_1()
                    .when(page && !searchable_page, |d| d.opacity(0.))
                    .child(crate::ui::text_input::flat(&input).cleanable(true)),
            );

        let view = cx.entity();
        // Font previews restyle the workspace, not the panel you read them in.
        let panel_font = self
            .settings_ui
            .drill
            .as_ref()
            .filter(|d| d.setting == "font-family")
            .and(self.settings_ui.preview_before.clone())
            .unwrap_or_else(|| self.state.settings.font_family.clone());
        let panel = v_flex()
            .id("settings-panel")
            .font_family(panel_font)
            .w(px(panel_w))
            .h(px(panel_h))
            .popover_style(cx)
            .rounded(rem_px(12.))
            .text_color(colors.fg)
            .text_sm()
            .overflow_hidden()
            .occlude()
            .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                this.settings_move(-1);
                cx.stop_propagation();
                cx.notify();
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                this.settings_move(1);
                cx.stop_propagation();
                cx.notify();
            }))
            .capture_action(cx.listener(|this, _: &MoveLeft, window, cx| {
                if this.settings_horizontal(-1, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveRight, window, cx| {
                if this.settings_horizontal(1, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, action: &InputEnter, window, cx| {
                if !action.secondary {
                    this.settings_enter(window, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &IndentInline, _, cx| {
                this.settings_cycle_category(1);
                cx.stop_propagation();
                cx.notify();
            }))
            .capture_action(cx.listener(|this, _: &OutdentInline, _, cx| {
                this.settings_cycle_category(-1);
                cx.stop_propagation();
                cx.notify();
            }))
            .capture_action(cx.listener(|this, _: &InputEscape, window, cx| {
                cx.stop_propagation();
                this.settings_escape(window, cx);
            }))
            .capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                let ks = &ev.keystroke;
                if ks.key == "escape" {
                    cx.stop_propagation();
                    this.settings_escape(window, cx);
                } else if ks.key == "backspace" && ks.modifiers.alt {
                    cx.stop_propagation();
                    this.settings_reset_selected(cx);
                }
            }))
            // A click inside the shell must never reach the dismissing scrim.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(search_row)
            .when(!page, |panel| {
                panel.child(self.settings_pills(&query, colors, cx))
            })
            .child(div().relative().flex_1().min_h(px(0.)).child(body))
            .child(
                h_flex()
                    .w_full()
                    .h(rem_px(FOOTER_H))
                    .flex_shrink_0()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .border_t_1()
                    .border_color(colors.border)
                    .text_xs()
                    .text_color(colors.muted)
                    .children(footer),
            )
            .when(choice, |panel| {
                // While the dropdown is open the list is inert: a click anywhere
                // in the panel dismisses it (and the wheel can't move its anchor).
                let view = cx.entity();
                panel.relative().child(
                    div()
                        .id("sp-dropdown-blocker")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                            cx.stop_propagation();
                            view.update(cx, |app, cx| {
                                app.settings_cancel_drill();
                                app.settings_after_change(cx);
                            });
                        }),
                )
            })
            .children(dropdown);

        let animated = if self.state.settings.disable_animations || cx.reduce_motion() {
            panel.into_any_element()
        } else {
            panel
                .with_animation(
                    "settings-appear",
                    motion::animation(motion::PANEL_MS),
                    |this, delta| this.opacity(delta),
                )
                .into_any_element()
        };
        let positioned = div()
            .absolute()
            .top(px(top))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(animated)
            .into_any_element();
        let focus = self.overlay_focus(cx);
        self.overlay_scrim_custom(
            "scrim-settings",
            view,
            |this| {
                // First outside click only closes an open dropdown.
                if this
                    .settings_ui
                    .drill
                    .as_ref()
                    .is_some_and(|d| d.input.is_some())
                {
                    this.settings_cancel_drill();
                } else {
                    this.close_settings_state();
                }
            },
            focus,
            positioned,
            cx,
        )
    }

    // ── main list ───────────────────────────────────────────────────────────

    fn settings_main_body(
        &mut self,
        query: &str,
        colors: Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Vec<AnyElement>, Option<AnyElement>) {
        if query != self.settings_ui.last_query {
            self.settings_ui.last_query = query.to_string();
            self.settings_ui.selected = 0;
            self.settings_scroll.set_offset(gpui::point(px(0.), px(0.)));
        }
        let searching = !query.trim().is_empty();
        let category = self.settings_ui.category;
        let hits: Vec<Hit> = self
            .settings_hits(query)
            .into_iter()
            .map(|(hit, _)| hit)
            .filter(|hit| category.is_none_or(|c| hit.def.category == c))
            .collect();
        self.settings_ui.visible = hits.iter().map(|hit| hit.def.id).collect();
        if let Some(id) = self.settings_ui.focus_id.take() {
            if let Some(index) = self.settings_ui.visible.iter().position(|v| *v == id) {
                self.settings_ui.selected = index;
                self.settings_ui.scroll_to_selected = true;
            }
        }
        let selected = self.settings_ui.selected.min(hits.len().saturating_sub(1));
        self.settings_ui.selected = selected;
        let defaults = Settings::default();

        let mut children: Vec<AnyElement> = Vec::new();
        let mut child_of_item = Vec::with_capacity(hits.len());
        let mut last_category = None;
        for (i, hit) in hits.iter().enumerate() {
            if !searching && last_category != Some(hit.def.category) {
                last_category = Some(hit.def.category);
                children.push(section_header(hit.def.category, colors));
            }
            child_of_item.push(children.len());
            children.push(self.settings_row(
                hit,
                i,
                i == selected,
                searching,
                &defaults,
                colors,
                window,
                cx,
            ));
        }
        if self.settings_ui.scroll_to_selected {
            self.settings_ui.scroll_to_selected = false;
            if let Some(&child) = child_of_item.get(selected) {
                self.settings_scroll.scroll_to_item(child);
            }
        }

        let body = if hits.is_empty() {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(colors.muted)
                .child(icon(AtlasIcon::Search, 28.0, colors.muted.opacity(0.6)))
                .child(rust_i18n::t!("sp_empty", query = query.trim()).to_string())
                .into_any_element()
        } else {
            div()
                .size_full()
                .relative()
                .child(
                    v_flex()
                        .id("settings-list")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.settings_scroll)
                        .px(rem_px(8.))
                        .py(rem_px(6.))
                        .children(children),
                )
                .vertical_scrollbar(&self.settings_scroll)
                .into_any_element()
        };

        let selected_def = hits.get(selected).map(|hit| hit.def);
        let mut footer = vec![
            div()
                .flex_1()
                .child(rust_i18n::t!("sp_count", count = hits.len()).to_string())
                .into_any_element(),
            hint(colors, &["up", "down"], "sp_hint_move"),
        ];
        match selected_def.map(|def| &def.control) {
            Some(Control::Slider(_) | Control::Segmented { .. }) => {
                footer.push(hint(colors, &["left", "right"], "sp_hint_adjust"))
            }
            Some(Control::Toggle { .. }) => footer.push(hint(colors, &["enter"], "sp_hint_toggle")),
            Some(Control::Choice { .. } | Control::Page { .. }) => {
                footer.push(hint(colors, &["enter"], "sp_hint_open"))
            }
            Some(Control::Action { .. }) => footer.push(hint(colors, &["enter"], "sp_hint_run")),
            _ => {}
        }
        if selected_def.is_some_and(|def| def.is_modified(self, &defaults)) {
            footer.push(hint(colors, &["alt-backspace"], "sp_hint_reset"));
        }
        footer.push(hint(colors, &["tab"], "sp_hint_category"));
        footer.push(hint(colors, &["escape"], "sp_hint_close"));
        (body, footer, None)
    }

    fn settings_pills(
        &mut self,
        query: &str,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let searching = !query.trim().is_empty();
        let counts: Vec<usize> = if searching {
            let hits = self.settings_hits(query);
            Category::ALL
                .iter()
                .map(|c| {
                    hits.iter()
                        .filter(|(hit, _)| hit.def.category == *c)
                        .count()
                })
                .collect()
        } else {
            Vec::new()
        };
        let total: usize = counts.iter().sum();
        let current = self.settings_ui.category;
        let view = cx.entity();
        let pill = |key: usize,
                    category: Option<Category>,
                    label: SharedString,
                    glyph: AtlasIcon,
                    count: Option<usize>| {
            let active = current == category;
            let view = view.clone();
            div()
                .id(SharedString::from(format!("sp-cat-{key}")))
                .accessibility_id(SharedString::from(format!("sp-cat-{key}")))
                .role(gpui::Role::Tab)
                .aria_label(label.clone())
                .aria_selected(active)
                .h(rem_px(26.))
                .px(rem_px(10.))
                .gap(rem_px(6.))
                .flex()
                .flex_shrink_0()
                .items_center()
                .rounded_full()
                .border_1()
                .text_xs()
                .cursor_pointer()
                .when(active, |d| {
                    d.bg(colors.primary.opacity(0.16))
                        .border_color(colors.primary.opacity(0.5))
                        .text_color(colors.fg)
                        .font_weight(FontWeight::MEDIUM)
                })
                .when(!active, |d| {
                    d.border_color(gpui::transparent_black())
                        .text_color(colors.muted)
                        .hover(|h| h.bg(colors.fg.opacity(0.06)).text_color(colors.fg))
                })
                .when(count == Some(0), |d| d.opacity(0.45))
                .child(icon(
                    glyph,
                    12.0,
                    if active { colors.fg } else { colors.muted },
                ))
                .child(label)
                .when_some(count.filter(|_| searching), |d, n| {
                    d.child(
                        div()
                            .px(rem_px(5.))
                            .rounded_full()
                            .bg(colors.fg.opacity(0.08))
                            .text_color(colors.muted)
                            .child(n.to_string()),
                    )
                })
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    view.update(cx, |app, cx| {
                        app.settings_set_category(category);
                        cx.notify();
                    });
                })
        };
        h_flex()
            .w_full()
            .h(rem_px(PILLS_H))
            .flex_shrink_0()
            .px_3()
            .gap_1()
            .items_center()
            .overflow_hidden()
            .border_b_1()
            .border_color(colors.border)
            .child(pill(
                0,
                None,
                rust_i18n::t!("sp_cat_all").to_string().into(),
                AtlasIcon::SlidersHorizontal,
                searching.then_some(total),
            ))
            .children(Category::ALL.iter().enumerate().filter_map(|(i, c)| {
                let count = counts.get(i).copied();
                (count != Some(0) || current == Some(*c))
                    .then(|| pill(i + 1, Some(*c), c.title(), c.icon(), count))
            }))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)] // reason: one row needs all of its render inputs
    fn settings_row(
        &mut self,
        hit: &Hit,
        index: usize,
        selected: bool,
        show_category: bool,
        defaults: &Settings,
        colors: Colors,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let def = hit.def;
        let view = cx.entity();
        let desc = def.desc();
        let modified = def.is_modified(self, defaults);
        let group = SharedString::from(format!("sp-row-{}", def.id));
        let title = highlighted(
            hit.title.clone(),
            hit.highlight.as_ref(),
            colors.fg,
            if selected { colors.fg } else { colors.primary },
            window,
        );
        let control = self.settings_control(def, colors, cx);
        let reset_view = view.clone();
        let reset = modified.then(|| {
            div()
                .id(SharedString::from(format!("sp-reset-{}", def.id)))
                .size(rem_px(24.))
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
                    gpui_component::tooltip::Tooltip::new(
                        rust_i18n::t!("sp_reset_tooltip").to_string(),
                    )
                    .build(window, cx)
                })
                .child(icon(AtlasIcon::ArrowCounterClockwise, 13.0, colors.muted))
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    cx.stop_propagation();
                    reset_view.update(cx, |app, cx| {
                        app.settings_ui.selected = index;
                        if def.reset(app) {
                            app.settings_after_change(cx);
                        }
                    });
                })
        });
        let row_view = view;
        let a11y = match &def.control {
            Control::Toggle { get, .. } => (gpui::Role::Switch, Some(get(self))),
            Control::Choice { .. } | Control::Page { .. } => (gpui::Role::MenuItem, None),
            _ => (gpui::Role::Button, None),
        };
        div()
            .id(SharedString::from(format!("sp-row-{index}")))
            .accessibility_id(SharedString::from(format!("sp-row-{}", def.id)))
            .role(a11y.0)
            .aria_label(hit.title.clone())
            .aria_selected(selected)
            .when_some(a11y.1, |d, on| {
                d.aria_toggled(if on {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
            })
            .group(group)
            .w_full()
            .h(rem_px(if desc.is_some() { ROW_DESC_H } else { ROW_H }))
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap(rem_px(12.))
            .pl(rem_px(10.))
            .pr(rem_px(10.))
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
                    .size(rem_px(30.))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .rounded(rem_px(8.))
                    .bg(colors.fg.opacity(if selected { 0.10 } else { 0.06 }))
                    .child(icon(
                        def.icon,
                        15.0,
                        if selected { colors.fg } else { colors.muted },
                    )),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(px(1.))
                    .child(
                        h_flex()
                            .gap(rem_px(6.))
                            .items_center()
                            .child(div().min_w(px(0.)).truncate().child(title))
                            .when(modified, |d| {
                                d.child(
                                    div()
                                        .size(rem_px(6.))
                                        .flex_shrink_0()
                                        .rounded_full()
                                        .bg(colors.primary),
                                )
                            }),
                    )
                    .when_some(desc, |col, desc| {
                        col.child(
                            div()
                                .text_xs()
                                .text_color(colors.muted)
                                .truncate()
                                .child(desc),
                        )
                    }),
            )
            .when(show_category, |d| {
                d.child(
                    div()
                        .flex_shrink_0()
                        .px(rem_px(6.))
                        .py(px(1.))
                        .rounded(rem_px(5.))
                        .bg(colors.fg.opacity(0.06))
                        .text_xs()
                        .text_color(colors.muted)
                        .child(def.category.title()),
                )
            })
            .children(reset)
            .child(div().flex_shrink_0().child(control))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                row_view.update(cx, |app, cx| {
                    app.settings_ui.selected = index;
                    if !matches!(
                        def.control,
                        Control::Slider(_) | Control::Info { .. } | Control::Segmented { .. }
                    ) {
                        app.settings_activate(def, window, cx);
                    }
                    cx.notify();
                });
            })
            .into_any_element()
    }

    fn settings_control(
        &mut self,
        def: &'static SettingDef,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        let animations = !self.state.settings.disable_animations && !cx.reduce_motion();
        match &def.control {
            Control::Toggle { get, .. } => {
                let animate = animations && self.settings_ui.animate == Some(def.id);
                switch(def.id, get(self), animate, colors)
            }
            Control::Slider(slider) => self.slider(def.id, slider, colors, view),
            Control::Segmented {
                labels,
                icons,
                get,
                set,
                ..
            } => {
                let current = get(self);
                let set = *set;
                let icons: &'static [AtlasIcon] = icons;
                h_flex()
                    .p(rem_px(2.))
                    .gap(rem_px(2.))
                    .rounded(rem_px(7.))
                    .bg(colors.fg.opacity(0.07))
                    .children(labels().into_iter().enumerate().map(|(i, label)| {
                        let view = view.clone();
                        let active = i == current;
                        div()
                            .id(SharedString::from(format!("sp-seg-{}-{i}", def.id)))
                            .h(rem_px(24.))
                            .px(rem_px(if icons.is_empty() { 10. } else { 8. }))
                            .gap(rem_px(5.))
                            .flex()
                            .items_center()
                            .rounded(rem_px(5.))
                            .text_xs()
                            .cursor_pointer()
                            .when(active, |d| {
                                d.bg(colors.surface)
                                    .text_color(colors.fg)
                                    .font_weight(FontWeight::MEDIUM)
                                    .shadow_xs()
                            })
                            .when(!active, |d| {
                                d.text_color(colors.muted)
                                    .hover(|h| h.text_color(colors.fg))
                            })
                            .children(icons.get(i).map(|glyph| {
                                icon(*glyph, 12.0, if active { colors.fg } else { colors.muted })
                            }))
                            .child(label)
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                cx.stop_propagation();
                                view.update(cx, |app, cx| {
                                    app.settings_select_id(def.id);
                                    set(app, i);
                                    app.settings_after_change(cx);
                                });
                            })
                    }))
                    .into_any_element()
            }
            Control::Choice {
                options, current, ..
            } => {
                let current_id = current(self);
                let (label, dots) = options(self, cx)
                    .into_iter()
                    .find(|option| option.id == current_id)
                    .map_or((current_id.clone(), None), |option| {
                        (option.label, option.swatch)
                    });
                let label = if label.is_empty() {
                    rust_i18n::t!("sp_choose").to_string().into()
                } else {
                    label
                };
                let open = self
                    .settings_ui
                    .drill
                    .as_ref()
                    .is_some_and(|d| d.setting == def.id);
                let anchors = self.settings_ui.choice_anchors.clone();
                let id = def.id;
                h_flex()
                    .relative()
                    .child(
                        canvas(
                            move |b, _, _| {
                                anchors.borrow_mut().insert(id, b);
                            },
                            |_, (), _, _| {},
                        )
                        .absolute()
                        .inset_0(),
                    )
                    .when(open, |d| {
                        d.border_color(colors.primary.opacity(0.6))
                            .bg(colors.primary.opacity(0.10))
                    })
                    .h(rem_px(26.))
                    .max_w(rem_px(220.))
                    .px(rem_px(10.))
                    .gap(rem_px(8.))
                    .items_center()
                    .rounded(rem_px(7.))
                    .when(!open, |d| {
                        d.bg(colors.fg.opacity(0.06))
                            .border_color(colors.fg.opacity(0.08))
                    })
                    .border_1()
                    .text_xs()
                    .children(dots.map(|dots| swatch(dots, colors.fg.opacity(0.25))))
                    .child(div().min_w(px(0.)).truncate().child(label))
                    .child(icon(
                        AtlasIcon::CaretDown,
                        11.0,
                        if open { colors.fg } else { colors.muted },
                    ))
                    .into_any_element()
            }
            Control::Action { external, .. } => icon(
                if *external {
                    AtlasIcon::ArrowUpRight
                } else {
                    AtlasIcon::CaretRight
                },
                13.0,
                colors.muted,
            ),
            Control::Info { value } => div()
                .text_xs()
                .text_color(colors.muted)
                .child(value(self))
                .into_any_element(),
            Control::Page { .. } => icon(AtlasIcon::CaretRight, 13.0, colors.muted),
        }
    }

    fn slider(
        &self,
        id: &'static str,
        slider: &Slider,
        colors: Colors,
        view: Entity<Self>,
    ) -> AnyElement {
        let (dragging, pending) = match self.settings_ui.drag {
            Some((drag_id, pending)) if drag_id == id => (true, pending),
            _ => (false, None),
        };
        let value = pending.unwrap_or_else(|| (slider.get)(self));
        let frac = slider.fraction(value);
        let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let prepaint_bounds = bounds.clone();
        let paint_view = view.clone();
        let tracker = canvas(
            move |b, _, _| prepaint_bounds.set(b),
            move |b, (), window, _| {
                if !dragging {
                    return;
                }
                // Window-level listeners keep the drag alive outside the track.
                let move_view = paint_view.clone();
                window.on_mouse_event(move |ev: &MouseMoveEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        move_view
                            .update(cx, |app, cx| app.settings_drag_to(id, ev.position.x, b, cx));
                    }
                });
                let up_view = paint_view.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Bubble {
                        up_view.update(cx, |app, cx| app.settings_end_drag(cx));
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let thumb = if dragging { 16.0 } else { 14.0 };
        h_flex()
            .gap(rem_px(10.))
            .items_center()
            .child(
                div()
                    .id(SharedString::from(format!("sp-slider-{id}")))
                    .accessibility_id(SharedString::from(format!("sp-slider-{id}")))
                    .role(gpui::Role::Slider)
                    .aria_numeric_value(f64::from(value))
                    .w(rem_px(SLIDER_W))
                    .h(rem_px(22.))
                    .px(rem_px(7.))
                    .flex()
                    .items_center()
                    .cursor(gpui::CursorStyle::ResizeLeftRight)
                    .child(
                        div()
                            .relative()
                            .w_full()
                            .h(rem_px(22.))
                            .flex()
                            .items_center()
                            .child(tracker)
                            .child(
                                div()
                                    .w_full()
                                    .h(rem_px(4.))
                                    .rounded_full()
                                    .bg(colors.fg.opacity(0.14)),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .h(rem_px(4.))
                                    .w(relative(frac))
                                    .rounded_full()
                                    .bg(colors.primary),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(relative(frac))
                                    .ml(rem_px(-thumb / 2.0))
                                    .size(rem_px(thumb))
                                    .rounded_full()
                                    .bg(gpui::white())
                                    .border_2()
                                    .border_color(colors.primary)
                                    .shadow_sm(),
                            ),
                    )
                    .on_mouse_down(MouseButton::Left, move |ev, _, cx| {
                        cx.stop_propagation();
                        let b = bounds.get();
                        view.update(cx, |app, cx| {
                            app.settings_select_id(id);
                            app.settings_ui.drag = Some((id, None));
                            app.settings_drag_to(id, ev.position.x, b, cx);
                        });
                    }),
            )
            .child(
                div()
                    .w(rem_px(54.))
                    .text_right()
                    .text_xs()
                    .text_color(if dragging { colors.fg } else { colors.muted })
                    .child(slider.format(value)),
            )
            .into_any_element()
    }

    // ── picker (dropdown) ───────────────────────────────────────────────────

    /// Searchable dropdown for the open choice, anchored under its chip (above
    /// it when the window has no room below). Returns the element and footer.
    fn settings_choice_dropdown(
        &mut self,
        colors: Colors,
        zoom: f32,
        (vw, vh): (f32, f32),
        fallback: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Vec<AnyElement>) {
        let Some(def) = self
            .settings_ui
            .drill
            .as_ref()
            .and_then(|d| find(d.setting))
        else {
            return (div().into_any_element(), Vec::new());
        };
        let Control::Choice {
            options,
            current,
            preview,
            ..
        } = &def.control
        else {
            return (div().into_any_element(), Vec::new());
        };
        let Some(input) = self
            .settings_ui
            .drill
            .as_ref()
            .and_then(|d| d.input.clone())
        else {
            return (div().into_any_element(), Vec::new());
        };
        let query = input.read(cx).value().to_string();
        let options = options(self, cx);
        let committed = current(self);
        let hits = self.settings_option_hits(options, &query);
        let count = hits.len();
        let Some(drill) = self.settings_ui.drill.as_mut() else {
            return (div().into_any_element(), Vec::new());
        };
        if query != drill.last_query {
            drill.last_query.clone_from(&query);
            drill.selected = 0;
            self.settings_ui
                .options_scroll
                .scroll_to_item(0, ScrollStrategy::Top);
        }
        drill.selected = drill.selected.min(count.saturating_sub(1));
        drill.visible = hits.iter().map(|hit| hit.option.id.clone()).collect();
        let selected_id = drill.visible.get(drill.selected).cloned();
        if let Some(preview) = preview {
            if selected_id.is_some() && drill.previewed != selected_id {
                drill.previewed = selected_id.clone();
                preview(self, selected_id.as_deref());
                self.notify_all_panes(cx);
            }
        }

        // Geometry (window px). Short lists (view, icons) shrink to fit.
        let search_h = DROPDOWN_SEARCH_H * zoom;
        let pad = 6.0 * zoom;
        let rows = count.clamp(1, DROPDOWN_ROWS) as f32;
        let list_h = rows * OPTION_H * zoom + pad * 2.0;
        let height = search_h + list_h + 2.0;
        let width = (DROPDOWN_W * zoom).min(vw - 16.0);
        let anchor = self
            .settings_ui
            .choice_anchors
            .borrow()
            .get(def.id)
            .copied();
        let (right, below, above) = match anchor {
            Some(b) => (
                f32::from(b.right()),
                f32::from(b.bottom()) + 6.0 * zoom,
                f32::from(b.top()) - 6.0 * zoom,
            ),
            None => (
                f32::from(fallback.x),
                f32::from(fallback.y),
                f32::from(fallback.y),
            ),
        };
        let fits_below = below + height <= vh - 8.0;
        let top = if fits_below || above - height < 8.0 {
            below.min(vh - 8.0 - height).max(8.0)
        } else {
            above - height
        };
        let left = (right - width).clamp(8.0, (vw - width - 8.0).max(8.0));

        let hits = Rc::new(hits);
        let view = cx.entity();
        let list = uniform_list(
            "settings-options",
            count,
            cx.processor(
                move |this: &mut App, range: Range<usize>, window: &mut Window, _cx| {
                    let selected = this.settings_ui.drill.as_ref().map_or(0, |d| d.selected);
                    range
                        .filter_map(|i| hits.get(i).map(|hit| (i, hit)))
                        .map(|(i, hit)| {
                            option_row(
                                hit,
                                i,
                                i == selected,
                                hit.option.id == committed,
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
        .px(px(pad))
        .py(px(pad))
        .track_scroll(&self.settings_ui.options_scroll);
        let list = if count == 0 {
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(colors.muted)
                .child(rust_i18n::t!("sp_empty", query = query.trim()).to_string())
                .into_any_element()
        } else {
            div()
                .size_full()
                .relative()
                .child(list)
                .vertical_scrollbar(&self.settings_ui.options_scroll)
                .into_any_element()
        };

        let panel = v_flex()
            .id("sp-dropdown")
            .accessibility_id("sp-dropdown")
            .role(gpui::Role::ListBox)
            .aria_label(plain_title(def))
            .w(px(width))
            .h(px(height))
            .popover_style(cx)
            .rounded(rem_px(10.))
            .overflow_hidden()
            .occlude()
            .text_color(colors.fg)
            .text_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(
                h_flex()
                    .w_full()
                    .h(px(search_h))
                    .flex_shrink_0()
                    .px(rem_px(10.))
                    .gap(rem_px(8.))
                    .items_center()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(icon(AtlasIcon::Search, 13.0, colors.muted))
                    .child(
                        div()
                            .min_w(px(0.))
                            .flex_1()
                            .child(crate::ui::text_input::flat(&input).cleanable(true)),
                    ),
            )
            .child(div().h(px(list_h)).w_full().child(list));
        let panel = if self.state.settings.disable_animations || cx.reduce_motion() {
            panel.into_any_element()
        } else {
            let rise = if top < below - 1.0 { -4.0 } else { 4.0 };
            panel
                .with_animation(
                    SharedString::from(format!("sp-dropdown-{}", def.id)),
                    motion::animation(motion::PANEL_MS),
                    move |this, delta| {
                        let t = motion::ease_out_quart(delta);
                        this.opacity(t).mt(px(rise * (1.0 - t)))
                    },
                )
                .into_any_element()
        };
        let _ = window;
        let element = gpui::deferred(
            gpui::anchored()
                .position(gpui::point(px(left), px(top)))
                .child(panel),
        )
        .priority(2)
        .into_any_element();

        let footer = vec![
            div()
                .flex_1()
                .child(rust_i18n::t!("sp_options_count", count = count).to_string())
                .into_any_element(),
            hint(
                colors,
                &["up", "down"],
                if preview.is_some() {
                    "sp_hint_preview"
                } else {
                    "sp_hint_move"
                },
            ),
            hint(colors, &["enter"], "sp_hint_apply"),
            hint(colors, &["escape"], "sp_hint_back"),
        ];
        (element, footer)
    }

    fn settings_page_body(
        &mut self,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Vec<AnyElement>, Option<AnyElement>) {
        let Some(def) = self
            .settings_ui
            .drill
            .as_ref()
            .and_then(|d| find(d.setting))
        else {
            return (div().into_any_element(), Vec::new(), None);
        };
        let body = div()
            .size_full()
            .relative()
            .child(
                v_flex()
                    .id("settings-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.settings_scroll)
                    .px_5()
                    .py_3()
                    .child(self.render_theme_editor(cx)),
            )
            .vertical_scrollbar(&self.settings_scroll)
            .into_any_element();
        let footer = vec![hint(colors, &["escape"], "sp_hint_back")];
        (body, footer, Some(self.settings_crumb(def, colors, cx)))
    }

    /// "‹ Settings / Theme" pill shown in the search row while drilled in.
    fn settings_crumb(
        &self,
        def: &'static SettingDef,
        colors: Colors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = cx.entity();
        h_flex()
            .id("sp-crumb")
            .flex_shrink_0()
            .h(rem_px(28.))
            .pl(rem_px(6.))
            .pr(rem_px(10.))
            .gap(rem_px(6.))
            .items_center()
            .rounded(rem_px(7.))
            .bg(colors.fg.opacity(0.06))
            .text_xs()
            .cursor_pointer()
            .hover(|h| h.bg(colors.fg.opacity(0.10)))
            .child(icon(AtlasIcon::ArrowLeft, 12.0, colors.muted))
            .child(
                div()
                    .text_color(colors.muted)
                    .child(rust_i18n::t!("settings_title").to_string()),
            )
            .child(div().text_color(colors.muted).child("/"))
            .child(icon(def.icon, 12.0, colors.fg))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(plain_title(def)),
            )
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                cx.stop_propagation();
                view.update(cx, |app, cx| app.settings_leave_drill(false, cx));
            })
            .into_any_element()
    }

    // ── behaviour ───────────────────────────────────────────────────────────

    /// Persist, repaint cached panes and the shell after any mutation.
    pub(crate) fn settings_after_change(&mut self, cx: &mut Context<Self>) {
        self.commit_settings();
        self.notify_all_panes(cx);
        cx.notify();
    }

    fn settings_select_id(&mut self, id: &'static str) {
        if let Some(index) = self.settings_ui.visible.iter().position(|v| *v == id) {
            self.settings_ui.selected = index;
        }
    }

    fn settings_selected_def(&self) -> Option<&'static SettingDef> {
        self.settings_ui
            .visible
            .get(self.settings_ui.selected)
            .and_then(|id| find(id))
    }

    fn settings_set_category(&mut self, category: Option<Category>) {
        self.settings_ui.category = category;
        self.settings_ui.selected = 0;
        self.settings_scroll.set_offset(gpui::point(px(0.), px(0.)));
    }

    fn settings_cycle_category(&mut self, dir: i32) {
        if self.settings_ui.drill.is_some() {
            return;
        }
        let slots = Category::ALL.len() as i32 + 1;
        let current = self
            .settings_ui
            .category
            .and_then(|c| Category::ALL.iter().position(|x| *x == c))
            .map_or(0, |i| i as i32 + 1);
        let next = (current + dir).rem_euclid(slots);
        self.settings_set_category(if next == 0 {
            None
        } else {
            Category::ALL.get(next as usize - 1).copied()
        });
    }

    fn settings_move(&mut self, delta: i32) {
        let (selected, count) = match &mut self.settings_ui.drill {
            Some(drill) => (&mut drill.selected, drill.visible.len()),
            None => (
                &mut self.settings_ui.selected,
                self.settings_ui.visible.len(),
            ),
        };
        if count == 0 {
            return;
        }
        let next = (*selected as i64 + i64::from(delta)).rem_euclid(count as i64) as usize;
        *selected = next;
        if self.settings_ui.drill.is_some() {
            self.settings_ui
                .options_scroll
                .scroll_to_item(next, ScrollStrategy::Nearest);
        } else {
            self.settings_ui.scroll_to_selected = true;
        }
    }

    /// ←/→ — returns whether the key was consumed (otherwise it moves the caret).
    fn settings_horizontal(
        &mut self,
        dir: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.settings_ui.drill.is_some() {
            let input = self
                .settings_ui
                .drill
                .as_ref()
                .and_then(|d| d.input.as_ref());
            let empty = input.is_none_or(|input| input.read(cx).value().is_empty());
            if dir < 0 && empty {
                self.settings_leave_drill(false, cx);
                return true;
            }
            return false;
        }
        let Some(def) = self.settings_selected_def() else {
            return false;
        };
        match &def.control {
            Control::Slider(slider) => {
                let value = slider.snap((slider.get)(self) + slider.step * dir as f32);
                (slider.set)(self, value);
            }
            Control::Toggle { get, set, .. } => {
                let on = dir > 0;
                if get(self) == on {
                    return true;
                }
                set(self, on);
                self.settings_ui.animate = Some(def.id);
            }
            Control::Segmented {
                labels, get, set, ..
            } => {
                let len = labels().len() as i32;
                let next = (get(self) as i32 + dir).clamp(0, len - 1) as usize;
                set(self, next);
            }
            Control::Choice { .. } | Control::Page { .. } if dir > 0 => {
                self.settings_enter_drill(def, window, cx);
                return true;
            }
            _ => return false,
        }
        self.settings_after_change(cx);
        true
    }

    fn settings_on_shortcut_page(&self) -> bool {
        self.settings_ui
            .drill
            .as_ref()
            .and_then(|d| find(d.setting))
            .is_some_and(|def| matches!(def.control, Control::Page { searchable: true }))
    }

    fn settings_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_on_shortcut_page() {
            self.settings_shortcut_capture_selected(cx);
        } else if self.settings_ui.drill.is_some() {
            self.settings_leave_drill(true, cx);
        } else if let Some(def) = self.settings_selected_def() {
            self.settings_activate(def, window, cx);
        }
    }

    fn settings_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_ui.drill.is_some() {
            self.settings_leave_drill(false, cx);
        } else {
            self.close_settings_state();
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn settings_reset_selected(&mut self, cx: &mut Context<Self>) {
        if self.settings_on_shortcut_page() {
            self.settings_shortcut_reset_selected(cx);
            return;
        }
        if self.settings_ui.drill.is_some() {
            return;
        }
        if let Some(def) = self.settings_selected_def() {
            if def.reset(self) {
                self.settings_after_change(cx);
            }
        }
    }

    fn settings_activate(
        &mut self,
        def: &'static SettingDef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &def.control {
            Control::Toggle { get, set, .. } => {
                let on = !get(self);
                set(self, on);
                self.settings_ui.animate = Some(def.id);
            }
            Control::Segmented {
                labels, get, set, ..
            } => {
                let len = labels().len().max(1);
                set(self, (get(self) + 1) % len);
            }
            Control::Choice { .. } | Control::Page { .. } => {
                self.settings_enter_drill(def, window, cx);
                return;
            }
            Control::Action { run, .. } => {
                run(self);
                if !self.state.overlay.show_settings {
                    self.focus_handle.focus(window, cx);
                }
            }
            Control::Slider(_) | Control::Info { .. } => return,
        }
        self.settings_after_change(cx);
    }

    fn settings_enter_drill(
        &mut self,
        def: &'static SettingDef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_choice = matches!(def.control, Control::Choice { .. });
        if def.id == "icon-style" {
            super::refresh_icon_inventory();
        }
        let selected = match &def.control {
            Control::Choice {
                options, current, ..
            } => {
                let committed = current(self);
                options(self, cx)
                    .iter()
                    .position(|o| o.id == committed)
                    .unwrap_or(0)
            }
            _ => 0,
        };
        // The dropdown gets its own field so the main query stays put behind it.
        let (input, input_sub) = if is_choice {
            let placeholder = rust_i18n::t!("sp_search_in", name = plain_title(def)).to_string();
            let input = crate::ui::text_input::single_line(window, cx, placeholder, "");
            let sub = cx.subscribe(
                &input,
                |_, _, ev: &gpui_component::input::InputEvent, cx| {
                    if matches!(ev, gpui_component::input::InputEvent::Change) {
                        cx.notify();
                    }
                },
            );
            (Some(input), Some(sub))
        } else {
            (None, None)
        };
        let searchable = matches!(def.control, Control::Page { searchable: true });
        let outer_query = searchable.then(|| {
            let main = self.settings_search_input(window, cx);
            let query = main.read(cx).value().to_string();
            let placeholder = rust_i18n::t!("sp_search_in", name = plain_title(def)).to_string();
            main.update(cx, |state, cx| {
                state.set_value("", window, cx);
                state.set_placeholder(placeholder, window, cx);
            });
            query
        });
        self.settings_ui.drill = Some(Drill {
            setting: def.id,
            input,
            _input_sub: input_sub,
            outer_query,
            selected,
            last_query: String::new(),
            previewed: None,
            visible: Vec::new(),
        });
        self.settings_ui
            .options_scroll
            .scroll_to_item(selected, ScrollStrategy::Center);
        if !is_choice {
            self.settings_scroll.set_offset(gpui::point(px(0.), px(0.)));
        }
        cx.notify();
    }

    /// Leave the picker; `apply` commits the highlighted option, otherwise the
    /// live preview reverts.
    fn settings_leave_drill(&mut self, apply: bool, cx: &mut Context<Self>) {
        let Some(drill) = self.settings_ui.drill.as_ref() else {
            return;
        };
        let was_page = drill.input.is_none();
        let restore = drill.outer_query.clone();
        let picked = drill.visible.get(drill.selected).cloned();
        let def = find(drill.setting);
        self.settings_ui.restore_query = restore;
        self.cancel_command_shortcut_capture();
        match (apply, def.map(|d| &d.control), picked) {
            (true, Some(Control::Choice { pick, .. }), Some(id)) => {
                self.settings_ui.drill = None;
                pick(self, &id);
            }
            _ => self.settings_cancel_drill(),
        }
        self.settings_ui.drill = None;
        self.settings_ui.scroll_to_selected = was_page;
        self.settings_after_change(cx);
    }

    fn settings_drag_to(
        &mut self,
        id: &'static str,
        x: Pixels,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(Control::Slider(slider)) = find(id).map(|d| &d.control) else {
            return;
        };
        let width = f32::from(bounds.size.width);
        if width <= 0.0 {
            return;
        }
        let frac = (f32::from(x - bounds.left()) / width).clamp(0.0, 1.0);
        let value = slider.snap(slider.min + frac * (slider.max - slider.min));
        if slider.live {
            if ((slider.get)(self) - value).abs() > f32::EPSILON {
                (slider.set)(self, value);
                self.notify_all_panes(cx);
            }
        } else {
            self.settings_ui.drag = Some((id, Some(value)));
        }
        cx.notify();
    }

    fn settings_end_drag(&mut self, cx: &mut Context<Self>) {
        let Some((id, pending)) = self.settings_ui.drag.take() else {
            return;
        };
        if let (Some(value), Some(Control::Slider(slider))) =
            (pending, find(id).map(|d| &d.control))
        {
            (slider.set)(self, value);
        }
        self.settings_after_change(cx);
    }
}

/// Title without a trailing ellipsis ("Edit theme…" → "Edit theme").
fn plain_title(def: &SettingDef) -> String {
    def.title().trim_end_matches(['…', '.']).to_string()
}

fn section_header(category: Category, colors: Colors) -> AnyElement {
    h_flex()
        .w_full()
        .h(rem_px(HEADER_H))
        .flex_shrink_0()
        .px(rem_px(10.))
        .pt(rem_px(6.))
        .gap(rem_px(6.))
        .items_center()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(colors.muted)
        .child(icon(category.icon(), 12.0, colors.muted))
        .child(category.title().to_uppercase())
        .into_any_element()
}

/// Footer key badge, matching the command palette's ("↑", "Enter", "Esc").
fn badge(text: &str, colors: Colors) -> AnyElement {
    let glyph = match text {
        "up" => "↑",
        "down" => "↓",
        "left" => "←",
        "right" => "→",
        "enter" => "Enter",
        "tab" => "Tab",
        "escape" => "Esc",
        "alt-backspace" => "Alt ⌫",
        "backspace" => "⌫",
        other => other,
    };
    div()
        .px(rem_px(5.))
        .py(px(1.))
        .rounded_sm()
        .bg(colors.fg.opacity(0.07))
        .border_1()
        .border_color(colors.fg.opacity(0.06))
        .flex_shrink_0()
        .child(glyph.to_string())
        .into_any_element()
}

fn hint(colors: Colors, keys: &[&str], label: &str) -> AnyElement {
    h_flex()
        .gap(rem_px(4.))
        .items_center()
        .flex_shrink_0()
        .children(keys.iter().map(|k| badge(k, colors)))
        .child(rust_i18n::t!(label).to_string())
        .into_any_element()
}

fn switch(id: &'static str, on: bool, animate: bool, colors: Colors) -> AnyElement {
    const TRAVEL: f32 = 14.0;
    let knob = div()
        .absolute()
        .top(rem_px(2.))
        .size(rem_px(16.))
        .rounded_full()
        .bg(if on { colors.primary_fg } else { gpui::white() })
        .shadow_sm();
    let knob = if animate {
        knob.with_animation(
            SharedString::from(format!("sp-switch-{id}-{on}")),
            motion::animation(SWITCH_MS),
            move |knob, delta| {
                let t = motion::ease_out_quart(delta);
                let t = if on { t } else { 1.0 - t };
                knob.left(rem_px(2.0 + TRAVEL * t))
            },
        )
        .into_any_element()
    } else {
        knob.left(rem_px(if on { 2.0 + TRAVEL } else { 2.0 }))
            .into_any_element()
    };
    div()
        .relative()
        .w(rem_px(34.))
        .h(rem_px(20.))
        .flex_shrink_0()
        .rounded_full()
        .bg(if on {
            colors.primary
        } else {
            colors.fg.opacity(0.18)
        })
        .child(knob)
        .into_any_element()
}

fn option_row(
    hit: &OptionHit,
    index: usize,
    selected: bool,
    committed: bool,
    colors: Colors,
    view: Entity<App>,
    window: &Window,
) -> AnyElement {
    let option = &hit.option;
    let label = highlighted(
        option.label.clone(),
        hit.highlight.as_ref(),
        colors.fg,
        if selected { colors.fg } else { colors.primary },
        window,
    );
    let hover_view = view.clone();
    div()
        .id(SharedString::from(format!("sp-opt-{index}")))
        .accessibility_id(SharedString::from(format!("sp-opt-{index}")))
        .role(gpui::Role::ListBoxOption)
        .aria_label(option.label.clone())
        .aria_selected(selected)
        .w_full()
        .h(rem_px(OPTION_H))
        .flex()
        .items_center()
        .gap(rem_px(10.))
        .px(rem_px(10.))
        .rounded(rem_px(7.))
        .border_1()
        .border_color(gpui::transparent_black())
        .cursor_pointer()
        .when(selected, |d| {
            d.bg(colors.accent.opacity(0.14))
                .border_color(colors.accent.opacity(0.45))
        })
        .child(
            div()
                .size(rem_px(16.))
                .flex_shrink_0()
                .children(committed.then(|| icon(AtlasIcon::Check, 14.0, colors.primary))),
        )
        .children(
            option
                .swatch
                .map(|dots| swatch(dots, colors.fg.opacity(0.25))),
        )
        .child(
            div()
                .min_w(px(0.))
                .flex_1()
                .truncate()
                .when_some(option.font.clone(), |d, font| d.font_family(font))
                .when(committed, |d| d.font_weight(FontWeight::MEDIUM))
                .child(label),
        )
        .children(option.detail.clone().map(|detail| {
            div()
                .flex_shrink_0()
                .text_xs()
                .text_color(colors.muted)
                .child(detail)
        }))
        .on_hover(move |hovered, _, cx| {
            if *hovered {
                hover_view.update(cx, |app, cx| {
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
                app.settings_leave_drill(true, cx);
            });
        })
        .into_any_element()
}

impl SettingsUi {
    /// Fresh state for an open that should land on `category`.
    pub(crate) fn opened_at(category: Option<Category>) -> Self {
        Self {
            category,
            ..Self::default()
        }
    }
}
