use crate::app::App;
use atlas_core::fs::transfer::{self, TransferPair};
use atlas_model::layout_tree::{Tile, TileId};
use atlas_model::ops::{
    ConflictPolicy, Resolution, TransferConflictState, TransferKind, TransferOrigin,
    TransferRequest,
};
use atlas_model::overlay::{Toast, ToastKind};
use atlas_model::undo::UndoEntry;
use std::path::PathBuf;

/// Translate a typed per-item failure into user-facing text ("name: detail").
pub(crate) fn op_error_text(err: &atlas_model::ops::OpError) -> String {
    use atlas_model::ops::OpErrorKind as K;
    let detail = match &err.kind {
        K::Gone => rust_i18n::t!("op_err_gone").into_owned(),
        K::TrashUnsupported => rust_i18n::t!("op_err_trash_unsupported").into_owned(),
        K::PermissionDenied => rust_i18n::t!("op_err_permission").into_owned(),
        K::InUse => rust_i18n::t!("op_err_in_use").into_owned(),
        K::DiskFull => rust_i18n::t!("op_err_disk_full").into_owned(),
        K::PathTooLong => rust_i18n::t!("op_err_path_too_long").into_owned(),
        K::NetworkUnreachable => rust_i18n::t!("op_err_network").into_owned(),
        K::AlreadyExists => rust_i18n::t!("op_err_already_exists").into_owned(),
        K::SelfNested => rust_i18n::t!("op_err_self_nested").into_owned(),
        K::SelfOverwrite => rust_i18n::t!("op_err_self_overwrite").into_owned(),
        K::ReplaceFailed(d) => rust_i18n::t!("op_err_replace_failed", detail = d).into_owned(),
        K::SourceNotRemoved(d) => {
            rust_i18n::t!("op_err_source_not_removed", detail = d).into_owned()
        }
        K::NotEnoughSpace { needed, free } => {
            rust_i18n::t!("op_err_disk_space", needed = needed, free = free).into_owned()
        }
        K::RepeatedAbort => rust_i18n::t!("op_err_repeated_abort").into_owned(),
        K::Cancelled => rust_i18n::t!("op_cancelled").into_owned(),
        K::Msg(s) => s.clone(),
    };
    let name = err
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.is_empty() {
        detail
    } else {
        format!("{name}: {detail}")
    }
}

/// Translate a typed filename-validation failure into user-facing text.
pub(crate) fn filename_error_text(err: &transfer::FilenameError) -> String {
    use atlas_core::fs::transfer::FilenameError as F;
    match err {
        F::Empty => rust_i18n::t!("filename_err_empty").into_owned(),
        F::InvalidChar(c) => {
            rust_i18n::t!("filename_err_invalid_char", ch = c.to_string()).into_owned()
        }
        F::TrailingDotOrSpace => rust_i18n::t!("filename_err_trailing").into_owned(),
        F::Reserved(name) => rust_i18n::t!("filename_err_reserved", name = name).into_owned(),
    }
}

pub(crate) fn rename_target_error(from: &std::path::Path, to: &std::path::Path) -> Option<String> {
    let name = to.file_name()?.to_string_lossy();
    if let Err(error) = transfer::validate_filename(&name) {
        return Some(filename_error_text(&error));
    }
    if to.symlink_metadata().is_ok() && !transfer::paths_equal_ci(from, to) {
        return Some(rust_i18n::t!("err_collision", name = name).to_string());
    }
    None
}

/// Ordered rename steps for a batch. Names may chain or swap (`1→2, 2→3`), so any
/// file whose target is another batch member's current name first parks under a
/// temporary name; everything else renames directly; parked files finish last.
pub(crate) fn plan_batch_renames(
    pairs: &[(std::path::PathBuf, std::path::PathBuf)],
) -> Vec<(std::path::PathBuf, std::path::PathBuf)> {
    use std::path::PathBuf;
    let moving: Vec<&PathBuf> = pairs.iter().map(|(from, _)| from).collect();
    let clashes = |from: &PathBuf, to: &PathBuf| {
        from != to
            && moving
                .iter()
                .any(|other| crate::app::ops::same_path(other, to))
    };
    let mut parked = Vec::new();
    let mut direct = Vec::new();
    let mut first = Vec::new();
    for (index, (from, to)) in pairs.iter().enumerate() {
        if from == to {
            continue;
        }
        if clashes(from, to) {
            let mut n = index;
            let temp = loop {
                let candidate = from.with_file_name(format!(".atlas-rename-{n}.tmp"));
                if candidate.symlink_metadata().is_err() {
                    break candidate;
                }
                n += pairs.len().max(1);
            };
            first.push((from.clone(), temp.clone()));
            parked.push((temp, to.clone()));
        } else {
            direct.push((from.clone(), to.clone()));
        }
    }
    first.into_iter().chain(direct).chain(parked).collect()
}

