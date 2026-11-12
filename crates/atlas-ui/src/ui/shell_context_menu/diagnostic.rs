//! Real GPUI dispatch and GPU captures without taking over the user's desktop.
use super::{App, AtlasCmd, CtxRow};
use anyhow::ensure;
use gpui::prelude::*;
use gpui::{point, px, size, Entity, Keystroke, Window, WindowBounds, WindowOptions};
use gpui_component::{Root, ThemeRegistry, TitleBar};
use std::{path::PathBuf, time::Instant};

pub(crate) fn prepare_open_with(
    app: &Entity<App>,
    path: PathBuf,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    app.update(cx, |app, cx| {
        app.open_ctx_menu(vec![path], point(px(400.), px(140.)), window, cx);
        let menu = app.ctx_menu.as_mut().unwrap();
        #[cfg(windows)]
        if let Some(worker) = &app.shell_menu {
            worker.send(crate::shell_menu::ShellMenuRequest::Dismiss { id: menu.menu_id });
        }
        // Ignore late events from the real shell build during this deterministic check.
        menu.menu_id += 1_000_000_000;
        menu.loading = false;
        menu.stale = false;
        menu.desktop_apps = (1..=8).map(|i| atlas_core::fs::desktop_apps::DesktopApp::parse_str(
            &format!("test-{i}.desktop"), &format!("[Desktop Entry]\nType=Application\nName=Aplicación {i}\nExec=fixture-only %F\n")
        ).unwrap()).collect();
        let leaf = |id, title: &str| super::UiNativeItem { id, title: title.into(), verb: None,
            icon: None, kind: super::UiItemKind::Normal, disabled: false, is_default: false };
        menu.native = vec![super::UiNativeItem {
            id: 100, title: "Native chooser in any language".into(), verb: Some("openwith".into()), icon: None,
            kind: super::UiItemKind::Submenu { children: Some(vec![leaf(101, "Editor nativo"), leaf(102, "Elegir otra aplicación…")]) },
            disabled: false, is_default: false,
        }, leaf(103, "Open with External Tool")];
        let rows = app.ctx_menu_rows("", "");
        let selected = rows.iter().filter(|row| row.selectable()).position(|row| matches!(row, CtxRow::Native { id: super::OPEN_WITH_GROUP, .. })).unwrap();
        app.ctx_menu.as_mut().unwrap().selected = selected;
        cx.notify();
    });
}

pub(crate) fn check_open_with(
    app: &Entity<App>,
    expanded: bool,
    cx: &gpui::App,
) -> anyhow::Result<()> {
    let app = app.read(cx);
    let rows = app.ctx_menu_rows("", "");
    ensure!(
        rows.iter()
            .filter(|row| matches!(
                row,
                CtxRow::Native {
                    id: super::OPEN_WITH_GROUP,
                    depth: 0,
                    ..
                }
            ))
            .count()
            == 1,
        "Expected exactly one Open with group"
    );
    ensure!(
        rows.iter()
            .filter(|row| matches!(row, CtxRow::DesktopApp { .. }))
            .count()
            == if expanded { 8 } else { 0 },
        "Applications must stay collapsed and all eight must be reachable"
    );
    ensure!(
        !rows.iter().any(|row| matches!(
            row,
            CtxRow::Native {
                id: 100 | 101 | 102 | 103,
                depth: 0,
                ..
            }
        )),
        "Native Open with escaped the group"
    );
    if expanded {
        ensure!(
            rows.iter().any(|row| matches!(
                row,
                CtxRow::Native {
                    id: 102,
                    depth: 1,
                    ..
                }
            )),
            "Native application chooser disappeared"
        );
    }
    ensure!(
        app.ctx_menu_rows("Aplicación 8", "")
            .iter()
            .any(|row| matches!(row, CtxRow::DesktopApp { app, .. } if app.name == "Aplicación 8")),
        "Search lost the eighth app"
    );
    Ok(())
}

fn draw(window: &mut Window, cx: &mut gpui::App) {
    window.simulate_next_frame(cx);
    window.refresh();
    window.draw(cx).clear(cx);
}

