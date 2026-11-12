//! Optional native render/interaction harness. Never linked into normal builds.
use crate::app::App;
use gpui::{Entity, Window};
use std::path::PathBuf;

pub(crate) fn install(view: &Entity<App>, window: &mut Window, cx: &mut gpui::App) {
    let Some(directory) = std::env::var_os("ATLAS_CONVERSION_LAB").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&directory).unwrap();
    let audio = std::env::var_os("ATLAS_CONVERSION_LAB_AUDIO").is_some();
    let package = std::env::var_os("ATLAS_CONVERSION_LAB_PACKAGE").is_some();
    let opus_package = std::env::var("ATLAS_CONVERSION_LAB_PACKAGE")
        .ok()
        .as_deref()
        == Some("opus");
    let video = std::env::var_os("ATLAS_CONVERSION_LAB_VIDEO").map(PathBuf::from);
    let source = directory.join(if package && !opus_package {
        "Documento.rst"
    } else if video.is_some() {
        "Clip.mov"
    } else if audio || opus_package {
        "Tone.wav"
    } else {
        "Sample.png"
    });
    if let Some(video) = video {
        std::fs::copy(video, &source).unwrap();
    } else if package && !opus_package {
        std::fs::write(&source, "Documento\n=========\n\nTexto para convertir.\n").unwrap();
    } else if audio || opus_package {
        // Explicit 16-bit PCM WAV fixture; keep the validation encoder unrelated
        // to the converter's audio implementation.
        let samples: Vec<_> = (0..22050)
            .flat_map(|i| {
                (((f64::from(i) * 440.0 * std::f64::consts::TAU / 44100.0).sin() * 16000.0).round()
                    as i16)
                    .to_le_bytes()
            })
            .collect();
        let mut wave = b"RIFF".to_vec();
        wave.extend_from_slice(&(36_u32 + samples.len() as u32).to_le_bytes());
        wave.extend_from_slice(
            b"WAVEfmt \x10\0\0\0\x01\0\x01\0\x44\xac\0\0\x88\x58\x01\0\x02\0\x10\0data",
        );
        wave.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        wave.extend_from_slice(&samples);
        std::fs::write(&source, wave).unwrap();
    } else {
        image::RgbaImage::from_pixel(160, 100, image::Rgba([240, 130, 65, 255]))
            .save(&source)
            .unwrap();
    }
    view.update(cx, |app, cx| {
        app.state.overlay.show_getting_started = false;
        // In-memory only (the lab never saves): render the glass treatment.
        if std::env::var_os("ATLAS_CONVERSION_LAB_GLASS").is_some()
            && !app.state.profiles.active().theme.ends_with(" Glass")
        {
            let theme = &mut app.state.profiles.active_mut().theme;
            theme.push_str(" Glass");
        }
        app.open_converter_for(vec![source]);
        cx.notify();
    });
    drive(view.clone(), directory, window, cx);
}

fn drive(view: Entity<App>, directory: PathBuf, window: &Window, cx: &mut gpui::App) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        for step in 0..=600 {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(20))
                .await;
            let done = handle
                .update(cx, |_, window, cx| {
                    // Exercise real layout/paint on a hidden window. No presenting
                    // or focusing a window on the user's desktop is necessary.
                    // A platform frame can consume the previous refresh before
                    // this task runs. Force a complete scene for offscreen capture.
                    window.refresh();
                    window.draw(cx).clear(cx);
                    frame(&view, &directory, step, window, cx)
                })
                .unwrap_or(true);
            if done {
                break;
            }
        }
    })
    .detach();
}

