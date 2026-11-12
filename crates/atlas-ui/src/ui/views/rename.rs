use crate::app::App;
use atlas_model::clamp::ClampLenient;
use atlas_model::pane::TabId;
use gpui::prelude::*;
use gpui::{div, px, AnyElement, Entity, Hsla, MouseButton, Pixels, SharedString, TextRun, Window};
use gpui_component::{input::InputState, ActiveTheme, Colorize};
use std::path::Path;

pub(super) fn active_input(
    app: &App,
    pane_id: TabId,
    entry_idx: usize,
) -> Option<Entity<InputState>> {
    app.rename_edit
        .as_ref()
        .filter(|edit| {
            edit.pane == pane_id
                && app
                    .state
                    .tab_states
                    .get(&pane_id)
                    .and_then(|ts| ts.entries.get(entry_idx))
                    .is_some_and(|entry| entry.path() == edit.path)
        })
        .map(|edit| edit.input.clone())
}

pub(super) fn active_input_for_path(
    app: &App,
    pane_id: TabId,
    path: &Path,
) -> Option<Entity<InputState>> {
    app.rename_edit
        .as_ref()
        .filter(|edit| edit.pane == pane_id && edit.path == path)
        .map(|edit| edit.input.clone())
}

pub(super) const DIM_OPACITY: f32 = 0.45;

pub(super) fn cut_path_opacity(app: &gpui::WeakEntity<App>, path: &Path, cx: &gpui::App) -> f32 {
    if app
        .upgrade()
        .is_some_and(|entity| entity.read(cx).state.system_cut_paths.contains(path))
    {
        0.5
    } else {
        1.0
    }
}

pub(super) fn cut_entry_opacity(
    app: &gpui::WeakEntity<App>,
    pane: TabId,
    index: usize,
    cx: &gpui::App,
) -> f32 {
    app.upgrade().map_or(1.0, |entity| {
        let app = entity.read(cx);
        app.state
            .tab_states
            .get(&pane)
            .and_then(|tab| tab.entries.get(index))
            .map_or(1.0, |entry| {
                if app.state.system_cut_paths.contains(&entry.path()) {
                    0.5
                } else {
                    1.0
                }
            })
    })
}

pub(super) fn dim_other_entry(app: &App, pane_id: TabId, entry_idx: usize) -> bool {
    app.rename_edit.as_ref().is_some_and(|edit| {
        edit.pane == pane_id
            && app.state.batch_rename.is_none()
            && active_input(app, pane_id, entry_idx).is_none()
    })
}

pub(super) fn dim_other_path(app: &App, pane_id: TabId, path: &Path) -> bool {
    app.rename_edit.as_ref().is_some_and(|edit| {
        edit.pane == pane_id && app.state.batch_rename.is_none() && edit.path != path
    })
}

/// `max_px` and the return value are real pixels — callers pass an already
/// zoom-scaled bound (`font_size` likewise already reflects the current zoom).
/// Only the fixed lower bound (72px at zoom 1) is scaled internally, since it
/// has no other source to inherit the scale from.
pub(super) fn input_width(name: &str, max_px: f32, font_size: Pixels, window: &mut Window) -> f32 {
    let width = text_width(name, font_size, window);
    let scale = f32::from(window.rem_size()) / crate::ui::BASE_PX;
    let max_px = max_px.max(0.0);
    // Include both insets, the caret and fractional glyph rounding. A tight
    // text-only width can scroll even a fully visible name by a few pixels.
    (width.ceil() + 6.0 * scale + 4.0).clamp_lenient((72.0 * scale).min(max_px), max_px)
}

fn text_width(name: &str, font_size: Pixels, window: &mut Window) -> f32 {
    let text: SharedString = name.to_string().into();
    let style = window.text_style();
    let run = TextRun {
        len: text.len(),
        font: style.font(),
        color: Hsla::default(),
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: None,
    };
    window
        .text_system()
        .shape_line(text, font_size, &[run], None)
        .width()
        .as_f32()
}

