//! Background cloud worker: owns the rclone daemon + provider and serializes all
//! cloud operations, replying through the shared `BgResult` channel.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::json;

use crate::cloud::provider::{CloudProvider, ProgressSink, RemoteEntry};
use crate::cloud::remote::{CloudRemote, RemoteId, RemoteKind, RemoteStatus};
use crate::cloud::sidecar_provider::SidecarProvider;
use crate::cloud::transfer::{Transfer, TransferId, TransferKind, TransferState};
use atlas_model::app_state::BgResult;
use atlas_model::file_entry::{FileEntry, FileKind, SortColumn, SortDir};
use atlas_model::pane::FolderOrder;
use atlas_model::pane::TabId;
use super::control::{TransferControl, TransferCancelled, OutcomeUnknown};

const MAX_CONCURRENT: usize = 4;
const MAX_PENDING_TRANSFERS: usize = 4096;
const MAX_COMPLETED_HISTORY: usize = 256;

/// Commands the UI sends to the cloud worker.
pub enum CloudCmd {
    StopProvider {
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
    /// Create/overwrite a remote's rclone config. `secret` is the password/key
    /// passphrase (obscured by rclone, never stored by us).
    AddRemote {
        remote: CloudRemote,
        secret: String,
    },
    RemoveRemote(RemoteId),
    /// Pre-warm the rclone daemon so it is ready before the user clicks.
    Warmup,
    /// Download and install WinFsp (Windows only). After success, optionally
    /// mount `retry_id` if the user originally requested a mount.
    #[cfg(windows)]
    InstallWinFsp {
        retry_id: Option<RemoteId>,
    },
    /// List a remote directory; result is delivered as `BgResult::DirListed` so
    /// it renders through the normal file views.
    ListRemote {
        tab: TabId,
        op_id: u64,
        id: RemoteId,
        path: String,
        sort_col: SortColumn,
        sort_dir: SortDir,
        folder_order: FolderOrder,
    },
    CancelList { tab: TabId },
    CancelPreview { tab: TabId },
    PreviewRemote { tab: TabId, id: RemoteId, path: String, key: PathBuf, size: Option<u64>, is_dir: bool },
    Mount {
        id: RemoteId,
    },
    Unmount {
        id: RemoteId,
    },
    /// Delete one or more remote paths (files or dirs).
    DeleteRemote {
        id: RemoteId,
        /// Remote-relative paths to delete; each carries whether it is a dir.
        entries: Vec<(String, bool)>,
        /// Parent dir path to re-list after the operation.
        parent: String,
    },
    /// Create a directory on the remote at `path`.
    MkdirRemote {
        id: RemoteId,
        path: String,
        /// Parent dir to re-list after creation.
        parent: String,
    },
    /// Server-side copy/move within the same remote (no local download).
    CopyRemoteEntries {
        id: RemoteId,
        /// `(src_path, dst_path, is_dir)` triples.
        entries: Vec<(String, String, bool)>,
        /// `true` → move (delete source after copy).
        is_move: bool,
        /// Dir to re-list after the operation.
        parent: String,
    },
    /// Rename/move a single remote entry.
    RenameRemoteEntry {
        id: RemoteId,
        from: String,
        to: String,
        /// Dir to re-list after the rename.
        parent: String,
    },
    EnqueueDownload {
        id: RemoteId,
        remote_path: String,
        local_path: PathBuf,
        label: String,
        open_on_done: bool,
    },
    /// Upload a local file into a remote (drag/paste into an Access-mode tab).
    EnqueueUpload {
        id: RemoteId,
        local_path: PathBuf,
        remote_path: String,
        label: String,
        /// Move-semantics: trash the local source after a successful upload.
        delete_source: bool,
    },
    EnqueueRemoteCopy { source_id: RemoteId, source: String, destination_id: RemoteId, destination: String, label: String },
    PauseTransfer(TransferId),
    ResumeTransfer(TransferId),
    CancelTransfer(TransferId),
    RetryTransfer(TransferId),
}

/// Internal progress/result events from spawned transfer tasks.
enum Ev {
    Configured { remote: CloudRemote, result: Result<(), String> },
    Progress {
        id: TransferId,
        attempt: u32,
        done: u64,
        total: u64,
    },
    Done { id: TransferId, attempt: u32 },
    Failed {
        id: TransferId,
        err: String,
        attempt: u32,
        cancelled: bool,
        unknown: bool,
    },
}

/// Spawn the cloud worker. Returns the command channel.
pub fn spawn(
    rt: &tokio::runtime::Handle,
    bg_tx: flume::Sender<BgResult>,
) -> flume::Sender<CloudCmd> {
    let (cmd_tx, cmd_rx) = flume::unbounded::<CloudCmd>();
    let (ev_tx, ev_rx) = flume::unbounded::<Ev>();
    let handle = rt.clone();
    rt.spawn(async move {
        let recovery = super::journal::TransferJournal::open(&crate::util::config::config_dir().join("cloud-transfers.sqlite"))
            .and_then(|journal| { let transfers = journal.load()?; Ok((journal, transfers)) });
        let (journal, transfers, journal_failed) = match recovery {
            Ok((journal, transfers)) => (Some(journal), transfers, false),
            Err(error) => {
                let _ = bg_tx.send(BgResult::CloudError(format!("Cannot recover the transfer journal; browsing remains available, transfers are stopped: {error}")));
                (None, Vec::new(), true)
            }
        };
        let next_id = transfers.iter().map(|transfer| transfer.id).max().unwrap_or(0).saturating_add(1);
        let mut worker = Worker {
            rt: handle,
            bg_tx,
            ev_tx,
            provider: None,
            transfers,
            journal,
            journal_failed,
            running: HashMap::new(),
            listings: HashMap::new(),
            progress_dirty: false,
            previews: crate::cloud::preview::PreviewQueue::default(),
            configuring: std::collections::HashSet::new(),
            next_id,
        };
        worker.emit_transfers();
        let mut progress_tick = tokio::time::interval(Duration::from_millis(100));
        progress_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                cmd = cmd_rx.recv_async() => match cmd {
                    Ok(cmd) => worker.handle_cmd(cmd).await,
                    Err(_) => break, // all senders dropped → app exiting
                },
                ev = ev_rx.recv_async() => {
                    if let Ok(ev) = ev { worker.handle_ev(ev); }
                },
                _ = progress_tick.tick() => {
                    worker.reap_finished_tasks().await;
                    if worker.progress_dirty {
                        worker.emit_transfers();
                        worker.progress_dirty = false;
                    }
                },
            }
            worker.pump_queue();
        }
        let _ = worker.stop_provider().await;
    });
    cmd_tx
}