fn frame(
    view: &Entity<App>,
    directory: &std::path::Path,
    step: usize,
    window: &mut Window,
    cx: &mut gpui::App,
) -> bool {
    if std::env::var_os("ATLAS_CONVERSION_LAB_PACKAGE").is_some() {
        return package_frame(view, directory, step, window, cx);
    }
    if std::env::var_os("ATLAS_CONVERSION_LAB_ACTIONS").is_some() {
        return actions_frame(view, directory, step, window, cx);
    }
    if step == 12 {
        if std::env::var_os("ATLAS_CONVERSION_LAB_VIDEO").is_some() {
            let session = view.read(cx).converter.as_ref().unwrap();
            assert!(matches!(session.phase, super::Phase::Choosing));
            assert_eq!(session.options()[0].engines, ["Windows"]);
            assert_eq!(session.options()[0].target.extension, "mp4");
        }
        if std::env::var_os("ATLAS_CONVERSION_LAB_AUDIO").is_some() {
            let session = view.read(cx).converter.as_ref().unwrap();
            if matches!(session.phase, super::Phase::Choosing) {
                let options = session.options();
                assert_eq!(options[0].engines, ["Windows"]);
                assert_eq!(options[0].target.extension, "mp3");
            } else {
                panic!("System audio discovery did not finish");
            }
        }
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("wheel-desktop.png"))
            .unwrap();
        window.resize(gpui::size(gpui::px(900.), gpui::px(560.)));
    }
    if step == 25 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("wheel-laptop.png"))
            .unwrap();
        // Full catalog (engine-gated targets too) exercises the outer rings.
        view.update(cx, |app, cx| {
            let session = app.converter.as_mut().unwrap();
            session.catalog = atlas_core::convert::options(&session.sources, &session.tools);
            cx.notify();
        });
    }
    if step == 30 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("rings-laptop.png"))
            .unwrap();
        view.update(cx, |app, cx| {
            let session = app.converter.as_mut().unwrap();
            session.catalog = atlas_core::convert::native_options(&session.sources, &session.tools);
            session.selected = 0;
            cx.notify();
        });
    }
    if step == 16 {
        window.dispatch_keystroke(gpui::Keystroke::parse("right").unwrap(), cx);
        assert_eq!(view.read(cx).converter.as_ref().unwrap().selected, 1);
        window.dispatch_keystroke(gpui::Keystroke::parse("left").unwrap(), cx);
        assert_eq!(view.read(cx).converter.as_ref().unwrap().selected, 0);
    }
    if step == 38 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("options-laptop.png"))
            .unwrap();
        if std::env::var_os("ATLAS_CONVERSION_LAB_MOUSE").is_some() {
            let bounds = view
                .read(cx)
                .converter
                .as_ref()
                .unwrap()
                .wheel_bounds
                .unwrap();
            let center = bounds.center();
            window.dispatch_event(
                gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                    position: gpui::point(center.x, center.y - bounds.size.height * 0.34),
                    button: gpui::MouseButton::Left,
                    modifiers: gpui::Modifiers::default(),
                    click_count: 2,
                    first_mouse: false,
                }),
                cx,
            );
        } else {
            window.dispatch_keystroke(gpui::Keystroke::parse("enter").unwrap(), cx);
        }
        assert!(matches!(
            view.read(cx).converter.as_ref().unwrap().phase,
            super::Phase::Running { .. }
        ));
    }
    // A clean success closes the session (a toast reports it); only failed or
    // cancelled runs stay open in `Finished`.
    let finished = view.read(cx).converter.as_ref().map(|s| match &s.phase {
        super::Phase::Finished(report) => {
            assert!(report.errors().is_empty(), "{:?}", report.errors());
            assert_eq!(report.outputs().len(), 1);
            Some(report.outputs()[0].clone())
        }
        _ => None,
    });
    let output = match finished {
        Some(output) => output,
        None => newest_output(directory),
    };
    if step >= 50 {
        if let Some(output) = output {
            window
                .render_to_image()
                .unwrap()
                .save(directory.join("results-laptop.png"))
                .unwrap();
            let message = if std::env::var_os("ATLAS_CONVERSION_LAB_VIDEO").is_some() {
                let bytes = std::fs::read(&output).unwrap();
                assert_eq!(&bytes[4..8], b"ftyp");
                assert!(bytes.len() > 1000);
                "Native window rendered at 1280x800 and 900x560. MOV to MP4 completed through Windows; no external engine was selected.
"
            } else if std::env::var_os("ATLAS_CONVERSION_LAB_AUDIO").is_some() {
                assert!(std::fs::metadata(&output).unwrap().len() > 500);
                "Native window rendered at 1280x800 and 900x560. WAV to MP3 completed through Windows; no external engine was selected.
"
            } else {
                assert!(image::open(&output).is_ok());
                "Native window rendered at 1280x800 and 900x560. JPEG conversion decoded successfully.
"
            };
            std::fs::write(directory.join("passed.txt"), message).unwrap();
            cx.quit();
            return true;
        }
    }
    if step >= 600 {
        panic!("Conversion lab did not finish");
    }
    window.refresh();
    false
}

