//! Hidden native regression harness. Cloud replies are fixtures; no user OAuth,
//! accounts, clipboard, configuration or remote data are touched.
use crate::{app::App, ui::drag_hover::DragHoverDropCommand};
use atlas_model::{
    app_state::BgResult,
    file_entry::{FileEntry, FileEntryData, FileKind},
    location::Location,
    settings::{Settings, ViewMode},
};
use gpui::{px, size, AppContext, Entity, Window, WindowBounds, WindowOptions};
use gpui_component::{Root, ThemeRegistry};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, UNIX_EPOCH},
};

fn draw(window: &mut Window, cx: &mut gpui::App) {
    window.simulate_next_frame(cx);
    window.refresh();
    window.draw(cx).clear(cx);
}

fn fixture_entries() -> Vec<FileEntry> {
    (0..40)
        .map(|index| {
            let name = if (20..28).contains(&index) {
                "Untitled document.docx".into()
            } else {
                format!("Document {index:02}.txt")
            };
            FileEntry::from(FileEntryData {
                path: PathBuf::from(&name),
                name: name.into(),
                kind: FileKind::File,
                size: Some(index + 1),
                modified: Some(UNIX_EPOCH + Duration::from_secs(index)),
                _is_hidden: false,
                _is_symlink: false,
                archive_root: None,
                recycle_info: None,
            })
        })
        .collect()
}

fn massive_entries(count: usize) -> Vec<FileEntry> {
    (0..count)
        .map(|index| {
            let name = format!("Object {index:07}.txt");
            FileEntry::from(FileEntryData {
                path: atlas_model::remote_path::join("", &format!("object-{index}"), &name, false)
                    .into(),
                name: name.into(),
                kind: FileKind::File,
                size: Some(index as u64),
                modified: None,
                _is_hidden: false,
                _is_symlink: false,
                archive_root: None,
                recycle_info: None,
            })
        })
        .collect()
}

fn fixture_reply(app: &Entity<App>, cx: &mut gpui::App) {
    app.update(cx, |app, cx| {
        let tab = app.state.focused_tile;
        let op_id = app.state.tab_states[&tab].last_nav_op_id;
        app.process_bg_results(vec![BgResult::DirListed {
            tab,
            op_id,
            entries: fixture_entries(),
        }]);
        app.notify_pane(tab, cx);
        cx.notify();
    });
}

fn capture(directory: &Path, name: &str, window: &mut Window) -> anyhow::Result<()> {
    window
        .render_to_image()?
        .save(directory.join(format!("{name}.png")))?;
    Ok(())
}

