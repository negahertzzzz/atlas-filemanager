//! Native captures of the branded first-run flow and feature guide, plus the
//! Atlas Ember pane material at low and full opacity. Writes screenshots.
use crate::app::App;
use crate::input::action::Action;
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
) -> anyhow::Result<()> {
    present(window, cx);
    present(window, cx);
    window
        .render_to_image()?
        .save(directory.join(format!("{name}.png")))?;
    Ok(())
}

fn act(view: &Entity<App>, cx: &mut gpui::App, f: impl FnOnce(&mut App)) {
    view.update(cx, |app, cx| {
        f(app);
        cx.notify();
    });
}

pub(super) fn install(
    view: &Entity<App>,
    window: &mut Window,
    cx: &mut gpui::App,
    directory: PathBuf,
) {
    rust_i18n::set_locale("es");
    let handle = window.window_handle();
    let view = view.clone();
    cx.spawn(async move |cx| {
        let result: anyhow::Result<()> = async {
            handle.update(cx, |_, window, cx| {
                window.resize(size(px(1280.), px(800.)));
                act(&view, cx, |app| {
                    app.state.settings.disable_animations = true;
                    app.state.overlay.show_getting_started = false;
                    app.process_actions([Action::SetTheme("Atlas Ember".into())]);
                    app.state.settings.glass_opacity = 0.2;
                    app.theme_dirty = true;
                });
                present(window, cx);
            })?;
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            handle.update(cx, |_, window, cx| -> anyhow::Result<()> {
                shot(window, cx, &directory, "01-ember-panes-20")?;
                act(&view, cx, |app| {
                    app.state.settings.glass_opacity = 1.0;
                    app.theme_dirty = true;
                });
                shot(window, cx, &directory, "02-ember-panes-100")?;
                act(&view, cx, |app| {
                    app.state.settings.glass_opacity = 0.6;
                    app.theme_dirty = true;
                    app.process_actions([Action::OpenGettingStarted]);
                });
                shot(window, cx, &directory, "03-setup-welcome")?;
                act(&view, cx, |app| app.state.overlay.getting_started_step = 1);
                shot(window, cx, &directory, "04-setup-theme")?;
                act(&view, cx, |app| app.state.overlay.getting_started_step = 2);
                shot(window, cx, &directory, "05-setup-workspace")?;
                act(&view, cx, |app| {
                    app.process_actions([Action::CloseGettingStarted, Action::OpenGuide(2)])
                });
                shot(window, cx, &directory, "06-guide")?;
                act(&view, cx, |app| {
                    app.process_actions([Action::StartGuideCoach(2)])
                });
                shot(window, cx, &directory, "07-coach")?;
                Ok(())
            })??;
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                let _ = std::fs::write(directory.join("passed.txt"), "ok");
            }
            Err(error) => {
                let _ = std::fs::write(directory.join("failed.txt"), format!("{error:#}"));
            }
        }
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
}
