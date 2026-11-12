//! Optional native preview/palette regression lab, excluded from normal builds.
//!
//! Run with `--features ui-lab`, `ATLAS_PREVIEW_LAB=<output directory>` and
//! `ATLAS_CONFIG_DIR=<isolated configuration directory>`. The real App renders
//! in a hidden native window; screenshots use the renderer's GPU readback.
use crate::app::{App, PreviewView};
use anyhow::{ensure, Context as _};
use atlas_model::preview_cache::PreviewResult;
use gpui::{point, px, size, Bounds, Entity, Pixels, Point, Window};
use std::path::PathBuf;

#[path = "preview_lab/settings_palette.rs"]
mod settings_palette;
#[path = "preview_lab/settings_scroll.rs"]
mod settings_scroll;

#[path = "preview_lab/zip_docx.rs"]
mod zip_docx;

#[path = "preview_lab/brand_flow.rs"]
mod brand_flow;
#[path = "preview_lab/compat.rs"]
mod compat;

const IMAGE_SIZE: (usize, usize) = (1536, 1024);
const VARIANTS: [(&str, &str, f32, f32); 4] = [
    ("desktop-dark", "Atlas Graphite", 1280., 800.),
    ("laptop-dark", "Atlas Graphite", 980., 760.),
    ("desktop-light", "Atlas Light", 1280., 800.),
    ("laptop-light", "Atlas Light", 980., 760.),
];

/// Called before settings/session loading so a lab cannot silently reuse the
/// user's configuration or restore their tabs.
pub(crate) fn prepare(initial_paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let Some(directory) = std::env::var_os("ATLAS_PREVIEW_LAB").map(PathBuf::from) else {
        return initial_paths;
    };
    assert!(
        std::env::var_os("ATLAS_CONFIG_DIR").is_some_and(|path| !path.is_empty()),
        "ATLAS_PREVIEW_LAB requires an isolated ATLAS_CONFIG_DIR"
    );
    let fixtures = directory.join("fixtures");
    // The format matrix is prepared externally with real Office/PSD/RAR files.
    // In particular, do not replace its ZIP with the generic preview fixture.
    if std::env::var_os("ATLAS_PREVIEW_COMPAT_LAB").is_some() {
        assert!(
            fixtures.join("document.docx").is_file(),
            "Run prepare-preview-compat.py first"
        );
        return vec![fixtures];
    }
    std::fs::create_dir_all(fixtures.join("Sprites")).unwrap();
    fixture_image()
        .save(fixtures.join("alpha-preview.png"))
        .unwrap();
    std::fs::write(
        fixtures.join("layout-notes.md"),
        "# Preview lab\n\nA small local search fixture.\n",
    )
    .unwrap();
    std::fs::write(
        fixtures.join("inventory.csv"),
        // Spanish Excel export: BOM, `;` separators and decimal commas.
        "\u{FEFF}sku;name;quantity;price;notes\n\
         A-100;Graphite pencil;240;0,85;\"Soft; 2B\"\n\
         A-101;Sketchbook A5;36;7,50;Dot grid\n\
         A-102;Ink pen 0.3;120;2,10;Archival\n\
         A-103;Watercolor set;12;24,00;24 pans\n",
    )
    .unwrap();
    std::fs::write(fixtures.join("cube.obj"), CUBE_OBJ).unwrap();
    std::fs::write(fixtures.join("sample.rs"), SAMPLE_RS).unwrap();
    if std::env::var_os("ATLAS_SETTINGS_SCROLL_LAB").is_some() {
        let source = (0..300)
            .map(|i| format!("// Preview scroll regression, line {i}\n"))
            .collect::<String>();
        std::fs::write(fixtures.join("sample.rs"), source).unwrap();
    }
    std::fs::write(
        fixtures.join("bundle.zip"),
        stored_zip(&[
            ("docs/readme.txt", b"Read me".as_slice()),
            ("docs/notes.md", b"# Notes".as_slice()),
            ("data.csv", b"a,b\n1,2\n".as_slice()),
            ("logo.svg", b"<svg/>".as_slice()),
        ]),
    )
    .unwrap();
    vec![fixtures]
}

const SAMPLE_RS: &str = "use std::collections::HashMap;

/// Count words.
fn main() {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for word in \"hola mundo hola\".split(' ') {
        *counts.entry(word.to_string()).or_default() += 1; // tally
    }
    println!(\"{counts:?}\");
}
";

const CUBE_OBJ: &str = "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1 1 0\nv 0 0 1\nv 1 0 1\nv 0 1 1\nv 1 1 1\n\
f 1 2 4 3\nf 5 7 8 6\nf 1 5 6 2\nf 3 4 8 7\nf 1 3 7 5\nf 2 6 8 4\n";