fn same_path(a: &std::path::Path, b: &std::path::Path) -> bool {
    transfer::paths_equal_ci(a, b)
}

#[cfg(test)]
mod batch_plan_tests {
    use super::plan_batch_renames;
    use std::path::PathBuf;

    fn p(name: &str) -> PathBuf {
        PathBuf::from("dir").join(name)
    }

    #[test]
    fn shifted_numbers_park_before_landing() {
        let steps = plan_batch_renames(&[(p("01.png"), p("02.png")), (p("02.png"), p("03.png"))]);
        // 01→02 lands on a member, so it parks; 02→03 is free and goes first
        // (vacating 02) before the parked file finishes.
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].0, p("01.png"));
        assert!(steps[0].1.to_string_lossy().contains(".atlas-rename-"));
        assert_eq!(steps[1], (p("02.png"), p("03.png")));
        assert_eq!(steps[2].1, p("02.png"));
    }

    #[test]
    fn independent_renames_stay_direct_and_noops_vanish() {
        let steps = plan_batch_renames(&[(p("a.png"), p("b.png")), (p("c.png"), p("c.png"))]);
        assert_eq!(steps, vec![(p("a.png"), p("b.png"))]);
    }
}

impl App {
    /// Rename many files in one ordered task (see [`plan_batch_renames`]); each step
    /// reports like a normal rename so the listing refreshes as before.
    pub(crate) fn rename_batch(&mut self, pairs: Vec<(PathBuf, PathBuf)>) {
        if !self.allow_edition_action(atlas_model::edition::Feature::BatchRename, pairs.len()) {
            return;
        }
        let moving: Vec<PathBuf> = pairs.iter().map(|(from, _)| from.clone()).collect();
        for (from, to) in &pairs {
            if from == to || moving.iter().any(|other| same_path(other, to)) {
                continue;
            }
            if let Some(error) = rename_target_error(from, to) {
                self.toast_error(error);
                return;
            }
        }
        let steps = plan_batch_renames(&pairs);
        for (from, to) in &pairs {
            if from != to {
                self.state.ops.undo_stack.push(UndoEntry::Rename {
                    from: from.clone(),
                    to: to.clone(),
                });
                let _ = self
                    .tag_db_tx
                    .send(atlas_core::fs::tag_db::TagDbCmd::RenamePath {
                        old: from.clone(),
                        new: to.clone(),
                    });
            }
        }
        let jobs: Vec<_> = steps
            .into_iter()
            .map(|(from, to)| {
                let op_id = self.state.next_op_id();
                let affected = self.affected_tabs_for_path(&from);
                (from, to, op_id, affected)
            })
            .collect();
        let tx = self.tx.clone();
        #[cfg(windows)]
        let owner = self.hwnd;
        self.rt.spawn(async move {
            for (from, to, op_id, affected) in jobs {
                #[cfg(windows)]
                crate::shell_file_operation::rename_file(
                    from,
                    to,
                    op_id,
                    tx.clone(),
                    affected,
                    owner,
                )
                .await;
                #[cfg(not(windows))]
                atlas_core::fs::ops::rename_file(from, to, op_id, tx.clone(), affected).await;
            }
        });
    }

