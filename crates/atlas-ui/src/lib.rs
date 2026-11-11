//! `atlas-ui`: the gpui application layer — the `App` entity, views, overlays,
//! input/actions and platform glue. Depends on `atlas-core` (backends) and
//! `atlas-model` (state); the binary (`src/main.rs`) is a thin launcher over
//! [`run`].
//!
// reason: gpui's actions! macro derives PartialEq (not Eq) on every action struct.
#![allow(clippy::derive_partial_eq_without_eq)]
// Panic-free stdio: std's print macros panic when the stream is a closed
// pipe, and a panic aborts the whole GUI process. These shadow the prelude
// macros for every module of this crate and drop write errors instead.
#[allow(unused_macros)]
macro_rules! println {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stdout(), $($arg)*);
    }};
}
#[allow(unused_macros)]
macro_rules! eprintln {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::writeln!(::std::io::stderr(), $($arg)*);
    }};
}
#[allow(unused_macros)]
macro_rules! print {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::write!(::std::io::stdout(), $($arg)*);
    }};
}
#[allow(unused_macros)]
macro_rules! eprint {
    ($($arg:tt)*) => {{
        use ::std::io::Write as _;
        let _ = ::std::write!(::std::io::stderr(), $($arg)*);
    }};
}


rust_i18n::i18n!("locales", fallback = "en");

mod input;

pub mod app;
pub mod assets;
/// Platform drag-and-drop: neutral types + Windows OLE implementation.
pub(crate) mod dnd;
pub(crate) mod icon_theme;
pub(crate) mod perf;
/// Windows shell context-menu integration (IContextMenu enumeration + invoke).
#[cfg(target_os = "windows")]
pub(crate) mod shell_menu;
/// Windows Shell thumbnail cache access via IShellItemImageFactory.
#[cfg(target_os = "windows")]
pub(crate) mod shell_thumb;
#[cfg(windows)]
pub(crate) mod shell_file_operation;
pub mod ui;
pub(crate) mod ui_wake;

use gpui::{px, size, App as GpuiApp, AppContext as _, KeyBinding, WindowBounds, WindowOptions};
use gpui_component::{Root, ThemeRegistry, TitleBar};
use std::time::Instant;

gpui::actions!(
    atlas,
    [
        Quit,
        NewTab,
        CloseTab,
        NextTab,
        PrevTab,
        NavUp,
        NavDown,
        NavHome,
        NavEnd,
        NavPageUp,
        NavPageDown,
        ExtendUp,
        ExtendDown,
        OpenSelected,
        SelectAllItems,
        RenameSelected,
        // Phase D: overlays / panels / view toggles.
        OpenPalette,
        OpenGotoBar,
        OpenSearchInPlace,
        TogglePreviewExpanded,
        ToggleInspector,
        ToggleSidebar,
        ToggleHidden,
        OpenSettings,
        OpenExtManager,
        CloseOverlays,
        // Phase D: pane structure.
        CloseFocusedPane,
        FocusNextPane,
        SplitHorizontal,
        SplitVertical,
        // Phase D: history / location / refresh.
        Undo,
        Redo,
        RefreshTab,
        EditLocation,
        NavBack,
        NavForward,
        NavParent,
        DeleteToTrash,
        DeletePermanent,
        SidebarNewGlobalTag,
        SidebarNewProfileTag,
        // Bottom-bar view selector (per-pane dropdown).
        ViewModeDetails,
        ViewModeGrid,
        ViewModeList,
        ViewModeMiller,
        ZoomIn,
        ZoomOut,
        ZoomReset,
    ]
);

#[cfg(feature = "provider-host")]
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct RemoveCloudRemote {
    pub id: String,
}

#[cfg(feature = "provider-host")]
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct StartRenameCloudRemote {
    pub id: String,
}

