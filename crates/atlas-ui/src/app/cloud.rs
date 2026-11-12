//! App-side glue for the cloud extension: routes `CloudAction`s to the worker,
//! applies `BgResult`s to `CloudState`, and handles Access-mode remote browsing.

use atlas_model::layout_tree::{Tile, TileId};
use std::path::PathBuf;

use crate::app::App;
use atlas_core::cloud::{
    AccessKind, CloudCmd, CloudRemote, RemoteId, RemoteKind, RemoteStatus, TransferId,
};
use atlas_model::app_state::BgResult;
use atlas_model::cloud_state::AddRemoteKind;
use atlas_model::location::Location;
use atlas_model::settings::ViewMode;
use atlas_model::tab::LoadState;

/// Parse a remote tab's sentinel path (`remote:<id>` or `remote:<id>/<sub>`) into
/// `(id, remote_relative_dir)`. Real local destinations are absolute (`/…`) so
/// they never match this `remote:` prefix.
fn parse_remote_sentinel(dest: &std::path::Path) -> Option<(String, String)> {
    let s = dest.to_str()?;
    let rest = s.strip_prefix("remote:")?;
    match rest.split_once('/') {
        Some((id, base)) => Some((id.to_string(), base.trim_end_matches('/').to_string())),
        None => Some((rest.to_string(), String::new())),
    }
}

/// Build the sentinel `PathBuf` for a remote directory (inverse of
/// [`parse_remote_sentinel`]). Lets drop/transfer call sites express "this dest
/// is a remote dir" through the ordinary `PathBuf` plumbing.
pub(crate) fn remote_sentinel(id: &str, path: &str) -> PathBuf {
    let path = path.trim_matches('/');
    if path.is_empty() {
        PathBuf::from(format!("remote:{id}"))
    } else {
        PathBuf::from(format!("remote:{id}/{path}"))
    }
}

/// UI-level cloud actions, dispatched from the sidebar and dialogs.
#[derive(Debug, Clone)]
pub enum CloudAction {
    ShowAddDialog(bool),
    /// Build a remote from the add-remote form and create its rclone config.
    SubmitAddRemote,
    RemoveRemote(RemoteId),
    Mount(RemoteId),
    Unmount(RemoteId),
    /// Open an Access-mode browsing tab for the remote in the focused pane.
    OpenAccess(RemoteId),
    /// Navigate to the mount point of an already-mounted remote.
    OpenMount(RemoteId),
    ShowTransfers(bool),
    PauseTransfer(TransferId),
    ResumeTransfer(TransferId),
    CancelTransfer(TransferId),
    RetryTransfer(TransferId),
    /// Open the rename overlay for a remote.
    StartRenameRemote(RemoteId),
    /// Commit the new name (read from input before dispatch).
    SubmitRenameRemote(String),
    /// Cancel the rename overlay.
    CancelRenameRemote,
}

impl App {
    pub(crate) fn drop_cloud_files(
        &mut self,
        drag: &crate::ui::root::DraggedCloudFiles,
        position: gpui::Point<gpui::Pixels>,
        shift: bool,
    ) {
        let destination =
            self.drag_hover_drop_command_at(f32::from(position.x), f32::from(position.y));
        let accepted = if shift {
            self.toast_error(rust_i18n::t!("cloud_drag_copy_only").into_owned());
            false
        } else if let Some(crate::ui::drag_hover::DragHoverDropCommand::TransferTo(destination)) =
            destination
        {
            self.copy_cloud_files_to(drag, &destination)
        } else {
            self.toast_error(rust_i18n::t!("dnd_intent_blocked").into_owned());
            false
        };
        if accepted {
            self.mark_drag_committed();
        } else {
            self.drag_hover_external_left();
        }
        self.drag_hover.clear();
    }