pub fn run(directory: PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(&directory)?;
    let directory = directory.canonicalize()?;
    let directory = PathBuf::from(directory.to_string_lossy().trim_start_matches(r"\\?\"));
    let fixtures = directory.join("local");
    std::fs::create_dir_all(&fixtures)?;
    std::fs::write(
        fixtures.join("local-only.txt"),
        "local directory survived cloud navigation",
    )?;
    let config = directory.join("config");
    std::fs::create_dir_all(&config)?;
    std::env::set_var("ATLAS_CONFIG_DIR", &config);
    std::env::set_var("ATLAS_DISABLE_SEARCH_SERVICE", "1");
    rust_i18n::set_locale("es");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let rt = runtime.handle().clone();
    let result = std::rc::Rc::new(std::cell::RefCell::new(Ok(())));
    let final_result = result.clone();
    gpui_platform::application().with_assets(crate::assets::AtlasAssets).run(move |cx| {
        gpui_component::init(cx);
        cx.text_system().add_fonts(crate::assets::bundled_fonts()).unwrap();
        ThemeRegistry::global_mut(cx).load_themes_from_str(
            include_str!("../../../assets/themes/atlas-themes.json")).unwrap();
        let mut app = None;
        let initial = fixtures.clone();
        let window = cx.open_window(WindowOptions {
            show: false, focus: false,
            window_bounds: Some(WindowBounds::centered(size(px(1000.), px(1050.)), cx)),
            ..Default::default()
        }, |window, cx| {
            let view = cx.new(|cx| App::new(rt, Settings {
                autosave_session: false, getting_started_seen: true, show_inspector: false,
                view_mode: ViewMode::Details, ..Default::default()
            }, Instant::now(), vec![initial], window, cx));
            app = Some(view.clone());
            cx.new(|cx| Root::new(view, window, cx))
        }).unwrap();
        let app = app.unwrap();
        let window: gpui::AnyWindowHandle = window.into();
        cx.spawn(async move |cx| {
            let checked: anyhow::Result<()> = async {
                window.update(cx, |_, window, _| window.resize(size(px(1000.), px(1050.))))?;
                for _ in 0..20 {
                    cx.background_executor().timer(Duration::from_millis(30)).await;
                    window.update(cx, |_, window, cx| draw(window, cx))?;
                }
                // Replace just the cloud transport with a recorded fixture channel.
                let (tx, rx) = flume::unbounded();
                let old_remote_op = window.update(cx, |_, window, cx| {
                    app.update(cx, |app, _| {
                        app.cloud_tx = tx;
                        let pane = app.state.focused_tile;
                        if let Some(atlas_model::layout_tree::Tile::Pane(tab)) = app.state.pane_tree.tiles.get_mut(pane) {
                            tab.view_mode = ViewMode::Details;
                        }
                        app.navigate_remote(pane, "drive-one".into(), "".into());
                    });
                    fixture_reply(&app, cx);
                    draw(window, cx);
                    capture(&directory, "cloud-duplicates-idle", window)?;
                    Ok::<_, anyhow::Error>(app.read(cx).state.tab_states[&app.read(cx).state.focused_tile].last_nav_op_id)
                })??;
                // Repeated frames with no pointer input must preserve all row slots.
                for _ in 0..30 {
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                    window.update(cx, |_, window, cx| draw(window, cx))?;
                }
                window.update(cx, |_, window, cx| {
                    {
                        let app = app.read(cx);
                        let pane = app.state.focused_tile;
                        let hit = &app.details_hit[&pane];
                        for (row, y) in &hit.presented_rows {
                            anyhow::ensure!((*y - *row as f32 * hit.row_h).abs() < 0.1, "duplicate rows overlap at row {row}");
                        }
                        let ring = &app.details_row_rings[&pane];
                        anyhow::ensure!(ring.collect_rows(hit.start_idx, hit.end_idx).len() == hit.end_idx - hit.start_idx,
                            "visible rows are missing without pointer input");
                        anyhow::ensure!(matches!(app.tab_drop_command(pane), Some(DragHoverDropCommand::TransferTo(path))
                            if path == PathBuf::from("remote:drive-one")));
                        anyhow::ensure!(matches!(app.folder_drop_command(pane, Path::new("nested")), DragHoverDropCommand::TransferTo(path)
                            if path == PathBuf::from("remote:drive-one/nested")));
                    }
                    capture(&directory, "cloud-duplicates-settled", window)?;
                    app.update(cx, |app, _| {
                        let pane = app.state.focused_tile;
                        app.navigate(pane, fixtures.clone());
                    });
                    {
                        let app = app.read(cx);
                        let pane = app.state.focused_tile;
                        anyhow::ensure!(app.tab_data_for(pane).unwrap().location == Location::Directory(fixtures.clone()),
                            "absolute local destination was sent to cloud");
                    }
                    app.update(cx, |app, _| {
                        let tab = app.state.focused_tile;
                        app.process_bg_results(vec![BgResult::DirListed { tab, op_id: old_remote_op, entries: fixture_entries() }]);
                    });
                    Ok::<_, anyhow::Error>(())
                })??;
                for _ in 0..20 {
                    cx.background_executor().timer(Duration::from_millis(30)).await;
                    window.update(cx, |_, window, cx| draw(window, cx))?;
                }
                window.update(cx, |_, window, cx| {
                    {
                        let app = app.read(cx);
                        let entries = &app.state.tab_states[&app.state.focused_tile].entries;
                        anyhow::ensure!(entries.len() == 1 && entries[0].name.as_str() == "local-only.txt", "stale cloud result replaced local listing");
                    }
                    capture(&directory, "local-after-cloud", window)?;
                    app.update(cx, |app, _| {
                        let pane = app.state.focused_tile;
                        app.navigate_back(pane);
                        assert!(matches!(&app.tab_data_for(pane).unwrap().location, Location::Remote { id, path } if id == "drive-one" && path.is_empty()));
                        app.navigate_clicked_folder(pane, "nested".into());
                        app.navigate_up(pane);
                        assert!(matches!(&app.tab_data_for(pane).unwrap().location, Location::Remote { id, path } if id == "drive-one" && path.is_empty()));
                    });
                    fixture_reply(&app, cx);
                    let first_key = app.update(cx, |app, _| {
                        app.load_cloud_preview_for_selection();
                        app.state.preview.paths[&app.state.focused_tile].clone()
                    });
                    app.update(cx, |app, _| {
                        app.navigate_remote(app.state.focused_tile, "drive-two".into(), "".into());
                    });
                    fixture_reply(&app, cx);
                    app.update(cx, |app, _| {
                        app.load_cloud_preview_for_selection();
                        let second_key = &app.state.preview.paths[&app.state.focused_tile];
                        assert_ne!(&first_key, second_key, "accounts share a preview cache identity");
                        app.navigate_back(app.state.focused_tile);
                        assert!(matches!(&app.tab_data_for(app.state.focused_tile).unwrap().location, Location::Remote { id, .. } if id == "drive-one"));
                    });
                    for command in rx.try_iter() {
                        if let atlas_core::cloud::CloudCmd::ListRemote { path, .. } = command {
                            anyhow::ensure!(!Path::new(&path).is_absolute(), "local path leaked into cloud transport");
                        }
                    }
                    // Start a real GPUI pointer drag, then change the focused
                    // destination to local while the payload retains Drive A.
                    let source = atlas_model::remote_path::join("", "selected-object", "dragged.txt", false);
                    app.update(cx, |app, cx| {
                        let tab = app.state.focused_tile;
                        app.navigate_remote(tab, "drive-one".into(), "".into());
                        let op_id = app.state.tab_states[&tab].last_nav_op_id;
                        app.process_bg_results(vec![BgResult::DirListed { tab, op_id, entries: vec![FileEntryData {
                            name: "dragged.txt".into(), path: source.clone().into(), kind: FileKind::File,
                            size: Some(12), modified: None, _is_hidden: false, _is_symlink: false,
                            archive_root: None, recycle_info: None,
                        }.into()] }]);
                        app.notify_pane(tab, cx); cx.notify();
                    });
                    draw(window, cx);
                    let start = {
                        let app = app.read(cx);
                        app.details_hit[&app.state.focused_tile].row_bounds(0).center()
                    };
                    window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                        position: start, modifiers: Default::default(), button: gpui::MouseButton::Left,
                        click_count: 1, first_mouse: false,
                    }), cx);
                    draw(window, cx);
                    window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                        position: start + gpui::point(px(25.), px(5.)), modifiers: Default::default(), pressed_button: Some(gpui::MouseButton::Left),
                    }), cx);
                    draw(window, cx);
                    anyhow::ensure!(cx.active_drag().is_some_and(|drag| drag.value.is::<crate::ui::root::DraggedCloudFiles>()),
                        "a remote pointer drag did not create a typed cloud payload");
                    app.update(cx, |app, _| app.navigate(app.state.focused_tile, fixtures.clone()));
                    draw(window, cx);
                    let destination = {
                        let app = app.read(cx);
                        let bounds = app.pane_view_bounds[&app.state.focused_tile];
                        gpui::point(bounds.center().x, bounds.bottom() - px(90.))
                    };
                    window.dispatch_event(gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                        position: destination, modifiers: Default::default(), pressed_button: Some(gpui::MouseButton::Left),
                    }), cx);
                    draw(window, cx);
                    capture(&directory, "cloud-drag-local-preview", window)?;
                    window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                        position: destination, modifiers: Default::default(), button: gpui::MouseButton::Left, click_count: 1,
                    }), cx);
                    draw(window, cx);
                    anyhow::ensure!(rx.try_iter().any(|command| matches!(command,
                        atlas_core::cloud::CloudCmd::EnqueueDownload { id, remote_path, local_path, open_on_done: false, .. }
                        if id == "drive-one" && remote_path == source && local_path == fixtures.join("dragged.txt"))),
                        "cloud drag lost its source account or used the local engine");
                    app.update(cx, |app, _| {
                        let payload = crate::ui::root::DraggedCloudFiles { account: "drive-one".into(), account_label: "Drive A".into(), paths: vec![source.clone().into()] };
                        assert!(app.copy_cloud_files_to(&payload, Path::new("remote:drive-two/folder")));
                    });
                    anyhow::ensure!(rx.try_iter().any(|command| matches!(command,
                        atlas_core::cloud::CloudCmd::EnqueueRemoteCopy { source_id, source: path, destination_id, destination, .. }
                        if source_id == "drive-one" && path == source && destination_id == "drive-two" && destination == "folder/dragged.txt")),
                        "cross-account cloud drag lost its destination account");
                    std::fs::write(directory.join("tests.json"), serde_json::to_vec_pretty(&[
                        serde_json::json!({"name":"Duplicate cloud rows remain visible without mouse input", "status":"passed"}),
                        serde_json::json!({"name":"Cloud to local navigation rejects stale cloud listings", "status":"passed"}),
                        serde_json::json!({"name":"Back, up, and account switching preserve typed locations", "status":"passed"}),
                        serde_json::json!({"name":"Pane and folder drops retain account and remote destination", "status":"passed"}),
                        serde_json::json!({"name":"Preview identity is isolated per account and navigation", "status":"passed"}),
                        serde_json::json!({"name":"Real GPUI cloud pointer drag into a local pane retains its object and account", "status":"passed"}),
                        serde_json::json!({"name":"Cloud drag routing queues an isolated cross-account copy", "status":"passed"}),
                    ])?)?;
                    Ok::<_, anyhow::Error>(())
                })??;
                if std::env::var_os("ATLAS_CLOUD_SCALE_DIAGNOSTIC").is_some() {
                    let mut measurements = Vec::new();
                    for count in [10_000, 100_000, 1_000_000] {
                        let entries = massive_entries(count);
                        let initial = Instant::now();
                        window.update(cx, |_, window, cx| {
                            app.update(cx, |app, cx| {
                                let tab = app.state.focused_tile;
                                app.navigate_remote(tab, "scale-account".into(), "".into());
                                let op_id = app.state.tab_states[&tab].last_nav_op_id;
                                app.process_bg_results(vec![BgResult::DirListed { tab, op_id, entries }]);
                                app.notify_pane(tab, cx);
                                cx.notify();
                            });
                            draw(window, cx);
                        })?;
                        let ingest_ms = initial.elapsed().as_secs_f64() * 1000.;
                        let mut frames = Vec::new();
                        for frame in 0..60 {
                            cx.background_executor().timer(Duration::from_millis(16)).await;
                            let started = Instant::now();
                            window.update(cx, |_, window, cx| {
                                app.update(cx, |app, cx| {
                                    let pane = app.state.focused_tile;
                                    if let Some(scroll) = app.scroll_handles.get(&pane) {
                                        let base = &scroll.0.borrow().base_handle;
                                        base.set_offset(gpui::point(px(0.), px(-(frame as f32) * 1400.)));
                                    }
                                    app.notify_pane(pane, cx);
                                    cx.notify();
                                });
                                draw(window, cx);
                            })?;
                            frames.push(started.elapsed().as_secs_f64() * 1000.);
                        }
                        frames.sort_by(f64::total_cmp);
                        measurements.push(serde_json::json!({
                            "name": format!("Native Details mock: {count} entries"),
                            "metrics": { "ingest_ms": ingest_ms, "frame_p50_ms": frames[30], "frame_p95_ms": frames[57], "frame_max_ms": frames[59] },
                            "status": if frames[57] <= 16.7 { "passed" } else { "failed" },
                            "context": "Hidden native window; scripted scroll; fixture listing; no network. Includes UI update and draw, excludes the 16ms timer."
                        }));
                        std::fs::write(directory.join("scale.json"), serde_json::to_vec_pretty(&measurements)?)?;
                    }
                }
                Ok(())
            }.await;
            if let Err(error) = &checked {
                let _ = std::fs::write(directory.join("failed.txt"), format!("{error:#}"));
            }
            *result.borrow_mut() = checked;
            let _ = cx.update(|cx| cx.quit());
        }).detach();
    });
    let outcome = std::mem::replace(&mut *final_result.borrow_mut(), Ok(()));
    outcome
}
