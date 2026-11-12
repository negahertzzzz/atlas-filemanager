//! Native run of the settings palette: categories, search, switches, sliders,
//! live-preview pickers (Esc reverts, Enter commits) and the command-palette
//! settings section. Writes screenshots plus `tests.json`.
use super::{control_bounds, key, mouse_down, type_text};
use crate::app::App;
use crate::input::action::Action;
use crate::ui::settings_palette::Category;
use anyhow::ensure;
use gpui::{px, size, Entity, Window};
use std::{path::PathBuf, time::Duration};

fn present(window: &mut Window, cx: &mut gpui::App) {
    window.refresh();
    window.draw(cx).clear(cx);
}

fn shot(
    window: &mut Window,
    cx: &mut gpui::App,
    directory: &std::path::Path,
    name: &str,
) -> anyhow::Result<String> {
    present(window, cx);
    present(window, cx);
    let file = format!("{name}.png");
    window.render_to_image()?.save(directory.join(&file))?;
    Ok(file)
}

fn clear_query(view: &Entity<App>, window: &mut Window, cx: &mut gpui::App) {
    let input = view.read(cx).settings_input.clone();
    if let Some(input) = input {
        input.update(cx, |state, cx| state.set_value("", window, cx));
    }
    present(window, cx);
}

pub(super) fn install(
    view: &Entity<App>,
    window: &mut Window,
    cx: &mut gpui::App,
    directory: PathBuf,
) {
    rust_i18n::set_locale("en");
    window.set_a11y_forced(true);
    let handle = window.window_handle();
    let view = view.clone();
    cx.spawn(async move |cx| {
        let result: anyhow::Result<Vec<serde_json::Value>> = async {
            let mut checks = Vec::new();
            let mut pass = |name: &str, shot: Option<String>| {
                checks.push(
                    serde_json::json!({"name": name, "status": "passed", "screenshot": shot}),
                );
            };
            handle.update(cx, |_, window, cx| {
                window.resize(size(px(1280.), px(800.)));
                view.update(cx, |app, cx| {
                    app.state.overlay.show_getting_started = false;
                    app.state.settings.disable_animations = true;
                    app.process_actions([Action::OpenSettings]);
                    cx.notify();
                });
                present(window, cx);
            })?;
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;

            handle.update(cx, |_, window, cx| -> anyhow::Result<()> {
                let file = shot(window, cx, &directory, "01-all")?;
                let visible = view.read(cx).settings_ui.visible.len();
                ensure!(
                    visible >= 35,
                    "Expected the full registry, saw {visible} rows"
                );
                pass("Settings opens on every category with all rows", Some(file));

                key("tab", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.category == Some(Category::General),
                    "Tab did not select General"
                );
                key("tab", window, cx)?;
                let file = shot(window, cx, &directory, "02-appearance")?;
                ensure!(
                    view.read(cx).settings_ui.category == Some(Category::Appearance),
                    "Tab did not advance"
                );
                ensure!(
                    view.read(cx)
                        .settings_ui
                        .visible
                        .iter()
                        .all(|id| crate::ui::settings_palette::find(id)
                            .is_some_and(|d| d.category == Category::Appearance)),
                    "Category filter leaked rows"
                );
                pass("Tab cycles category pills and filters rows", Some(file));
                key("shift-tab", window, cx)?;
                key("shift-tab", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.category.is_none(),
                    "Shift+Tab did not return to All"
                );

                type_text("hidden", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.visible.first() == Some(&"show-hidden"),
                    "Search did not rank Show hidden first: {:?}",
                    view.read(cx).settings_ui.visible
                );
                let before = view.read(cx).state.settings.show_hidden;
                key("enter", window, cx)?;
                let file = shot(window, cx, &directory, "03-search-toggle")?;
                ensure!(
                    view.read(cx).state.settings.show_hidden != before,
                    "Enter did not flip the switch"
                );
                ensure!(
                    view.read(cx).state.overlay.show_settings,
                    "Enter on a switch closed Settings"
                );
                key("left", window, cx)?;
                present(window, cx);
                ensure!(
                    !view.read(cx).state.settings.show_hidden,
                    "Left did not turn the switch off"
                );
                pass("Search ranks, Enter toggles, Left turns off", Some(file));

                clear_query(&view, window, cx);
                type_text("glass", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.visible.first() == Some(&"glass-opacity"),
                    "Glass slider not first"
                );
                let matched = view.read(cx).settings_ui.visible.len();
                ensure!(
                    matched <= 3,
                    "Search too loose: 'glass' matched {matched} rows: {:?}",
                    view.read(cx).settings_ui.visible
                );
                let before = view.read(cx).state.settings.glass_opacity;
                key("right", window, cx)?;
                key("right", window, cx)?;
                let file = shot(window, cx, &directory, "04-slider")?;
                let after = view.read(cx).state.settings.glass_opacity;
                ensure!(
                    (after - (before + 0.1).min(1.0)).abs() < 1e-3,
                    "Right did not step the slider: {before} → {after}"
                );
                ensure!(
                    view.read(cx)
                        .settings_input
                        .as_ref()
                        .is_some_and(|i| i.read(cx).value() == "glass"),
                    "Arrow keys edited the query"
                );
                key("alt-backspace", window, cx)?;
                present(window, cx);
                ensure!(
                    (view.read(cx).state.settings.glass_opacity
                        - atlas_model::settings::default_glass_opacity())
                    .abs()
                        < 1e-3,
                    "Alt+Backspace did not reset"
                );
                pass(
                    "Arrows step sliders without touching the query; Alt+Backspace resets",
                    Some(file),
                );

                // Pointer drag: press near the left end, drag past the right
                // end (outside the track) and release — window-level listeners.
                let track = control_bounds("sp-slider-glass-opacity", window)?;
                let start = gpui::point(track.left() + px(10.), track.center().y);
                mouse_down(start, window, cx);
                present(window, cx);
                let low = view.read(cx).state.settings.glass_opacity;
                ensure!(
                    low < 0.4,
                    "Pressing the track start did not jump the value: {low}"
                );
                let far = gpui::point(track.right() + px(120.), track.center().y + px(60.));
                window.dispatch_event(
                    gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                        position: far,
                        pressed_button: Some(gpui::MouseButton::Left),
                        modifiers: gpui::Modifiers::default(),
                    }),
                    cx,
                );
                present(window, cx);
                ensure!(
                    (view.read(cx).state.settings.glass_opacity - 1.0).abs() < 1e-3,
                    "Dragging past the end did not clamp to max"
                );
                window.dispatch_event(
                    gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        position: far,
                        button: gpui::MouseButton::Left,
                        modifiers: gpui::Modifiers::default(),
                        click_count: 1,
                    }),
                    cx,
                );
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.drag.is_none(),
                    "Release outside the track did not end the drag"
                );
                ensure!(
                    view.read(cx).state.overlay.show_settings,
                    "Releasing outside the panel closed Settings"
                );
                pass(
                    "Slider drags with the pointer, clamps, and releases outside the track",
                    None,
                );

                clear_query(&view, window, cx);
                type_text("theme", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.visible.first() == Some(&"theme"),
                    "Theme row not first: {:?}",
                    view.read(cx).settings_ui.visible
                );
                let original = view.read(cx).state.profiles.active().theme.clone();
                key("enter", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.drill.is_some(),
                    "Enter did not open the theme picker"
                );
                ensure!(
                    view.read(cx)
                        .settings_ui
                        .drill
                        .as_ref()
                        .and_then(|d| d.input.as_ref())
                        .is_some_and(|i| i.read(cx).value().is_empty()),
                    "Dropdown search not empty"
                );
                ensure!(
                    view.read(cx)
                        .settings_input
                        .as_ref()
                        .is_some_and(|i| i.read(cx).value() == "theme"),
                    "Dropdown cleared the main query"
                );
                let file_dd = shot(window, cx, &directory, "05a-theme-dropdown")?;
                key("down", window, cx)?;
                present(window, cx);
                let file = shot(window, cx, &directory, "05-theme-preview")?;
                let previewed = view.read(cx).state.profiles.active().theme.clone();
                ensure!(previewed != original, "Down did not preview another theme");
                key("escape", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.drill.is_none(),
                    "Escape did not leave the picker"
                );
                ensure!(
                    view.read(cx).state.overlay.show_settings,
                    "Escape in the picker closed Settings"
                );
                ensure!(
                    view.read(cx).state.profiles.active().theme == original,
                    "Escape did not revert the preview"
                );
                ensure!(
                    view.read(cx)
                        .settings_input
                        .as_ref()
                        .is_some_and(|i| i.read(cx).value() == "theme"),
                    "Outer query not kept"
                );
                pass("Theme dropdown anchors over the list", Some(file_dd));
                pass(
                    "Theme dropdown previews live; Esc reverts and keeps the query",
                    Some(file),
                );

                key("enter", window, cx)?;
                key("down", window, cx)?;
                present(window, cx);
                key("enter", window, cx)?;
                present(window, cx);
                let committed = view.read(cx).state.profiles.active().theme.clone();
                ensure!(
                    committed != original && view.read(cx).settings_ui.drill.is_none(),
                    "Enter did not commit the theme"
                );
                ensure!(
                    view.read(cx).theme_preview_before.is_none(),
                    "Commit left a pending preview"
                );
                view.update(cx, |app, _| app.set_theme(original.clone()));
                present(window, cx);
                pass("Enter commits the highlighted theme", None);

                clear_query(&view, window, cx);
                type_text("font family", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.visible.first() == Some(&"font-family"),
                    "Font row not first"
                );
                let font = view.read(cx).state.settings.font_family.clone();
                key("enter", window, cx)?;
                present(window, cx);
                let count = view
                    .read(cx)
                    .settings_ui
                    .drill
                    .as_ref()
                    .map_or(0, |d| d.visible.len());
                ensure!(count > 3, "Font picker listed only {count} families");
                key("down", window, cx)?;
                key("down", window, cx)?;
                key("down", window, cx)?;
                let file = shot(window, cx, &directory, "06-font-picker")?;
                ensure!(
                    view.read(cx).state.settings.font_family != font,
                    "Font browsing did not preview"
                );
                key("escape", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).state.settings.font_family == font,
                    "Font preview not reverted"
                );
                pass(
                    &format!("Font picker lists {count} installed families with live preview"),
                    Some(file),
                );

                clear_query(&view, window, cx);
                type_text("file icons", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.visible.first() == Some(&"icon-style"),
                    "Icon row not first: {:?}",
                    view.read(cx).settings_ui.visible
                );
                ensure!(
                    !view
                        .read(cx)
                        .settings_ui
                        .visible
                        .iter()
                        .any(|id| *id == "icon-theme"),
                    "Duplicate icon-theme row still listed"
                );
                key("enter", window, cx)?;
                present(window, cx);
                let ids: Vec<String> = view
                    .read(cx)
                    .settings_ui
                    .drill
                    .as_ref()
                    .map(|d| d.visible.iter().map(ToString::to_string).collect())
                    .unwrap_or_default();
                ensure!(
                    ids.iter().any(|id| id == "windows")
                        && ids.iter().any(|id| id.contains("flow")),
                    "Icon dropdown missing styles or themes: {ids:?}"
                );
                type_text("flo", window, cx)?;
                present(window, cx);
                let file = shot(window, cx, &directory, "06a-icons-dropdown")?;
                let first = view
                    .read(cx)
                    .settings_ui
                    .drill
                    .as_ref()
                    .and_then(|d| d.visible.first().cloned());
                ensure!(
                    first.as_deref().is_some_and(|id| id.contains("flow")),
                    "Dropdown search did not filter: {first:?}"
                );
                key("escape", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.drill.is_none(),
                    "Escape did not close the icon dropdown"
                );
                pass(
                    "One icon setting: styles + downloadable themes, searchable",
                    Some(file),
                );

                clear_query(&view, window, cx);
                type_text("edit theme", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.visible.first() == Some(&"theme-editor"),
                    "Theme editor row not first"
                );
                key("enter", window, cx)?;
                let file = shot(window, cx, &directory, "06b-theme-editor")?;
                ensure!(
                    view.read(cx)
                        .settings_ui
                        .drill
                        .as_ref()
                        .is_some_and(|d| d.setting == "theme-editor"),
                    "Theme editor page did not open"
                );
                pass("Theme editor opens as its own page", Some(file));
                let before = view.read(cx).state.profiles.active().theme.clone();
                view.update(cx, |app, cx| app.fork_active_theme(window, cx));
                let file = shot(window, cx, &directory, "06b2-theme-editing")?;
                ensure!(
                    view.read(cx).theme_editor.is_some(),
                    "Fork did not open the editor"
                );
                ensure!(
                    view.read(cx).state.profiles.active().theme != before,
                    "Fork did not become the active theme"
                );
                view.update(cx, |app, cx| {
                    app.close_theme_editor(cx);
                    app.set_theme(before.clone());
                });
                present(window, cx);
                key("escape", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.drill.is_none()
                        && view.read(cx).state.overlay.show_settings,
                    "Esc did not return from the page"
                );
                pass(
                    "Fork opens colour editing with live preview; Esc returns",
                    Some(file),
                );

                clear_query(&view, window, cx);
                type_text("shortcuts", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.visible.first() == Some(&"shortcuts"),
                    "Shortcuts row not first"
                );
                key("enter", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx)
                        .settings_ui
                        .drill
                        .as_ref()
                        .is_some_and(|d| d.setting == "shortcuts"),
                    "Shortcut editor did not open"
                );
                ensure!(
                    view.read(cx).state.overlay.show_settings
                        && !view.read(cx).state.overlay.show_palette,
                    "Shortcut editor left Settings"
                );
                let listed = view
                    .read(cx)
                    .settings_ui
                    .drill
                    .as_ref()
                    .map_or(0, |d| d.visible.len());
                ensure!(listed > 50, "Shortcut editor listed only {listed} commands");
                let file = shot(window, cx, &directory, "06d-shortcuts")?;
                pass(
                    &format!("Shortcut editor lists {listed} remappable commands"),
                    Some(file),
                );
                type_text("new tab", window, cx)?;
                present(window, cx);
                let first = view
                    .read(cx)
                    .settings_ui
                    .drill
                    .as_ref()
                    .and_then(|d| d.visible.first().cloned());
                ensure!(
                    first.as_deref() == Some("tab.new"),
                    "Search did not rank tab.new first: {first:?}"
                );
                key("enter", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).command_shortcut_capture.as_deref() == Some("tab.new"),
                    "Enter did not start capture"
                );
                let file = shot(window, cx, &directory, "06e-shortcut-capture")?;
                key("ctrl-alt-shift-y", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).command_shortcut_capture.is_none(),
                    "Capture did not finish"
                );
                ensure!(
                    view.read(cx).command_shortcut_string("tab.new").as_deref()
                        == Some("ctrl-alt-shift-y"),
                    "Binding not stored: {:?}",
                    view.read(cx).command_shortcut_string("tab.new")
                );
                let file_set = shot(window, cx, &directory, "06f-shortcut-set")?;
                pass("Enter records a new combination", Some(file));
                key("alt-backspace", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).command_shortcut_string("tab.new").as_deref() == Some("ctrl-t"),
                    "Alt+Backspace did not restore the default"
                );
                pass(
                    "Alt+Backspace restores the built-in shortcut",
                    Some(file_set),
                );
                key("enter", window, cx)?;
                present(window, cx);
                key("escape", window, cx)?;
                present(window, cx);
                ensure!(
                    view.read(cx).command_shortcut_capture.is_none()
                        && view.read(cx).settings_ui.drill.is_some(),
                    "Esc during capture did not just cancel"
                );
                key("escape", window, cx)?;
                present(window, cx);
                present(window, cx);
                ensure!(
                    view.read(cx).settings_ui.drill.is_none(),
                    "Esc did not leave the shortcut editor"
                );
                ensure!(
                    view.read(cx)
                        .settings_input
                        .as_ref()
                        .is_some_and(|i| i.read(cx).value() == "shortcuts"),
                    "Main query not restored"
                );
                pass("Esc cancels capture, then returns with the query", None);

                clear_query(&view, window, cx);
                type_text("default view", window, cx)?;
                let file = shot(window, cx, &directory, "06g-segmented")?;
                pass(
                    "Default view and typing mode are inline segments",
                    Some(file),
                );

                clear_query(&view, window, cx);
                view.update(cx, |app, _| {
                    app.settings_ui.category = Some(Category::About)
                });
                let file = shot(window, cx, &directory, "06c-about")?;
                view.update(cx, |app, _| app.settings_ui.category = None);
                pass("About lists version and links", Some(file));

                clear_query(&view, window, cx);
                type_text("zzzzqx", window, cx)?;
                let file = shot(window, cx, &directory, "07-empty")?;
                ensure!(
                    view.read(cx).settings_ui.visible.is_empty(),
                    "Nonsense query matched rows"
                );
                pass("Empty state for queries without matches", Some(file));

                clear_query(&view, window, cx);
                rust_i18n::set_locale("es");
                let file = shot(window, cx, &directory, "08-spanish")?;
                rust_i18n::set_locale("en");
                pass("Spanish strings render", Some(file));

                key("escape", window, cx)?;
                present(window, cx);
                ensure!(
                    !view.read(cx).state.overlay.show_settings,
                    "Escape did not close Settings"
                );
                ensure!(
                    view.read(cx).settings_input.is_none(),
                    "Settings retained its input"
                );
                ensure!(
                    view.read(cx).focus_handle.is_focused(window),
                    "Escape did not restore workspace focus"
                );
                pass("Escape closes and restores workspace focus", None);

                view.update(cx, |app, cx| {
                    app.process_actions([Action::OpenPalette]);
                    cx.notify();
                });
                present(window, cx);
                type_text("hidden files", window, cx)?;
                let file = shot(window, cx, &directory, "09-command-palette")?;
                pass(
                    "Command palette lists matching settings with their value",
                    Some(file),
                );
                key("escape", window, cx)?;
                present(window, cx);

                view.update(cx, |app, cx| {
                    app.process_actions([Action::OpenSettingAt("sidebar-width")]);
                    cx.notify();
                });
                let file = shot(window, cx, &directory, "10-open-at")?;
                let ui = &view.read(cx).settings_ui;
                ensure!(
                    ui.category == Some(Category::Layout),
                    "OpenSettingAt did not pick the category"
                );
                ensure!(
                    ui.visible.get(ui.selected) == Some(&"sidebar-width"),
                    "OpenSettingAt did not select the row"
                );
                pass("OpenSettingAt lands on the requested row", Some(file));
                Ok(())
            })??;
            Ok(checks)
        }
        .await;
        match result {
            Ok(checks) => {
                let _ = std::fs::write(
                    directory.join("tests.json"),
                    serde_json::to_vec_pretty(&checks).unwrap_or_default(),
                );
                let _ = std::fs::write(
                    directory.join("passed.txt"),
                    format!("{} checks", checks.len()),
                );
            }
            Err(error) => {
                let _ = std::fs::write(directory.join("failed.txt"), format!("{error:#}"));
                let _ = handle.update(cx, |_, window, _| {
                    let _ = window
                        .render_to_image()
                        .map(|image| image.save(directory.join("failed.png")));
                });
            }
        }
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
}