    pub(crate) fn copy_cloud_files_to(
        &mut self,
        drag: &crate::ui::root::DraggedCloudFiles,
        destination: &std::path::Path,
    ) -> bool {
        if let Some((destination_id, base)) = parse_remote_sentinel(destination) {
            if drag.account == destination_id
                && drag.paths.iter().any(|path| {
                    let path = path.to_string_lossy();
                    base == path || base.starts_with(&format!("{path}/"))
                })
            {
                self.toast_error(rust_i18n::t!("dnd_intent_blocked").into_owned());
                return false;
            }
            for path in &drag.paths {
                let source = path.to_string_lossy().into_owned();
                let name = atlas_model::remote_path::file_name(&source);
                if name.is_empty()
                    || name == "."
                    || name == ".."
                    || name.contains(['/', '\\', '\0'])
                {
                    self.toast_error(rust_i18n::t!("cloud_invalid_local_name").into_owned());
                    return false;
                }
            }
            for path in &drag.paths {
                let source = path.to_string_lossy().into_owned();
                let name = atlas_model::remote_path::file_name(&source);
                let target = if base.is_empty() {
                    name.clone()
                } else {
                    format!("{base}/{name}")
                };
                let _ = self.cloud_tx.send(CloudCmd::EnqueueRemoteCopy {
                    source_id: drag.account.clone(),
                    source,
                    destination_id: destination_id.clone(),
                    destination: target,
                    label: name,
                });
            }
        } else {
            if !destination.is_absolute() {
                return false;
            }
            if drag.paths.iter().any(|path| {
                !atlas_model::remote_path::safe_local_name(&atlas_model::remote_path::file_name(
                    &path.to_string_lossy(),
                ))
            }) {
                self.toast_error(rust_i18n::t!("cloud_invalid_local_name").into_owned());
                return false;
            }
            for path in &drag.paths {
                let remote_path = path.to_string_lossy().into_owned();
                let name = atlas_model::remote_path::file_name(&remote_path);
                let _ = self.cloud_tx.send(CloudCmd::EnqueueDownload {
                    id: drag.account.clone(),
                    remote_path,
                    local_path: destination.join(&name),
                    label: name,
                    open_on_done: false,
                });
            }
        }
        self.state.cloud.show_transfers = true;
        true
    }

    pub(crate) fn cloud_drop_label(&self, destination: &std::path::Path) -> String {
        match parse_remote_sentinel(destination) {
            Some((id, path)) => {
                let account = self
                    .state
                    .cloud
                    .remotes
                    .iter()
                    .find(|remote| remote.id == id)
                    .map_or(id.as_str(), |remote| remote.name.as_str());
                format!("{account} / {}", atlas_model::remote_path::display(&path))
            }
            None => destination.display().to_string(),
        }
    }

    pub(crate) fn cloud_drag_destination_allowed(
        &self,
        drag: &crate::ui::root::DraggedCloudFiles,
        command: &crate::ui::drag_hover::DragHoverDropCommand,
        shift: bool,
    ) -> bool {
        let crate::ui::drag_hover::DragHoverDropCommand::TransferTo(destination) = command else {
            return false;
        };
        if shift {
            return false;
        }
        match parse_remote_sentinel(destination) {
            Some((id, base)) => {
                !(id == drag.account
                    && drag.paths.iter().any(|path| {
                        let path = path.to_string_lossy();
                        base == path || base.starts_with(&format!("{path}/"))
                    }))
            }
            None => destination.is_absolute(),
        }
    }

    /// Hydrate one remote selection through the bounded provider preview queue.
    pub(crate) fn load_cloud_preview_for_selection(&mut self) -> bool {
        let tile = self.state.focused_tile;
        let Some(Location::Remote { id, .. }) =
            self.tab_data_for(tile).map(|tab| tab.location.clone())
        else {
            return false;
        };
        let entry = self.state.tab_states.get(&tile).and_then(|state| {
            let view = &self.tab_data_for(tile)?.view_mode;
            let selected = state.selected_entries(view);
            (selected.len() == 1).then(|| {
                (
                    state.last_nav_op_id,
                    selected[0].path(),
                    selected[0].size(),
                    selected[0].kind() == atlas_model::file_entry::FileKind::Dir,
                )
            })
        });
        let Some((op_id, path, size, is_dir)) = entry else {
            self.state.clear_preview(tile);
            let _ = self.cloud_tx.send(CloudCmd::CancelPreview { tab: tile });
            return true;
        };
        let key = atlas_model::cloud_model::preview_key(&id, op_id, &path);
        if self.state.preview.paths.get(&tile) != Some(&key) {
            self.state.clear_preview(tile);
            self.preview_textures.remove(&tile);
            self.preview_editor = None;
            self.state.preview.paths.insert(tile, key.clone());
            let _ = self.cloud_tx.send(CloudCmd::PreviewRemote {
                tab: tile,
                id,
                path: path.to_string_lossy().into_owned(),
                key,
                size,
                is_dir,
            });
        }
        true
    }

