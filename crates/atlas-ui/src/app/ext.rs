//! App-side glue for the extension system: routes `ExtAction`s, runs commands,
//! and applies `BgResult::ExtCommandDone`.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::app::App;
use atlas_model::ext_state::PendingCommand;

/// Bundled media-thumbnailer extension (video + PSD/raster grid thumbnails).
pub const MEDIA_THUMBS_MANIFEST: &str =
    include_str!("../../../../extensions/media-thumbs/extension.toml");
pub const MEDIA_THUMBS_ICON: &[u8] = include_bytes!("../../../../extensions/media-thumbs/icon.svg");

/// Bundled icon-manager extension (Open VSX VSCode icon themes).
pub const ICON_MANAGER_MANIFEST: &str =
    include_str!("../../../../extensions/icon-manager/extension.toml");
pub const ICON_MANAGER_ICON: &[u8] = include_bytes!("../../../../extensions/icon-manager/icon.svg");

/// Hidden shared tool-provider extension (portable binaries by OS/arch).
pub const RUNTIME_TOOLS_MANIFEST: &str =
    include_str!("../../../../extensions/runtime-tools/extension.toml");

pub const CONVERSION_PANDOC_MANIFEST: &str =
    include_str!("../../../../extensions/conversion-pandoc/extension.toml");

pub const CONVERSION_OPUS_MANIFEST: &str =
    include_str!("../../../../extensions/conversion-opus/extension.toml");

/// Bundled cloud virtual drives extension.
pub const CLOUD_MANIFEST: &str = include_str!("../../../../extensions/cloud/extension.toml");
pub const CLOUD_ICON: &[u8] = include_bytes!("../../../../extensions/cloud/icon.svg");

/// UI-level extension actions.
#[derive(Debug, Clone)]
pub enum ExtAction {
    OpenManager(bool),
    /// Run a contributed command against the current selection.
    Run {
        ext_id: String,
        cmd_id: String,
    },
    /// Run the pending command after its prompts were filled in.
    ConfirmPending,
    CancelPending,
    SetEnabled {
        id: String,
        enabled: bool,
    },
    /// Pick a `.zip`/folder and install it.
    Install,
    Uninstall(String),
    /// Fetch the remote marketplace catalogue from R2.
    FetchRegistry,
    /// Download + install an extension from the marketplace.
    InstallRemote {
        id: String,
        download_url: String,
        sha256: String,
    },
}

impl App {
    pub(crate) fn reload_ext_registry(&mut self) {
        self.ext_registry = atlas_core::ext::Registry::load(&self.state.ext.enabled);
        // A selected file must be retried when its provider changes.
        self.state.preview.paths.clear();
        self.refresh_conversion_tools();
        #[cfg(feature = "provider-host")]
        if self.ext_registry.has_sidebar_section("cloud") {
            self.state.profiles.ensure_cloud_section();
        }
    }

    pub(crate) fn handle_ext_action(&mut self, action: ExtAction) {
        match action {
            ExtAction::OpenManager(show) => {
                if show {
                    self.reload_ext_registry();
                }
                self.state.ext.show_manager = show;
            }
            ExtAction::Run { ext_id, cmd_id } => self.start_ext_command(&ext_id, &cmd_id),
            ExtAction::ConfirmPending => {
                if let Some(p) = self.state.ext.pending.take() {
                    let answers = p
                        .answers
                        .iter()
                        .map(|(name, _, val)| (name.clone(), val.clone()))
                        .collect();
                    atlas_core::ext::runner::run(
                        &self.rt,
                        self.tx.clone(),
                        p.cmd,
                        p.paths,
                        answers,
                    );
                }
            }
            ExtAction::CancelPending => {
                self.state.ext.pending = None;
            }
            ExtAction::SetEnabled { id, enabled } => {
                #[cfg(feature = "provider-host")]
                if id == "cloud" && !enabled {
                    self.stop_cloud_extension(false);
                    return;
                }
                self.state.ext.enabled.insert(id, enabled);
                self.reload_ext_registry();
            }
            ExtAction::Install => self.install_extension(),
            ExtAction::Uninstall(id) => {
                #[cfg(feature = "provider-host")]
                if id == "cloud" {
                    self.stop_cloud_extension(true);
                    return;
                }
                if let Err(e) = atlas_core::ext::install::uninstall(&id) {
                    self.state.ext.last_error = Some(e.to_string());
                } else {
                    self.state.ext.enabled.remove(&id);
                    self.reload_ext_registry();
                }
            }
            ExtAction::FetchRegistry => {
                self.state.ext.remote_loading = true;
                self.state.ext.last_error = None;
                let tx = self.tx.clone();
                self.rt.spawn(async move {
                    let result = atlas_core::ext::marketplace::fetch_registry()
                        .await
                        .map_err(|e| e.to_string());

                    let _ = tx.send(atlas_model::app_state::BgResult::ExtRegistryFetched(result));
                });
            }
            ExtAction::InstallRemote {
                id,
                download_url,
                sha256,
            } => {
                self.state.ext.installing.insert(id.clone());
                self.state.ext.last_error = None;
                let tx = self.tx.clone();
                self.rt.spawn(async move {
                    let result = async {
                        let zip_path =
                            atlas_core::ext::marketplace::download_zip(&download_url, &sha256)
                                .await
                                .map_err(|e| e.to_string())?;
                        atlas_core::ext::install::install_zip(&zip_path)
                            .map_err(|e| e.to_string())?;
                        // Clean up temp zip.
                        let _ = std::fs::remove_file(&zip_path);
                        Ok::<(), String>(())
                    }
                    .await;
                    let _ = tx
                        .send(atlas_model::app_state::BgResult::ExtRemoteInstalled { id, result });
                });
            }
        }
    }

