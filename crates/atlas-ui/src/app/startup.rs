//! `App::new`: construct the app, restore session/profiles, kick off loads.
#![allow(clippy::too_many_lines)] // reason: cohesive startup/drain kept whole

use atlas_model::layout_tree::Tile;
use std::collections::{HashMap, HashSet};

use gpui::{Context, Window};
use smol::Timer;

use super::*;
use atlas_core::fs::list::list_dir;
use atlas_core::search::nucleo_pool::NucleoPool;
use atlas_core::util::config::{load_profiles, load_session};
use atlas_model::app_state::{AppState, BgResult};
use atlas_model::tab::LoadState;

impl App {
    pub fn new(
        rt: tokio::runtime::Handle,
        settings: atlas_model::settings::Settings,
        startup_started: std::time::Instant,
        initial_paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let app_new_started = std::time::Instant::now();
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let rubber_escape_listener = cx.listener(|this, ev: &gpui::KeystrokeEvent, window, cx| {
            if ev.keystroke.key == "escape"
                && (this.rubber.is_some() || this.pending_folder_click.is_some())
            {
                let pane = this.rubber.as_ref().and_then(|r| match r.target {
                    RubberTarget::Pane(pane) | RubberTarget::MillerColumn(pane, _) => Some(pane),
                    RubberTarget::Dropzone => None,
                });
                this.cancel_rubber();
                if let Some(pane) = pane {
                    this.notify_pane(pane, cx);
                }
                cx.notify();
                window.refresh();
                cx.stop_propagation();
            }
        });
        let rubber_escape_sub = cx.intercept_keystrokes(rubber_escape_listener);
        let profiles = load_profiles();
        tracing::info!(
            total_ms = startup_started.elapsed().as_millis(),
            phase_ms = app_new_started.elapsed().as_millis(),
            "startup: profiles"
        );

        let (tx, incoming_rx) = flume::unbounded();
        #[cfg(windows)]
        {
            let volume_tx = tx.clone();
            rt.spawn_blocking(move || {
                let _ = volume_tx.send(BgResult::VolumeSnapshot(
                    atlas_win_space::storage::mounted_volume_serials(),
                ));
            });
        }
        let (drain_tx, rx) = flume::unbounded();
        let (icon_tx, incoming_icon_rx) = flume::unbounded();
        let (drain_icon_tx, icon_rx) = flume::unbounded();
        let wake_rx = crate::ui_wake::subscribe();
        cx.observe_self(|_, _| crate::ui_wake::wake()).detach();
        cx.on_release(|_, _| crate::ui_wake::wake()).detach();
        cx.observe_global::<gpui_component::ThemeRegistry>(|this, cx| {
            this.theme_dirty = true;
            cx.notify();
        })
        .detach();
        // Windows light/dark app mode changed: profiles that follow the system
        // switch to their light or dark theme on the next frame.
        cx.observe_window_appearance(window, |this, _window, cx| {
            this.theme_follow_pending = true;
            this.theme_dirty = true;
            cx.notify();
        })
        .detach();
        // Forward off the foreground executor: the retained ingress queues must
        // remain readable by the OLE modal-loop drag-hover drain as well.
        cx.background_executor()
            .spawn(async move {
                while let Ok(result) = incoming_rx.recv_async().await {
                    if drain_tx.send(result).is_err() {
                        break;
                    }
                    crate::ui_wake::wake();
                }
            })
            .detach();
        cx.background_executor()
            .spawn(async move {
                while let Ok(result) = incoming_icon_rx.recv_async().await {
                    if drain_icon_tx.send(result).is_err() {
                        break;
                    }
                    crate::ui_wake::wake();
                }
            })
            .detach();

        #[cfg(windows)]
        let explorer_choice = explorer_choice_at_startup();
        // Restore previous session tree if available, else create fresh state.
        // If initial_paths were provided (e.g. CLI path or "open in new window"),
        // start a clean session focused on that location rather than restoring older tabs.
        let session_started = std::time::Instant::now();
        #[cfg(windows)]
        let transferred_tab = std::env::var("ATLAS_TAB_TRANSFER_TOKEN")
            .ok()
            .and_then(|token| {
                crate::dnd::windows::ole_clipboard::take_tab_transfer(&token)
                    .map(|tab| (token, tab))
            });
        #[cfg(windows)]
        let importing_tab = transferred_tab.is_some();
        #[cfg(not(windows))]
        let importing_tab = false;
        let restored_tree = if initial_paths.is_empty() && !importing_tab {
            load_session()
        } else {
            None
        };
        let (mut state, _initial_tile_id) = if let Some(tree) = restored_tree {
            let (s, id) = AppState::from_tree(tree, settings);
            // Discard a corrupted/empty session and start fresh
            if s.all_pane_tile_ids().is_empty() {
                AppState::new(s.settings)
            } else {
                (s, id)
            }
        } else {
            AppState::new(settings)
        };
        let mut initial_select = None;
        if let Some(path) = initial_paths.first().cloned() {
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            #[cfg(windows)]
            let path = {
                let s = path.to_string_lossy();
                if let Some(stripped) = s.strip_prefix(r"\\?\") {
                    std::path::PathBuf::from(stripped)
                } else {
                    path
                }
            };
            let selected_file = path.is_file().then(|| path.clone());
            let path = if path.is_dir() {
                Some(path)
            } else if path.is_file() {
                path.parent().map(std::path::Path::to_path_buf)
            } else {
                tracing::warn!(path = %path.display(), "startup path ignored: not found");
                None
            };
            if let Some(path) = path {
                if let Some(tile) = state.all_pane_tile_ids().first().copied() {
                    if let Some(atlas_model::layout_tree::Tile::Pane(tab)) =
                        state.pane_tree.tiles.get_mut(tile)
                    {
                        tab.location = atlas_model::location::Location::Directory(path);
                        tab.volume_serial = None;
                        state.focused_tile = tile;
                        state.pane_tree.make_active(|id, _| id == tile);
                        initial_select = selected_file.map(|file| (tile, file));
                    }
                }
            }
        }
        #[cfg(windows)]
        let mut pending_tab_restore = None;
        #[allow(unused_mut)]
        let mut initial_type_filters = HashMap::new();
        #[allow(unused_mut)]
        let mut initial_size_filters = HashMap::new();
        #[cfg(windows)]
        if let Some((token, tab)) = transferred_tab {
            if let Some(Tile::Pane(current)) = state.pane_tree.tiles.get_mut(state.focused_tile) {
                *current = tab.tab.clone();
                if let Some(show_hidden) = tab.view_filters.show_hidden {
                    state.settings.show_hidden = show_hidden;
                }
                initial_type_filters.insert(state.focused_tile, tab.view_filters.types.clone());
                initial_size_filters.insert(state.focused_tile, tab.view_filters.sizes.clone());
                if let Some(runtime) = state.tab_states.get_mut(&state.focused_tile) {
                    tab.apply_runtime(runtime);
                }
                pending_tab_restore = Some((state.focused_tile, tab));
                crate::dnd::windows::ole_clipboard::acknowledge_tab_transfer(&token);
            }
        }
        state.profiles = profiles;
        migrate_global_recent_folders(&mut state);
        if !state.settings.getting_started_seen {
            state.overlay.show_getting_started = true;
        }
        tracing::info!(
            total_ms = startup_started.elapsed().as_millis(),
            phase_ms = session_started.elapsed().as_millis(),
            "startup: session"
        );
        // Profiles saved before the cloud feature lack the Network section, which
        // would hide configured remotes — backfill it.
        #[cfg(windows)]
        state.profiles.ensure_linux_section();

        // Resolve the startup icon theme from the active profile's saved selection
        // (official per-profile store), falling back to VSCode auto-discovery.
        let icon_theme = {
            let icon_started = std::time::Instant::now();
            let map = state
                .profiles
                .active()
                .scoped::<crate::app::profiles::IconSelection>();
            let theme = if map.is_empty() {
                crate::icon_theme::find_vscode_theme()
            } else {
                let paths = map
                    .into_iter()
                    .map(|(ext, path)| (ext, std::path::PathBuf::from(path)))
                    .collect();
                crate::icon_theme::IconThemeIndex::from_mappings(paths)
            };
            tracing::info!(
                total_ms = startup_started.elapsed().as_millis(),
                phase_ms = icon_started.elapsed().as_millis(),
                "startup: icon theme"
            );
            theme
        };

        // Spawn the SQLite tag-db worker. On a brand-new database it imports the tags
        // that previously lived only in profile config (global + per-profile).
        let migration = collect_tag_migration(&state.profiles);
        let tag_db_tx = atlas_core::fs::tag_db::spawn(
            &rt,
            atlas_core::util::config::data_dir().join("tags.db"),
            migration,
            tx.clone(),
        );
        tracing::info!(
            total_ms = startup_started.elapsed().as_millis(),
            "startup: tag db spawned"
        );

        // The resource limit must be in place before the first heavy job.
        atlas_core::resources::apply(state.settings.cpu_limit);

        // Trigger directory loads for all panes (focused first, rest on demand)
        let list_started = std::time::Instant::now();
        let pane_ids = state.all_pane_tile_ids();
        let pane_count = pane_ids.len();
        let show_hidden = state.settings.show_hidden;
        for tile_id in pane_ids {
            let (location, sort_col, sort_dir, folder_order, is_branch) =
                match state.pane_tree.tiles.get(tile_id) {
                    Some(Tile::Pane(p)) => (
                        p.location.clone(),
                        p.sort_col.clone(),
                        p.sort_dir.clone(),
                        p.folder_order,
                        p.is_branch_view,
                    ),
                    _ => continue,
                };
            // A restored Access-mode remote tab can't reconnect before rclone is up.
            #[cfg(feature = "provider-host")]
            if matches!(location, atlas_model::location::Location::Remote { .. }) {
                continue;
            }
            // A restored virtual tag tab re-queries its paths instead of listing a dir.
            if let atlas_model::location::Location::Tag(tag) = location {
                let op_id = state.next_op_id();
                let ts = state.tab_states.entry(tile_id).or_default();
                ts.load_state = LoadState::Loading;
                ts.last_nav_op_id = op_id;
                let _ = tag_db_tx.send(atlas_core::fs::tag_db::TagDbCmd::QueryPathsForTag(tag));
                continue;
            }
            // A restored archive tab lists its inner directory on startup.
            if let atlas_model::location::Location::Archive { archive, inner } = location {
                let op_id = state.next_op_id();
                let ts = state.tab_states.entry(tile_id).or_default();
                ts.load_state = LoadState::Loading;
                ts.last_nav_op_id = op_id;
                let tx_clone = tx.clone();
                let archive2 = archive.clone();
                let inner2 = inner.clone();
                rt.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        let provider = atlas_core::fs::archive::open(&archive)
                            .ok_or_else(|| rust_i18n::t!("app_archive_unsupported").to_string())?;
                        provider.list(&inner).map_err(|e| e.to_string())
                    })
                    .await;
                    let send_result = match result {
                        Ok(Ok(entries)) => BgResult::ArchiveListed {
                            tab: tile_id,
                            op_id,
                            archive: archive2,
                            inner: inner2,
                            entries,
                        },
                        Ok(Err(e)) => BgResult::ArchiveListError {
                            tab: tile_id,
                            op_id,
                            error: e,
                        },
                        Err(e) => BgResult::ArchiveListError {
                            tab: tile_id,
                            op_id,
                            error: e.to_string(),
                        },
                    };
                    let _ = tx_clone.send(send_result);
                });
                continue;
            }
            let path = match location.fs_dir() {
                Some(p) => p.to_path_buf(),
                None => continue,
            };
            let op_id = state.next_op_id();
            let ts = state.tab_states.entry(tile_id).or_default();
            ts.load_state = LoadState::Loading;
            ts.last_nav_op_id = op_id;

            let tx_clone = tx.clone();
            rt.spawn(async move {
                let started = std::time::Instant::now();
                let listing = if is_branch {
                    atlas_core::fs::list::list_branch_dir_with_preview(
                        &path,
                        show_hidden,
                        sort_col,
                        sort_dir,
                        |_| {},
                    )
                    .await
                } else {
                    list_dir(&path, show_hidden, sort_col, sort_dir, folder_order).await
                };
                match listing {
                    Ok(entries) => {
                        let has_excluded_items = super::fs_sync::has_excluded_directory_items(
                            &path,
                            show_hidden,
                            entries.is_empty(),
                        )
                        .await;
                        let _ = tx_clone.send(BgResult::DirListExclusions {
                            tab: tile_id,
                            op_id,
                            has_excluded_items,
                        });
                        let _ = tx_clone.send(BgResult::DirListTiming {
                            tab: tile_id,
                            op_id,
                            elapsed: started.elapsed(),
                        });
                        let _ = tx_clone.send(BgResult::DirListed {
                            tab: tile_id,
                            op_id,
                            entries,
                        });
                    }
                    Err(e) => {
                        let _ = tx_clone.send(BgResult::DirListError {
                            tab: tile_id,
                            op_id,
                            error: e.to_string(),
                        });
                    }
                }
            });
        }
        tracing::info!(
            total_ms = startup_started.elapsed().as_millis(),
            phase_ms = list_started.elapsed().as_millis(),
            panes = pane_count,
            "startup: pane loads queued"
        );

        let update_tx = tx.clone();
        let check_updates = state.settings.check_updates;
        rt.spawn(async move {
            if !check_updates {
                return;
            }
            match atlas_core::update::check_for_update(atlas_core::update::APP_VERSION).await {
                Ok(Some(update)) => {
                    let _ = update_tx.send(BgResult::AppUpdateAvailable {
                        version: update.version,
                        url: update.url,
                    });
                }
                Ok(None) => {}
                Err(err) => tracing::debug!(?err, "update check failed"),
            }
        });

        // Premium entitlement (Store add-on or Polar key) updates the global edition.
        rt.spawn(async {
            // Load the daily counter off the UI thread so Free limit checks never hit the disk.
            let _ = tokio::task::spawn_blocking(atlas_core::usage::preload).await;
            if let Some(source) = atlas_core::license::refresh().await {
                tracing::info!(?source, "premium entitlement active");
            }
        });

        // ── Windows OLE drag-and-drop setup ────────────────────────────────
        // Extract the HWND from gpui's HasWindowHandle impl. Stored as isize
        // so it is Send/clone-able; reconstructed to HWND only in dnd::windows.
        #[cfg(windows)]
        let (hwnd_opt, dnd_opt) = {
            use raw_window_handle::HasWindowHandle;
            let hwnd_raw = window.window_handle().ok().and_then(|h| {
                if let raw_window_handle::RawWindowHandle::Win32(w) = h.as_raw() {
                    Some(w.hwnd.get())
                } else {
                    None
                }
            });
            let dnd = hwnd_raw.and_then(|h| {
                // HiDPI scale from gpui (no cx needed for scale_factor).
                let scale = window.scale_factor();
                crate::dnd::windows::setup_dnd(h, scale)
            });
            (hwnd_raw, dnd)
        };

        // Pre-warm the shell-menu STA worker so the first right-click is instant.
        #[cfg(windows)]
        let shell_menu = Some(crate::shell_menu::spawn());
        #[cfg(windows)]
        if let Some(handle) = &shell_menu {
            handle.prewarm();
        }

        // Extensions: retire official ones whose features are now built in
        // (moved to extensions/.trash, never deleted, never fatal), seed the
        // bundled ones, load enabled overrides and build the registry.
        let retired = atlas_core::ext::install::retire_bundled();
        if !retired.is_empty() {
            tracing::info!("retired bundled extensions: {retired:?}");
        }
        let _ = atlas_core::ext::install::ensure_bundled(
            "media-thumbs",
            crate::app::ext::MEDIA_THUMBS_MANIFEST,
        );
        let _ = atlas_core::ext::install::ensure_asset(
            "media-thumbs",
            "icon.svg",
            crate::app::ext::MEDIA_THUMBS_ICON,
        );
        let _ = atlas_core::ext::install::ensure_bundled(
            "icon-manager",
            crate::app::ext::ICON_MANAGER_MANIFEST,
        );
        let _ = atlas_core::ext::install::ensure_asset(
            "icon-manager",
            "icon.svg",
            crate::app::ext::ICON_MANAGER_ICON,
        );
        let runtime_tools_src =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/runtime-tools");
        let _ = atlas_core::ext::install::ensure_bundled(
            "conversion-pandoc",
            crate::app::ext::CONVERSION_PANDOC_MANIFEST,
        );
        let _ = atlas_core::ext::install::ensure_bundled(
            "conversion-opus",
            crate::app::ext::CONVERSION_OPUS_MANIFEST,
        );
        let _ = atlas_core::ext::install::ensure_bundled_with_assets(
            "runtime-tools",
            crate::app::ext::RUNTIME_TOOLS_MANIFEST,
            &runtime_tools_src,
        );
        let cloud_src =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/cloud");
        let _ = atlas_core::ext::install::ensure_bundled_with_assets(
            "cloud",
            crate::app::ext::CLOUD_MANIFEST,
            &cloud_src,
        );
        let _ = atlas_core::ext::install::ensure_asset(
            "cloud",
            "icon.svg",
            crate::app::ext::CLOUD_ICON,
        );
        state.ext.enabled = atlas_core::util::config::load_ext_enabled();
        let ext_started = std::time::Instant::now();
        let ext_registry = atlas_core::ext::Registry::load(&state.ext.enabled);
        #[cfg(feature = "provider-host")]
        if ext_registry.has_sidebar_section("cloud") {
            state.profiles.ensure_cloud_section();
        }
        tracing::info!(
            total_ms = startup_started.elapsed().as_millis(),
            phase_ms = ext_started.elapsed().as_millis(),
            "startup: extensions"
        );

        // Spawn the cloud worker (rclone sidecar is started lazily on first use)
        // and restore previously-configured remotes for the sidebar.
        #[cfg(feature = "provider-host")]
        let cloud_tx = atlas_core::cloud::spawn(&rt, tx.clone());
        #[cfg(feature = "provider-host")]
        {
            state.cloud.remotes = atlas_core::util::config::load_cloud_remotes();
            // Auto-mount remotes the user flagged. Not added to `navigate_on_mount`,
            // so they mount silently without hijacking the startup tab.
            if ext_registry.is_enabled("cloud") {
                for r in &state.cloud.remotes {
                    if r.auto_mount && r.access == atlas_core::cloud::AccessKind::Mount {
                        let _ =
                            cloud_tx.send(atlas_core::cloud::CloudCmd::Mount { id: r.id.clone() });
                    }
                }
            }
        }

        // Channels and input wake this task. Timers exist only for active
        // animations, queries, caret edges and toast deadlines.
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let mut delay = Some(Duration::ZERO);
            loop {
                smol::future::or(
                    async {
                        let _ = wake_rx.recv_async().await;
                    },
                    async {
                        if let Some(delay) = delay {
                            Timer::after(delay).await;
                        } else {
                            std::future::pending::<()>().await;
                        }
                    },
                )
                .await;
                let alive = this
                    .update(cx, |app, cx| {
                        let mut dirty = app.drain_bg_results(cx);
                        dirty |= app.tick_ctx_input_blink();
                        dirty |= app.tick_batch_blink(cx);
                        // Drive matching only until this query's snapshot is ready;
                        // streamed items must not repaint an already published list.
                        if app.state.overlay.show_goto_bar
                            && !app.state.profiles.active().search_in_content
                            && !app.state.overlay.goto_state.query.trim().is_empty()
                            && app.goto_snapshot.is_pending()
                        {
                            dirty |= app.nucleo.tick();
                        }
                        // Drain Windows OLE drop payloads enqueued by our IDropTarget.
                        #[cfg(windows)]
                        {
                            dirty |= app.drain_dnd_queue();
                            dirty |= app.drain_shell_menu_events(cx);
                        }
                        // Finalize a palette's exit animation once its fade-out has elapsed.
                        dirty |= app.tick_palette_close();
                        // Finalize pane close-out animations: queue their dock removal.
                        dirty |= app.tick_pane_close();
                        dirty |= app.tick_tab_close();
                        dirty |= app.tick_toasts();
                        if dirty {
                            cx.notify();
                        }
                        delay = app.maintenance_delay();
                        if app.close_after_tab_transfer {
                            let _ = cx.update_window(window_handle, |_, window, _| {
                                window.remove_window()
                            });
                        }
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        // Restore per-profile Details column widths + order (falls back to defaults
        // when the active profile has never customized them).
        let details_prefs = state
            .profiles
            .active()
            .scoped::<crate::app::profiles::DetailsColumns>();

        let nucleo = NucleoPool::new();
        let goto_warm_nucleo = NucleoPool::new();
        atlas_core::fs::index::build_warm_navigation_index(goto_warm_nucleo.injector());
        let goto_root = state
            .active_tab_data()
            .and_then(|td| td.location.fs_dir().map(std::path::Path::to_path_buf))
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        atlas_core::fs::index::build_index(goto_root.clone(), nucleo.injector());
        let watchers = state
            .all_pane_tile_ids()
            .into_iter()
            .filter_map(|id| {
                let Tile::Pane(pane) = state.pane_tree.tiles.get(id)? else {
                    return None;
                };
                let path = pane.location.fs_dir()?;
                atlas_core::fs::watcher::watch_path(path, tx.clone())
                    .ok()
                    .map(|watcher| (id, watcher))
            })
            .collect();

        // Ensure background search service is active (instant search daemon)
        if std::env::var_os("ATLAS_DISABLE_SEARCH_SERVICE").is_none() {
            rt.spawn_blocking(atlas_core::search::ensure_service_started);
        }

        // transfer_journal resume is WIP and not wired into atlas-core yet.

        #[cfg(windows)]
        if let Some(crash) = atlas_win_space::crash::previous_crash() {
            tracing::warn!(summary = ?crash.summary, reports = crash.reports.len(), "previous session crashed");
            state
                .overlay
                .op_errors
                .push_back(atlas_model::overlay::Toast::new(
                    rust_i18n::t!("crash_previous_session").into_owned(),
                    atlas_model::overlay::ToastKind::Error,
                ));
        }

        let thumbnail_cache_mb = state.settings.thumbnail_cache_mb;
        Self {
            mounted_volumes: Vec::new(),
            state,
            startup_started,
            startup_first_render_logged: false,
            close_after_tab_transfer: false,
            crash_report_sent: false,
            #[cfg(windows)]
            pending_tab_restore,
            tx,
            rx,
            rt,
            focus_handle,
            scroll_handles: HashMap::new(),
            miller_scroll_handles: HashMap::new(),
            tree_loading: HashSet::new(),
            watchers,
            nucleo,
            goto_warm_nucleo,
            goto_index_root: Some((goto_root, false)),
            goto_snapshot: Default::default(),
            goto_service_cache: None,
            goto_service_inflight: None,
            goto_service_failed: None,
            goto_service_generation: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            goto_prefill: None,
            fuzzy_scorer: atlas_core::search::fuzzy::FuzzyScorer::new(),
            palette_scroll: gpui::ScrollHandle::new(),
            ext_registry,
            tag_db_tx,
            #[cfg(feature = "provider-host")]
            cloud_tx,
            // Phase5: preview texture caches + overlay focus start empty.
            preview_textures: HashMap::new(),
            multi_textures: HashMap::new(),
            dir_ring: None,
            tree_rows_cache: HashMap::new(),

            preview_view: HashMap::new(),
            inspector_animating: false,
            last_viewport_size: None,
            window_resize_suppress_until: None,
            preview_drag: None,
            overlay_focus: None,
            batch_rename_field: BatchRenameField::Pattern,
            // Phase4: grid thumbnail cache starts empty.
            thumb_textures: crate::ui::thumbnail_cache::ThumbnailCache::new(thumbnail_cache_mb),
            shell_icons: std::collections::HashMap::new(),
            shell_icon_inflight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            shell_icon_lru: std::collections::VecDeque::new(),
            ext_thumb_inflight: HashSet::new(),
            // Icon theme resolved above from the active profile (selection or
            // VSCode auto-discovery).
            icon_theme,
            icon_cache: HashMap::new(),
            liquid_glass: crate::app::liquid_glass::LiquidGlassCache::new(),
            light_surfaces: false,
            icon_tx,
            icon_rx,
            // Phase A: dock built lazily on first render from the restored pane_tree.
            file_panel_cache: HashMap::new(),
            space_lens: HashMap::new(),
            pane_tabs_cache: HashMap::new(),
            pane_tabs_fp: HashMap::new(),
            pane_tabs_bounds: HashMap::new(),
            dock_flip_last_frame: false,
            dock_settling: false,
            sidebar_panel: None,
            inspector_host: None,
            dock_host: None,
            focus_history: std::collections::VecDeque::new(),
            palette_host: None,
            palette_closing: None,
            layout_revision: 0,
            // Phase C: rubber-band selection state.
            rubber: None,
            pending_folder_click: None,
            recent_folder_navigation: None,
            row_rects: std::cell::RefCell::new(HashMap::new()),
            miller_row_rects: std::cell::RefCell::new(HashMap::new()),
            details_hit: HashMap::new(),
            details_row_rings: HashMap::new(),
            details_row_texts: Default::default(),
            grid_hit: HashMap::new(),
            grid_hover: None,
            file_surface_shapes: Default::default(),
            list_hit: HashMap::new(),
            list_hover: None,
            grid_flip_states: std::rc::Rc::new(std::cell::RefCell::new(HashMap::new())),

            details_hover: None,
            miller_column_bounds: HashMap::new(),
            dropzone_selection: Default::default(),
            dropzone_rects: Vec::new(),
            dropzone_bounds: None,
            dropzone_drag_paths: HashSet::new(),
            dropzone_pending_tags: HashMap::new(),
            operation_feedback: Default::default(),
            hidden_operation_notices: Default::default(),
            toast_stack: Default::default(),
            recycle_bin_full: None,
            recycle_bin_status_inflight: false,
            recycle_bin_status_checked: false,
            recycle_bin_status_dirty: false,
            recycle_bin_icon_dirty: false,
            view_selection_pending: Default::default(),
            tab_reorder_preview: None,
            view_scroll_pending: Default::default(),
            pending_transfers: std::collections::VecDeque::new(),
            pending_pane_zones: std::cell::RefCell::new(Vec::new()),
            goto_input: None,
            palette_input: None,
            ext_palette_input: None,
            search_inputs: HashMap::new(),
            batch_pattern_input: None,
            batch_replacement_input: None,
            rename_profile_input: None,
            getting_started_profile_input: None,
            create_tag_input: None,
            renaming_profile_idx: None,
            settings_input: None,
            theme_editor: None,
            theme_fork_error: None,
            settings_ui: crate::ui::settings_palette::SettingsUi::default(),
            settings_font_families: None,
            rename_edit: None,
            batch_input_sub: None,
            content_results: Vec::new(),
            content_gen: 0,
            content_cancel: None,
            theme_dirty: true,
            theme_follow_pending: true,
            theme_preview_before: None,
            settings_theme_previewing: false,
            view_preview_before: None,
            icon_style_preview_before: None,
            sidebar_width_dirty: true,
            sidebar_render_width: None,
            sidebar_visual_collapsed: false,
            sidebar_width_animating: false,
            #[cfg(windows)]
            hwnd: hwnd_opt,
            #[cfg(windows)]
            network: Default::default(),
            #[cfg(windows)]
            dnd: dnd_opt,
            #[cfg(windows)]
            shell_menu,
            dropped_in_background: false,
            sidebar_hit_model: None,
            was_inactive: false,
            inactive_since: None,
            eco_cleared: false,
            converter: None,
            cleanup_panel: None,
            compare_panel: None,
            pending_compare: None,
            git_panel: None,
            pending_git: None,
            git_probes: Default::default(),
            vault: None,
            #[cfg(windows)]
            vault_mount: None,
            ctx_menu: None,
            ctx_menu_seq: 0,
            #[cfg(windows)]
            ctx_native: Default::default(),
            suppress_next_shell_ctx_menu: false,
            type_filter_scroll: gpui::ScrollHandle::new(),
            settings_scroll: gpui::ScrollHandle::new(),
            getting_started_theme_scroll: gpui::ScrollHandle::new(),
            ext_manager_scroll: gpui::ScrollHandle::new(),
            pane_zoom_sliders: HashMap::new(),
            pane_zoom_slider_subs: HashMap::new(),
            open_view_selectors: HashSet::new(),
            preview_editor: None,
            tag_picker_menu: None,
            tag_picker_input: None,
            tag_picker_sub: None,
            _rubber_escape_sub: Some(rubber_escape_sub),
            tag_picker_scroll: gpui::ScrollHandle::new(),
            thumb_wanted: std::sync::Arc::new(std::sync::Mutex::new(HashSet::new())),
            thumb_wanted_by_pane: HashMap::new(),
            native_thumb_inflight: 0,
            pane_animations: HashMap::new(),
            pane_signals: HashMap::new(),
            dirty_panes: HashSet::new(),
            focus_painted: None,
            sidebar_scroll_region: None,
            atlas_launcher_profiles_anchor: gpui::Point::default(),
            pane_layout_bounds: HashMap::new(),
            pane_layout_animations: HashMap::new(),
            dock_root_bounds: None,
            dock_split_animations: HashMap::new(),
            pane_closing: HashMap::new(),
            tab_closing: HashMap::new(),
            pending_column_reflow: HashSet::new(),
            pane_items_appear: HashMap::new(),
            pane_view_transitions: HashMap::new(),
            ext_manager_input: None,
            ext_prompt_inputs: Vec::new(),
            #[cfg(feature = "provider-host")]
            cloud_add_inputs: None,
            #[cfg(feature = "provider-host")]
            rename_remote_input: None,
            pending_rename: None,
            pending_select: initial_select,
            pending_batch_select: None,
            batch_blink_origin: std::time::Instant::now(),
            batch_blink_visible: true,
            col_widths: details_prefs.widths,
            typed_selection: None,
            #[cfg(windows)]
            win_e_enabled: explorer_choice.start_page,
            #[cfg(windows)]
            explorer_folders: explorer_choice.folders,
            preview_delay: Default::default(),
            details_column_order: details_prefs.order,
            col_resize: None,
            col_dragging: None,
            col_drag_x: 0.0,
            col_drag_grab_x: 0.0,
            col_drag_pane: None,
            type_filters: initial_type_filters,
            size_filters: initial_size_filters,
            file_filter_projections: HashMap::new(),
            type_filter_menu: None,
            type_filter_input: None,
            type_filter_sub: None,
            breadcrumb_menu: None,
            breadcrumb_menu_input: None,
            breadcrumb_menu_sub: None,
            size_filter_menu: None,
            drag_hover: crate::ui::drag_hover::DragHoverState::default(),
            pending_drag_hover_targets: Vec::new(),
            active_external_drag: None,
            pane_view_bounds: HashMap::new(),
            dock_linear_bounds: HashMap::new(),
            dock_resize: None,
            sidebar_resize: None,
            inspector_resize: None,
            dirty_all_panes: false,
            command_shortcut_capture: None,
            command_shortcut_conflict: None,
            vim_pending_key: None,
        }
    }
}

fn migrate_global_recent_folders(state: &mut AppState) {
    if state.settings.recent_folders.is_empty() {
        return;
    }
    use atlas_model::profiles::RecentFoldersFeature;
    let legacy = std::mem::take(&mut state.settings.recent_folders);
    for profile in &mut state.profiles.profiles {
        if profile.scoped::<RecentFoldersFeature>().is_empty() {
            profile.set_scoped::<RecentFoldersFeature>(&legacy);
        }
    }
}

/// Which Explorer windows open Atlas. Win+E turned on before the redirect
/// agent existed only set the registry override, which recent Windows 11
/// builds ignore for Win+E; complete that choice once so the shortcut works.
/// The agent is (re)started off the UI thread when any part is on.
#[cfg(windows)]
fn explorer_choice_at_startup() -> atlas_win_space::explorer_redirect::Redirect {
    use atlas_win_space::explorer_redirect;
    let mut choice = explorer_redirect::current();
    let migrate = !explorer_redirect::configured()
        && atlas_win_space::integration::win_e_enabled().unwrap_or(false);
    if migrate {
        choice.start_page = true;
    }
    if choice.any() {
        std::thread::spawn(move || {
            let result = if migrate {
                explorer_redirect::set(choice)
            } else {
                explorer_redirect::ensure_agent()
            };
            if let Err(error) = result {
                tracing::warn!(%error, "cannot start the Explorer redirect agent");
            }
        });
    }
    choice
}