    pub(crate) fn handle_cloud_action(&mut self, action: CloudAction) {
        match action {
            CloudAction::ShowAddDialog(show) => {
                if show {
                    if !self.allow_edition_action(
                        atlas_model::edition::Feature::SavedConnections,
                        self.state.cloud.remotes.len().saturating_add(1),
                    ) {
                        return;
                    }
                    self.state.cloud.add_form = atlas_model::cloud_state::AddRemoteForm::default();
                }
                self.state.cloud.show_add_dialog = show;
            }
            CloudAction::SubmitAddRemote => self.submit_add_remote(),
            CloudAction::RemoveRemote(id) => {
                let _ = self.cloud_tx.send(CloudCmd::RemoveRemote(id));
            }
            CloudAction::Mount(id) => {
                // User-initiated: navigate into it once mounted.
                self.state.cloud.navigate_on_mount.insert(id.clone());
                let _ = self.cloud_tx.send(CloudCmd::Mount { id });
            }
            CloudAction::Unmount(id) => {
                let _ = self.cloud_tx.send(CloudCmd::Unmount { id });
            }
            CloudAction::OpenAccess(id) => self.open_remote_access(id),
            CloudAction::OpenMount(id) => {
                let mount = atlas_core::cloud::mount_point_for(&id);
                if mount.is_dir() {
                    self.navigate(self.state.focused_tile, mount);
                }
            }
            CloudAction::ShowTransfers(show) => self.state.cloud.show_transfers = show,
            CloudAction::PauseTransfer(id) => {
                let _ = self.cloud_tx.send(CloudCmd::PauseTransfer(id));
            }
            CloudAction::ResumeTransfer(id) => {
                let _ = self.cloud_tx.send(CloudCmd::ResumeTransfer(id));
            }
            CloudAction::CancelTransfer(id) => {
                let _ = self.cloud_tx.send(CloudCmd::CancelTransfer(id));
            }
            CloudAction::RetryTransfer(id) => {
                let _ = self.cloud_tx.send(CloudCmd::RetryTransfer(id));
            }
            CloudAction::StartRenameRemote(id) => {
                self.state.cloud.renaming_remote_id = Some(id);
                self.rename_remote_input = None; // re-seed on next render
            }
            CloudAction::SubmitRenameRemote(new_name) => {
                if let Some(id) = self.state.cloud.renaming_remote_id.clone() {
                    let trimmed = new_name.trim().to_string();
                    if !trimmed.is_empty() {
                        if let Some(r) = self.state.cloud.remote_mut(&id) {
                            r.name = trimmed;
                        }
                        atlas_core::util::config::save_cloud_remotes(&self.state.cloud.remotes);
                    }
                }
                self.state.cloud.renaming_remote_id = None;
                self.rename_remote_input = None;
            }
            CloudAction::CancelRenameRemote => {
                self.state.cloud.renaming_remote_id = None;
                self.rename_remote_input = None;
            }
        }
    }

