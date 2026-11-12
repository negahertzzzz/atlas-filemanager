//! Shared file drag-and-drop wiring used by every view's rows/cells.
//!
//! The `on_drag` source closure (drag the full selection, route through the
//! Windows OLE path) was copy-pasted into all four view renderers. It lives here
//! once; each view calls [`file_drag_handler`].

use std::path::{Path, PathBuf};

use gpui::prelude::*;
use gpui::{
    point, px, AnyDrag, CursorStyle, Entity, ExternalDragPayload, FileDragPaths,
    InteractiveElement, Window,
};

use crate::app::App;
#[cfg(windows)]
use crate::ui::root::DragNoop;
#[cfg(not(windows))]
use crate::ui::root::DraggedFiles;
use atlas_model::pane::TabId;

/// Active outgoing drag session state used to resolve external (OS) payloads
/// when an Atlas drag leaves the window.
#[derive(Clone, Debug, Default)]
pub(crate) struct ActiveExternalDrag {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) archive_items: Option<Vec<(PathBuf, PathBuf)>>,
}

/// The view entity gpui renders as the drag ghost. On Windows the OLE path owns
/// the cursor, so we hand gpui an invisible [`DragNoop`]; elsewhere gpui paints
/// the [`DraggedFiles`] ghost itself.
#[cfg(windows)]
pub(crate) type DragView = DragNoop;
#[cfg(not(windows))]
pub(crate) type DragView = DraggedFiles;

/// The drag *payload* gpui tracks for its native DnD. It must be [`DragView`] so
/// that on Windows it's [`DragNoop`] — a type no `on_drop::<DraggedFiles>` handler
/// matches. Otherwise gpui's synthetic mouse-up (injected after `DoDragDrop`)
/// would fire a second, native drop and move the file even when OLE was cancelled.
#[cfg(windows)]
pub(crate) fn file_drag_value(_paths: Vec<PathBuf>) -> DragView {
    DragNoop
}
#[cfg(not(windows))]
pub(crate) fn file_drag_value(paths: Vec<PathBuf>) -> DragView {
    DraggedFiles { paths }
}

pub(crate) fn file_drag_handler(
    view: Entity<App>,
    anchor: PathBuf,
) -> impl Fn(&DragView, gpui::Point<gpui::Pixels>, &mut Window, &mut gpui::App) -> Entity<DragView>
       + 'static {
    build_file_drag_handler(view, None, anchor)
}

pub(crate) fn file_drag_handler_for_item(
    view: Entity<App>,
    pane: TabId,
    idx: usize,
    anchor: PathBuf,
) -> impl Fn(&DragView, gpui::Point<gpui::Pixels>, &mut Window, &mut gpui::App) -> Entity<DragView>
       + 'static {
    build_file_drag_handler(view, Some((pane, idx)), anchor)
}

pub(crate) fn file_drag_handler_for_details_content(
    view: Entity<App>,
    pane: TabId,
) -> impl Fn(&DragView, gpui::Point<gpui::Pixels>, &mut Window, &mut gpui::App) -> Entity<DragView>
       + 'static {
    move |_d, cursor_offset, window, cx| {
        let item = {
            let app = view.read(&*cx);
            app.details_hit
                .get(&pane)
                .and_then(|model| model.hit_entry_at_content_offset(cursor_offset))
                .and_then(|idx| {
                    app.details_row_selected(pane, idx).then(|| {
                        app.state
                            .tab_states
                            .get(&pane)
                            .and_then(|ts| ts.entries.get(idx))
                            .map(|entry| (idx, entry.path()))
                    })?
                })
        };

        let Some((idx, anchor)) = item else {
            tracing::debug!("dnd details content drag ignored outside selected row");
            return empty_drag_view(cx);
        };

        build_file_drag_start(&view, Some((pane, idx)), anchor, window, cx)
    }
}