    #[cfg(feature = "provider-host")]
    fn stop_cloud_extension(&mut self, uninstall: bool) {
        let cloud = self.cloud_tx.clone();
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let (reply, done) = std::sync::mpsc::channel();
                cloud
                    .send(atlas_core::cloud::CloudCmd::StopProvider { reply })
                    .map_err(|error| error.to_string())?;
                done.recv_timeout(std::time::Duration::from_secs(330))
                    .map_err(|error| error.to_string())?
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);
            let _ = tx
                .send(atlas_model::app_state::BgResult::CloudProviderStopped { uninstall, result });
        });
    }

    #[cfg(feature = "provider-host")]
    pub(crate) fn complete_cloud_shutdown(&mut self, uninstall: bool, result: Result<(), String>) {
        if let Err(error) = result {
            self.state.ext.last_error = Some(format!(
                "Cloud shutdown could not be confirmed; the extension was kept: {error}"
            ));
            return;
        }
        self.state.cloud.ready = false;
        if uninstall {
            if let Err(error) = atlas_core::ext::install::uninstall("cloud") {
                self.state.ext.last_error = Some(error.to_string());
                return;
            }
            self.state.ext.enabled.remove("cloud");
        } else {
            self.state.ext.enabled.insert("cloud".into(), false);
        }
        self.reload_ext_registry();
    }

    /// Handle background results for the marketplace (called from `bg.rs`).
    pub(crate) fn handle_ext_registry_fetched(
        &mut self,
        result: Result<atlas_model::ext_model::RemoteRegistry, String>,
    ) {
        self.state.ext.remote_loading = false;
        match result {
            Ok(registry) => {
                let mut list: Vec<atlas_model::ext_model::RemoteExtension> =
                    registry.extensions.into_values().collect();
                list.sort_by(|a, b| a.name.cmp(&b.name));
                self.state.ext.remote = Some(list);
                self.state.ext.marketplace_visible = 40;
            }
            Err(e) => {
                self.state.ext.last_error = Some(format!("Marketplace: {e}"));
            }
        }
    }

    /// Handle background result for a remote-install (called from `bg.rs`).
    pub(crate) fn handle_ext_remote_installed(&mut self, id: String, result: Result<(), String>) {
        self.state.ext.installing.remove(&id);
        match result {
            Ok(()) => {
                self.state.ext.feedback =
                    Some(rust_i18n::t!("ext_installed", id = &id).to_string());
                self.reload_ext_registry();
            }
            Err(e) => {
                self.state.ext.last_error = Some(format!("{id}: {e}"));
            }
        }
    }

    /// Handle background result for a local file/folder install (called from `bg.rs`).
    pub(crate) fn handle_ext_local_installed(&mut self, result: Result<String, String>) {
        match result {
            Ok(id) => {
                self.state.ext.feedback =
                    Some(rust_i18n::t!("ext_installed", id = &id).to_string());
                self.reload_ext_registry();
            }
            Err(e) => {
                self.state.ext.last_error = Some(e);
            }
        }
    }

    /// Resolve a command + selection and either run it or open the prompt modal.
    fn start_ext_command(&mut self, ext_id: &str, cmd_id: &str) {
        let paths = self.state.selected_paths();
        self.start_ext_command_for(ext_id, cmd_id, paths);
    }

    /// Run a contributed command on explicit `paths` (the Actions ring passes
    /// the sources it was opened with, not whatever is selected now).
    pub(crate) fn start_ext_command_for(
        &mut self,
        ext_id: &str,
        cmd_id: &str,
        paths: Vec<std::path::PathBuf>,
    ) {
        tracing::info!(
            "start_ext_command called with ext_id={}, cmd_id={}",
            ext_id,
            cmd_id
        );
        let Some(cmd) = self.ext_registry.command(ext_id, cmd_id) else {
            tracing::error!(
                "Command not found in registry: ext_id={}, cmd_id={}",
                ext_id,
                cmd_id
            );
            return;
        };
        if paths.is_empty() && !cmd.command.accepts.any {
            tracing::warn!("Paths are empty and accepts.any is false. Returning early.");
            return;
        }
        if cmd.command.prompts.is_empty() {
            atlas_core::ext::runner::run(&self.rt, self.tx.clone(), cmd, paths, HashMap::new());
        } else {
            let answers = cmd
                .command
                .prompts
                .iter()
                .map(|p| (p.name.clone(), p.label.clone(), p.default.clone()))
                .collect();
            self.state.ext.pending = Some(PendingCommand {
                cmd,
                paths,
                answers,
            });
        }
    }

    fn install_extension(&mut self) {
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            // pick_file/pick_folder are blocking COM calls — spawn_blocking keeps
            // them off the GPUI render thread so the window stays responsive.
            let outcome = tokio::task::spawn_blocking(|| -> Option<Result<String, String>> {
                let path = rfd::FileDialog::new()
                    .add_filter("Extension", &["zip"])
                    .pick_file()
                    .or_else(|| rfd::FileDialog::new().pick_folder())?; // None = cancelled
                Some(if path.is_dir() {
                    atlas_core::ext::install::install_folder(&path).map_err(|e| e.to_string())
                } else {
                    atlas_core::ext::install::install_zip(&path).map_err(|e| e.to_string())
                })
            })
            .await;
            if let Ok(Some(result)) = outcome {
                let _ = tx.send(atlas_model::app_state::BgResult::ExtLocalInstalled(result));
            }
        });
    }

    pub(crate) fn handle_ext_bg(
        &mut self,
        ext_id: String,
        error: Option<String>,
        dirs: Vec<PathBuf>,
    ) {
        let failed = error.is_some();
        if let Some(e) = error {
            self.state.ext.last_error = Some(format!("{ext_id}: {e}"));
            tracing::warn!("extension {ext_id} command failed: {e}");
        }

        // If the extension generated a mapping file (like an icon theme), cache it.
        let mappings_path = atlas_core::util::config::data_dir()
            .join("extensions")
            .join(&ext_id)
            .join("theme_mappings.json");

        let mut icons_changed = false;
        if ext_id == "icon-manager" {
            crate::ui::settings_palette::refresh_icon_inventory();
        }
        // A failed command (e.g. a download error) must not wipe the user's icons.
        if ext_id == "icon-manager" && !failed && !mappings_path.exists() {
            // Reset to default Liquid Glass icons.
            self.persist_icon_selection(std::collections::HashMap::new());
            icons_changed = true;
            self.state.ext.feedback = Some(rust_i18n::t!("icons_reset_default").to_string());
        } else if mappings_path.exists() {
            #[allow(clippy::disallowed_methods)] // reason: small JSON read on command completion
            let contents = std::fs::read_to_string(&mappings_path);
            if let Ok(contents) = contents {
                if let Ok(map) =
                    serde_json::from_str::<std::collections::HashMap<String, String>>(&contents)
                {
                    // Persist the selection on the active profile (official
                    // per-profile store) and apply it to the live icon cache.
                    self.persist_icon_selection(map);
                    icons_changed = true;
                    let message = rust_i18n::t!("icons_theme_activated").to_string();
                    self.toast_info(message.clone());
                    self.state.ext.feedback = Some(message);
                }
            }
        }
        // Re-list any directory tab whose contents may have changed.
        self.relist_panes(|p| icons_changed || dirs.iter().any(|d| d.as_path() == p));
    }

    /// Re-list every directory pane whose path passes `filter`, re-warming the
    /// icon cache (icons are loaded per listing).
    pub(crate) fn relist_panes(&mut self, filter: impl Fn(&std::path::Path) -> bool) {
        let targets: Vec<(atlas_model::layout_tree::TileId, PathBuf, _, _)> = self
            .state
            .all_pane_tile_ids()
            .into_iter()
            .filter_map(|tile| {
                let td = self.tab_data_for(tile)?;
                td.location
                    .fs_dir()
                    .filter(|p| filter(p))
                    .map(|p| p.to_path_buf())
                    .map(|path| (tile, path, td.sort_col.clone(), td.sort_dir.clone()))
            })
            .collect();
        for (tile, path, col, dir) in targets {
            self.spawn_list_dir(tile, path, col, dir);
        }
    }
}