/// `height` is the exact real-pixel box the plain (non-editing) name text
/// occupies at this row — callers pass `window.line_height()` (list/grid) or
/// the row's own already-scaled height (details), never a separate guessed
/// constant. Matching it exactly is what keeps the input from jumping a few
/// px relative to the static text it replaces when rename starts/ends.
pub(super) fn input_element(
    input: &Entity<InputState>,
    view: &Entity<App>,
    height: Pixels,
    font_size: Pixels,
    window: &mut Window,
    cx: &gpui::App,
) -> AnyElement {
    input_element_aligned(input, view, height, font_size, false, window, cx)
}

pub(super) fn centered_input_element(
    input: &Entity<InputState>,
    view: &Entity<App>,
    height: Pixels,
    font_size: Pixels,
    width: Pixels,
    window: &mut Window,
    cx: &gpui::App,
) -> AnyElement {
    // Center names that fit, like the static grid label. Overflowing names
    // start at the left and use normal cursor scrolling instead of hiding
    // their first letters outside a centered viewport.
    let value = input.read(cx).value();
    let scale = f32::from(window.rem_size()) / crate::ui::BASE_PX;
    let centered = text_width(&value, font_size, window) + 6.0 * scale + 4.0 <= width.as_f32();
    input_element_aligned(input, view, height, font_size, centered, window, cx)
}

fn input_element_aligned(
    input: &Entity<InputState>,
    view: &Entity<App>,
    height: Pixels,
    font_size: Pixels,
    centered: bool,
    window: &mut Window,
    cx: &gpui::App,
) -> AnyElement {
    let commit_view = view.clone();
    let theme = cx.theme();
    let foreground = theme.foreground;
    let selection = theme.selection;
    let padding = px(3.0 * f32::from(window.rem_size()) / crate::ui::BASE_PX);
    div()
        .id(("inline-rename-input", input.entity_id()))
        .w_full()
        .h(height)
        .bg(gpui::transparent_black())
        .text_color(foreground)
        .rounded_sm()
        .overflow_hidden()
        .on_mouse_down(MouseButton::Left, |_ev, _window, cx| {
            cx.stop_propagation();
        })
        .on_mouse_down(MouseButton::Right, |_ev, _window, cx| {
            cx.stop_propagation();
        })
        .on_click(|_ev, _window, cx| {
            // A cursor/selection click belongs to the editor, not the file row.
            cx.stop_propagation();
        })
        .child(
            crate::ui::text_input::flat(input)
                .min_h(height)
                .max_h(height)
                .px(padding)
                .py_0()
                .selection_color(selection)
                .text_size(font_size)
                .line_height(height)
                .when(centered, |input| input.text_center()),
        )
        .capture_action(window.listener_for(view, App::rename_next_action))
        .capture_action(window.listener_for(view, App::rename_previous_action))
        .capture_key_down(move |ev, window, cx| match ev.keystroke.key.as_str() {
            key if commit_view.read(cx).state.batch_rename.is_some()
                && !ev.keystroke.modifiers.control
                && !ev.keystroke.modifiers.platform
                && {
                    let key = if ev.keystroke.key_char.as_deref() == Some("#") {
                        "#"
                    } else {
                        key
                    };
                    commit_view.update(cx, |this, cx| {
                        this.batch_rename_key(
                            key,
                            ev.keystroke.modifiers.alt,
                            ev.keystroke.modifiers.shift,
                            window,
                            cx,
                        )
                    })
                } =>
            {
                cx.stop_propagation();
            }
            "enter" => {
                commit_view.update(cx, App::commit_rename);
                cx.stop_propagation();
            }
            "tab" => {
                let forward = !ev.keystroke.modifiers.shift;
                commit_view.update(cx, |this, cx| {
                    this.commit_rename_and_move(forward, window, cx)
                });
                cx.stop_propagation();
            }
            "escape" => {
                commit_view.update(cx, App::cancel_rename);
                cx.stop_propagation();
            }
            _ => {}
        })
        .map(|editor| {
            if cx.reduce_motion() {
                editor.into_any_element()
            } else {
                use gpui::AnimationExt;
                editor
                    .with_animation(
                        "rename-editor-enter",
                        gpui::Animation::new(std::time::Duration::from_millis(100)),
                        |editor, t| {
                            editor.opacity(0.55 + 0.45 * crate::ui::motion::ease_out_quart(t))
                        },
                    )
                    .into_any_element()
            }
        })
}
