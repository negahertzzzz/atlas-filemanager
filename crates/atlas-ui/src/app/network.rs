//! Native SMB, the way File Explorer does it: Windows' own "Map Network Drive"
//! dialog, its credential prompt when a share refuses access, and
//! disconnecting mapped drives.
//!
//! Sign-ins are capped: one attempt per answer in the credential dialog and at
//! most two refused attempts per server before `network_guard` pauses it, so
//! Atlas never produces the bursts of failed logons that lock accounts or
//! alert security monitoring.
use crate::app::App;
use atlas_core::fs::network_guard as guard;
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

fn paused(pause: Duration) -> NetworkOutcome {
    NetworkOutcome::Failed {
        code: None,
        message: guard::paused_error(pause),
    }
}

/// Counts a refused attempt against `server`; `Some` when it is now paused.
fn refused(server: Option<&str>, error: &std::io::Error) -> Option<NetworkOutcome> {
    let code = error.raw_os_error()?;
    if !guard::is_sign_in_failure(code) {
        return None;
    }
    server.and_then(guard::record_failure).map(paused)
}

/// Signs in to `target` with at most two attempts: the credentials Windows
/// has saved (only for a remembered drive letter), then exactly one attempt
/// with what the user types. The dialog itself never contacts the server.
fn sign_in(
    owner: isize,
    target: &str,
    local: Option<&str>,
    server: Option<&str>,
) -> NetworkOutcome {
    use atlas_win_space::network as net;
    let mut last_refusal = 0;
    if local.is_some() {
        match net::connect(target, local, None) {
            Ok(()) => {
                if let Some(server) = server {
                    guard::record_success(server);
                }
                return NetworkOutcome::Connected;
            }
            Err(error) => {
                if let Some(outcome) = refused(server, &error) {
                    return outcome;
                }
                let code = error.raw_os_error().unwrap_or_default();
                if !guard::is_sign_in_failure(code) && !net::credentials_may_help(code) {
                    return failure(&error);
                }
                last_refusal = code as u32;
            }
        }
    }
    let credentials = match net::prompt_credentials(owner, target, last_refusal) {
        Ok(Some(credentials)) => credentials,
        Ok(None) => return NetworkOutcome::Cancelled,
        Err(error) => return failure(&error),
    };
    match net::connect(target, local, Some(&credentials)) {
        Ok(()) => {
            if let Some(server) = server {
                guard::record_success(server);
                if credentials.save {
                    if let Err(error) = net::remember_credentials(server, &credentials) {
                        tracing::warn!(%error, "cannot remember network credentials");
                    }
                }
            }
            NetworkOutcome::Connected
        }
        Err(error) => refused(server, &error).unwrap_or_else(|| failure(&error)),
    }
}

/// User-facing text for a failed SMB request.
pub(crate) fn failure_text(code: Option<i32>, message: &str) -> String {
    if let Some(minutes) = guard::parse_paused(message) {
        return paused_text(minutes);
    }
    match code {
        Some(1219) => rust_i18n::t!("network_err_conflict").into_owned(),
        Some(2401 | 2404) => rust_i18n::t!("network_err_open_files").into_owned(),
        Some(53 | 67 | 1231 | 1203) => rust_i18n::t!("network_err_unreachable").into_owned(),
        Some(5 | 86 | 1326) => rust_i18n::t!("network_err_denied").into_owned(),
        Some(85) => rust_i18n::t!("network_err_letter_in_use").into_owned(),
        _ => rust_i18n::t!("network_err_generic", detail = message.trim()).into_owned(),
    }
}

/// "Paused after two refused sign-ins" with the minutes left.
pub(crate) fn paused_text(minutes: u64) -> String {
    rust_i18n::t!(
        "network_signin_paused",
        attempts = guard::MAX_FAILED_SIGN_INS,
        minutes = minutes
    )
    .into_owned()
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
        let server = remote
            .as_deref()
            .and_then(|remote| atlas_core::fs::network::unc_parts(Path::new(remote)))
            .map(|(server, _)| server);
        if let Some(pause) = server.as_deref().and_then(guard::paused_for) {
            self.toast_error(paused_text(pause.as_secs().div_ceil(60).max(1)));
            return;
        }
        std::thread::spawn(move || {
            let result = atlas_win_space::network::with_sta(|| {
                atlas_win_space::network::map_drive_dialog(owner, remote.as_deref())
            });
            let (navigate, outcome) = match result {
                Ok(Some(letter)) => {
                    if let Some(server) = &server {
                        guard::record_success(server);
                    }
                    (
                        Some(PathBuf::from(format!("{letter}\\"))),
                        NetworkOutcome::Mapped(letter),
                    )
                }
                Ok(None) => (None, NetworkOutcome::Cancelled),
                Err(error) => (
                    None,
                    refused(server.as_deref(), &error).unwrap_or_else(|| failure(&error)),
                ),
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
        let server =
            atlas_core::fs::network::unc_parts(Path::new(&target)).map(|(server, _)| server);
        if server.as_deref().and_then(guard::paused_for).is_some() {
            return false;
        }
        self.network.prompt_open = Some(target.clone());
        self.network.prompted.insert(target.clone(), Instant::now());
        let owner = self.owner_window();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = atlas_win_space::network::with_sta(|| {
                sign_in(owner, &target, local.as_deref(), server.as_deref())
            });
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
