//! Native SMB, the way File Explorer does it: Windows' own "Map Network Drive"
//! dialog, its credential prompt when a share refuses access, and
//! disconnecting mapped drives. Passwords never pass through Atlas; Windows
//! keeps them in Credential Manager when the user asks it to.
use crate::app::App;
use atlas_model::app_state::{BgResult, NetworkOutcome};
use atlas_model::layout_tree::{Tile, TileId};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A share that just accepted credentials but still refuses the listing is an
/// ACL problem, not a login problem: don't prompt again in a loop.
const REPROMPT_AFTER: Duration = Duration::from_secs(20);

#[derive(Default)]
pub(crate) struct NetworkUi {
    /// Target (`\\server\share`) of the credential prompt currently open.
    prompt_open: Option<String>,
    /// When each target last showed the credential prompt.
    prompted: std::collections::HashMap<String, Instant>,
}

fn failure(error: &std::io::Error) -> NetworkOutcome {
    NetworkOutcome::Failed {
        code: error.raw_os_error(),
        message: error.to_string(),
    }
}

/// User-facing text for a failed SMB request.
pub(crate) fn failure_text(code: Option<i32>, message: &str) -> String {
    match code {
        Some(1219) => rust_i18n::t!("network_err_conflict").into_owned(),
        Some(2401 | 2404) => rust_i18n::t!("network_err_open_files").into_owned(),
        Some(53 | 67 | 1231 | 1203) => rust_i18n::t!("network_err_unreachable").into_owned(),
        Some(5 | 86 | 1326) => rust_i18n::t!("network_err_denied").into_owned(),
        Some(85) => rust_i18n::t!("network_err_letter_in_use").into_owned(),
        _ => rust_i18n::t!("network_err_generic", detail = message.trim()).into_owned(),
    }
}

impl App {
    fn owner_window(&self) -> isize {
        self.hwnd.unwrap_or(0)
    }

    /// Windows' "Map Network Drive" dialog (drive letter, reconnect at sign-in,
    /// different credentials). `remote` pre-fills the folder, e.g. when the
    /// user maps the share they are browsing. The focused pane opens the new
    /// drive on success.
    pub(crate) fn map_network_drive(&mut self, remote: Option<PathBuf>) {
        let owner = self.owner_window();
        let tx = self.tx.clone();
        let tab = self.state.focused_tile;
        let remote = remote.and_then(|path| {
            atlas_core::fs::network::share_root(&path)
                .and_then(|root| root.to_str().map(str::to_owned))
        });
        std::thread::spawn(move || {
            let result = atlas_win_space::network::with_sta(|| {
                atlas_win_space::network::map_drive_dialog(owner, remote.as_deref())
            });
            let (navigate, outcome) = match result {
                Ok(Some(letter)) => (
                    Some(PathBuf::from(format!("{letter}\\"))),
                    NetworkOutcome::Mapped(letter),
                ),
                Ok(None) => (None, NetworkOutcome::Cancelled),
                Err(error) => (None, failure(&error)),
            };
            let _ = tx.send(BgResult::Network {
                tab: Some(tab),
                target: remote.unwrap_or_default(),
                navigate,
                outcome,
            });
        });
    }

    /// Disconnect a mapped drive (`Z:\`) or a `\\server\share` connection.
    pub(crate) fn disconnect_network_drive(&mut self, path: PathBuf) {
        let tx = self.tx.clone();
        let name = path
            .to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_owned();
        std::thread::spawn(move || {
            let outcome = match atlas_win_space::network::disconnect(&name, false) {
                Ok(()) => NetworkOutcome::Disconnected(name.clone()),
                Err(error) => failure(&error),
            };
            let _ = tx.send(BgResult::Network {
                tab: None,
                target: name,
                navigate: None,
                outcome,
            });
        });
    }