/// Drag source for the retained Grid surface.  The hit model is populated in
/// `GridSurfaceElement::prepaint`, so the closure does no widget traversal.
pub(crate) fn file_drag_handler_for_grid_content(
    view: Entity<App>,
    pane: TabId,
) -> impl Fn(&DragView, gpui::Point<gpui::Pixels>, &mut Window, &mut gpui::App) -> Entity<DragView>
       + 'static {
    file_drag_handler_for_surface_content(view, pane, false)
}

pub(crate) fn file_drag_handler_for_list_content(
    view: Entity<App>,
    pane: TabId,
) -> impl Fn(&DragView, gpui::Point<gpui::Pixels>, &mut Window, &mut gpui::App) -> Entity<DragView>
       + 'static {
    file_drag_handler_for_surface_content(view, pane, true)
}

fn file_drag_handler_for_surface_content(
    view: Entity<App>,
    pane: TabId,
    list: bool,
) -> impl Fn(&DragView, gpui::Point<gpui::Pixels>, &mut Window, &mut gpui::App) -> Entity<DragView>
       + 'static {
    move |_d, cursor_offset, window, cx| {
        let item = {
            let app = view.read(&*cx);
            let hit_model = if list {
                app.list_hit.get(&pane)
            } else {
                app.grid_hit.get(&pane)
            };
            hit_model
                .and_then(|model| model.hit_entry_at_content_offset(cursor_offset))
                .and_then(|idx| {
                    app.details_row_selected(pane, idx).then(|| {
                        app.state
                            .tab_states
                            .get(&pane)
                            .and_then(|ts| ts.entries.get(idx))
                            .map(|entry| (idx, entry.path()))
                    })?
                })
        };

        let Some((idx, anchor)) = item else {
            tracing::debug!(
                surface = if list { "list" } else { "grid" },
                "dnd surface content drag ignored outside selected cell"
            );
            return empty_drag_view(cx);
        };

        build_file_drag_start(&view, Some((pane, idx)), anchor, window, cx)
    }
}

fn build_file_drag_handler(
    view: Entity<App>,
    item: Option<(TabId, usize)>,
    anchor: PathBuf,
) -> impl Fn(&DragView, gpui::Point<gpui::Pixels>, &mut Window, &mut gpui::App) -> Entity<DragView>
       + 'static {
    move |_d, _pos, window, cx| build_file_drag_start(&view, item, anchor.clone(), window, cx)
}

pub(crate) fn empty_drag_view(cx: &mut gpui::App) -> Entity<DragView> {
    #[cfg(windows)]
    {
        cx.new(|_| DragNoop)
    }
    #[cfg(not(windows))]
    {
        cx.new(|_| DraggedFiles { paths: Vec::new() })
    }
}

pub(crate) fn empty_any_drag(cx: &mut gpui::App) -> AnyDrag {
    #[cfg(windows)]
    {
        AnyDrag::new(DragNoop, cx.new(|_| DragNoop))
    }
    #[cfg(not(windows))]
    {
        AnyDrag::new(
            DraggedFiles { paths: Vec::new() },
            cx.new(|_| DraggedFiles { paths: Vec::new() }),
        )
    }
}