struct Worker {
    rt: tokio::runtime::Handle,
    bg_tx: flume::Sender<BgResult>,
    ev_tx: flume::Sender<Ev>,
    provider: Option<Arc<dyn CloudProvider>>,
    transfers: Vec<Transfer>,
    running: HashMap<TransferId, RunningTransfer>,
    listings: HashMap<TabId, tokio::task::JoinHandle<()>>,
    progress_dirty: bool,
    previews: super::preview::PreviewQueue,
    configuring: std::collections::HashSet<RemoteId>,
    next_id: TransferId,
    journal: Option<super::journal::TransferJournal>,
    journal_failed: bool,
}

struct RunningTransfer {
    task: tokio::task::JoinHandle<()>,
    control: TransferControl,
}

impl Worker {
    /// Calls `ensure_ready()`; on error sends `on_err(e)` through the bg channel and returns None.
    async fn require_ready(
        &mut self,
        on_err: impl FnOnce(anyhow::Error) -> BgResult,
    ) -> Option<Arc<dyn CloudProvider>> {
        match self.ensure_ready().await {
            Ok(p) => Some(p),
            Err(e) => {
                let _ = self.bg_tx.send(on_err(e));
                None
            }
        }
    }

    /// Start the installed Cloud provider extension on first use.
    async fn ensure_ready(&mut self) -> anyhow::Result<Arc<dyn CloudProvider>> {
        if let Some(p) = &self.provider {
            return Ok(p.clone());
        }
        let startup_bg = self.bg_tx.clone();
        let provider: Arc<dyn CloudProvider> = tokio::task::spawn_blocking(move || {
            let (directory, sidecar) = super::installation::prepare()?;
            let progress: ProgressSink = Arc::new(move |done, total| {
                let _ = startup_bg.send(BgResult::CloudRcloneProgress { done, total });
            });
            let provider: Arc<dyn CloudProvider> = Arc::new(SidecarProvider::spawn_with_progress(&directory, &sidecar.command, &sidecar.args, Some(progress))?);
            anyhow::Ok(provider)
        }).await?.map_err(|error| anyhow::anyhow!("Cloud could not start: {error:#}"))?;
        self.provider = Some(provider.clone());
        let _ = self.bg_tx.send(BgResult::CloudReady);
        Ok(provider)
    }