    #[cfg(windows)]
    pub(crate) fn paste_shortcuts_focused(&mut self) {
        let Some(dest) = self
            .tab_data_for(self.state.focused_tile)
            .and_then(|tab| tab.location.fs_dir())
            .map(std::path::Path::to_path_buf)
        else {
            return;
        };
        let Some(files) = crate::dnd::windows::ole_clipboard::get_clipboard_files() else {
            return;
        };
        if files.paths.is_empty() {
            return;
        }
        let affected_tabs = self.affected_tabs_for_path(&dest);
        let (id, cancel) = self.state.ops.start_op_labeled(
            files.paths.len(),
            rust_i18n::t!("op_creating_shortcuts").to_string(),
        );
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let total = files.paths.len() as u64;
            let mut errors = Vec::new();
            for (index, source) in files.paths.into_iter().enumerate() {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    errors.push(atlas_model::ops::OpError {
                        path: source,
                        kind: atlas_model::ops::OpErrorKind::Cancelled,
                    });
                    break;
                }
                let _ = tx.send(atlas_model::app_state::BgResult::OpProgress {
                    id,
                    done: index as u64,
                    total,
                    bytes_done: 0,
                    bytes_total: 0,
                    current_file: source.clone(),
                });
                let target = dest.clone();
                let error_path = source.clone();
                match tokio::task::spawn_blocking(move || {
                    crate::shell_file_operation::create_shortcut(source, target)
                })
                .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => errors.push(error),
                    Err(error) => errors.push(atlas_model::ops::OpError {
                        path: error_path,
                        kind: atlas_model::ops::OpErrorKind::Msg(error.to_string()),
                    }),
                }
            }
            let _ = tx.send(atlas_model::app_state::BgResult::OpComplete {
                id,
                result: if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors)
                },
                affected_tabs,
            });
        });
    }

    pub(crate) fn affected_tabs_for_path(&self, path: &PathBuf) -> Vec<TileId> {
        self.state
            .pane_tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| {
                if let Tile::Pane(p) = tile {
                    if let Some(dir) = p.location.fs_dir() {
                        if path.starts_with(dir) || dir.starts_with(path) {
                            return Some(*id);
                        }
                    }
                }
                None
            })
            .collect()
    }

    fn toast(&mut self, kind: ToastKind, msg: String) {
        if self.state.overlay.op_errors.len() >= 5 {
            self.state.overlay.op_errors.pop_front();
        }
        self.state
            .overlay
            .op_errors
            .push_back(Toast::new(msg, kind));
    }

    /// Queue a dismissible error toast (capped to the 5 most recent).
    pub(crate) fn toast_error(&mut self, msg: String) {
        self.toast(ToastKind::Error, msg);
    }

    /// Queue an informational toast (non-fatal).
    pub(crate) fn toast_info(&mut self, msg: String) {
        self.toast(ToastKind::Info, msg);
    }

    // -----------------------------------------------------------------------
    // Unified transfer pipeline (paste, drag-and-drop, menu copy/move, undo)
    // -----------------------------------------------------------------------

    /// Single entry point for every copy/move: validates, detects conflicts
    /// (opening the dialog when the policy is `Ask`), then spawns the engine.
    pub(crate) fn request_transfer(&mut self, req: TransferRequest) {
        if self.state.overlay.transfer_conflict.is_some() {
            self.pending_transfers.push_back(req);
            return;
        }
        tracing::info!(origin = ?req.origin, kind = ?req.kind, dest = %req.dest.display(), sources = ?req.sources, "transfer requested");
        // Remote sentinel dest (`remote:<id>/…`): route to the cloud uploader
        // instead of the local pipeline. Centralized here so every entry point
        // (paste, DnD, spring-load drops) gets it for free.
        #[cfg(feature = "provider-host")]
        if self.try_remote_upload(
            &req.sources,
            &req.dest,
            matches!(req.kind, TransferKind::Move),
        ) {
            return;
        }
        // Never transfer INTO a Recycle Bin folder: `$R` dirs are real on disk,
        // so validation would happily move payloads next to the `$I` metadata.
        if atlas_core::fs::paths::is_recycle_bin_path(&req.dest) {
            self.toast_error(rust_i18n::t!("op_err_dest_recycle").into_owned());
            return;
        }
        let outcome = transfer::validate(&req);
        if let Some(fatal) = outcome.fatal {
            self.toast_error(
                rust_i18n::t!(
                    "op_err_dest_gone",
                    name = fatal.file_name().unwrap_or_default().to_string_lossy()
                )
                .into_owned(),
            );
            return;
        }
        for err in &outcome.errors {
            if err.kind == atlas_model::ops::OpErrorKind::SelfNested {
                continue;
            }
            self.toast_error(op_error_text(err));
        }
        if outcome.valid.is_empty() {
            return;
        }
        let target_names = req.target_names.as_ref().map(|names| {
            outcome
                .valid
                .iter()
                .filter_map(|valid| {
                    req.sources
                        .iter()
                        .position(|source| source == valid)
                        .and_then(|index| names.get(index).cloned())
                })
                .collect::<Vec<_>>()
        });
        let (conflicts, clean) =
            transfer::detect_conflicts(&outcome.valid, target_names.as_deref(), &req.dest);
        if !conflicts.is_empty() && req.policy == ConflictPolicy::Ask {
            self.state.overlay.transfer_conflict = Some(TransferConflictState {
                request: TransferRequest {
                    target_names,
                    sources: outcome.valid,
                    ..req
                },
                conflicts,
                clean,
                per_item: None,
                selected_action: 0,
            });
            return;
        }
        let built = transfer::build_pairs(&clean, &conflicts, &req.policy, &req.dest);
        self.spawn_transfer(built.pairs, req.kind, req.origin);
    }

    /// Apply the user's choice from the conflict dialog. `None` cancels.
    pub(crate) fn apply_transfer_resolution(&mut self, policy: Option<ConflictPolicy>) {
        let Some(state) = self.state.overlay.transfer_conflict.take() else {
            return;
        };
        if let Some(policy) = policy {
            let built =
                transfer::build_pairs(&state.clean, &state.conflicts, &policy, &state.request.dest);
            self.spawn_transfer(built.pairs, state.request.kind, state.request.origin);
        }
        while self.state.overlay.transfer_conflict.is_none() {
            let Some(next) = self.pending_transfers.pop_front() else {
                break;
            };
            self.request_transfer(next);
        }
    }

    /// Spawn the transfer engine over already-resolved pairs.
    pub(crate) fn spawn_transfer(
        &mut self,
        pairs: Vec<TransferPair>,
        kind: TransferKind,
        origin: TransferOrigin,
    ) {
        // A cut is consumed by its paste even if everything else fails later.
        if origin == TransferOrigin::Paste && kind == TransferKind::Move {
            self.state.ops.clipboard = None;
            #[cfg(windows)]
            crate::dnd::windows::ole_clipboard::clear();
        }
        if pairs.is_empty() {
            return;
        }
        let affected: std::collections::HashSet<_> = pairs
            .iter()
            .flat_map(|p| {
                let mut tabs = self.affected_tabs_for_path(&p.src);
                tabs.extend(self.affected_tabs_for_path(&p.dst));
                tabs
            })
            .collect();
        let label = match kind {
            TransferKind::Copy => rust_i18n::t!("op_copying").to_string(),
            TransferKind::Move => rust_i18n::t!("op_moving").to_string(),
        };
        let (op_id, cancel) = self
            .state
            .ops
            .start_op_labeled(pairs.len(), label.to_string());
        let tx = self.tx.clone();
        let affected: Vec<_> = affected.into_iter().collect();
        self.operation_feedback.borrow_mut().begin(
            op_id,
            pairs
                .iter()
                .flat_map(|pair| [pair.src.clone(), pair.dst.clone()]),
        );
        #[cfg(windows)]
        let owner = self.hwnd;
        self.rt.spawn(async move {
            #[cfg(windows)]
            crate::shell_file_operation::transfer_with_fallback(
                pairs, kind, origin, op_id, tx, cancel, affected, owner,
            )
            .await;
            #[cfg(not(windows))]
            transfer::execute(pairs, kind, origin, op_id, tx, cancel, affected).await;
        });
    }

    /// Re-run exact pairs (undo/redo): no conflict dialog, rename on collision.
    fn spawn_transfer_exact(&mut self, pairs: Vec<(PathBuf, PathBuf)>, kind: TransferKind) {
        let pairs = pairs
            .into_iter()
            .map(|(src, dst)| TransferPair {
                src,
                dst,
                on_conflict: Resolution::Rename,
            })
            .collect();
        self.spawn_transfer(pairs, kind, TransferOrigin::Undo);
    }

    // -----------------------------------------------------------------------
    // Entry-point wrappers
    // -----------------------------------------------------------------------

    /// Set the file clipboard: internal state always; on Windows the system
    /// clipboard too (`CF_HDROP` + preferred effect), so other apps can paste.
    /// `remote_id`: `Some(id)` when paths are remote-relative (not local FS paths).
    pub(crate) fn set_clipboard(&mut self, paths: Vec<PathBuf>, is_cut: bool) {
        self.set_clipboard_with_remote(paths, is_cut, None);
    }

    pub(crate) fn set_clipboard_with_names(
        &mut self,
        paths: Vec<PathBuf>,
        target_names: Option<Vec<std::ffi::OsString>>,
        is_cut: bool,
    ) {
        self.set_clipboard_full(paths, target_names, is_cut, None);
    }

    pub(crate) fn set_clipboard_with_remote(
        &mut self,
        paths: Vec<PathBuf>,
        is_cut: bool,
        remote_id: Option<String>,
    ) {
        self.set_clipboard_full(paths, None, is_cut, remote_id);
    }

    fn set_clipboard_full(
        &mut self,
        paths: Vec<PathBuf>,
        target_names: Option<Vec<std::ffi::OsString>>,
        is_cut: bool,
        remote_id: Option<String>,
    ) {
        // Only put local paths on the system clipboard (CF_HDROP / text/uri-list); remote paths
        // are virtual and have no meaning outside Atlas.
        if remote_id.is_none() {
            #[cfg(windows)]
            if let Err(e) = crate::dnd::windows::ole_clipboard::set_clipboard_files(
                &paths,
                is_cut,
                self.tx.clone(),
            ) {
                self.toast_error(rust_i18n::t!("err_clipboard", e = e).to_string());
            }

            #[cfg(unix)]
            {
                let uri_list =
                    crate::dnd::format_uri_list_with_cut(&paths, is_cut).replace('\0', "");
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(uri_list.clone());
                    #[cfg(target_os = "linux")]
                    {
                        use arboard::{LinuxClipboardKind, SetExtLinux};
                        let _ = cb
                            .set()
                            .clipboard(LinuxClipboardKind::Primary)
                            .text(uri_list);
                    }
                }
            }
        }
        self.state.ops.clipboard = Some(atlas_model::ops::ClipboardState {
            paths,
            target_names,
            is_cut,
            remote_id,
        });
    }

    pub(crate) fn paste_files(&mut self, dest: PathBuf) {
        // The system clipboard is the source of truth on Windows: it sees files
        // copied in Explorer, and goes stale-aware when another app replaces it.
        #[cfg(windows)]
        let source = {
            match crate::dnd::windows::ole_clipboard::get_clipboard_files() {
                Some(c) if c.from_atlas => {
                    // Our own content: keep the richer internal cut semantics.
                    let state = self.state.ops.clipboard.as_ref();
                    let is_cut = state.is_some_and(|s| s.is_cut);
                    let target_names = state.and_then(|s| s.target_names.clone());
                    Some((c.paths, target_names, is_cut, TransferOrigin::Paste))
                }
                Some(c) => {
                    let origin = if c.preferred_move {
                        TransferOrigin::PasteExternalCut
                    } else {
                        TransferOrigin::Paste
                    };
                    Some((c.paths, None, c.preferred_move, origin))
                }
                None => {
                    // No files on the system clipboard (another app replaced
                    // it): our internal cut/copy is stale too.
                    self.state.ops.clipboard = None;
                    None
                }
            }
        };
        #[cfg(not(windows))]
        let source = {
            let sys_text = arboard::Clipboard::new()
                .ok()
                .and_then(|mut cb| cb.get_text().ok());

            let mut sys_files = sys_text.as_deref().and_then(|text| {
                let (paths, ext_cut) = crate::dnd::parse_uri_list_with_cut(text);
                if paths.is_empty() {
                    None
                } else {
                    Some((paths, ext_cut))
                }
            });

            // Also check arboard file_list in case the external app only exposed atoms.URI_LIST
            if sys_files.is_none() {
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    if let Ok(paths) = cb.get().file_list() {
                        if !paths.is_empty() {
                            sys_files = Some((paths, false));
                        }
                    }
                }
            }

            match sys_files {
                Some((sys_paths, ext_cut)) => {
                    if let Some(internal) = &self.state.ops.clipboard {
                        if internal.paths == sys_paths {
                            Some((
                                internal.paths.clone(),
                                internal.target_names.clone(),
                                internal.is_cut,
                                TransferOrigin::Paste,
                            ))
                        } else {
                            let origin = if ext_cut {
                                TransferOrigin::PasteExternalCut
                            } else {
                                TransferOrigin::Paste
                            };
                            Some((sys_paths, None, ext_cut, origin))
                        }
                    } else {
                        let origin = if ext_cut {
                            TransferOrigin::PasteExternalCut
                        } else {
                            TransferOrigin::Paste
                        };
                        Some((sys_paths, None, ext_cut, origin))
                    }
                }
                None => {
                    let is_remote = self
                        .state
                        .ops
                        .clipboard
                        .as_ref()
                        .and_then(|c| c.remote_id.as_ref())
                        .is_some();
                    if let Some(text) = &sys_text {
                        if !text.trim().is_empty() && !is_remote {
                            self.state.ops.clipboard = None;
                        }
                    }
                    self.state.ops.clipboard.as_ref().map(|c| {
                        (
                            c.paths.clone(),
                            c.target_names.clone(),
                            c.is_cut,
                            TransferOrigin::Paste,
                        )
                    })
                }
            }
        };

        let Some((paths, target_names, is_cut, origin)) = source else {
            return;
        };
        #[cfg(feature = "provider-host")]
        if self.try_remote_upload(&paths, &dest, is_cut) {
            return;
        }
        self.request_transfer(TransferRequest {
            sources: paths,
            target_names,
            dest,
            kind: if is_cut {
                TransferKind::Move
            } else {
                TransferKind::Copy
            },
            policy: ConflictPolicy::Ask,
            origin,
        });
    }

    pub(crate) fn copy_files(
        &mut self,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        origin: TransferOrigin,
    ) {
        #[cfg(feature = "provider-host")]
        if self.try_remote_upload(&paths, &dest, false) {
            return;
        }
        self.request_transfer(TransferRequest {
            sources: paths,
            target_names: None,
            dest,
            kind: TransferKind::Copy,
            policy: ConflictPolicy::Ask,
            origin,
        });
    }

    pub(crate) fn move_files(
        &mut self,
        paths: Vec<PathBuf>,
        dest: PathBuf,
        origin: TransferOrigin,
    ) {
        // Moving into a remote uploads then trashes the local source on success.
        #[cfg(feature = "provider-host")]
        if self.try_remote_upload(&paths, &dest, true) {
            return;
        }
        self.request_transfer(TransferRequest {
            sources: paths,
            target_names: None,
            dest,
            kind: TransferKind::Move,
            policy: ConflictPolicy::Ask,
            origin,
        });
    }

    // -----------------------------------------------------------------------
    // Delete / rename / create
    // -----------------------------------------------------------------------

    pub(crate) fn delete_files(&mut self, paths: Vec<PathBuf>, to_trash: bool) {
        self.delete_files_opts(paths, to_trash, to_trash);
    }

    pub(crate) fn delete_files_opts(
        &mut self,
        paths: Vec<PathBuf>,
        to_trash: bool,
        record_undo: bool,
    ) {
        let to_trash = to_trash
            && !paths
                .iter()
                .any(|path| atlas_core::vault::live::is_private(path));
        if record_undo && to_trash && !paths.is_empty() {
            self.state.ops.undo_stack.push(UndoEntry::Trash {
                paths: paths.clone(),
            });
        }
        let mut affected: Vec<_> = paths
            .iter()
            .flat_map(|p| self.affected_tabs_for_path(p))
            .collect();
        if to_trash {
            affected.extend(self.recycle_bin_tabs());
        }
        let (op_id, cancel) = self
            .state
            .ops
            .start_op_labeled(paths.len(), rust_i18n::t!("op_deleting").to_string());
        let tx = self.tx.clone();
        #[cfg(windows)]
        let owner = self.hwnd;
        self.rt.spawn(async move {
            #[cfg(windows)]
            crate::shell_file_operation::delete_files(
                paths, to_trash, op_id, tx, cancel, affected, owner,
            )
            .await;
            #[cfg(not(windows))]
            atlas_core::fs::ops::delete_files(paths, to_trash, op_id, tx, cancel, affected).await;
        });
    }

    /// Compress `paths` into a new zip in their parent directory, named after
    /// the first item ("photos.zip", uniqued to "photos (1).zip" on collision).
    pub(crate) fn compress_to_zip(&mut self, paths: Vec<PathBuf>) {
        let Some(first) = paths.first() else {
            return;
        };
        let Some(parent) = first.parent().map(std::path::Path::to_path_buf) else {
            return;
        };
        let stem = first.file_stem().map_or_else(
            || "archive".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let dest = transfer::unique_target(&parent, std::ffi::OsStr::new(&format!("{stem}.zip")));
        let affected = self.affected_tabs_for_path(&dest);
        let (op_id, cancel) = self
            .state
            .ops
            .start_op_labeled(paths.len(), rust_i18n::t!("op_compressing").to_string());
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            atlas_core::fs::compress::compress_to_zip(paths, dest, op_id, tx, cancel, affected)
                .await;
        });
    }

    // -----------------------------------------------------------------------
    // Recycle Bin operations
    // -----------------------------------------------------------------------

    /// Panes currently showing the recycle bin (refreshed after every bin op).
    fn recycle_bin_tabs(&self) -> Vec<TileId> {
        self.state
            .pane_tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| match tile {
                Tile::Pane(p) if p.location == atlas_model::location::Location::RecycleBin => {
                    Some(*id)
                }
                _ => None,
            })
            .collect()
    }

    /// `(recycled $R, $I metadata, original path)` for each selected entry of the
    /// focused pane. Empty when the focused pane is not a recycle-bin listing.
    fn selected_recycle_items(&self) -> Vec<(PathBuf, PathBuf, PathBuf)> {
        let tile = self.state.focused_tile;
        let Some(ts) = self.state.tab_states.get(&tile) else {
            return Vec::new();
        };
        ts.selection
            .selected
            .iter()
            .filter_map(|&i| ts.entries.get(i))
            .filter_map(|e| {
                e.recycle_info()
                    .as_ref()
                    .map(|info| (e.path(), info.i_file.clone(), info.original_path.clone()))
            })
            .collect()
    }

    pub(crate) fn restore_selected_from_recycle_bin(&mut self) {
        let items = self.selected_recycle_items();
        if items.is_empty() {
            return;
        }
        let mut affected: Vec<TileId> = self.recycle_bin_tabs();
        for (_, _, original) in &items {
            affected.extend(self.affected_tabs_for_path(original));
        }
        let (op_id, _cancel) = self
            .state
            .ops
            .start_op_labeled(items.len(), rust_i18n::t!("op_restoring").to_string());
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            atlas_core::fs::recycle_bin::restore_items(items, op_id, tx, affected).await;
        });
    }

    pub(crate) fn purge_selected_from_recycle_bin(&mut self) {
        let items: Vec<(PathBuf, PathBuf)> = self
            .selected_recycle_items()
            .into_iter()
            .map(|(r, i, _)| (r, i))
            .collect();
        if items.is_empty() {
            return;
        }
        let affected = self.recycle_bin_tabs();
        let (op_id, _cancel) = self
            .state
            .ops
            .start_op_labeled(items.len(), rust_i18n::t!("op_deleting").to_string());
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            atlas_core::fs::recycle_bin::purge_items(items, op_id, tx, affected).await;
        });
    }

    pub(crate) fn empty_recycle_bin(&mut self) {
        let affected = self.recycle_bin_tabs();
        let (op_id, _cancel) = self
            .state
            .ops
            .start_op_labeled(1, rust_i18n::t!("op_emptying_bin").to_string());
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            atlas_core::fs::recycle_bin::empty_bins(op_id, tx, affected).await;
        });
    }

    pub(crate) fn rename_file(&mut self, from: PathBuf, to: PathBuf) {
        // Renaming onto an existing entry would fail (or worse); only the
        // case-change of the same file is allowed.
        if let Some(error) = rename_target_error(&from, &to) {
            self.toast_error(error);
            return;
        }
        self.state.ops.undo_stack.push(UndoEntry::Rename {
            from: from.clone(),
            to: to.clone(),
        });
        // Keep any tags attached to the renamed path (and its subtree).
        let _ = self
            .tag_db_tx
            .send(atlas_core::fs::tag_db::TagDbCmd::RenamePath {
                old: from.clone(),
                new: to.clone(),
            });
        let op_id = self.state.next_op_id();
        let affected = self.affected_tabs_for_path(&from);
        let tx = self.tx.clone();
        #[cfg(windows)]
        let owner = self.hwnd;
        self.rt.spawn(async move {
            #[cfg(windows)]
            crate::shell_file_operation::rename_file(from, to, op_id, tx, affected, owner).await;
            #[cfg(not(windows))]
            atlas_core::fs::ops::rename_file(from, to, op_id, tx, affected).await;
        });
    }

    pub(crate) fn create_dir(&mut self, path: PathBuf) {
        if let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) {
            if let Err(e) = transfer::validate_filename(&name) {
                self.toast_error(filename_error_text(&e));
                return;
            }
        }
        if path.exists() {
            self.toast_error(
                rust_i18n::t!(
                    "err_collision",
                    name = path.file_name().unwrap_or_default().to_string_lossy()
                )
                .to_string(),
            );
            return;
        }
        let op_id = self.state.next_op_id();
        let affected = self.affected_tabs_for_path(&path);
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            atlas_core::fs::ops::create_dir(path, op_id, tx, affected).await;
        });
    }

    pub(crate) fn create_file(&mut self, path: PathBuf) {
        if let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) {
            if let Err(e) = transfer::validate_filename(&name) {
                self.toast_error(filename_error_text(&e));
                return;
            }
        }
        if path.exists() {
            self.toast_error(
                rust_i18n::t!(
                    "err_collision",
                    name = path.file_name().unwrap_or_default().to_string_lossy()
                )
                .to_string(),
            );
            return;
        }
        let op_id = self.state.next_op_id();
        let affected = self.affected_tabs_for_path(&path);
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            atlas_core::fs::ops::create_file(path, op_id, tx, affected).await;
        });
    }

    // -----------------------------------------------------------------------
    // Undo / redo
    // -----------------------------------------------------------------------

    pub(crate) fn undo(&mut self) {
        if let Some(entry) = self.state.ops.undo_stack.undo.pop_back() {
            match &entry {
                UndoEntry::Rename { from, to } => {
                    let from = from.clone();
                    let to = to.clone();
                    let op_id = self.state.next_op_id();
                    let affected = self.affected_tabs_for_path(&to);
                    let tx = self.tx.clone();
                    // Reverse: rename 'to' back to 'from'
                    #[cfg(windows)]
                    let owner = self.hwnd;
                    self.rt.spawn(async move {
                        #[cfg(windows)]
                        crate::shell_file_operation::rename_file(
                            to, from, op_id, tx, affected, owner,
                        )
                        .await;
                        #[cfg(not(windows))]
                        atlas_core::fs::ops::rename_file(to, from, op_id, tx, affected).await;
                    });
                }
                UndoEntry::Move { pairs } => {
                    // Reverse each executed pair exactly: dst goes back to src.
                    let rev = pairs.iter().map(|(src, dst)| (dst.clone(), src.clone()));
                    self.spawn_transfer_exact(rev.collect(), TransferKind::Move);
                }
                UndoEntry::Copy { pairs } => {
                    let created: Vec<PathBuf> = pairs.iter().map(|(_, dst)| dst.clone()).collect();
                    self.delete_files_opts(created, true, false);
                }
                UndoEntry::Trash { paths } => {
                    let paths_set: std::collections::HashSet<PathBuf> =
                        paths.iter().cloned().collect();
                    let recycled = atlas_core::fs::recycle_bin::read_recycle_bin(
                        &atlas_model::file_entry::SortColumn::Modified,
                        &atlas_model::file_entry::SortDir::Desc,
                    );
                    let mut items = Vec::new();
                    let mut affected = self.recycle_bin_tabs();
                    for entry in recycled {
                        if let Some(info) = &entry.recycle_info() {
                            if paths_set.contains(&info.original_path) {
                                affected.extend(self.affected_tabs_for_path(&info.original_path));
                                items.push((
                                    entry.path(),
                                    info.i_file.clone(),
                                    info.original_path.clone(),
                                ));
                            }
                        }
                    }
                    if !items.is_empty() {
                        let (op_id, _cancel) = self.state.ops.start_op_labeled(
                            items.len(),
                            rust_i18n::t!("op_restoring").to_string(),
                        );
                        let tx = self.tx.clone();
                        self.rt.spawn(async move {
                            atlas_core::fs::recycle_bin::restore_items(items, op_id, tx, affected)
                                .await;
                        });
                    }
                }
            }
            self.state.ops.undo_stack.redo.push_back(entry);
        }
    }

    pub(crate) fn redo(&mut self) {
        if let Some(entry) = self.state.ops.undo_stack.redo.pop_back() {
            match &entry {
                UndoEntry::Move { pairs } => {
                    self.spawn_transfer_exact(pairs.clone(), TransferKind::Move);
                }
                UndoEntry::Copy { pairs } => {
                    self.spawn_transfer_exact(pairs.clone(), TransferKind::Copy);
                }
                UndoEntry::Rename { from, to } => {
                    let op_id = self.state.next_op_id();
                    let affected = self.affected_tabs_for_path(from);
                    let tx = self.tx.clone();
                    let from = from.clone();
                    let to = to.clone();
                    #[cfg(windows)]
                    let owner = self.hwnd;
                    self.rt.spawn(async move {
                        #[cfg(windows)]
                        crate::shell_file_operation::rename_file(
                            from, to, op_id, tx, affected, owner,
                        )
                        .await;
                        #[cfg(not(windows))]
                        atlas_core::fs::ops::rename_file(from, to, op_id, tx, affected).await;
                    });
                }
                UndoEntry::Trash { paths } => {
                    self.delete_files_opts(paths.clone(), true, false);
                }
            }
            self.state.ops.undo_stack.undo.push_back(entry);
        }
    }
}