pub(crate) fn build_file_any_drag(
    view: &Entity<App>,
    item: Option<(TabId, usize)>,
    anchor: PathBuf,
    cursor_style: Option<CursorStyle>,
    _window: &mut Window,
    cx: &mut gpui::App,
) -> AnyDrag {
    if !view.update(cx, |app, _| app.prepare_selection_file_drag()) {
        tracing::info!(anchor = %anchor.display(), "dnd file drag blocked by active rubber");
        return empty_any_drag(cx);
    }
    #[cfg(feature = "provider-host")]
    {
        let source = {
            let app = view.read(&*cx);
            let pane = item.map_or(app.state.focused_tile, |(pane, _)| pane);
            match app.tab_data_for(pane).map(|tab| &tab.location) {
                Some(atlas_model::location::Location::Remote { id, .. }) => {
                    Some((pane, id.clone()))
                }
                _ => None,
            }
        };
        if let Some((pane, account)) = source {
            let payload = view.update(cx, |app, cx| {
                app.pending_folder_click = None;
                app.state.focused_tile = pane;
                if let Some((_, index)) = item {
                    app.ensure_drag_selection(pane, index);
                }
                let mut paths = app.selected_paths_for_pane(pane);
                if paths.is_empty() || !paths.contains(&anchor) {
                    paths = vec![anchor.clone()];
                }
                let account_label = app
                    .state
                    .cloud
                    .remotes
                    .iter()
                    .find(|remote| remote.id == account)
                    .map_or_else(|| account.clone(), |remote| remote.name.clone());
                app.begin_cloud_drag();
                cx.notify();
                crate::ui::root::DraggedCloudFiles {
                    account,
                    account_label,
                    paths,
                }
            });
            let ghost = payload.clone();
            return AnyDrag::new(payload, cx.new(|_| ghost))
                .offset(point(px(-12.), px(-12.)))
                .cursor_style(cursor_style);
        }
    }
    // Recycle Bin rows carry mangled `$R` payload paths and remote rows carry
    // virtual paths — an OS drag would ship garbage. Block at the source.
    {
        use atlas_model::location::Location;
        let source_pane = item.map(|(pane, _)| pane);
        let blocked_key = {
            let app = view.read(&*cx);
            let pane = source_pane.unwrap_or(app.state.focused_tile);
            match app.tab_data_for(pane).map(|t| &t.location) {
                Some(Location::RecycleBin) => Some("dnd_recycle_drag_blocked"),
                _ => None,
            }
        };
        if let Some(key) = blocked_key {
            view.update(cx, |app, cx| {
                let msg = if key == "dnd_recycle_drag_blocked" {
                    rust_i18n::t!("dnd_recycle_drag_blocked")
                } else {
                    rust_i18n::t!("dnd_remote_drag_blocked")
                };
                app.toast_error(msg.into_owned());
                cx.notify();
            });
            return empty_any_drag(cx);
        }
    }
    view.update(cx, |app, cx| {
        app.pending_folder_click = None;
        if let Some((pane, idx)) = item {
            app.state.focused_tile = pane;
            app.ensure_drag_selection(pane, idx);
        }
        app.begin_drag_hover();
        cx.notify();
    });
    // Full multi-selection at drag-start time for the dragged pane.
    let (paths, archive_items) = {
        let app = view.read(&*cx);
        let mut arc = if let Some((pane, _)) = item {
            app.state.selected_archive_items_for_pane(pane)
        } else {
            app.state.selected_archive_items()
        };
        // Fallback: single-entry archive drag when the selection set is empty.
        if let Some(ref mut items) = arc {
            if items.is_empty() {
                let archive = if let Some((pane, _)) = item {
                    app.tab_data_for(pane)
                } else {
                    app.state.active_tab_data()
                }
                .and_then(|t| t.location.as_archive().map(|(a, _)| a.to_path_buf()));
                if let Some(archive) = archive {
                    items.push((archive, anchor.clone()));
                }
            }
        }
        // Archive tabs: entry paths are virtual (nothing on disk) — passing
        // them as real paths would raise one "path does not exist" error per
        // selected item. The extraction drag rides on `arc` alone.
        let p = if arc.is_some() {
            Vec::new()
        } else {
            let p = if let Some((pane, _)) = item {
                app.selected_paths_for_pane(pane)
            } else {
                app.state.selected_paths()
            };
            if p.is_empty() || !p.contains(&anchor) {
                vec![anchor.clone()]
            } else {
                p
            }
        };
        (p, arc)
    };
    // Windows: schedule DoDragDrop via PostMessage so it runs AFTER this closure
    // returns and gpui's App RefCell is no longer borrowed (prevents a
    // "RefCell already borrowed" panic from DoDragDrop's nested message loop).
    #[cfg(windows)]
    {
        tracing::info!(paths = paths.len(), "dnd file drag source triggered");
        view.read(&*cx)
            .trigger_os_drag(paths.clone(), archive_items);
        // Invisible zero-size placeholder; OLE provides cursor feedback and a
        // synthetic WM_LBUTTONUP clears gpui's active_drag without firing any
        // on_drop handler (DragNoop has no matching handlers).
        AnyDrag::new(DragNoop, cx.new(|_| DragNoop)).cursor_style(cursor_style)
    }
    #[cfg(not(windows))]
    {
        let _ = _window;
        let ext_view = view.clone();
        let ext_paths = paths.clone();
        view.update(cx, |app, _cx| {
            app.active_external_drag = Some(ActiveExternalDrag {
                paths: paths.clone(),
                archive_items,
            });
        });
        let payload = DraggedFiles {
            paths: paths.clone(),
        };
        let ghost_view = cx.new(|_| DraggedFiles { paths });
        AnyDrag::new(payload, ghost_view)
            .offset(point(px(-12.0), px(-12.0)))
            .cursor_style(cursor_style)
            .external_payload(move |_window, cx| {
                resolve_external_drag_payload(&ext_view, &DraggedFiles { paths: ext_paths }, cx)
            })
    }
}