    async fn handle_cmd(&mut self, cmd: CloudCmd) {
        if matches!(&cmd, CloudCmd::EnqueueDownload { .. } | CloudCmd::EnqueueUpload { .. } | CloudCmd::EnqueueRemoteCopy { .. })
            && self.transfers.iter().filter(|transfer| !transfer.state.is_terminal() || matches!(transfer.state, TransferState::Interrupted(_))).count() >= MAX_PENDING_TRANSFERS {
            let _ = self.bg_tx.send(BgResult::CloudError("The cloud queue is full (4096 jobs). Wait for transfers to finish before adding more.".into()));
            return;
        }
        match cmd {
            CloudCmd::StopProvider { reply } => {
                let _ = reply.send(self.stop_provider().await.map_err(|error| error.to_string()));
            }
            CloudCmd::Warmup => {
                if let Err(error) = self.ensure_ready().await { let _ = self.bg_tx.send(BgResult::CloudError(error.to_string())); }
            }
            #[cfg(windows)]
            CloudCmd::InstallWinFsp { retry_id } => {
                self.install_winfsp(retry_id).await;
            }
            CloudCmd::AddRemote { remote, secret } => self.add_remote(remote, secret).await,
            CloudCmd::RemoveRemote(id) => self.remove_remote(id).await,
            CloudCmd::ListRemote {
                tab,
                op_id,
                id,
                path,
                sort_col, sort_dir, folder_order,
            } => {
                self.list_remote(tab, op_id, id, path, sort_col, sort_dir, folder_order).await;
            }
            CloudCmd::CancelList { tab } => {
                if let Some(task) = self.listings.remove(&tab) { task.abort(); }
                self.previews.cancel(tab);
            }
            CloudCmd::CancelPreview { tab } => self.previews.cancel(tab),
            CloudCmd::PreviewRemote { tab, id, path, key, size, is_dir } => {
                match self.ensure_ready().await {
                    Ok(provider) => self.previews.request(&self.rt, provider, self.bg_tx.clone(), tab, id, path, key, size, is_dir),
                    Err(error) => { let _ = self.bg_tx.send(BgResult::PreviewReady { tab, path: key, result: atlas_model::preview_cache::PreviewResult::Error(error.to_string()) }); }
                }
            }
            CloudCmd::Mount { id } => self.mount(id).await,
            CloudCmd::Unmount { id } => self.unmount(id).await,
            CloudCmd::DeleteRemote {
                id,
                entries,
                parent,
            } => {
                self.delete_remote(id, entries, parent).await;
            }
            CloudCmd::MkdirRemote { id, path, parent } => {
                self.mkdir_remote(id, path, parent).await;
            }
            CloudCmd::RenameRemoteEntry {
                id,
                from,
                to,
                parent,
            } => {
                self.rename_remote_entry(id, from, to, parent).await;
            }
            CloudCmd::CopyRemoteEntries {
                id,
                entries,
                is_move,
                parent,
            } => {
                self.copy_remote_entries(id, entries, is_move, parent).await;
            }
            CloudCmd::EnqueueDownload {
                id,
                remote_path,
                local_path,
                label,
                open_on_done,
            } => {
                let error = self.ensure_ready().await.err().map(|error| error.to_string());
                let t = Transfer {
                    id: self.alloc_id(),
                    kind: TransferKind::Download,
                    label,
                    remote_id: id,
                    remote_path,
                    local_path,
                    state: error.map_or(TransferState::Queued, TransferState::Failed),
                    bytes_done: 0,
                    bytes_total: 0,
                    retries: 0,
                    open_on_done,
                    delete_source: false,
                    source: None,
                };
                self.mutate_transfers(|ts| ts.push(t));
            }
            CloudCmd::EnqueueUpload {
                id,
                local_path,
                remote_path,
                label,
                delete_source,
            } => {
                let error = if delete_source {
                    Some("Moving local files to cloud requires a stable source snapshot; copy the files instead. The source was kept unchanged.".into())
                } else { self.ensure_ready().await.err().map(|error| error.to_string()) };
                let t = Transfer {
                    id: self.alloc_id(),
                    kind: TransferKind::Upload,
                    label,
                    remote_id: id,
                    remote_path,
                    local_path,
                    state: error.map_or(TransferState::Queued, TransferState::Failed),
                    bytes_done: 0,
                    bytes_total: 0,
                    retries: 0,
                    open_on_done: false,
                    delete_source,
                    source: None,
                };
                self.mutate_transfers(|ts| ts.push(t));
            }
            CloudCmd::EnqueueRemoteCopy { source_id, source, destination_id, destination, label } => {
                let error = self.ensure_ready().await.err().map(|error| error.to_string());
                let transfer = Transfer {
                    id: self.alloc_id(), kind: TransferKind::RemoteCopy, label,
                    remote_id: destination_id, remote_path: destination, local_path: PathBuf::new(),
                    state: error.map_or(TransferState::Queued, TransferState::Failed), bytes_done: 0, bytes_total: 0, retries: 0,
                    open_on_done: false, delete_source: false,
                    source: Some(atlas_model::cloud_model::RemoteSource { id: source_id, path: source }),
                };
                self.mutate_transfers(|transfers| transfers.push(transfer));
            }
            CloudCmd::PauseTransfer(id) => {
                if let Some(running) = self.running.get(&id) { running.control.cancel(); }
                self.set_state(id, |s| match s {
                    TransferState::Queued => *s = TransferState::Paused,
                    TransferState::Running => *s = TransferState::Pausing,
                    _ => {},
                });
            }
            CloudCmd::ResumeTransfer(id) => {
                if let Err(error) = self.ensure_ready().await {
                    self.set_state(id, |_| {});
                    let _ = self.bg_tx.send(BgResult::CloudError(error.to_string()));
                    return;
                }
                self.mutate_transfers(|transfers| {
                if let Some(transfer) = transfers.iter_mut().find(|transfer| transfer.id == id && transfer.state == TransferState::Paused) {
                    transfer.state = TransferState::Queued;
                    transfer.retries = transfer.retries.saturating_add(1);
                    transfer.bytes_done = 0;
                }
                });
            }
            CloudCmd::RetryTransfer(id) => {
                if let Err(error) = self.ensure_ready().await {
                    let _ = self.bg_tx.send(BgResult::CloudError(error.to_string()));
                    return;
                }
                self.mutate_transfers(|ts| {
                if let Some(t) = ts.iter_mut().find(|t| t.id == id) {
                    if matches!(t.state, TransferState::Failed(_)) {
                        t.state = TransferState::Queued;
                        t.bytes_done = 0;
                        t.retries += 1;
                    }
                }
                });
            }
            CloudCmd::CancelTransfer(id) => {
                if let Some(running) = self.running.get(&id) { running.control.cancel(); }
                self.set_state(id, |state| match state {
                    TransferState::Running | TransferState::Pausing => *state = TransferState::Cancelling,
                    TransferState::Queued | TransferState::Paused => *state = TransferState::Cancelled,
                    _ => {},
                });
            }
        }
    }