fn key(value: &str, window: &mut Window, cx: &mut gpui::App) -> anyhow::Result<()> {
    window.dispatch_keystroke(Keystroke::parse(value)?, cx);
    draw(window, cx);
    Ok(())
}

fn type_text(value: &str, window: &mut Window, cx: &mut gpui::App) {
    for character in value.chars() {
        window.dispatch_keystroke(
            Keystroke {
                key: character.to_string(),
                key_char: Some(character.to_string()),
                modifiers: Default::default(),
            },
            cx,
        );
        draw(window, cx);
    }
}

fn assert_query(
    app: &Entity<App>,
    query: &str,
    window: &Window,
    cx: &gpui::App,
) -> anyhow::Result<()> {
    let app = app.read(cx);
    let menu = app
        .ctx_menu
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("menu closed"))?;
    ensure!(
        menu.input.text == query,
        "expected query {query:?}, got {:?}",
        menu.input.text
    );
    ensure!(menu.focus.is_focused(window), "DG search lost focus");
    ensure!(
        !app.state
            .active_tab_state()
            .is_some_and(|tab| tab.search.active),
        "menu typing activated the pane filter"
    );
    Ok(())
}

fn check_menu(
    app: &Entity<App>,
    directory: &std::path::Path,
    label: &str,
    window: &mut Window,
    cx: &mut gpui::App,
) -> anyhow::Result<()> {
    assert_query(app, "", window, cx)?;
    let state = app.read(cx).ctx_menu.as_ref().unwrap();
    ensure!(!state.loading, "native shell menu did not finish loading");
    #[cfg(windows)]
    ensure!(
        !state.native.is_empty(),
        "expected real native shell commands"
    );
    ensure!(
        !state
            .native
            .iter()
            .any(|item| item.title.eq_ignore_ascii_case("properties")
                || item.title.eq_ignore_ascii_case("propiedades")),
        "native Properties duplicates Atlas Properties"
    );
    window
        .render_to_image()?
        .save(directory.join(format!("{label}-open.png")))?;
    app.update(cx, |app, cx| {
        let menu = app.ctx_menu.as_mut().unwrap();
        menu.scroll_to_selected = false;
        menu.scroll_top = f32::MAX;
        cx.notify();
    });
    draw(window, cx);
    window
        .render_to_image()?
        .save(directory.join(format!("{label}-bottom.png")))?;
    type_text("prop", window, cx);
    assert_query(app, "prop", window, cx)?;
    app.update(cx, |app, _| {
        let menu = app.ctx_menu.as_ref().unwrap();
        let rows = app.ctx_menu_rows(&menu.input.text, &menu.input.text);
        ensure!(
            rows.iter().any(|row| matches!(
                row,
                CtxRow::Atlas {
                    cmd: AtlasCmd::Properties
                }
            )),
            "Properties not searchable"
        );
        Ok::<_, anyhow::Error>(())
    })?;
    window
        .render_to_image()?
        .save(directory.join(format!("{label}-search.png")))?;
    key("ctrl-a", window, cx)?;
    type_text("café🚀", window, cx);
    assert_query(app, "café🚀", window, cx)?;
    key("backspace", window, cx)?;
    assert_query(app, "café", window, cx)?;
    key("ctrl-z", window, cx)?;
    assert_query(app, "café🚀", window, cx)?;
    key("ctrl-a", window, cx)?;
    key("backspace", window, cx)?;
    assert_query(app, "", window, cx)?;
    // Use the painted input geometry for the pointer path, then type again.
    let position = app
        .read(cx)
        .ctx_menu
        .as_ref()
        .unwrap()
        .input_bounds
        .unwrap()
        .center();
    for down in [true, false] {
        let input = if down {
            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position,
                button: gpui::MouseButton::Left,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            })
        } else {
            gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                position,
                button: gpui::MouseButton::Left,
                modifiers: Default::default(),
                click_count: 1,
            })
        };
        window.dispatch_event(input, cx);
        draw(window, cx);
    }
    type_text("abrir", window, cx);
    assert_query(app, "abrir", window, cx)?;
    key("escape", window, cx)?;
    ensure!(
        app.read(cx).ctx_menu.is_none(),
        "Escape did not close the menu"
    );
    Ok(())
}