#[cfg(all(feature = "provider-host", windows))]
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct InstallWinFspForRemote {
    pub id: String,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct SidebarToggleSection {
    pub index: usize,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct SidebarHideSection {
    pub index: usize,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct SidebarMoveSectionUp {
    pub index: usize,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct SidebarMoveSectionDown {
    pub index: usize,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct SidebarResetSections;

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct OpenSpaceLensForPath {
    pub path: std::path::PathBuf,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct OpenPropertiesForPath {
    pub path: std::path::PathBuf,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct OpenThisPcProperties;

/// Windows "Map Network Drive" dialog, optionally pre-filled with a share.
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct MapNetworkDrive {
    pub remote: Option<std::path::PathBuf>,
}

/// Disconnect a mapped network drive (`Z:\`).
#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct DisconnectNetworkDrive {
    pub path: std::path::PathBuf,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct SidebarDeleteTag {
    pub tag: atlas_model::tag_cache::TagId,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct SidebarRenameTag {
    pub tag: atlas_model::tag_cache::TagId,
    pub name: String,
    pub color: [u8; 3],
    pub global: bool,
}

#[derive(gpui::Action, Clone, PartialEq, Eq, serde::Deserialize)]
#[action(namespace = atlas, no_json)]
pub struct SidebarRemoveBookmark {
    pub index: usize,
}


/// Launch the gpui application. The caller owns the tokio runtime and passes its
/// handle in; gpui drives the UI on the calling (main) thread.
pub fn run(
    handle: tokio::runtime::Handle,
    app_started: Instant,
    on_quit: Option<Box<dyn FnOnce() + 'static>>,
    initial_paths: Vec<std::path::PathBuf>,
) {
    let run_started = Instant::now();
    #[cfg(feature = "ui-lab")]
    let initial_paths = crate::ui::preview_lab::prepare(initial_paths);
    let settings = atlas_core::util::config::load_settings();
    let lang = settings
        .language
        .clone()
        .unwrap_or_else(|| "en".to_string());
    rust_i18n::set_locale(&lang);
    // Build the translation backend now, on a shallow stack. Left lazy, its
    // large init frame lands at the bottom of the first window's layout
    // recursion and overflows the main thread.
    let _ = rust_i18n::available_locales!();
    tracing::info!(
        total_ms = app_started.elapsed().as_millis(),
        phase_ms = run_started.elapsed().as_millis(),
        "startup: settings+locale"
    );

    let application = gpui_platform::application().with_assets(crate::assets::AtlasAssets);
    application.run(move |cx: &mut GpuiApp| {
        let gpui_started = Instant::now();
        gpui_component::init(cx);
        if let Err(err) = cx.text_system().add_fonts(crate::assets::bundled_fonts()) {
            tracing::warn!(?err, "failed to load bundled Geist fonts");
        }
        tracing::info!(
            total_ms = app_started.elapsed().as_millis(),
            phase_ms = gpui_started.elapsed().as_millis(),
            "startup: gpui init"
        );

        // Register the bundled Atlas themes (Mac-minimal set) and watch the user
        // themes dir for custom `ThemeSet` JSON files. The active profile's theme is
        // applied on first render (see `App::apply_active_theme`).
        let themes_started = Instant::now();
        let json = include_str!("../assets/themes/atlas-themes.json");
        match ThemeRegistry::global_mut(cx).load_themes_from_str(json) {
            Ok(()) => {
                eprintln!(
                    "[atlas] {} themes registered",
                    ThemeRegistry::global(cx).themes().len()
                );
            }
            Err(err) => {
                eprintln!("[atlas] FAILED to load bundled themes: {err:#}");
            }
        }
        // CRITICAL: Load bundled themes immediately in-memory so the active profile
        // theme is available on the first frame. Filesystem sync and directory watching
        // for custom user themes are deferred to the background so they do not block startup.
        let themes_dir = atlas_core::util::config::themes_dir();
        cx.spawn(async move |cx| {
            if let Err(err) = std::fs::create_dir_all(&themes_dir)
                .and_then(|()| std::fs::write(themes_dir.join("atlas-themes.json"), json))
            {
                tracing::error!(?err, "failed to persist bundled themes to themes_dir");
            }
            _ = cx.update(|cx| {
                if let Err(err) = ThemeRegistry::watch_dir(themes_dir, cx, |cx| {
                    cx.refresh_windows();
                }) {
                    tracing::error!(?err, "failed to watch themes dir");
                }
            });
        })
        .detach();

        tracing::info!(
            total_ms = app_started.elapsed().as_millis(),
            phase_ms = themes_started.elapsed().as_millis(),
            "startup: themes"
        );

        cx.bind_keys([
            #[cfg(target_os = "macos")]
            KeyBinding::new("cmd-q", Quit, None),
            #[cfg(not(target_os = "macos"))]
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("up", NavUp, Some("Atlas")),
            KeyBinding::new("down", NavDown, Some("Atlas")),
            KeyBinding::new("shift-up", ExtendUp, Some("Atlas")),
            KeyBinding::new("shift-down", ExtendDown, Some("Atlas")),
            KeyBinding::new("home", NavHome, Some("Atlas")),
            KeyBinding::new("end", NavEnd, Some("Atlas")),
            KeyBinding::new("pageup", NavPageUp, Some("Atlas")),
            KeyBinding::new("pagedown", NavPageDown, Some("Atlas")),
            KeyBinding::new("ctrl-tab", NextTab, Some("Atlas")),
            KeyBinding::new("ctrl-shift-tab", PrevTab, Some("Atlas")),
            // Phase D: overlays / panels / toggles.
            KeyBinding::new("ctrl-f", OpenSearchInPlace, Some("Atlas")),
            KeyBinding::new("space", ToggleInspector, Some("Atlas")),
            KeyBinding::new("escape", CloseOverlays, Some("Atlas")),
            // Phase D: pane structure.
            KeyBinding::new("alt-tab", FocusNextPane, Some("Atlas")),
            // Phase D: history / location / refresh.
            KeyBinding::new("ctrl-r", RefreshTab, Some("Atlas")),
            KeyBinding::new("ctrl-l", EditLocation, Some("Atlas")),
            KeyBinding::new("alt-up", NavParent, Some("Atlas")),
            // Open/rename/refresh and file clipboard keys resolve through the
            // profile command registry, so remapping removes the old binding.
            // Text input clipboard bindings remain owned by Input context.
        ]);
        cx.on_action(|_: &Quit, cx: &mut GpuiApp| {
            // Keep copied files available to other apps after Atlas exits.
            #[cfg(windows)]
            crate::dnd::windows::ole_clipboard::flush();
            cx.quit();
        });

        #[cfg(feature = "ui-lab")]
        let hidden_preview_lab = std::env::var_os("ATLAS_PREVIEW_LAB").is_some();
        #[cfg(not(feature = "ui-lab"))]
        let hidden_preview_lab = false;
        let window_options = WindowOptions {
            // Windows delivers demand-driven frames through WM_PAINT. A hidden
            // window may never receive it, so visibility cannot wait for a frame.
            show: !hidden_preview_lab,
            focus: !hidden_preview_lab,
            titlebar: Some(TitleBar::title_bar_options()),
            window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
            window_min_size: Some(size(px(900.), px(560.))),
            app_id: Some("atlas".to_string()),
            ..Default::default()
        };

        let app_settings = settings.clone();
        let window_started = Instant::now();
        if let Err(err) = cx.open_window(window_options, move |window, cx| {
            tracing::info!(
                total_ms = app_started.elapsed().as_millis(),
                phase_ms = window_started.elapsed().as_millis(),
                "startup: window callback"
            );
            window.set_window_title("Atlas");
            let initial_paths = initial_paths.clone();
            let view = cx.new(|cx| {
                app::App::new(handle, app_settings, app_started, initial_paths, window, cx)
            });
            #[cfg(feature = "conversion-lab")]
            crate::ui::converter::lab::install(&view, window, cx);
            #[cfg(feature = "ui-lab")]
            crate::ui::preview_lab::install(&view, window, cx);
            app::App::schedule_startup_frames(window, 2);
            // Autosave: persist settings/session/profiles when the app is
            // about to quit, gated by the user's autosave-session setting.
            let weak_app = view.downgrade();
            let closing_app = view.downgrade();
            window.on_window_should_close(cx, move |_, cx| {
                closing_app.update(cx, |app, cx| {
                    let allowed = app.close_native_vault(false);
                    cx.notify();
                    allowed
                }).unwrap_or(true)
            });
            cx.on_app_quit(move |cx| {
                #[cfg(feature = "conversion-lab")]
                if std::env::var_os("ATLAS_CONVERSION_LAB").is_some() { return std::future::ready(()); }
                #[cfg(feature = "ui-lab")]
                if std::env::var_os("ATLAS_PREVIEW_LAB").is_some() { return std::future::ready(()); }
                weak_app.update(cx, |app, _cx| app.save_on_quit()).ok();
                // OS end-session exits the process from inside gpui, so this,
                // not the end of `main`, is the last orderly point.
                #[cfg(windows)]
                atlas_win_space::crash::mark_clean_exit();
                std::future::ready(())
            })
            .detach();
            let checkpoint_app = view.downgrade();
            cx.spawn(async move |cx| loop {
                cx.background_executor().timer(std::time::Duration::from_secs(60)).await;
                if checkpoint_app.update(cx, |app, _| app.autosave_checkpoint()).is_err() {
                    break;
                }
            })
            .detach();
            // The top-level window view must be `Root` (mount point for
            // gpui-component overlays, modals and notifications).
            cx.new(|cx| Root::new(view, window, cx))
        }) {
            tracing::error!(?err, "failed to open window");
        }
        tracing::info!(
            total_ms = app_started.elapsed().as_millis(),
            phase_ms = window_started.elapsed().as_millis(),
            "startup: open window"
        );

        if let Some(on_quit) = on_quit {
            let mut on_quit_opt = Some(on_quit);
            cx.on_app_quit(move |_| {
                let cb = on_quit_opt.take();
                async move {
                    if let Some(cb) = cb {
                        cb();
                    }
                }
            })
            .detach();
        }
    });
}
#[cfg(feature = "ui-lab")]
pub fn run_vault_diagnostic(directory: std::path::PathBuf) -> anyhow::Result<()> {
    app::vault::diagnostic::run(directory)
}