/// Minimal STORED zip writer for the archive fixture.
fn stored_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let header = |sig: u32, central: bool| {
            let mut h = sig.to_le_bytes().to_vec();
            if central {
                h.extend_from_slice(&20u16.to_le_bytes());
            }
            h.extend_from_slice(&20u16.to_le_bytes());
            h.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes());
            if central {
                h.extend_from_slice(&[0; 6]);
                h.extend_from_slice(&0u32.to_le_bytes());
                h.extend_from_slice(&offset.to_le_bytes());
            }
            h.extend_from_slice(name.as_bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend_from_slice(data);
        central.extend(header(0x0201_4b50, true));
    }
    let (start, size) = (out.len() as u32, central.len() as u32);
    out.extend(central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn select_named(view: &Entity<App>, name: &str, cx: &mut gpui::App) -> bool {
    view.update(cx, |app, cx| {
        let tab = app.state.focused_tile;
        let Some(index) = app
            .state
            .tab_states
            .get(&tab)
            .and_then(|state| state.entries.iter().position(|entry| entry.name == name))
        else {
            return false;
        };
        let state = app.state.tab_states.get_mut(&tab).unwrap();
        state.selection.selected = std::iter::once(index).collect();
        state.selection.keyboard_cursor = Some(index);
        app.notify_pane(tab, cx);
        cx.notify();
        true
    })
}

fn fixture_image() -> image::RgbaImage {
    let mut image = image::RgbaImage::new(IMAGE_SIZE.0 as u32, IMAGE_SIZE.1 as u32);
    let colors = [
        [240, 121, 158],
        [255, 179, 70],
        [153, 139, 240],
        [78, 193, 173],
        [107, 171, 238],
    ];
    // Transparency surrounds five crisp, deliberately distinct pixel-art faces.
    // The partly transparent shadow also exercises alpha blending on both themes.
    for (index, color) in colors.into_iter().enumerate() {
        let left = 110 + index as u32 * 270;
        for y in 366..666 {
            for x in left..left + 230 {
                let corner = !(left + 20..left + 210).contains(&x) && !(386..646).contains(&y);
                if corner {
                    continue;
                }
                let eye = (427..462).contains(&y)
                    && ((left + 52..left + 74).contains(&x)
                        || (left + 153..left + 175).contains(&x));
                let mouth = (540..556).contains(&y) && (left + 86..left + 145).contains(&x);
                let edge = x < left + 8 || x >= left + 222 || y < 374 || y >= 658;
                let rgba = if eye || mouth || edge {
                    [38, 42, 53, 255]
                } else {
                    [color[0], color[1], color[2], 255]
                };
                image.put_pixel(x, y, image::Rgba(rgba));
            }
        }
        for y in 680..698 {
            for x in left + 12..left + 218 {
                image.put_pixel(x, y, image::Rgba([30, 35, 45, 70]));
            }
        }
    }
    image
}

pub(crate) fn install(view: &Entity<App>, window: &mut Window, cx: &mut gpui::App) {
    let Some(directory) = std::env::var_os("ATLAS_PREVIEW_LAB").map(PathBuf::from) else {
        return;
    };
    if std::env::var_os("ATLAS_PREVIEW_COMPAT_LAB").is_some() {
        compat::install(view, window, cx, directory);
        return;
    }
    if std::env::var_os("ATLAS_MILLER_CONVERT_LAB").is_some() {
        crate::ui::converter::crash_lab::install(view, window, cx, directory);
        return;
    }
    if std::env::var_os("ATLAS_BRAND_FLOW_LAB").is_some() {
        brand_flow::install(view, window, cx, directory);
        return;
    }
    if std::env::var_os("ATLAS_SETTINGS_PALETTE_LAB").is_some() {
        settings_palette::install(view, window, cx, directory);
        return;
    }
    if std::env::var_os("ATLAS_SETTINGS_SCROLL_LAB").is_some() {
        settings_scroll::install(view, window, cx, directory);
        return;
    }
    if std::env::var_os("ATLAS_ZIP_DOCX_LAB").is_some() {
        zip_docx::install(view, window, cx, directory);
        return;
    }
    rust_i18n::set_locale("en");
    window.set_a11y_forced(true);
    view.update(cx, |app, cx| {
        app.state.overlay.show_getting_started = false;
        app.state.settings.disable_animations = true;
        app.state.settings.show_inspector = true;
        app.state.settings.inspector_width = 360.;
        cx.notify();
    });
    let handle = window.window_handle();
    let view = view.clone();
    let mut lab = Lab {
        directory,
        variant: 0,
        stage: 0,
        ticks: 0,
        captures: Vec::new(),
        checks: Vec::new(),
        origin_pane: None,
        split_pane: None,
        deep_zoomed: false,
        stack_tested: false,
    };
    cx.spawn(async move |cx| {
        for _ in 0..2400 {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(20))
                .await;
            let done = handle.update(cx, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
                match lab.frame(&view, window, cx) {
                    Ok(done) => done,
                    Err(error) => {
                        let variant = VARIANTS
                            .get(lab.variant)
                            .map_or("completed", |variant| variant.0);
                        let message = format!("{variant} stage {}: {error:#}\n", lab.stage);
                        eprintln!("[preview-lab] {message}");
                        let _ = std::fs::write(lab.directory.join("failed.txt"), message);
                        let _ = window.render_to_image().and_then(|image| {
                            image
                                .save(lab.directory.join("failed.png"))
                                .map_err(Into::into)
                        });
                        let _ = lab.report();
                        cx.quit();
                        true
                    }
                }
            });
            match done {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    let _ = std::fs::write(
                        lab.directory.join("failed.txt"),
                        format!("Window update failed: {error:#}\n"),
                    );
                    return;
                }
            }
        }
        let _ = std::fs::write(lab.directory.join("failed.txt"), "Preview lab timed out\n");
        let _ = cx.update(|cx| cx.quit());
    })
    .detach();
}