    /// A listing of `path` in `tab` was refused. Shows the Windows credential
    /// prompt (once per share at a time) and reloads the pane when it is
    /// accepted. False when no prompt was started, so the caller shows the
    /// error instead.
    pub(crate) fn request_network_credentials(&mut self, tab: TileId, path: &Path) -> bool {
        // A remembered drive letter missing from this session is reconnected
        // under the same letter; Windows asks for credentials only if needed.
        let reconnect = atlas_core::fs::network::reconnect_target(path);
        let local = reconnect.as_ref().map(|(letter, _)| letter.clone());
        let Some(target) = reconnect
            .map(|(_, remote)| remote)
            .or_else(|| atlas_core::fs::network::connection_target(path))
        else {
            return false;
        };
        if self.network.prompt_open.is_some() {
            // Another pane already asked; it reloads every pane of the share.
            return self.network.prompt_open.as_deref() == Some(target.as_str());
        }
        if self
            .network
            .prompted
            .get(&target)
            .is_some_and(|at| at.elapsed() < REPROMPT_AFTER)
        {
            return false;
        }
        self.network.prompt_open = Some(target.clone());
        self.network.prompted.insert(target.clone(), Instant::now());
        let owner = self.owner_window();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = atlas_win_space::network::with_sta(|| {
                let prompt = local.is_none();
                atlas_win_space::network::connect(owner, &target, local.as_deref(), prompt, false)
            });
            let outcome = match result {
                Ok(()) => NetworkOutcome::Connected,
                Err(error) if error.raw_os_error() == Some(1223) => NetworkOutcome::Cancelled,
                Err(error) => failure(&error),
            };
            let _ = tx.send(BgResult::Network {
                tab: Some(tab),
                target,
                navigate: None,
                outcome,
            });
        });
        true
    }

    /// Panes showing anything on the share (or server) `target` refers to.
    fn panes_on(&self, target: &str) -> Vec<TileId> {
        let target = Path::new(target);
        let wanted = atlas_core::fs::network::unc_parts(target);
        self.state
            .pane_tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| match tile {
                Tile::Pane(pane) => pane.location.fs_dir().map(|dir| (*id, dir.to_path_buf())),
                _ => None,
            })
            .filter(|(_, dir)| {
                let here = atlas_core::fs::network::unc_parts(dir);
                match (&wanted, here) {
                    // `\\server\IPC$` unlocks the share list of `\\server`.
                    (Some((server, Some(share))), Some((s, sh))) => {
                        server.eq_ignore_ascii_case(&s)
                            && (share.eq_ignore_ascii_case("IPC$")
                                || sh.is_some_and(|sh| sh.eq_ignore_ascii_case(share)))
                    }
                    _ => {
                        // Mapped drive: compare the share behind the letter.
                        atlas_core::fs::network::connection_target(dir)
                            .is_some_and(|t| t.eq_ignore_ascii_case(&target.to_string_lossy()))
                    }
                }
            })
            .map(|(id, _)| id)
            .collect()
    }

    pub(crate) fn apply_network_result(
        &mut self,
        tab: Option<TileId>,
        target: String,
        navigate: Option<PathBuf>,
        outcome: NetworkOutcome,
    ) {
        if self.network.prompt_open.as_deref() == Some(target.as_str()) {
            self.network.prompt_open = None;
        }
        atlas_core::fs::mounts::invalidate_drive_cache();
        match outcome {
            NetworkOutcome::Connected => {
                let mut panes = self.panes_on(&target);
                panes.extend(tab.filter(|tab| !panes.contains(tab)));
                self.refresh_tabs(panes);
            }
            NetworkOutcome::Mapped(letter) => {
                self.toast_info(rust_i18n::t!("network_mapped", drive = letter).into_owned());
                if let (Some(tab), Some(path)) = (tab, navigate) {
                    self.navigate(tab, path);
                }
            }
            NetworkOutcome::Disconnected(name) => {
                self.toast_info(rust_i18n::t!("network_disconnected", drive = name).into_owned());
                // Panes left on the removed drive would only show errors.
                let home = dirs::home_dir();
                let stale: Vec<TileId> = self
                    .state
                    .pane_tree
                    .tiles
                    .iter()
                    .filter_map(|(id, tile)| match tile {
                        Tile::Pane(pane) => pane
                            .location
                            .fs_dir()
                            .filter(|dir| {
                                dir.to_string_lossy()
                                    .to_ascii_lowercase()
                                    .starts_with(&name.to_ascii_lowercase())
                            })
                            .map(|_| *id),
                        _ => None,
                    })
                    .collect();
                if let Some(home) = home {
                    for id in stale {
                        self.navigate(id, home.clone());
                    }
                }
            }
            NetworkOutcome::Cancelled => {
                if let Some(tab) = tab.filter(|_| !target.is_empty()) {
                    self.set_network_error(tab, rust_i18n::t!("network_err_denied").into_owned());
                }
            }
            NetworkOutcome::Failed { code, message } => {
                let text = failure_text(code, &message);
                if let Some(tab) = tab.filter(|_| !target.is_empty() && navigate.is_none()) {
                    self.set_network_error(tab, text.clone());
                }
                self.toast_error(text);
            }
        }
    }

    fn set_network_error(&mut self, tab: TileId, text: String) {
        if let Some(state) = self.state.tab_states.get_mut(&tab) {
            if matches!(state.load_state, atlas_model::tab::LoadState::Loading) {
                state.load_state = atlas_model::tab::LoadState::Error(text);
            }
        }
    }
}