fn build_file_drag_start(
    view: &Entity<App>,
    item: Option<(TabId, usize)>,
    anchor: PathBuf,
    window: &mut Window,
    cx: &mut gpui::App,
) -> Entity<DragView> {
    let any_drag = build_file_any_drag(view, item, anchor, None, window, cx);
    #[cfg(windows)]
    {
        let _ = any_drag;
        cx.new(|_| DragNoop)
    }
    #[cfg(not(windows))]
    {
        let paths = any_drag
            .value
            .downcast_ref::<DraggedFiles>()
            .map(|d| d.paths.clone())
            .unwrap_or_default();
        cx.new(|_| DraggedFiles { paths })
    }
}

/// Extension trait on interactive elements to cleanly attach Atlas file drag behavior.
/// Ensures correct selection resolution from the dragging pane, compact mouse offset
/// for the ghost badge, and proper internal payload propagation on Linux/macOS.
pub trait FileDragSource: gpui::StatefulInteractiveElement {
    fn on_file_drag_item(
        self,
        view: Entity<App>,
        pane: TabId,
        idx: usize,
        anchor: PathBuf,
    ) -> Self {
        self.on_drag_alt(move |_cursor_offset, cursor_style, window, cx| {
            build_file_any_drag(
                &view,
                Some((pane, idx)),
                anchor.clone(),
                cursor_style,
                window,
                cx,
            )
        })
    }

    fn on_file_drag_anchor(self, view: Entity<App>, anchor: PathBuf) -> Self {
        self.on_drag_alt(move |_cursor_offset, cursor_style, window, cx| {
            build_file_any_drag(&view, None, anchor.clone(), cursor_style, window, cx)
        })
    }

    fn on_file_drag_details(self, view: Entity<App>, pane: TabId) -> Self {
        self.on_drag_alt(move |cursor_offset, cursor_style, window, cx| {
            let item = {
                let app = view.read(&*cx);
                app.details_hit
                    .get(&pane)
                    .and_then(|model| model.hit_entry_at_content_offset(cursor_offset))
                    .and_then(|idx| {
                        app.details_row_selected(pane, idx).then(|| {
                            app.state
                                .tab_states
                                .get(&pane)
                                .and_then(|ts| ts.entries.get(idx))
                                .map(|entry| (idx, entry.path()))
                        })?
                    })
            };

            let Some((idx, anchor)) = item else {
                tracing::debug!("dnd details content drag ignored outside selected row");
                return empty_any_drag(cx);
            };

            build_file_any_drag(&view, Some((pane, idx)), anchor, cursor_style, window, cx)
        })
    }

    fn on_file_drag_grid(self, view: Entity<App>, pane: TabId) -> Self {
        self.on_drag_alt(move |cursor_offset, cursor_style, window, cx| {
            let item = {
                let app = view.read(&*cx);
                app.grid_hit
                    .get(&pane)
                    .and_then(|model| model.hit_entry_at_content_offset(cursor_offset))
                    .and_then(|idx| {
                        app.details_row_selected(pane, idx).then(|| {
                            app.state
                                .tab_states
                                .get(&pane)
                                .and_then(|ts| ts.entries.get(idx))
                                .map(|entry| (idx, entry.path()))
                        })?
                    })
            };

            let Some((idx, anchor)) = item else {
                tracing::debug!("dnd grid content drag ignored outside selected cell");
                return empty_any_drag(cx);
            };

            build_file_any_drag(&view, Some((pane, idx)), anchor, cursor_style, window, cx)
        })
    }

