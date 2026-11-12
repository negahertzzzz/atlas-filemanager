//! Hidden-window visual checks for WSL labels and the light/dark logo.
//! All file activity stays in the supplied fixture directory.
use crate::app::App;
use atlas_model::{layout_tree::Tile, location::Location, settings::Settings};
use gpui::{px, size, AppContext, Window, WindowBounds, WindowOptions};
use gpui_component::{Root, ThemeRegistry, TitleBar};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn draw(window: &mut Window, cx: &mut gpui::App) {
    window.simulate_next_frame(cx);
    window.refresh();
    window.draw(cx).clear(cx);
}

pub fn run(directory: PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(&directory)?;
    let directory = directory.canonicalize()?;
    let directory = PathBuf::from(directory.to_string_lossy().trim_start_matches(r"\\?\"));
    let fixture = directory.join("fixtures");
    std::fs::create_dir_all(fixture.join("boot"))?;
    std::fs::create_dir_all(fixture.join("home"))?;
    std::fs::create_dir_all(fixture.join("usr"))?;
    std::env::set_var("ATLAS_CONFIG_DIR", directory.join("config"));
    std::env::set_var("ATLAS_DISABLE_SEARCH_SERVICE", "1");
    rust_i18n::set_locale("es");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let rt = runtime.handle().clone();
    let result = std::rc::Rc::new(std::cell::RefCell::new(Ok(())));
    let final_result = result.clone();
    gpui_platform::application()
        .with_assets(crate::assets::AtlasAssets)
        .run(move |cx| {
            gpui_component::init(cx);
            cx.text_system()
                .add_fonts(crate::assets::bundled_fonts())
                .unwrap();
            ThemeRegistry::global_mut(cx)
                .load_themes_from_str(include_str!("../../../assets/themes/atlas-themes.json"))
                .unwrap();
            let mut app = None;
            let window = cx
                .open_window(
                    WindowOptions {
                        show: false,
                        focus: false,
                        titlebar: Some(TitleBar::title_bar_options()),
                        window_bounds: Some(WindowBounds::centered(size(px(1100.), px(700.)), cx)),
                        ..Default::default()
                    },
                    |window, cx| {
                        let view = cx.new(|cx| {
                            App::new(
                                rt,
                                Settings {
                                    autosave_session: false,
                                    getting_started_seen: true,
                                    show_inspector: false,
                                    sidebar_collapsed: true,
                                    disable_animations: true,
                                    ..Default::default()
                                },
                                Instant::now(),
                                vec![fixture.clone()],
                                window,
                                cx,
                            )
                        });
                        app = Some(view.clone());
                        cx.new(|cx| Root::new(view, window, cx))
                    },
                )
                .unwrap();
            let app = app.unwrap();
            let window: gpui::AnyWindowHandle = window.into();
            cx.spawn(async move |cx| {
                let checked: anyhow::Result<()> = async {
                    for _ in 0..20 {
                        cx.background_executor()
                            .timer(Duration::from_millis(30))
                            .await;
                        window.update(cx, |_, window, cx| draw(window, cx))?;
                    }
                    // Use recorded UNC locations, never enumerate the user's WSL files.
                    window.update(cx, |_, _, cx| {
                        app.update(cx, |app, cx| {
                            app.watchers.clear();
                            let pane = app.state.focused_tile;
                            if let Some(Tile::Pane(tab)) = app.state.pane_tree.tiles.get_mut(pane) {
                                tab.location =
                                    Location::Directory(PathBuf::from(r"\\wsl.localhost\Arch\"));
                                tab.breadcrumb_trail = None;
                            }
                            app.notify_all_panes(cx);
                            cx.notify();
                        })
                    })?;
                    for (name, theme, width) in [
                        ("arch-light", "Atlas Light", 1100.),
                        ("arch-light-narrow", "Atlas Light", 640.),
                        ("arch-light-glass", "Atlas Light Glass", 1100.),
                        ("arch-dark", "Atlas Graphite", 1100.),
                    ] {
                        window.update(cx, |_, window, cx| {
                            window.resize(size(px(width), px(700.)));
                            app.update(cx, |app, cx| {
                                app.set_theme(theme.into());
                                app.notify_all_panes(cx);
                                cx.notify();
                            });
                            draw(window, cx);
                        })?;
                        for _ in 0..8 {
                            cx.background_executor()
                                .timer(Duration::from_millis(30))
                                .await;
                            window.update(cx, |_, window, cx| draw(window, cx))?;
                        }
                        window.update(cx, |_, window, _| {
                            window
                                .render_to_image()?
                                .save(directory.join(format!("{name}.png")))?;
                            Ok::<_, anyhow::Error>(())
                        })??;
                    }
                    Ok(())
                }
                .await;
                *result.borrow_mut() = checked;
                let _ = cx.update(|cx| cx.quit());
            })
            .detach();
        });
    let outcome = std::mem::replace(&mut *final_result.borrow_mut(), Ok(()));
    outcome
}