struct Lab {
    directory: PathBuf,
    variant: usize,
    stage: usize,
    ticks: usize,
    captures: Vec<String>,
    checks: Vec<String>,
    origin_pane: Option<atlas_model::layout_tree::TileId>,
    split_pane: Option<atlas_model::layout_tree::TileId>,
    deep_zoomed: bool,
    stack_tested: bool,
}

impl Lab {
    fn frame(
        &mut self,
        view: &Entity<App>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> anyhow::Result<bool> {
        self.ticks += 1;
        if self.ticks < 12 {
            return Ok(false);
        }
        match self.stage {
            0 => {
                let (_, theme, width, height) = VARIANTS[self.variant];
                rust_i18n::set_locale(if width < 1100. { "es" } else { "en" });
                window.resize(size(px(width), px(height)));
                let ready = view.update(cx, |app, cx| {
                    let tab = app.state.focused_tile;
                    let entry = app.state.tab_states.get(&tab).and_then(|state| {
                        state
                            .entries
                            .iter()
                            .enumerate()
                            .find(|(_, entry)| entry.name == "alpha-preview.png")
                            .map(|(index, entry)| (index, entry.path()))
                    });
                    let Some((index, path)) = entry else {
                        return false;
                    };
                    let state = app.state.tab_states.get_mut(&tab).unwrap();
                    state.selection.selected.clear();
                    state.selection.selected.insert(index);
                    state.selection.keyboard_cursor = Some(index);
                    app.state.settings.show_inspector = true;
                    app.preview_view.insert(tab, PreviewView::default());
                    app.state.profiles.active_mut().theme = theme.to_owned();
                    app.theme_dirty = true;
                    app.state.tag_cache.defs = vec![
                        atlas_model::tag_cache::TagDef {
                            id: atlas_model::tag_cache::TagId(-1),
                            name: "Artwork".into(),
                            color: [157, 125, 232],
                            profile: None,
                        },
                        atlas_model::tag_cache::TagDef {
                            id: atlas_model::tag_cache::TagId(-2),
                            name: "Review".into(),
                            color: [236, 171, 72],
                            profile: None,
                        },
                    ];
                    app.state
                        .tag_cache
                        .assign_local(atlas_model::tag_cache::TagId(-1), &[path.clone()]);
                    app.load_preview_gpui(tab, path);
                    app.notify_pane(tab, cx);
                    app.focus_handle.focus(window, cx);
                    cx.notify();
                    true
                });
                if !ready {
                    ensure!(self.ticks < 250, "Local fixture directory did not load");
                    return Ok(false);
                }
            }
            1 => {
                let loaded = matches!(
                    view.read(cx)
                        .state
                        .preview
                        .results
                        .get(&view.read(cx).state.focused_tile),
                    Some(PreviewResult::Image { .. })
                );
                if !loaded {
                    ensure!(self.ticks < 250, "Real image preview did not decode");
                    return Ok(false);
                }
                let geometry = geometry(view, cx)?;
                ensure!(
                    geometry.minimap.is_none(),
                    "Fit preview unexpectedly overflows"
                );
                ensure!(
                    !current_view(view, cx).details_open,
                    "Metadata must start collapsed"
                );
                control_bounds("preview-open-external", window)?;
                control_bounds("preview-expand", window)?;
                control_bounds("preview-close", window)?;
                self.capture("preview-fit", window)?;
                click_control("preview-details", window, cx)?;
                ensure!(
                    current_view(view, cx).details_open,
                    "Metadata click did not open details"
                );
                self.checked("Metadata is collapsed initially and opens through its button");
            }
            2 => {
                self.capture("preview-details", window)?;
                click_control("preview-details", window, cx)?;
                ensure!(
                    !current_view(view, cx).details_open,
                    "Metadata did not collapse"
                );
                click_control("preview-add-tag", window, cx)?;
                ensure!(
                    view.read(cx).tag_picker_menu.is_some(),
                    "Add tag did not open its picker"
                );
                self.checked("Add tag opens the real picker");
            }
            3 => {
                self.capture("preview-tags", window)?;
                key("escape", window, cx)?;
                ensure!(
                    view.read(cx).tag_picker_menu.is_none(),
                    "Escape did not dismiss tag picker"
                );
                click_control("preview-remove-tag--1", window, cx)?;
                ensure!(
                    view.read(cx).selected_paths().iter().all(|path| {
                        !view
                            .read(cx)
                            .state
                            .tag_cache
                            .tags_of(path)
                            .contains(&atlas_model::tag_cache::TagId(-1))
                    }),
                    "Tag chip did not remove its assignment"
                );
                click_control("preview-zoom-actual", window, cx)?;
                ensure!(
                    (geometry(view, cx)?.scale - 1.).abs() < 0.01,
                    "1:1 did not select native pixel scale"
                );
                self.checked("Escape dismisses tag picker; 1:1 selects native pixel scale");
            }
            4 => {
                self.capture("preview-actual", window)?;
                let before = current_view(view, cx).zoom;
                click_control("preview-zoom-in", window, cx)?;
                ensure!(
                    current_view(view, cx).zoom > before,
                    "Zoom in did not increase scale"
                );
                self.checked("Zoom in increases scale");
            }
            5 => {
                self.capture("preview-zoom-minimap", window)?;
                let map = geometry(view, cx)?
                    .minimap
                    .context("Zoomed image has no minimap")?;
                let before = current_view(view, cx);
                let target = point(
                    map.image.left() + map.image.size.width * 0.72,
                    map.image.top() + map.image.size.height * 0.68,
                );
                mouse_down(map.image.center(), window, cx);
                ensure!(
                    current_view(view, cx).minimap_drag,
                    "Minimap did not start its drag gesture"
                );
                window.dispatch_event(
                    gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                        position: target,
                        pressed_button: Some(gpui::MouseButton::Left),
                        modifiers: gpui::Modifiers::default(),
                    }),
                    cx,
                );
                mouse_up(target, window, cx);
                let after = current_view(view, cx);
                ensure!(
                    after.pan_x != before.pan_x || after.pan_y != before.pan_y,
                    "Minimap click did not pan the image"
                );
                ensure!(
                    !after.minimap_drag,
                    "Minimap drag remained active after release"
                );
                self.checked("Minimap drag changes image pan and releases its drag state");
            }
            6 => {
                self.capture("preview-minimap-panned", window)?;
                let stage = geometry(view, cx)?.viewport;
                let before = current_view(view, cx);
                let start = stage.center();
                let middle = point(start.x + px(20.), start.y + px(14.));
                let end = point(start.x + px(42.), start.y + px(28.));
                mouse_down(start, window, cx);
                window.dispatch_event(
                    gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                        position: middle,
                        pressed_button: Some(gpui::MouseButton::Left),
                        modifiers: gpui::Modifiers::default(),
                    }),
                    cx,
                );
                window.dispatch_event(
                    gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                        position: end,
                        pressed_button: Some(gpui::MouseButton::Left),
                        modifiers: gpui::Modifiers::default(),
                    }),
                    cx,
                );
                mouse_up(end, window, cx);
                let after = current_view(view, cx);
                ensure!(
                    (after.pan_x - before.pan_x - 42.).abs() < 0.01
                        && (after.pan_y - before.pan_y - 28.).abs() < 0.01,
                    "Image drag lost movement between frames: before ({}, {}), after ({}, {})",
                    before.pan_x,
                    before.pan_y,
                    after.pan_x,
                    after.pan_y
                );
                ensure!(
                    view.read(cx).preview_drag.is_none(),
                    "Image drag remained active after release"
                );
                self.checked("Dragging the image pans it and releases its drag state");
            }
            7 => {
                if !self.deep_zoomed {
                    self.deep_zoomed = true;
                    view.update(cx, |app, cx| {
                        let tab = app.state.focused_tile;
                        let state = app.preview_view.entry(tab).or_default();
                        state.zoom = 40.;
                        state.pan_x = 0.;
                        state.pan_y = 0.;
                        cx.notify();
                    });
                    self.ticks = 0;
                    return Ok(false);
                }
                let deep = geometry(view, cx)?;
                let map = deep.minimap.context("Deep zoom lost the minimap")?;
                ensure!(
                    map.image.contains(&map.visible.origin)
                        && map.visible.size.width >= px(8.)
                        && map.visible.size.height >= px(8.),
                    "Deep-zoom marker left the navigator or collapsed: {:?}",
                    map.visible
                );
                self.capture("preview-zoom-deep", window)?;
                self.checked("Deep zoom keeps a visible, grabbable navigator marker");
                let before = current_view(view, cx).zoom;
                click_control("preview-zoom-out", window, cx)?;
                ensure!(
                    current_view(view, cx).zoom < before,
                    "Zoom out did not reduce scale"
                );
                click_control("preview-zoom-fit", window, cx)?;
                let state = current_view(view, cx);
                ensure!(
                    state.zoom == 1. && state.pan_x == 0. && state.pan_y == 0.,
                    "Fit did not reset zoom and pan"
                );
                self.checked("Zoom out reduces scale; Fit resets zoom and pan");
            }
            8 => {
                self.capture("preview-fit-reset", window)?;
                click_control("preview-expand", window, cx)?;
                ensure!(
                    current_view(view, cx).expanded,
                    "Expand did not open the large preview"
                );
            }
            9 => {
                ensure!(
                    geometry(view, cx)?.viewport.size.width > window.viewport_size().width * 0.75,
                    "Expanded preview did not use the available window width"
                );
                self.capture("preview-expanded", window)?;
                click_control("preview-expand", window, cx)?;
                ensure!(
                    !current_view(view, cx).expanded,
                    "Expand button did not return to the docked preview"
                );
                window.refresh();
                window.draw(cx).clear(cx);
                click_control("preview-expand", window, cx)?;
                ensure!(
                    current_view(view, cx).expanded,
                    "Expand button did not reopen the large preview"
                );
                window.refresh();
                window.draw(cx).clear(cx);
                key("escape", window, cx)?;
                ensure!(
                    !current_view(view, cx).expanded,
                    "Escape did not collapse expanded preview"
                );
                let focus = view.read(cx).focus_handle.clone();
                focus.focus(window, cx);
                key("ctrl-shift-p", window, cx)?;
                ensure!(
                    view.read(cx).state.overlay.show_palette,
                    "Command palette shortcut did not open it"
                );
                self.checked("Expand button opens and returns; Escape collapses; command palette shortcut works");
            }
            10 => {
                self.capture("commands", window)?;
                let before = view.read(cx).state.overlay.palette_state.selected_idx;
                key("down", window, cx)?;
                ensure!(
                    view.read(cx).state.overlay.palette_state.selected_idx != before,
                    "Down did not navigate commands"
                );
                key("up", window, cx)?;
                key("tab", window, cx)?;
                type_text("preview", window, cx)?;
                self.checked(
                    "Command palette supports arrow navigation, Tab and native text input",
                );
            }
            11 => {
                ensure!(
                    view.read(cx).state.overlay.palette_state.query == "preview",
                    "Command query was not synchronized from native input"
                );
                self.capture("commands-search", window)?;
                key("ctrl-a", window, cx)?;
                type_text("zzzzunfindablezzzz", window, cx)?;
            }
            12 => {
                let count = view.update(cx, |app, cx| {
                    app.command_palette_spec(cx)
                        .sections
                        .iter()
                        .map(|section| section.items.len())
                        .sum::<usize>()
                });
                ensure!(
                    count == 0,
                    "No-results command query still has {count} rows"
                );
                self.capture("commands-empty", window)?;
                key("escape", window, cx)?;
                ensure!(
                    !view.read(cx).state.overlay.show_palette,
                    "Escape did not close commands"
                );
                let focus = view.read(cx).focus_handle.clone();
                focus.focus(window, cx);
                key("ctrl-k", window, cx)?;
                ensure!(
                    view.read(cx).state.overlay.show_goto_bar,
                    "File search shortcut did not open it"
                );
                self.checked(
                    "Unmatched commands show no results; Escape dismisses; Ctrl+K opens search",
                );
            }
            13 => {
                self.capture("file-search", window)?;
                // An absolute prefix confines search to our fixture directory,
                // keeping this run independent of the user's system-wide index.
                type_text(&self.scoped_query("alpha")?, window, cx)?;
            }
            14 => {
                let count = view.update(cx, |app, cx| {
                    app.goto_spec(cx)
                        .sections
                        .iter()
                        .filter(|section| section.id != "create")
                        .map(|section| section.items.len())
                        .sum::<usize>()
                });
                if count == 0 {
                    ensure!(
                        self.ticks < 200,
                        "Scoped fuzzy search did not find the image fixture"
                    );
                    return Ok(false);
                }
                self.capture("file-search-results", window)?;
                key("ctrl-a", window, cx)?;
                type_text(&self.scoped_query("zzzzunfindablezzzz")?, window, cx)?;
            }
            15 => {
                let count = view.update(cx, |app, cx| {
                    app.goto_spec(cx)
                        .sections
                        .iter()
                        .filter(|section| section.id != "create")
                        .map(|section| section.items.len())
                        .sum::<usize>()
                });
                ensure!(count == 0, "Unmatched file query still has {count} rows");
                self.capture("file-search-empty", window)?;
                key("escape", window, cx)?;
                ensure!(
                    view.read(cx)
                        .goto_input
                        .as_ref()
                        .is_some_and(|input| input.read(cx).value().is_empty()),
                    "First Escape did not clear the scoped query"
                );
                key("escape", window, cx)?;
                ensure!(
                    !view.read(cx).state.overlay.show_goto_bar,
                    "Escape did not close file search"
                );
                self.checked(
                    "Scoped fuzzy search finds fixture; no matching files; Escape clears then closes",
                );
            }
            16 => {
                let selected = view.update(cx, |app, cx| {
                    let tab = app.state.focused_tile;
                    let Some((index, path)) = app.state.tab_states.get(&tab).and_then(|state| {
                        state
                            .entries
                            .iter()
                            .enumerate()
                            .find(|(_, entry)| entry.name == "inventory.csv")
                            .map(|(index, entry)| (index, entry.path()))
                    }) else {
                        return false;
                    };
                    let state = app.state.tab_states.get_mut(&tab).unwrap();
                    state.selection.selected.clear();
                    state.selection.selected.insert(index);
                    state.selection.keyboard_cursor = Some(index);
                    app.load_preview_gpui(tab, path);
                    app.notify_pane(tab, cx);
                    cx.notify();
                    true
                });
                ensure!(selected, "CSV fixture is missing from the pane");
            }
            17 => {
                let table = match view
                    .read(cx)
                    .state
                    .preview
                    .results
                    .get(&view.read(cx).state.focused_tile)
                {
                    Some(PreviewResult::Table { headers, rows, .. }) => {
                        Some((headers.len(), rows.len()))
                    }
                    Some(PreviewResult::Error(error)) => {
                        anyhow::bail!("CSV preview failed: {error}")
                    }
                    _ => None,
                };
                let Some((columns, rows)) = table else {
                    ensure!(self.ticks < 250, "CSV preview did not produce a table");
                    return Ok(false);
                };
                ensure!(
                    columns == 5 && rows == 4,
                    "CSV table shape is {rows}x{columns}"
                );
                self.capture("preview-csv", window)?;
                self.checked("CSV renders as a 4x5 table");
                view.update(cx, |app, cx| {
                    let tab = app.state.focused_tile;
                    let state = app.state.tab_states.get_mut(&tab).unwrap();
                    state.selection.selected = (0..state.entries.len()).collect();
                    app.notify_pane(tab, cx);
                    cx.notify();
                });
            }
            18 => {
                let count = view.read(cx).selected_paths().len();
                ensure!(count >= 4, "Multi-selection has {count} items");
                if self.ticks < 40 {
                    return Ok(false);
                }
                if !self.stack_tested {
                    self.capture("preview-multi", window)?;
                    self.checked("Multi-selection shows stack, kind chips and item list");
                    let start = current_view(view, cx).stack_index;
                    click_control("preview-stack-next", window, cx)?;
                    let clicked = current_view(view, cx).stack_index;
                    ensure!(
                        clicked != start,
                        "Clicking the next card did not advance the stack"
                    );
                    let stack = control_bounds("preview-stack", window)?;
                    let from = stack.center();
                    let to = point(from.x - px(90.), from.y);
                    mouse_down(from, window, cx);
                    window.dispatch_event(
                        gpui::PlatformInput::MouseMove(gpui::MouseMoveEvent {
                            position: to,
                            pressed_button: Some(gpui::MouseButton::Left),
                            modifiers: gpui::Modifiers::default(),
                        }),
                        cx,
                    );
                    ensure!(
                        current_view(view, cx).stack_dx < -40.,
                        "Swiping did not move the front photo"
                    );
                    mouse_up(to, window, cx);
                    let swiped = current_view(view, cx);
                    ensure!(
                        swiped.stack_index != clicked && swiped.stack_drag.is_none(),
                        "Swipe did not advance the stack or release its drag"
                    );
                    self.checked(
                        "Stack advances by clicking a peeking card and by swiping the photo",
                    );
                    self.stack_tested = true;
                    self.ticks = 0;
                    return Ok(false);
                }
                self.capture("preview-multi-swiped", window)?;
                self.stack_tested = false;
                let origin = view.read(cx).state.focused_tile;
                self.origin_pane = Some(origin);
                view.update(cx, |app, cx| {
                    app.process_actions(vec![crate::input::action::Action::SplitHorizontalFocused]);
                    cx.notify();
                });
            }
            19 => {
                let origin = self.origin_pane.context("origin pane missing")?;
                let (focused, loaded) = {
                    let app = view.read(cx);
                    let focused = app.state.focused_tile;
                    let loaded = app
                        .state
                        .tab_states
                        .get(&focused)
                        .is_some_and(|state| !state.entries.is_empty());
                    (focused, loaded)
                };
                if focused == origin || !loaded || self.ticks < 30 {
                    ensure!(self.ticks < 250, "Split did not focus a loaded new pane");
                    return Ok(false);
                }
                ensure!(
                    view.read(cx).preview_visible_for_focused(),
                    "Preview closed when focus moved to the new pane"
                );
                self.capture("two-panes-new-focus", window)?;
                self.checked("Preview stays open when focus moves to a split pane");
                self.split_pane = Some(focused);
                view.update(cx, |app, cx| {
                    app.activate_tab(origin);
                    cx.notify();
                });
            }
            20 => {
                if self.ticks < 30 {
                    return Ok(false);
                }
                let count = view.read(cx).selected_paths().len();
                ensure!(count >= 4, "Refocused pane preview shows {count} items");
                self.capture("two-panes-refocus", window)?;
                self.checked("Refocusing a pane previews its own selection");
                if let Some(split) = self.split_pane.take() {
                    view.update(cx, |app, cx| {
                        app.close_tab(split);
                        app.activate_tab(self.origin_pane.unwrap_or(split));
                        cx.notify();
                    });
                }
            }
            21 => {
                if self.ticks < 20 {
                    return Ok(false);
                }
                ensure!(
                    select_named(view, "cube.obj", cx),
                    "cube.obj fixture missing"
                );
            }
            22 => {
                let ready = matches!(
                    view.read(cx)
                        .state
                        .preview
                        .results
                        .get(&view.read(cx).state.focused_tile),
                    Some(PreviewResult::Image { .. })
                );
                if !ready {
                    ensure!(self.ticks < 250, "3D model did not render");
                    return Ok(false);
                }
                self.capture("preview-model", window)?;
                self.checked("OBJ model renders through the software rasterizer");
                ensure!(
                    select_named(view, "bundle.zip", cx),
                    "bundle.zip fixture missing"
                );
            }
            23 => {
                let count = match view
                    .read(cx)
                    .state
                    .preview
                    .results
                    .get(&view.read(cx).state.focused_tile)
                {
                    Some(PreviewResult::Dir { total_count, .. }) => Some(*total_count),
                    Some(PreviewResult::Error(error)) => {
                        anyhow::bail!("Archive preview failed: {error}")
                    }
                    _ => None,
                };
                let Some(count) = count else {
                    ensure!(self.ticks < 250, "Archive listing did not load");
                    return Ok(false);
                };
                ensure!(count >= 5, "Archive lists {count} items");
                self.capture("preview-archive", window)?;
                self.checked("Zip archive previews its listing and totals");
                view.update(cx, |app, cx| {
                    let tab = app.state.focused_tile;
                    if let Some(state) = app.state.tab_states.get_mut(&tab) {
                        state.selection.selected.clear();
                        state.selection.keyboard_cursor = None;
                    }
                    app.notify_pane(tab, cx);
                    cx.notify();
                });
            }
            24 => {
                let folder = {
                    let app = view.read(cx);
                    let tab = app.state.focused_tile;
                    let fixtures = app
                        .tab_data_for(tab)
                        .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()));
                    matches!(
                        app.state.preview.results.get(&tab),
                        Some(PreviewResult::Dir { .. })
                    ) && app.state.preview.paths.get(&tab) == fixtures.as_ref()
                };
                if !folder {
                    ensure!(
                        self.ticks < 250,
                        "No-selection state did not preview the folder"
                    );
                    return Ok(false);
                }
                self.capture("preview-folder", window)?;
                self.checked("No selection previews the browsed folder");
                let handle = control_bounds("inspector-resize-handle", window)?;
                let grab = point(handle.left() + px(3.), handle.center().y);
                mouse_down(grab, window, cx);
                ensure!(
                    view.read(cx).inspector_resize.is_some(),
                    "Resize grab zone outside the panel edge did not start a resize"
                );
                mouse_up(grab, window, cx);
                view.update(cx, |app, _| app.inspector_resize = None);
                self.checked("Inspector resize handle is grabbable left of the panel edge");
            }
            25 => {
                if self.ticks == 12 {
                    view.update(cx, |app, cx| {
                        let tab = app.state.focused_tile;
                        let Some((index, path)) =
                            app.state.tab_states.get(&tab).and_then(|state| {
                                state
                                    .entries
                                    .iter()
                                    .enumerate()
                                    .find(|(_, entry)| entry.name == "sample.rs")
                                    .map(|(index, entry)| (index, entry.path()))
                            })
                        else {
                            return;
                        };
                        let state = app.state.tab_states.get_mut(&tab).unwrap();
                        state.selection.selected.clear();
                        state.selection.selected.insert(index);
                        state.selection.keyboard_cursor = Some(index);
                        app.load_preview_gpui(tab, path);
                        app.notify_pane(tab, cx);
                        cx.notify();
                    });
                    return Ok(false);
                }
                let ready = {
                    let app = view.read(cx);
                    matches!(
                        app.state.preview.results.get(&app.state.focused_tile),
                        Some(PreviewResult::Text { .. })
                    )
                };
                if !ready || self.ticks < 60 {
                    ensure!(self.ticks < 250, "Code preview did not load");
                    return Ok(false);
                }
                self.capture("preview-code", window)?;
                self.checked("Source file renders in the code preview");
                let kept = view.update(cx, |app, _| {
                    let tab = app.state.focused_tile;
                    let path = app.state.tab_states.get(&tab).and_then(|state| {
                        state
                            .entries
                            .iter()
                            .find(|e| e.name == "layout-notes.md")
                            .map(|e| e.path())
                    });
                    let Some(path) = path else { return false };
                    app.load_preview_gpui(tab, path);
                    app.state.preview.results.contains_key(&tab)
                        && app.preview_is_stale_for_lab(tab)
                });
                ensure!(kept, "Switching files dropped the previous preview (flash)");
                self.checked("Switching files keeps the previous preview until the next one lands");
            }
            26 => {
                click_control("preview-close", window, cx)?;
                ensure!(
                    !view.read(cx).preview_visible_for_focused(),
                    "Preview close button did not hide the panel"
                );
                self.checked("Preview close button hides the panel");
                self.variant += 1;
                self.deep_zoomed = false;
                if self.variant == VARIANTS.len() {
                    self.report()?;
                    std::fs::write(self.directory.join("passed.txt"), format!(
                        "PASS: {} native GPU screenshots and {} interaction checks.\n1280x800 and 980x760, dark and light. Real image decoding, metadata, tag picker, 1:1/zoom/Fit, minimap, image panning, expanded preview, command navigation/search and scoped file search.\n", self.captures.len(), self.checks.len()))?;
                    cx.quit();
                    return Ok(true);
                }
                self.stage = 0;
                self.ticks = 0;
                return Ok(false);
            }
            _ => unreachable!(),
        }
        self.stage += 1;
        self.ticks = 0;
        Ok(false)
    }

    fn scoped_query(&self, query: &str) -> anyhow::Result<String> {
        let path = std::fs::canonicalize(self.directory.join("fixtures"))?;
        let path = path.to_string_lossy();
        let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
        Ok(format!("{path}{}{query}", std::path::MAIN_SEPARATOR))
    }

    fn checked(&mut self, message: &str) {
        self.checks
            .push(format!("{}: {message}", VARIANTS[self.variant].0));
    }

    fn capture(&mut self, name: &str, window: &mut Window) -> anyhow::Result<()> {
        // Keep a compact review set while executing every gesture and assertion
        // at both window sizes and in both themes: 24 screenshots in total.
        let common = matches!(
            name,
            "preview-fit"
                | "preview-zoom-minimap"
                | "preview-csv"
                | "preview-code"
                | "preview-multi"
                | "two-panes-new-focus"
                | "two-panes-refocus"
                | "preview-model"
                | "preview-zoom-deep"
                | "preview-multi-swiped"
                | "preview-tags"
                | "preview-archive"
                | "preview-folder"
                | "commands"
        );
        let desktop = VARIANTS[self.variant].0.starts_with("desktop")
            && matches!(
                name,
                "preview-details"
                    | "preview-tags"
                    | "preview-expanded"
                    | "commands-empty"
                    | "file-search-results"
                    | "file-search-empty"
            );
        if !common && !desktop {
            return Ok(());
        }
        let name = format!("{}-{name}", VARIANTS[self.variant].0);
        window
            .render_to_image()?
            .save(self.directory.join(format!("{name}.png")))?;
        if let Some(tree) = window.debug_a11y_tree_json() {
            std::fs::write(self.directory.join(format!("{name}.a11y.json")), tree)?;
        }
        self.captures.push(format!("{name}.png"));
        self.report()
    }

    fn report(&self) -> anyhow::Result<()> {
        let report = serde_json::json!({
            "renderer": "native GPUI GPU readback", "fixture": "1536x1024 RGBA with transparency",
            "captures": self.captures, "checks": self.checks,
        });
        std::fs::write(
            self.directory.join("report.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        Ok(())
    }
}

fn current_view(view: &Entity<App>, cx: &gpui::App) -> PreviewView {
    let app = view.read(cx);
    app.preview_view
        .get(&app.state.focused_tile)
        .copied()
        .unwrap_or_default()
}

fn geometry(
    view: &Entity<App>,
    cx: &gpui::App,
) -> anyhow::Result<super::preview_image::ImageGeometry> {
    let state = current_view(view, cx);
    let viewport = state
        .viewport
        .context("Image canvas did not publish its viewport")?;
    Ok(super::preview_image::ImageGeometry::new(
        viewport, IMAGE_SIZE, state,
    ))
}

fn control_bounds(id: &str, window: &Window) -> anyhow::Result<Bounds<Pixels>> {
    let tree = window
        .a11y_tree()
        .context("No accessibility tree was built")?;
    let (node, _) = tree
        .nodes
        .iter()
        .find(|(_, node)| node.author_id() == Some(id))
        .with_context(|| format!("Control {id} is absent from the accessibility tree"))?;
    let bounds = window
        .a11y_node_bounds(*node)
        .with_context(|| format!("Control {id} has no bounds"))?;
    ensure!(
        bounds.size.width > px(0.) && bounds.size.height > px(0.),
        "Control {id} has empty bounds"
    );
    let viewport = window.viewport_size();
    ensure!(
        bounds.left() >= px(0.)
            && bounds.top() >= px(0.)
            && bounds.right() <= viewport.width
            && bounds.bottom() <= viewport.height,
        "Control {id} is outside the window viewport: {bounds:?}"
    );
    Ok(bounds)
}

fn click_control(id: &str, window: &mut Window, cx: &mut gpui::App) -> anyhow::Result<()> {
    // A preceding Escape can remove an overlay before its old hitboxes are
    // retired. Present that state before dispatching the next pointer action.
    window.refresh();
    window.draw(cx).clear(cx);
    let position = control_bounds(id, window)?.center();
    mouse_down(position, window, cx);
    mouse_up(position, window, cx);
    Ok(())
}

fn mouse_down(position: Point<Pixels>, window: &mut Window, cx: &mut gpui::App) {
    window.simulate_mouse_move(position, cx);
    window.dispatch_event(
        gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position,
            button: gpui::MouseButton::Left,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }),
        cx,
    );
}

fn mouse_up(position: Point<Pixels>, window: &mut Window, cx: &mut gpui::App) {
    window.dispatch_event(
        gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position,
            button: gpui::MouseButton::Left,
            modifiers: gpui::Modifiers::default(),
            click_count: 1,
        }),
        cx,
    );
}

fn key(key: &str, window: &mut Window, cx: &mut gpui::App) -> anyhow::Result<()> {
    window.dispatch_keystroke(gpui::Keystroke::parse(key)?, cx);
    Ok(())
}

fn type_text(text: &str, window: &mut Window, cx: &mut gpui::App) -> anyhow::Result<()> {
    for character in text.chars() {
        let keystroke = gpui::Keystroke {
            key: character.to_string(),
            key_char: Some(character.to_string()),
            modifiers: gpui::Modifiers::default(),
        };
        ensure!(
            window.dispatch_keystroke(keystroke, cx),
            "Native text input did not accept {character:?}"
        );
    }
    Ok(())
}