/// Actions side: M switches modes through the real key dispatcher, the info
/// column renders, and "Rotate right" runs end to end on the PNG fixture.
fn actions_frame(
    view: &Entity<App>,
    directory: &std::path::Path,
    step: usize,
    window: &mut Window,
    cx: &mut gpui::App,
) -> bool {
    use atlas_core::convert::actions::Action;
    if step == 12 {
        let session = view.read(cx).converter.as_ref().unwrap();
        assert!(matches!(session.phase, super::Phase::Choosing));
        assert_eq!(session.mode, super::Mode::Convert);
        assert!(session
            .actions
            .contains(&super::RingAction::Native(Action::Resize)));
        assert!(session
            .actions
            .contains(&super::RingAction::Native(Action::RotateRight)));
        assert!(
            !session
                .actions
                .contains(&super::RingAction::Native(Action::CompressPdf)),
            "PDF-only action must not be offered for a PNG"
        );
        window.dispatch_keystroke(gpui::Keystroke::parse("m").unwrap(), cx);
        assert_eq!(
            view.read(cx).converter.as_ref().unwrap().mode,
            super::Mode::Actions
        );
    }
    if step == 16 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("actions-desktop.png"))
            .unwrap();
        window.resize(gpui::size(gpui::px(900.), gpui::px(560.)));
    }
    if step == 25 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("actions-laptop.png"))
            .unwrap();
        let rotate = view
            .read(cx)
            .converter
            .as_ref()
            .unwrap()
            .actions
            .iter()
            .position(|a| *a == super::RingAction::Native(Action::RotateRight))
            .unwrap();
        view.update(cx, |app, cx| {
            app.converter.as_mut().unwrap().selected = rotate;
            cx.notify();
        });
    }
    if step == 30 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("actions-rotate.png"))
            .unwrap();
        window.dispatch_keystroke(gpui::Keystroke::parse("enter").unwrap(), cx);
    }
    let output = directory.join("Sample-rotated.png");
    if step > 30 && view.read(cx).converter.is_none() && output.is_file() {
        let rotated = image::open(&output).unwrap();
        assert_eq!((rotated.width(), rotated.height()), (100, 160));
        std::fs::write(
            directory.join("passed.txt"),
            "Actions ring: M switched modes, PNG rotated 160x100 -> 100x160 beside the source.
",
        )
        .unwrap();
        cx.quit();
        return true;
    }
    if let Some(super::Phase::Finished(report)) = view.read(cx).converter.as_ref().map(|s| &s.phase)
    {
        panic!("Action failed: {:?}", report.errors());
    }
    if step >= 400 {
        panic!("Actions lab did not finish");
    }
    window.refresh();
    false
}

/// Most recent conversion output in the lab directory, once the session closed.
fn newest_output(directory: &std::path::Path) -> Option<PathBuf> {
    std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ["jpg", "mp4", "mp3"].contains(&&*ext.to_string_lossy()))
        })
        .max_by_key(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok())
}