    const fn alloc_id(&mut self) -> TransferId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn set_state(&mut self, id: TransferId, f: impl Fn(&mut TransferState)) {
        if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
            f(&mut t.state);
        }
        self.save_transfers();
        self.emit_transfers();
    }

    async fn add_remote(&mut self, remote: CloudRemote, secret: String) {
        let remote_id = remote.id.clone();
        let Some(provider) = self
            .require_ready(|e| BgResult::RemoteConfigFailed { id: remote_id, error: e.to_string() })
            .await
        else {
            return;
        };
        // Build backend params from the remote kind + secret and let the provider
        // persist them (passwords are obscured by the backend, never stored by us).
        let id = remote.id.clone();
        let is_oauth = remote.kind.is_oauth();
        let params = match &remote.kind {
            RemoteKind::Sftp { host, port, user }
            | RemoteKind::Ftp { host, port, user } => json!({
                "host": host, "port": port.to_string(), "user": user, "pass": secret,
            }),
            RemoteKind::Smb { host, port, user, domain } => {
                let mut params = json!({
                    "host": host, "port": port.to_string(), "user": user, "pass": secret,
                });
                if !domain.is_empty() {
                    params["domain"] = json!(domain);
                }
                params
            }
            RemoteKind::WebDav { url, vendor, user } => json!({
                "url": url, "vendor": vendor, "user": user, "pass": secret,
            }),
            // OAuth backends take no secret; rclone's built-in client id is used.
            RemoteKind::GoogleDrive => json!({ "scope": "drive" }),
            RemoteKind::OneDrive => json!({}),
        };
        let backend = remote.kind.rclone_type().to_string();
        if !self.configuring.insert(id.clone()) { return; }
        let events = self.ev_tx.clone();
        self.rt.spawn(async move {
            let result = if is_oauth { provider.configure_oauth(&id, &backend, params).await }
                else { provider.configure(&id, &backend, params).await };
            let _ = events.send(Ev::Configured { remote, result: result.map_err(|error| error.to_string()) });
        });
    }

    /// Fetch storage usage in the background and report it (best-effort).
    fn fetch_quota(&self, provider: Arc<dyn CloudProvider>, id: RemoteId) {
        let bg = self.bg_tx.clone();
        self.rt.spawn(async move {
            if let Ok((used, total)) = provider.about(&id).await {
                if total > 0 || used > 0 {
                    let _ = bg.send(BgResult::RemoteQuota { id, used, total });
                }
            }
        });
    }

    async fn remove_remote(&mut self, id: RemoteId) {
        if self.configuring.contains(&id) {
            let _ = self.bg_tx.send(BgResult::CloudError("Finish account authorization before removing it".into()));
            return;
        }
        if self.transfers.iter().any(|transfer| (transfer.remote_id == id || transfer.source.as_ref().is_some_and(|source| source.id == id))
            && (!transfer.state.is_terminal() || matches!(transfer.state, TransferState::Interrupted(_)))) {
            let _ = self.bg_tx.send(BgResult::CloudError("Finish or cancel this account's transfers before removing it".into()));
            return;
        }
        match self.ensure_ready().await {
            Ok(provider) => match provider.deconfigure(&id).await {
                Ok(()) => { let _ = self.bg_tx.send(BgResult::RemoteRemoved(id)); }
                Err(error) => { let _ = self.bg_tx.send(BgResult::CloudError(format!("Cannot remove {id}: {error}"))); }
            },
            Err(error) => { let _ = self.bg_tx.send(BgResult::CloudError(format!("Cannot remove {id}: {error}"))); }
        }
    }

    async fn list_remote(&mut self, tab: TabId, op_id: u64, id: RemoteId, path: String,
        sort_col: SortColumn, sort_dir: SortDir, folder_order: FolderOrder) {
        self.listings.retain(|_, task| !task.is_finished());
        if let Some(task) = self.listings.remove(&tab) { task.abort(); }
        let Some(provider) = self
            .require_ready(|e| BgResult::DirListError {
                tab,
                op_id,
                error: e.to_string(),
            })
            .await
        else {
            return;
        };
        let bg = self.bg_tx.clone();
        let task = self.rt.spawn(async move {
            let started = std::time::Instant::now();
            match provider.list(&id, &path).await {
                Ok(entries) => {
                    // Mapping and natural sorting large directories never run
                    // on the UI thread or block a Tokio network worker.
                    match tokio::task::spawn_blocking(move || {
                        let mut mapped = entries.into_iter().map(remote_to_file_entry).collect::<Vec<_>>();
                        crate::fs::list::sort_entries(&mut mapped, &sort_col, &sort_dir, folder_order);
                        mapped
                    }).await {
                        Ok(entries) => { let _ = bg.send(BgResult::DirListed { tab, op_id, entries }); }
                        Err(error) => { let _ = bg.send(BgResult::DirListError { tab, op_id, error: error.to_string() }); }
                    }
                    let _ = bg.send(BgResult::DirListTiming { tab, op_id, elapsed: started.elapsed() });
                }
                Err(e) => {
                    let _ = bg.send(BgResult::DirListError {
                        tab,
                        op_id,
                        error: e.to_string(),
                    });
                }
            }
        });
        self.listings.insert(tab, task);
    }

    async fn mount(&mut self, id: RemoteId) {
        let Some(provider) = self
            .require_ready(|e| BgResult::RemoteStatusChanged {
                id: id.clone(),
                status: RemoteStatus::Error(e.to_string()),
            })
            .await
        else {
            return;
        };
        let _ = self.bg_tx.send(BgResult::RemoteStatusChanged {
            id: id.clone(),
            status: RemoteStatus::Mounting,
        });

        let mount_point = crate::cloud::mount_point_for(&id);
        let bg = self.bg_tx.clone();
        self.rt.spawn(async move {
            let status = match provider.mount(&id, &mount_point).await {
                Ok(()) => RemoteStatus::Mounted(mount_point),
                Err(e) => {
                    let error = e.to_string();
                    #[cfg(windows)]
                    {
                        if is_windows_fuse_mount_error(&error) {
                            RemoteStatus::BrowseFallback(
                                "WinFsp missing or not working — reinstall WinFsp to enable drive mounting"
                                    .to_string(),
                            )
                        } else {
                            RemoteStatus::Error(error)
                        }
                    }
                    #[cfg(not(windows))]
                    {
                        RemoteStatus::Error(error)
                    }
                }
            };
            let _ = bg.send(BgResult::RemoteStatusChanged { id, status });
        });
    }

    async fn unmount(&mut self, id: RemoteId) {
        let Ok(provider) = self.ensure_ready().await else {
            return;
        };
        let mount_point = crate::cloud::mount_point_for(&id);
        let bg = self.bg_tx.clone();
        self.rt.spawn(async move {
            let status = match provider.unmount(&mount_point).await {
                Ok(()) => RemoteStatus::Idle,
                Err(error) => RemoteStatus::Error(format!("Unmount was not confirmed: {error}")),
            };
            let _ = bg.send(BgResult::RemoteStatusChanged {
                id,
                status,
            });
        });
    }

    fn handle_ev(&mut self, ev: Ev) {
        let ev = match ev {
            Ev::Configured { remote, result } => {
                self.configuring.remove(&remote.id);
                match result {
                    Ok(()) => {
                        if let Some(provider) = &self.provider { self.fetch_quota(provider.clone(), remote.id.clone()); }
                        let _ = self.bg_tx.send(BgResult::RemoteConfigured(remote));
                    }
                    Err(error) => { let _ = self.bg_tx.send(BgResult::RemoteConfigFailed { id: remote.id, error }); }
                }
                return;
            }
            event => event,
        };
        let (id, attempt) = match &ev {
            Ev::Progress { id, attempt, .. } | Ev::Done { id, attempt } | Ev::Failed { id, attempt, .. } => (*id, *attempt),
            Ev::Configured { .. } => unreachable!(),
        };
        if !self.transfers.iter().any(|transfer| transfer.id == id && transfer.retries == attempt
            && matches!(transfer.state, TransferState::Running | TransferState::Pausing | TransferState::Cancelling)) { return; }
        match ev {
            Ev::Configured { .. } => unreachable!(),
            Ev::Progress { id, done, total, .. } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.bytes_done = done;
                    if total > 0 {
                        t.bytes_total = total;
                    }
                }
                self.progress_dirty = true;
                return;
            }
            Ev::Done { id, .. } => {
                self.running.remove(&id);
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.state = TransferState::Done;
                    t.bytes_done = t.bytes_total;
                    if t.open_on_done {
                        let _ = open::that_detached(&t.local_path);
                    }
                    if t.kind == TransferKind::Download {
                        // Refresh any pane showing the destination dir (paste
                        // remote→local lands outside the watcher's event stream
                        // when the dest is a network/WSL share).
                        let _ = self.bg_tx.send(BgResult::FsEvent(t.local_path.clone()));
                    }
                    if matches!(t.kind, TransferKind::Upload | TransferKind::RemoteCopy) {
                        // Refresh any tab showing the destination dir, and (for a
                        // move) trash the now-uploaded local source.
                        let dir = t
                            .remote_path
                            .rsplit_once('/')
                            .map(|(d, _)| d.to_string())
                            .unwrap_or_default();
                        let _ = self.bg_tx.send(BgResult::RemoteChanged {
                            id: t.remote_id.clone(),
                            paths: vec![dir],
                        });
                        // Source deletion is forbidden until snapshot/revision
                        // verification is implemented. Recovered legacy intents
                        // cannot turn a completed copy into an unsafe deletion.
                    }
                }
            }
            Ev::Failed { id, err, cancelled, unknown, .. } => {
                self.running.remove(&id);
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    tracing::warn!(
                        transfer = t.label,
                        remote = t.remote_id,
                        local = %t.local_path.display(),
                        %err,
                        "cloud transfer failed"
                    );
                    t.state = if unknown { TransferState::Interrupted(err) }
                        else if cancelled && t.state == TransferState::Pausing { TransferState::Paused }
                        else if cancelled { TransferState::Cancelled }
                        else { TransferState::Failed(err) };
                }
            }
        }
        self.save_transfers();
        self.emit_transfers();
    }

    /// Start queued transfers up to the concurrency cap.
    fn pump_queue(&mut self) {
        if self.journal_failed { return; }
        if self.running.len() >= MAX_CONCURRENT {
            return;
        }
        let provider = match &self.provider {
            Some(p) => p.clone(),
            None => return,
        };
        let ids: Vec<TransferId> = self
            .transfers
            .iter()
            .filter(|t| t.state == TransferState::Queued)
            .take(MAX_CONCURRENT - self.running.len())
            .map(|t| t.id)
            .collect();
        if ids.is_empty() { return; }
        for id in ids {
            if self.running.len() >= MAX_CONCURRENT {
                break;
            }
            let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) else {
                continue;
            };
            t.state = TransferState::Running;
            let (kind, rid, rpath, lpath) = (
                t.kind,
                t.remote_id.clone(),
                t.remote_path.clone(),
                t.local_path.clone(),
            );
            let attempt = t.retries;
            let source = t.source.clone();
            if !self.save_transfers() { break; }
            let ev_tx = self.ev_tx.clone();
            let control = TransferControl::default();
            let task_control = control.clone();
            let p = provider.clone();
            let handle = self.rt.spawn(async move {
                let sink_tx = ev_tx.clone();
                let sink: ProgressSink = Arc::new(move |done, total| {
                    let _ = sink_tx.send(Ev::Progress { id, attempt, done, total });
                });
                let res = match kind {
                    TransferKind::Download => p.download_controlled(&rid, &rpath, &lpath, sink, task_control).await,
                    TransferKind::Upload => p.upload_controlled(&rid, &lpath, &rpath, sink, task_control).await,
                    TransferKind::RemoteCopy => match source {
                        Some(source) => p.copy_between(&source.id, &source.path, &rid, &rpath, sink, task_control).await,
                        None => Err(anyhow::anyhow!("Remote copy has no source account")),
                    },
                };
                match res {
                    Ok(()) => {
                        let _ = ev_tx.send(Ev::Done { id, attempt });
                    }
                    Err(e) => {
                        let _ = ev_tx.send(Ev::Failed {
                            id,
                            err: e.to_string(),
                            attempt,
                            cancelled: e.is::<TransferCancelled>(),
                            unknown: e.is::<OutcomeUnknown>(),
                        });
                    }
                }
            });
            self.running.insert(id, RunningTransfer { task: handle, control });
        }
        self.emit_transfers();
    }

    #[cfg(windows)]
    async fn install_winfsp(&mut self, retry_id: Option<RemoteId>) {
        let Ok(provider) = self.ensure_ready().await else {
            return;
        };
        let bg = self.bg_tx.clone();
        let progress: ProgressSink = Arc::new({
            let bg2 = bg.clone();
            move |done: u64, total: u64| {
                let _ = bg2.send(BgResult::WinFspProgress { done, total });
            }
        });
        match provider.install_mount_support(progress).await {
            Ok(()) => {
                let _ = bg.send(BgResult::WinFspInstalled { retry_id });
            }
            Err(e) => {
                let _ = bg.send(BgResult::CloudError(format!("WinFsp install: {e}")));
            }
        }
    }

    async fn delete_remote(&mut self, id: RemoteId, entries: Vec<(String, bool)>, parent: String) {
        let Some(provider) = self
            .require_ready(|e| BgResult::CloudError(format!("{id}: {e}")))
            .await
        else {
            return;
        };
        let bg = self.bg_tx.clone();
        self.rt.spawn(async move {
            let mut errors = Vec::new();
            for (path, is_dir) in entries {
                if let Err(e) = provider.delete(&id, &path, is_dir).await {
                    errors.push(format!("{path}: {e}"));
                }
            }
            if !errors.is_empty() {
                let _ = bg.send(BgResult::CloudError(format!("{id}: {}", errors.join("; "))));
            }
            let _ = bg.send(BgResult::RemoteChanged {
                id,
                paths: vec![parent],
            });
        });
    }

    async fn mkdir_remote(&mut self, id: RemoteId, path: String, parent: String) {
        let Some(provider) = self
            .require_ready(|e| BgResult::CloudError(format!("{id}: {e}")))
            .await
        else {
            return;
        };
        let bg = self.bg_tx.clone();
        self.rt.spawn(async move {
            if let Err(e) = provider.mkdir(&id, &path).await {
                let _ = bg.send(BgResult::CloudError(format!("{id}: mkdir: {e}")));
            }
            let _ = bg.send(BgResult::RemoteChanged {
                id,
                paths: vec![parent],
            });
        });
    }

    async fn rename_remote_entry(
        &mut self,
        id: RemoteId,
        from: String,
        to: String,
        parent: String,
    ) {
        let Some(provider) = self
            .require_ready(|e| BgResult::CloudError(format!("{id}: {e}")))
            .await
        else {
            return;
        };
        let bg = self.bg_tx.clone();
        self.rt.spawn(async move {
            if let Err(e) = provider.rename(&id, &from, &to).await {
                let _ = bg.send(BgResult::CloudError(format!("{id}: rename: {e}")));
            }
            let _ = bg.send(BgResult::RemoteChanged {
                id,
                paths: remote_dirs([parent, remote_parent(&from), remote_parent(&to)]),
            });
        });
    }

    async fn copy_remote_entries(
        &mut self,
        id: RemoteId,
        entries: Vec<(String, String, bool)>,
        is_move: bool,
        parent: String,
    ) {
        let Some(provider) = self
            .require_ready(|e| BgResult::CloudError(format!("{id}: {e}")))
            .await
        else {
            return;
        };
        let bg = self.bg_tx.clone();
        self.rt.spawn(async move {
            let mut errors = Vec::new();
            let mut changed_dirs = Vec::new();
            for (src, dst, _is_dir) in entries {
                changed_dirs.push(remote_parent(&dst));
                if is_move {
                    changed_dirs.push(remote_parent(&src));
                }
                let res = if is_move {
                    provider.rename(&id, &src, &dst).await
                } else {
                    provider.copy(&id, &src, &dst).await
                };
                if let Err(e) = res {
                    errors.push(format!("{src}: {e}"));
                }
            }
            if !errors.is_empty() {
                let _ = bg.send(BgResult::CloudError(format!("{id}: {}", errors.join("; "))));
            }
            changed_dirs.push(parent);
            let _ = bg.send(BgResult::RemoteChanged {
                id,
                paths: remote_dirs(changed_dirs),
            });
        });
    }

    fn emit_transfers(&self) {
        let _ = self
            .bg_tx
            .send(BgResult::TransferUpdate(self.transfers.clone()));
    }

    fn mutate_transfers(&mut self, f: impl FnOnce(&mut Vec<Transfer>)) {
        f(&mut self.transfers);
        if !self.save_transfers() {
            for transfer in &mut self.transfers {
                if transfer.state == TransferState::Queued {
                    transfer.state = TransferState::Failed("The transfer journal is unavailable; no transfer was started".into());
                }
            }
        }
        self.emit_transfers();
    }

    fn save_transfers(&mut self) -> bool {
        let mut retained = 0;
        self.transfers.reverse();
        self.transfers.retain(|transfer| {
            if matches!(transfer.state, TransferState::Done | TransferState::Cancelled | TransferState::Failed(_)) {
                retained += 1;
                retained <= MAX_COMPLETED_HISTORY
            } else { true }
        });
        self.transfers.reverse();
        if self.journal_failed { return false; }
        if let Some(journal) = &self.journal {
            if let Err(error) = journal.save(&self.transfers) {
                self.journal_failed = true;
                for transfer in &mut self.transfers {
                    if transfer.state == TransferState::Queued || (transfer.state == TransferState::Running && !self.running.contains_key(&transfer.id)) {
                        transfer.state = TransferState::Failed("Transfer journal is unavailable; this attempt was not started".into());
                    }
                }
                let _ = self.bg_tx.send(BgResult::CloudError(format!("Transfer journal write failed; cloud queue stopped: {error}")));
                return false;
            }
        }
        true
    }

    async fn stop_provider(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(self.configuring.is_empty(), "Finish account authorization before stopping the Cloud extension");
        if let Some(provider) = &self.provider { provider.prepare_shutdown().await?; }
        for task in self.listings.drain().map(|(_, task)| task) { task.abort(); }
        self.previews.cancel_all();
        for transfer in &mut self.transfers {
            transfer.state = match &transfer.state {
                TransferState::Queued => TransferState::Paused,
                TransferState::Running | TransferState::Pausing => TransferState::Pausing,
                state => state.clone(),
            };
        }
        for running in self.running.values() { running.control.cancel(); }
        let running = std::mem::take(&mut self.running);
        let stop_deadline = tokio::time::Instant::now() + Duration::from_secs(150);
        for (id, mut running) in running {
            // All jobs were cancelled together. An unconfirmed shutdown is
            // quarantined in the journal; it must never be replayed as queued.
            if tokio::time::timeout_at(stop_deadline, &mut running.task).await.is_err() {
                running.task.abort();
            }
            if let Some(transfer) = self.transfers.iter_mut().find(|transfer| transfer.id == id) {
                transfer.state = TransferState::Interrupted("Provider stopped. Verify the destination before starting another attempt.".into());
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(150);
        while self.provider.as_ref().is_some_and(|provider| Arc::strong_count(provider) > 1) {
            anyhow::ensure!(tokio::time::Instant::now() < deadline, "Cloud operations have not finished; the provider was kept running");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.provider = None;
        self.save_transfers();
        self.emit_transfers();
        Ok(())
    }

    async fn reap_finished_tasks(&mut self) {
        let finished = self.running.iter().filter(|(_, running)| running.task.is_finished())
            .map(|(id, _)| *id).collect::<Vec<_>>();
        for id in finished {
            if let Some(running) = self.running.remove(&id) {
                if let Err(error) = running.task.await {
                    if let Some(transfer) = self.transfers.iter().find(|transfer| transfer.id == id) {
                        self.handle_ev(Ev::Failed { id, attempt: transfer.retries,
                            err: format!("Transfer worker stopped without a confirmed result: {error}"), cancelled: false, unknown: true });
                    }
                }
            }
        }
    }
}

fn remote_parent(path: &str) -> String {
    path.rsplit_once('/')
        .map_or("", |(parent, _)| parent)
        .to_string()
}

fn remote_dirs(paths: impl IntoIterator<Item = String>) -> Vec<String> {
    paths.into_iter().fold(Vec::new(), |mut out, path| {
        let path = path.trim_matches('/').to_string();
        if !out.contains(&path) {
            out.push(path);
        }
        out
    })
}

#[cfg(test)]
mod remote_change_tests {
    use super::*;

    fn fixture_worker(state: TransferState, attempt: u32) -> (Worker, flume::Receiver<BgResult>) {
        let (bg_tx, bg_rx) = flume::unbounded();
        let (ev_tx, _) = flume::unbounded();
        (Worker {
            rt: tokio::runtime::Handle::current(), bg_tx, ev_tx, provider: None,
            transfers: vec![Transfer {
                id: 1, kind: TransferKind::Upload, label: "backup".into(), remote_id: "drive".into(),
                remote_path: "backup".into(), local_path: "backup".into(), state,
                bytes_done: 0, bytes_total: 1000, retries: attempt, open_on_done: false, delete_source: false, source: None,
            }], running: HashMap::new(), listings: HashMap::new(), progress_dirty: false, next_id: 2,
            journal: None, journal_failed: false, previews: crate::cloud::preview::PreviewQueue::default(),
            configuring: std::collections::HashSet::new(),
        }, bg_rx)
    }

    #[tokio::test]
    async fn pause_and_cancel_wait_for_acknowledgement_and_stale_attempts_cannot_complete() {
        let (mut worker, _) = fixture_worker(TransferState::Running, 1);
        worker.handle_ev(Ev::Done { id: 1, attempt: 0 });
        assert_eq!(worker.transfers[0].state, TransferState::Running);
        worker.handle_cmd(CloudCmd::PauseTransfer(1)).await;
        assert_eq!(worker.transfers[0].state, TransferState::Pausing);
        worker.handle_ev(Ev::Failed { id: 1, attempt: 1, cancelled: true, unknown: false, err: "cancelled".into() });
        assert_eq!(worker.transfers[0].state, TransferState::Paused);
        worker.handle_cmd(CloudCmd::CancelTransfer(1)).await;
        assert_eq!(worker.transfers[0].state, TransferState::Cancelled);
        worker.handle_ev(Ev::Done { id: 1, attempt: 1 });
        assert_eq!(worker.transfers[0].state, TransferState::Cancelled);
        worker.transfers[0].state = TransferState::Running;
        worker.handle_cmd(CloudCmd::CancelTransfer(1)).await;
        assert_eq!(worker.transfers[0].state, TransferState::Cancelling);
        worker.handle_ev(Ev::Failed { id: 1, attempt: 1, cancelled: false, unknown: true, err: "connection lost".into() });
        assert!(matches!(worker.transfers[0].state, TransferState::Interrupted(_)));
        worker.pump_queue();
        assert!(worker.running.is_empty());
    }

    #[tokio::test]
    async fn unavailable_journal_rejects_new_attempts_and_completed_history_is_bounded() {
        let (mut worker, _) = fixture_worker(TransferState::Done, 0);
        let completed = worker.transfers[0].clone();
        for id in 2..=1000 { worker.transfers.push(Transfer { id, ..completed.clone() }); }
        worker.journal_failed = true;
        worker.mutate_transfers(|transfers| transfers.push(Transfer { id: 1001, state: TransferState::Queued, ..completed }));
        assert_eq!(worker.transfers.len(), MAX_COMPLETED_HISTORY + 1);
        assert!(matches!(worker.transfers.last().unwrap().state, TransferState::Failed(_)));
        assert!(worker.running.is_empty());
    }

    #[test]
    fn remote_dirs_dedupes_and_normalizes() {
        assert_eq!(
            remote_dirs([
                "docs".to_string(),
                "/docs/".to_string(),
                remote_parent("photos/a.jpg"),
                remote_parent("b.jpg"),
            ]),
            vec!["docs".to_string(), "photos".to_string(), "".to_string()]
        );
    }

    #[tokio::test]
    async fn transfer_progress_is_coalesced_instead_of_cloning_a_snapshot_per_event() {
        let (bg_tx, bg_rx) = flume::unbounded();
        let (ev_tx, _) = flume::unbounded();
        let mut worker = Worker {
            rt: tokio::runtime::Handle::current(), bg_tx, ev_tx, provider: None,
            transfers: vec![Transfer {
                id: 1, kind: TransferKind::Upload, label: "backup".into(),
                remote_id: "drive".into(), remote_path: "backup".into(), local_path: "backup".into(),
                state: TransferState::Running, bytes_done: 0, bytes_total: 1000, retries: 0,
                open_on_done: false, delete_source: false,
                source: None,
            }],
            running: HashMap::new(), listings: HashMap::new(), progress_dirty: false, next_id: 2,
            journal: None, journal_failed: false,
            previews: crate::cloud::preview::PreviewQueue::default(),
            configuring: std::collections::HashSet::new(),
        };
        for done in 1..=1000 {
            worker.handle_ev(Ev::Progress { id: 1, attempt: 0, done, total: 1000 });
            worker.pump_queue();
        }
        assert!(bg_rx.is_empty(), "progress must wait for the bounded publication tick");
        assert!(worker.progress_dirty);
        worker.emit_transfers();
        let BgResult::TransferUpdate(snapshot) = bg_rx.recv().unwrap() else { panic!("expected snapshot"); };
        assert_eq!(snapshot[0].bytes_done, 1000);
        assert!(bg_rx.is_empty());
    }
}

/// Map a provider entry to a `FileEntry` for the file views. The path is the
/// remote-relative path (a stable key); the owning tab's `TabSource::Remote`
/// marks it as remote so navigation/open route through the worker.
fn remote_to_file_entry(e: RemoteEntry) -> FileEntry {
    let kind = if e.is_dir {
        FileKind::Dir
    } else {
        FileKind::File
    };
    let modified = e
        .modified
        .and_then(|secs| u64::try_from(secs).ok())
        // Server-supplied: an absurd value must not overflow `SystemTime`.
        .and_then(|secs| UNIX_EPOCH.checked_add(Duration::from_secs(secs)));
    FileEntry::from(atlas_model::file_entry::FileEntryData {
        name: e.name.into(),
        path: PathBuf::from(e.path),
        kind,
        size: (!e.is_dir && !e.mime.as_deref().is_some_and(|mime| mime.starts_with("application/vnd.google-apps."))).then_some(e.size),
        modified,
        _is_hidden: false,
        _is_symlink: false,
        archive_root: None,
        recycle_info: None,
    })
}

#[cfg(windows)]
fn is_windows_fuse_mount_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("fuse")
        || lower.contains("winfsp")
        || lower.contains("cgofuse")
        || lower.contains("failed to mount")
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_fuse_mount_errors_are_dependency_errors() {
        assert!(is_windows_fuse_mount_error(
            "rc mount/mount: failed to mount FUSE fs: mount stopped before calling Init"
        ));
        assert!(is_windows_fuse_mount_error("WinFsp is not installed"));
        assert!(!is_windows_fuse_mount_error("oauth token expired"));
    }
}