    fn on_file_drag_list(self, view: Entity<App>, pane: TabId) -> Self {
        self.on_drag_alt(move |cursor_offset, cursor_style, window, cx| {
            let item = {
                let app = view.read(&*cx);
                app.list_hit
                    .get(&pane)
                    .and_then(|model| model.hit_entry_at_content_offset(cursor_offset))
                    .and_then(|idx| {
                        app.details_row_selected(pane, idx).then(|| {
                            app.state
                                .tab_states
                                .get(&pane)
                                .and_then(|ts| ts.entries.get(idx))
                                .map(|entry| (idx, entry.path()))
                        })?
                    })
            };

            let Some((idx, anchor)) = item else {
                tracing::debug!("dnd list content drag ignored outside selected cell");
                return empty_any_drag(cx);
            };

            build_file_any_drag(&view, Some((pane, idx)), anchor, cursor_style, window, cx)
        })
    }
}

impl<E: gpui::StatefulInteractiveElement> FileDragSource for E {}

/// Staging directory for extracting virtual archive items for external OS DnD:
/// `$XDG_CACHE_HOME/atlas/dnd_staging` (falling back to `~/.cache/atlas/dnd_staging` or `$TMP/atlas/dnd_staging`).
pub(crate) fn dnd_staging_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::cache_dir().unwrap_or_else(std::env::temp_dir));
    base.join("atlas").join("dnd_staging")
}

/// Extracts virtual archive entries into a local temporary staging directory
/// so that external applications can read the real files on disk.
/// Returns a list of `(dest_path, is_dir)`.
pub(crate) fn extract_archive_items_to_staging(
    items: &[(PathBuf, PathBuf)],
) -> Vec<(PathBuf, bool)> {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let session_dir = dnd_staging_dir().join(format!("drag_{}_{}", std::process::id(), timestamp));
    #[allow(clippy::disallowed_methods)]
    if let Err(e) = std::fs::create_dir_all(&session_dir) {
        tracing::warn!(
            "Failed to create dnd staging dir {}: {e}",
            session_dir.display()
        );
        return Vec::new();
    }

    let mut by_archive: std::collections::HashMap<&Path, Vec<&Path>> =
        std::collections::HashMap::new();
    for (archive, inner) in items {
        by_archive
            .entry(archive.as_path())
            .or_default()
            .push(inner.as_path());
    }

    let mut extracted = Vec::new();
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut errors = Vec::new();

    for (archive_path, inners) in by_archive {
        let Some(provider) = atlas_core::fs::archive::open(archive_path) else {
            tracing::warn!(archive = %archive_path.display(), "Failed to open archive for external drag extraction");
            continue;
        };
        for inner in inners {
            let name = inner
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("extracted"));
            let dest = session_dir.join(name);
            crate::app::fs_sync::extract_entry_recursive(
                provider.as_ref(),
                inner,
                &dest,
                &cancel,
                &mut errors,
            );
            if dest.exists() {
                let is_dir = dest.is_dir();
                extracted.push((dest, is_dir));
            }
        }
    }
    if !errors.is_empty() {
        tracing::warn!(?errors, "Errors extracting archive items for external drag");
    }
    extracted
}