/// Visual state fixtures only. Real HTTPS/package/conversion behavior is tested
/// independently by ext::tool_package::tests::real_pandoc_package_download_and_conversion_when_enabled.
fn package_frame(
    view: &Entity<App>,
    directory: &std::path::Path,
    step: usize,
    window: &mut Window,
    cx: &mut gpui::App,
) -> bool {
    use atlas_core::{
        convert::{self, Tool},
        ext::{registry::ToolInstallRequest, tool_package::Progress},
    };
    use std::sync::{atomic::AtomicBool, Arc};
    let opus = std::env::var("ATLAS_CONVERSION_LAB_PACKAGE")
        .ok()
        .as_deref()
        == Some("opus");
    let tool = if opus { Tool::Opus } else { Tool::Pandoc };
    if step == 8 {
        view.update(cx, |app, cx| {
            let session = app.converter.as_mut().unwrap();
            assert!(matches!(session.phase, super::Phase::Choosing));
            session.tools.0.clear();
            session.catalog = convert::options(&session.sources, &session.tools);
            session.selected = session
                .catalog
                .iter()
                .position(|option| option.target.extension == if opus { "opus" } else { "docx" })
                .unwrap();
            session.disabled_providers.clear();
            let manifest = atlas_model::ext_model::Manifest::parse(if opus {
                crate::app::ext::CONVERSION_OPUS_MANIFEST
            } else {
                crate::app::ext::CONVERSION_PANDOC_MANIFEST
            })
            .unwrap();
            session.installers.insert(
                tool,
                ToolInstallRequest {
                    provider_dir: directory.join("fixture-provider"),
                    tool: manifest.tools[0].clone(),
                },
            );
            cx.notify();
        });
    }
    if step == 12 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("package-desktop.png"))
            .unwrap();
        window.resize(gpui::size(gpui::px(900.), gpui::px(560.)));
    }
    if step == 25 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("package-laptop.png"))
            .unwrap();
        view.update(cx, |app, cx| {
            let total = app.converter.as_ref().unwrap().installers[&tool]
                .tool
                .download_bytes;
            app.converter.as_mut().unwrap().package = Some(super::PackageJob {
                tool,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Progress::Download {
                    bytes: total / 4,
                    total: Some(total),
                },
            });
            cx.notify();
        });
    }
    if step == 38 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("download-laptop.png"))
            .unwrap();
        view.update(cx, |app, cx| {
            app.cancel_conversion_package();
            assert!(app
                .converter
                .as_ref()
                .unwrap()
                .package
                .as_ref()
                .unwrap()
                .cancel
                .load(std::sync::atomic::Ordering::Relaxed));
            cx.notify();
        });
    }
    if step == 44 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("cancelling-laptop.png"))
            .unwrap();
        view.update(cx, |app, cx| {
            let session = app.converter.as_mut().unwrap();
            session.package = None;
            session.package_error = Some(format!(
                "Connection interrupted. Select Install {} to retry.",
                tool.name()
            ));
            cx.notify();
        });
    }
    if step == 50 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("error-laptop.png"))
            .unwrap();
        view.update(cx, |app, cx| {
            let session = app.converter.as_mut().unwrap();
            session.package_error = None;
            session.disabled_providers.insert(
                tool,
                if opus {
                    "conversion-opus"
                } else {
                    "conversion-pandoc"
                }
                .into(),
            );
            cx.notify();
        });
    }
    if step == 62 {
        window
            .render_to_image()
            .unwrap()
            .save(directory.join("disabled-laptop.png"))
            .unwrap();
        view.update(cx, |app, cx| {
            let session = app.converter.as_mut().unwrap();
            session.disabled_providers.clear();
            // Validate stale discovery cannot take ownership of a running plan.
            session.phase = super::Phase::Running {
                cancel: Arc::new(AtomicBool::new(false)),
                done: 0,
                total: 1,
                current: session.sources[0].clone(),
            };
            session
                .tx
                .send(super::Event::Discovered {
                    revision: session.discovery,
                    tools: convert::Tools::default(),
                    catalog: Vec::new(),
                    action_tools: convert::Tools::default(),
                    actions: Vec::new(),
                    extra: Vec::new(),
                    locked: Vec::new(),
                })
                .unwrap();
            app.drain_converter();
            assert!(matches!(
                app.converter.as_ref().unwrap().phase,
                super::Phase::Running { .. }
            ));
            app.converter.as_mut().unwrap().phase = super::Phase::Choosing;
            cx.notify();
        });
    }
    if step == 74 {
        // Alt+I exercises the real GPUI key dispatcher; the isolated fixture path
        // fails before contacting the network, and the UI receives its error event.
        window.dispatch_keystroke(gpui::Keystroke::parse("alt-i").unwrap(), cx);
        assert!(view.read(cx).converter.as_ref().unwrap().package.is_some());
    }
    if step >= 90
        && view
            .read(cx)
            .converter
            .as_ref()
            .unwrap()
            .package_error
            .is_some()
    {
        std::fs::write(directory.join("passed.txt"), "Native package UI state fixtures rendered at 1280x800 and 900x560: install, download, retry, disabled provider. Alt+I dispatched a real install attempt against an isolated missing provider and received its error. Stale discovery preserved the running phase. Network installation/conversion is verified separately by the package integration test.\n").unwrap();
        cx.quit();
        return true;
    }
    assert!(step < 600, "Package lab did not finish");
    window.refresh();
    false
}