    fn submit_add_remote(&mut self) {
        // The optimistic row reserves the slot synchronously, including while
        // authorization is pending. Recheck even if the dialog was already open.
        if !self.allow_edition_action(
            atlas_model::edition::Feature::SavedConnections,
            self.state.cloud.remotes.len().saturating_add(1),
        ) {
            return;
        }
        let form = self.state.cloud.add_form.clone();
        let host = form.host.trim().to_string();
        let user = form.user.trim().to_string();
        let kind = match form.kind {
            AddRemoteKind::Sftp => RemoteKind::Sftp {
                host: host.clone(),
                port: form.port.trim().parse().unwrap_or(22),
                user,
            },
            AddRemoteKind::Ftp => RemoteKind::Ftp {
                host: host.clone(),
                port: form.port.trim().parse().unwrap_or(21),
                user,
            },
            AddRemoteKind::WebDav => {
                let url = if host.starts_with("http://") || host.starts_with("https://") {
                    host.clone()
                } else {
                    let port_str = form.port.trim();
                    let port = port_str.parse::<u16>().unwrap_or(80);
                    if port == 443 {
                        format!("https://{host}")
                    } else if port == 80 {
                        format!("http://{host}")
                    } else {
                        format!("http://{host}:{port}")
                    }
                };
                RemoteKind::WebDav {
                    url,
                    vendor: "other".to_string(),
                    user,
                }
            }
            AddRemoteKind::Smb => {
                let (user, domain) = atlas_model::cloud_model::split_smb_login(&user);
                RemoteKind::Smb {
                    host: host.clone(),
                    port: form.port.trim().parse().unwrap_or(445),
                    user,
                    domain,
                }
            }
            AddRemoteKind::GoogleDrive => RemoteKind::GoogleDrive,
            AddRemoteKind::OneDrive => RemoteKind::OneDrive,
        };
        // Non-OAuth backends need a host; OAuth backends need nothing but a name.
        if !kind.is_oauth() && host.is_empty() {
            self.state.cloud.last_error = Some(rust_i18n::t!("cloud_host_required").into());
            return;
        }
        let display = match form.name.trim() {
            "" if kind.is_oauth() => kind.label().to_string(),
            "" => host,
            name => name.to_string(),
        };
        let id = self.unique_remote_id(&display);
        let remote = CloudRemote {
            id,
            name: display,
            kind,
            access: form.access,
            auto_mount: form.access == AccessKind::Mount,
            status: RemoteStatus::Connecting,
            quota: None,
        };
        // Show the row immediately (Connecting) for instant feedback; the worker
        // replaces it on success or marks it Error on failure.
        self.state.cloud.last_error = None;
        self.state.cloud.remotes.push(remote.clone());
        let _ = self.cloud_tx.send(CloudCmd::AddRemote {
            remote,
            secret: form.password,
        });
        self.state.cloud.show_add_dialog = false;
    }