/// Resolves the [`ExternalDragPayload`] to hand to the platform (Wayland / macOS)
/// when an Atlas drag leaves the window.
pub(crate) fn resolve_external_drag_payload(
    view: &Entity<App>,
    drag_view: &DragView,
    cx: &mut gpui::App,
) -> Option<ExternalDragPayload> {
    #[cfg(windows)]
    {
        let _ = (view, drag_view, cx);
        None
    }
    #[cfg(not(windows))]
    {
        let (paths, archive_items) = {
            let app = view.read(&*cx);
            if let Some(active) = app.active_external_drag.as_ref() {
                (active.paths.clone(), active.archive_items.clone())
            } else if !drag_view.paths.is_empty() {
                (drag_view.paths.clone(), app.state.selected_archive_items())
            } else {
                (
                    app.state.selected_paths(),
                    app.state.selected_archive_items(),
                )
            }
        };

        let mut entries = Vec::new();

        for p in paths {
            let p = if p.is_absolute() {
                p
            } else if let Ok(abs) = p.canonicalize() {
                abs
            } else {
                p
            };
            if p.exists() {
                let is_dir = p.is_dir();
                entries.push((p, is_dir));
            }
        }

        if let Some(items) = archive_items {
            if !items.is_empty() {
                let extracted = extract_archive_items_to_staging(&items);
                entries.extend(extracted);
            }
        }

        let mut seen = std::collections::HashSet::new();
        entries.retain(|(p, _)| seen.insert(p.clone()));

        if entries.is_empty() {
            tracing::debug!("dnd external drag payload empty: no valid paths or archive items");
            return None;
        }

        tracing::info!(
            count = entries.len(),
            "dnd external drag payload resolved for OS drag"
        );
        Some(ExternalDragPayload::Files(FileDragPaths::new(entries)))
    }
}

/// Constructs the external drag payload resolver callback for [`gpui::InteractiveElement::external_drag_payload`].
pub(crate) fn file_external_drag_payload(
    view: Entity<App>,
) -> impl Fn(&DragView, &mut Window, &mut gpui::App) -> Option<ExternalDragPayload> + 'static {
    move |drag_view, _window, cx| resolve_external_drag_payload(&view, drag_view, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dnd_staging_dir_resolution() {
        let staging = dnd_staging_dir();
        assert!(staging.ends_with(Path::new("atlas").join("dnd_staging")));
    }

    #[test]
    fn test_extract_archive_items_empty() {
        let extracted = extract_archive_items_to_staging(&[]);
        assert!(extracted.is_empty());
    }

    #[test]
    fn test_extract_archive_items_nonexistent_archive() {
        let items = vec![(
            PathBuf::from("/nonexistent/atlas_dummy_archive.zip"),
            PathBuf::from("test.txt"),
        )];
        let extracted = extract_archive_items_to_staging(&items);
        assert!(extracted.is_empty());
    }

    #[test]
    fn test_file_drag_paths_entries() {
        let entries = vec![
            (PathBuf::from("/tmp/atlas_test_file.txt"), false),
            (PathBuf::from("/tmp/atlas_test_dir"), true),
        ];
        let file_drag_paths = FileDragPaths::new(entries.clone());
        assert_eq!(file_drag_paths.entries(), &entries[..]);
    }

    #[test]
    fn test_staging_with_real_files_and_filtering() {
        let tmp = std::env::temp_dir().join(format!("atlas_dnd_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let file_path = tmp.join("test_file.txt");
        let dir_path = tmp.join("test_dir");
        let nonexistent = tmp.join("nonexistent.txt");

        std::fs::write(&file_path, b"test").unwrap();
        let _ = std::fs::create_dir_all(&dir_path);

        let input_paths = vec![
            file_path.clone(),
            nonexistent,
            dir_path.clone(),
            file_path.clone(), // duplicate
        ];

        let mut entries = Vec::new();
        for p in input_paths {
            if p.exists() {
                let is_dir = p.is_dir();
                entries.push((p, is_dir));
            }
        }
        let mut seen = std::collections::HashSet::new();
        entries.retain(|(p, _)| seen.insert(p.clone()));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], (file_path.clone(), false));
        assert_eq!(entries[1], (dir_path.clone(), true));

        let payload = ExternalDragPayload::Files(FileDragPaths::new(entries));
        let ExternalDragPayload::Files(paths) = payload;
        assert_eq!(paths.entries().len(), 2);
        assert_eq!(paths.entries()[0], (file_path, false));
        assert_eq!(paths.entries()[1], (dir_path, true));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