pub fn run(directory: PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(&directory)?;
    // Shell namespace parsing expects a normal drive path, not std's verbatim
    // filesystem prefix (the fixtures themselves are still canonical paths).
    let directory = directory.canonicalize()?;
    let directory = PathBuf::from(directory.to_string_lossy().trim_start_matches(r"\\?\"));
    let fixtures = directory.join("fixtures");
    std::fs::create_dir_all(&fixtures)?;
    let file = fixtures.join("menu-fixture.txt");
    std::fs::write(&file, "Context menu regression fixture\n")?;
    let config = directory.join("config");
    std::fs::create_dir_all(&config)?;
    std::env::set_var("ATLAS_CONFIG_DIR", &config);
    let mut profiles = atlas_model::profiles::ProfileManager::default();
    profiles.profiles[0].theme = "Atlas Dracula".into();
    std::fs::write(
        config.join("profiles.json"),
        serde_json::to_vec_pretty(&profiles)?,
    )?;
    rust_i18n::set_locale("es");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let rt = runtime.handle().clone();
    let failure = directory.join("failed.txt");
    let result = std::rc::Rc::new(std::cell::RefCell::new(Ok(())));
    let final_result = result.clone();
    gpui_platform::application().with_assets(crate::assets::AtlasAssets).run(move |cx| {
        gpui_component::init(cx);
        cx.text_system().add_fonts(crate::assets::bundled_fonts()).unwrap();
        ThemeRegistry::global_mut(cx).load_themes_from_str(include_str!("../../../assets/themes/atlas-themes.json")).unwrap();
        let mut app = None;
        let window = cx.open_window(WindowOptions {
            show: false, focus: false,
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
            ..Default::default()
        }, |window, cx| {
            let view = cx.new(|cx| App::new(rt, atlas_model::settings::Settings {
                disable_animations: true, autosave_session: false, getting_started_seen: true, show_inspector: false,
                ..Default::default()
            }, Instant::now(), vec![fixtures], window, cx));
            app = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        }).unwrap();
        let app = app.unwrap();
        let window: gpui::AnyWindowHandle = window.into();
        cx.spawn(async move |cx| {
            window.update(cx, |_, window, _| window.resize(size(px(1280.), px(800.)))).unwrap();
            for _ in 0..30 {
                cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
                window.update(cx, |_, window, cx| draw(window, cx)).unwrap();
            }
            let checked: anyhow::Result<()> = async {
                let mut checks = Vec::new();
                for (label, theme, zoom) in [("dark", "Atlas Dracula", 1.0), ("light", "Atlas Light", 1.0), ("zoom", "Atlas Dracula", 1.5)] {
                    window.update(cx, |_, window, cx| {
                        app.update(cx, |app, cx| {
                            app.set_theme(theme.into());
                            app.state.settings.global_zoom = zoom;
                            app.apply_active_theme(window, cx);
                            app.open_ctx_menu(vec![file.clone()], point(px(420.), px(120.)), window, cx);
                            cx.notify();
                        });
                        draw(window, cx);
                    })?;
                    for _ in 0..30 {
                        cx.background_executor().timer(std::time::Duration::from_millis(50)).await;
                        window.update(cx, |_, window, cx| draw(window, cx))?;
                    }
                    window.update(cx, |_, window, cx| check_menu(&app, &directory, label, window, cx))??;
                    checks.push(format!("{label}: immediate typing, search, Unicode, editing, undo, click focus and Escape"));
                }
                std::fs::write(directory.join("verification.json"), serde_json::to_vec_pretty(&checks)?)?;
                Ok(())
            }.await;
            if let Err(error) = &checked {
                let _ = std::fs::write(&failure, format!("{error:#}"));
            }
            *result.borrow_mut() = checked;
            let _ = cx.update(|cx| cx.quit());
        }).detach();
    });
    let outcome = std::mem::replace(&mut *final_result.borrow_mut(), Ok(()));
    outcome
}