    /// Derive a unique rclone remote name (alphanumerics + `-_`) from a label.
    fn unique_remote_id(&self, label: &str) -> String {
        let base: String = label
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect();
        let base = base.trim_matches('-').to_lowercase();
        let base = if base.is_empty() {
            "remote".into()
        } else {
            base
        };
        let exists = |id: &str| self.state.cloud.remotes.iter().any(|r| r.id == id);
        if !exists(&base) {
            return base;
        }
        let mut n = 2u32;
        loop {
            let candidate = format!("{base}-{n}");
            if !exists(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Open Access-mode browsing in the focused tab.
    fn open_remote_access(&mut self, id: RemoteId) {
        let tab_id = self.state.focused_tile;
        // Miller assumes a navigable local tree; remote browsing is flat-per-dir.
        let view_mode = match self.state.settings.view_mode.clone() {
            ViewMode::Miller | ViewMode::Tree => ViewMode::Details,
            other => other,
        };
        if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(tab_id) {
            pane.view_mode = view_mode;
        }
        self.navigate_remote(tab_id, id, String::new());
    }

    /// Re-list a remote tab at `path` (Access-mode directory descent).
    pub(crate) fn navigate_remote(&mut self, tab_id: TileId, id: RemoteId, path: String) {
        self.navigate_to(
            tab_id,
            Location::Remote { id, path },
            super::nav::NavKind::Push,
        );
    }

    /// Ask the worker to list a remote dir; the answer comes back as `DirListed`.
    pub(crate) fn spawn_list_remote(&mut self, tab_id: TileId, id: RemoteId, path: String) {
        if let Some(Tile::Pane(pane)) = self.state.pane_tree.tiles.get_mut(tab_id) {
            if matches!(pane.view_mode, ViewMode::Miller | ViewMode::Tree) {
                pane.view_mode = ViewMode::Details;
            }
        }
        let (sort_col, sort_dir, folder_order) = self
            .tab_data_for(tab_id)
            .map(|tab| (tab.sort_col.clone(), tab.sort_dir.clone(), tab.folder_order))
            .unwrap_or_default();
        let op_id = self.state.next_op_id();
        let ts = self.state.tab_states.entry(tab_id).or_default();
        ts.load_state = LoadState::Loading;
        ts.last_nav_op_id = op_id;
        let _ = self.cloud_tx.send(CloudCmd::ListRemote {
            tab: tab_id,
            op_id,
            id,
            path,
            sort_col,
            sort_dir,
            folder_order,
        });
    }

    /// If `dest` is an Access-mode remote sentinel (`remote:<id>/<path>`), enqueue
    /// uploads of `paths` into it via the Transfer Manager and return `true`.
    /// Used to intercept drag/paste into a remote tab, which would otherwise run a
    /// local filesystem copy/move on a non-existent path. `delete_source` trashes
    /// each local source after its upload succeeds (move-semantics).
    pub(crate) fn try_remote_upload(
        &mut self,
        paths: &[PathBuf],
        dest: &std::path::Path,
        delete_source: bool,
    ) -> bool {
        let Some((id, base)) = parse_remote_sentinel(dest) else {
            return false;
        };
        for src in paths {
            let Some(name) = src.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            let remote_path = if base.is_empty() {
                name.clone()
            } else {
                format!("{base}/{name}")
            };
            let _ = self.cloud_tx.send(CloudCmd::EnqueueUpload {
                id: id.clone(),
                local_path: src.clone(),
                remote_path,
                label: name,
                delete_source,
            });
        }
        self.state.cloud.show_transfers = true;
        true
    }

    /// Release mounts + kill the rclone daemon on app exit. Synchronous and
    /// best-effort: the daemon lives on a never-joined runtime thread, so its
    /// `Drop` won't run at process exit — we unmount and kill the PID directly.
    pub(crate) fn cleanup_cloud_on_exit(&self) {
        use atlas_core::cloud::RemoteStatus;
        for r in &self.state.cloud.remotes {
            if let RemoteStatus::Mounted(_path) = &r.status {
                // Windows: WinFsp FUSE mounts are cleaned up when the rclone daemon
                // process is killed below — no extra unmount step needed.
                #[cfg(not(windows))]
                {
                    let p = _path.to_string_lossy();
                    // fusermount3 (fuse3) → fusermount (fuse2) → umount
                    let _ = std::process::Command::new("fusermount3")
                        .args(["-u", &p])
                        .status()
                        .or_else(|_| {
                            std::process::Command::new("fusermount")
                                .args(["-u", &p])
                                .status()
                        })
                        .or_else(|_| {
                            std::process::Command::new("umount")
                                .arg(p.as_ref())
                                .status()
                        });
                }
            }
        }
    }

    /// If the focused tab is an Access-mode remote, enqueue downloads of `paths`
    /// (remote-relative) into the downloads folder and open them when done.
    /// Returns `true` when handled (so the caller skips the local open path).
    pub(crate) fn open_remote_paths(&mut self, paths: &[PathBuf]) -> bool {
        let tile = self.state.focused_tile;
        self.open_remote_paths_for(tile, paths)
    }

    /// If the focused tab is an Access-mode remote tab, return `(remote_id, current_path)`.
    /// Used by action interceptors to route Delete/Rename/Mkdir to the cloud worker.
    pub(crate) fn focused_remote_tab(&self) -> Option<(RemoteId, String)> {
        let tile = self.state.focused_tile;
        match self.tab_data_for(tile)?.location.clone() {
            Location::Remote { id, path } => Some((id, path)),
            _ => None,
        }
    }

    /// Paste local clipboard files into a remote directory (upload).
    pub(crate) fn paste_local_into_remote(&mut self, dst_id: RemoteId, dst_path: String) {
        let Some(cb) = self.state.ops.clipboard.take() else {
            return;
        };
        let is_cut = cb.is_cut;
        for local_path in cb.paths {
            let name = local_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let remote_path = if dst_path.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", dst_path.trim_end_matches('/'), name)
            };
            let label = name.clone();
            let _ = self.cloud_tx.send(CloudCmd::EnqueueUpload {
                id: dst_id.clone(),
                local_path: local_path.clone(),
                remote_path,
                label,
                delete_source: is_cut,
            });
        }
    }

    /// Paste remote clipboard files into a local directory (download).
    pub(crate) fn paste_remote_into_local(&mut self, local_dest: PathBuf) {
        let Some(cb) = self.state.ops.clipboard.take() else {
            return;
        };
        let Some(src_id) = cb.remote_id else {
            return;
        };
        for remote_path in cb.paths {
            let remote_str = remote_path.to_string_lossy().replace('\\', "/");
            let name = atlas_model::remote_path::file_name(&remote_str);
            if !atlas_model::remote_path::safe_local_name(&name) {
                self.toast_error(rust_i18n::t!("cloud_invalid_local_name").into_owned());
                continue;
            }
            let local_path = local_dest.join(&name);
            let _ = self.cloud_tx.send(CloudCmd::EnqueueDownload {
                id: src_id.clone(),
                remote_path: remote_str,
                local_path,
                label: name,
                open_on_done: false,
            });
        }
        if cb.is_cut {
            // Source files will be deleted after each download completes.
            // The EnqueueDownload + delete_source pattern isn't hooked here yet;
            // for now cut from remote to local is treated as copy (safe default).
        }
    }

    /// Copy/move files within the same remote (server-side, no download).
    pub(crate) fn paste_remote_within(
        &mut self,
        src_id: RemoteId,
        dst_id: RemoteId,
        dst_path: String,
    ) {
        let Some(cb) = self.state.ops.clipboard.take() else {
            return;
        };
        if cb.is_cut && src_id != dst_id {
            self.state.ops.clipboard = Some(cb);
            self.toast_error(rust_i18n::t!("cloud_cross_remote_move_unavailable").into_owned());
            return;
        }
        let is_move = cb.is_cut;
        let tile = self.state.focused_tile;
        let is_dir_map: std::collections::HashMap<PathBuf, bool> = self
            .state
            .tab_states
            .get(&tile)
            .map(|ts| {
                ts.entries
                    .iter()
                    .map(|e| {
                        (
                            e.path(),
                            matches!(e.kind(), atlas_model::file_entry::FileKind::Dir),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        let entries: Vec<(String, String, bool)> = cb
            .paths
            .iter()
            .filter_map(|p| {
                let src_str = p.to_string_lossy().replace('\\', "/");
                let name = atlas_model::remote_path::file_name(&src_str);
                let dst_str = if dst_path.is_empty() {
                    name
                } else {
                    format!("{}/{}", dst_path.trim_end_matches('/'), name)
                };
                let is_dir = is_dir_map.get(p).copied().unwrap_or(false);
                Some((src_str, dst_str, is_dir))
            })
            .collect();

        if !entries.is_empty() {
            if is_move {
                let _ = self.cloud_tx.send(CloudCmd::CopyRemoteEntries {
                    id: src_id,
                    entries,
                    is_move,
                    parent: dst_path,
                });
            } else {
                for (source, destination, _) in entries {
                    let label = atlas_model::remote_path::file_name(&source);
                    let _ = self.cloud_tx.send(CloudCmd::EnqueueRemoteCopy {
                        source_id: src_id.clone(),
                        source,
                        destination_id: dst_id.clone(),
                        destination,
                        label,
                    });
                }
                self.state.cloud.show_transfers = true;
            }
        }
    }

    /// Same as `open_remote_paths` but checks `tile` instead of the focused tile.
    /// Used by `Action::Activate` which carries the tile id explicitly.
    pub(crate) fn open_remote_paths_for(
        &mut self,
        tile: atlas_model::layout_tree::TileId,
        paths: &[PathBuf],
    ) -> bool {
        let Some(Location::Remote { id, .. }) =
            self.tab_data_for(tile).map(|td| td.location.clone())
        else {
            return false;
        };
        let generation = self
            .state
            .tab_states
            .get(&tile)
            .map_or(0, |state| state.last_nav_op_id);
        for path in paths {
            let remote_path = path.to_string_lossy().into_owned();
            let name = atlas_model::remote_path::file_name(&remote_path);
            if !atlas_model::remote_path::safe_local_name(&name) {
                self.toast_error(rust_i18n::t!("cloud_invalid_local_name").into_owned());
                continue;
            }
            let dest_dir = atlas_core::cloud::open_cache_dir(&id, generation, &remote_path);
            // Re-opening downloads the current revision into a fresh location.
            let dest_dir = dest_dir.join(format!("open-{}", self.state.next_op_id()));
            let _ = self.cloud_tx.send(CloudCmd::EnqueueDownload {
                id: id.clone(),
                remote_path,
                local_path: dest_dir.join(&name),
                label: name,
                open_on_done: true,
            });
        }
        self.state.cloud.show_transfers = true;
        true
    }

    /// Apply a cloud-related `BgResult` to `CloudState`.
    pub(crate) fn handle_cloud_bg(&mut self, result: BgResult) {
        match result {
            BgResult::CloudProviderStopped { uninstall, result } => {
                self.complete_cloud_shutdown(uninstall, result)
            }
            BgResult::CloudRcloneProgress { done, total } => {
                self.state.cloud.rclone_progress = Some((done, total));
            }
            BgResult::CloudReady => {
                self.state.cloud.ready = true;
                self.state.cloud.rclone_progress = None;
                self.state.cloud.last_error = None;
                let restored = self
                    .state
                    .all_pane_tile_ids()
                    .into_iter()
                    .filter_map(|tile| {
                        if self
                            .state
                            .tab_states
                            .get(&tile)
                            .is_some_and(|state| state.load_state != LoadState::NotLoaded)
                        {
                            return None;
                        }
                        match self.tab_data_for(tile)?.location.clone() {
                            Location::Remote { id, path } => Some((tile, id, path)),
                            _ => None,
                        }
                    })
                    .collect::<Vec<_>>();
                for (tile, id, path) in restored {
                    let legacy_drive = self.state.cloud.remotes.iter().any(|remote| {
                        remote.id == id && matches!(remote.kind, RemoteKind::GoogleDrive)
                    }) && !path.is_empty()
                        && atlas_model::remote_path::last(&path).is_none();
                    if legacy_drive {
                        self.navigate_remote(tile, id, String::new());
                    } else {
                        self.spawn_list_remote(tile, id, path);
                    }
                }
            }
            BgResult::CloudError(e) => {
                self.state.cloud.rclone_progress = None;
                self.state.cloud.last_error = Some(e);
            }
            BgResult::RemoteConfigured(remote) => {
                self.state.cloud.rclone_progress = None;
                let auto_mount = remote.auto_mount;
                let id = remote.id.clone();
                match self.state.cloud.remote_mut(&id) {
                    Some(existing) => *existing = remote,
                    None => self.state.cloud.remotes.push(remote),
                }
                if auto_mount {
                    let _ = self.cloud_tx.send(CloudCmd::Mount { id });
                }
                atlas_core::util::config::save_cloud_remotes(&self.state.cloud.remotes);
            }
            BgResult::RemoteConfigFailed { id, error } => {
                // Mark the optimistic row as errored so the user sees why + can remove it.
                if let Some(r) = self.state.cloud.remote_mut(&id) {
                    r.status = RemoteStatus::Error(error.clone());
                }
                self.state.cloud.last_error = Some(format!("{id}: {error}"));
            }
            BgResult::RemoteRemoved(id) => {
                self.state.cloud.remotes.retain(|r| r.id != id);
                atlas_core::util::config::save_cloud_remotes(&self.state.cloud.remotes);
            }
            BgResult::RemoteStatusChanged { id, status } => {
                let mount_path = match &status {
                    RemoteStatus::Mounted(p) => Some(p.clone()),
                    RemoteStatus::Error(e) => {
                        let msg = format!("{id}: {e}");
                        self.state.cloud.last_error = Some(msg.clone());
                        self.toast_error(msg);
                        None
                    }
                    RemoteStatus::BrowseFallback(reason) => {
                        // Informational: not an error, just a capability downgrade.
                        let msg = format!("{id}: {reason}");
                        self.toast_info(msg);
                        None
                    }
                    _ => None,
                };
                if let Some(r) = self.state.cloud.remote_mut(&id) {
                    r.status = status;
                }
                // Jump into the mount only if the user initiated it (not auto-mount).
                if let Some(path) = mount_path {
                    if self.state.cloud.navigate_on_mount.remove(&id) && path.is_dir() {
                        self.navigate(self.state.focused_tile, path);
                    }
                }
            }
            BgResult::RemoteQuota { id, used, total } => {
                if let Some(r) = self.state.cloud.remote_mut(&id) {
                    r.quota = Some((used, total));
                }
            }
            BgResult::RemoteChanged { id, paths } => {
                // Re-list every Access-mode tab currently showing this remote dir.
                let paths: std::collections::HashSet<String> = paths
                    .into_iter()
                    .map(|p| p.trim_matches('/').to_string())
                    .collect();
                let targets: Vec<TileId> = self
                    .state
                    .all_pane_tile_ids()
                    .into_iter()
                    .filter(|tile| {
                        matches!(
                            self.tab_data_for(*tile).map(|td| td.location.clone()),
                            Some(Location::Remote { id: ref rid, path: ref rp })
                                if *rid == id && paths.contains(rp.trim_matches('/'))
                        )
                    })
                    .collect();
                for tile in targets {
                    if let Some(Location::Remote { path, .. }) =
                        self.tab_data_for(tile).map(|td| td.location.clone())
                    {
                        self.spawn_list_remote(tile, id.clone(), path);
                    }
                }
            }
            BgResult::TransferUpdate(snapshot) => {
                let completed = snapshot
                    .iter()
                    .filter(|transfer| {
                        transfer.kind == atlas_model::cloud_model::TransferKind::Upload
                            && transfer.state == atlas_model::cloud_model::TransferState::Done
                            && !self.state.cloud.transfers.iter().any(|old| {
                                old.id == transfer.id
                                    && old.state == atlas_model::cloud_model::TransferState::Done
                            })
                    })
                    .map(|transfer| transfer.local_path.clone())
                    .collect::<Vec<_>>();
                self.consume_completed_staged_paths(&completed);
                let any_active = snapshot.iter().any(|t| !t.state.is_terminal());
                self.state.cloud.transfers = snapshot;
                if any_active {
                    self.state.cloud.show_transfers = true;
                }
            }
            #[cfg(windows)]
            BgResult::WinFspProgress { done, total } => {
                // Re-use the rclone download progress indicator for WinFsp.
                let _ = self
                    .tx
                    .send(atlas_model::app_state::BgResult::CloudRcloneProgress { done, total });
            }
            #[cfg(windows)]
            BgResult::WinFspInstalled { retry_id } => {
                self.toast_info("WinFsp installed. Drive mounting is now available.".to_string());
                if let Some(id) = retry_id {
                    // Retry the mount that originally triggered the install prompt.
                    let _ = self.cloud_tx.send(CloudCmd::Mount { id });
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_sentinel_round_trips() {
        assert_eq!(
            parse_remote_sentinel(&remote_sentinel("gdrive", "")),
            Some(("gdrive".to_string(), String::new()))
        );
        assert_eq!(
            parse_remote_sentinel(&remote_sentinel("gdrive", "docs/sub/")),
            Some(("gdrive".to_string(), "docs/sub".to_string()))
        );
        // Real local paths never parse as sentinels.
        assert_eq!(parse_remote_sentinel(std::path::Path::new(r"C:\tmp")), None);
    }
}
