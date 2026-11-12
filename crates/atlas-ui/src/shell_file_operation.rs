#![allow(unsafe_code, non_snake_case)]
//! Native Windows operations. Run on blocking workers, never GPUI callbacks.
//! Each call owns an STA and an item progress sink, so cancellation, per-item
//! HRESULTs, and Windows-selected collision names are not mistaken for success.
use atlas_model::ops::{OpError, OpErrorKind, Resolution, TransferKind};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use windows::core::Interface as _;
use windows::core::{implement, Ref, HRESULT, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::*;

/// Create a real Shell link without replacing a pre-existing destination.
pub(crate) fn create_shortcut(source: PathBuf, destination: PathBuf) -> Result<PathBuf, OpError> {
    let error_path = source.clone();
    std::thread::spawn(move || {
        let run = || -> Result<PathBuf, OpError> {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() }.map_err(|error| {
                OpError {
                    path: source.clone(),
                    kind: error_kind(&error),
                }
            })?;
            let _apartment = Apartment;
            let name = source
                .file_name()
                .map(std::ffi::OsStr::to_os_string)
                .unwrap_or_else(|| {
                    // A copied drive root has no file_name; a literal `C:\` is not
                    // a valid Windows filename for its link.
                    let label: String = source
                        .to_string_lossy()
                        .chars()
                        .filter(|ch| ch.is_alphanumeric())
                        .collect();
                    if label.is_empty() {
                        "Drive".into()
                    } else {
                        label.into()
                    }
                });
            let suffix = rust_i18n::t!("shortcut_filename_suffix");
            let mut counter = 1u32;
            let target = loop {
                let mut link_name = name.clone();
                link_name.push(format!(" - {suffix}"));
                if counter > 1 {
                    link_name.push(format!(" ({counter})"));
                }
                link_name.push(".lnk");
                let path = destination.join(link_name);
                match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                {
                    Ok(file) => {
                        drop(file);
                        break path;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        counter = counter.checked_add(1).ok_or_else(|| OpError {
                            path: path.clone(),
                            kind: OpErrorKind::Msg("Too many shortcuts".into()),
                        })?;
                    }
                    Err(error) => {
                        return Err(OpError {
                            path,
                            kind: OpErrorKind::Msg(error.to_string()),
                        })
                    }
                }
            };
            let save = || -> windows::core::Result<()> {
                unsafe {
                    let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_ALL)?;
                    link.SetPath(PCWSTR(wide(source.as_os_str()).as_ptr()))?;
                    if let Some(parent) = source.parent() {
                        link.SetWorkingDirectory(PCWSTR(wide(parent.as_os_str()).as_ptr()))?;
                    }
                    let persistence: windows::Win32::System::Com::IPersistFile = link.cast()?;
                    persistence.Save(PCWSTR(wide(target.as_os_str()).as_ptr()), true)
                }
            };
            if let Err(error) = save() {
                let _ = std::fs::remove_file(&target);
                return Err(OpError {
                    path: target,
                    kind: error_kind(&error),
                });
            }
            Ok(target)
        };
        run()
    })
    .join()
    .unwrap_or_else(|_| {
        Err(OpError {
            path: error_path,
            kind: OpErrorKind::Msg("Windows shortcut worker failed".into()),
        })
    })
}

#[derive(Debug, Clone)]
pub(crate) enum Operation {
    Copy {
        source: PathBuf,
        destination: PathBuf,
        conflict: Resolution,
    },
    Move {
        source: PathBuf,
        destination: PathBuf,
        conflict: Resolution,
    },
    Rename {
        source: PathBuf,
        destination: PathBuf,
    },
    Delete {
        source: PathBuf,
        recycle: bool,
    },
}

impl Operation {
    fn source(&self) -> &Path {
        match self {
            Self::Copy { source, .. }
            | Self::Move { source, .. }
            | Self::Rename { source, .. }
            | Self::Delete { source, .. } => source,
        }
    }
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn shell_path(item: &IShellItem) -> windows::core::Result<PathBuf> {
    unsafe {
        let value = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = std::ffi::OsString::from_wide(value.as_wide());
        CoTaskMemFree(Some(value.0.cast()));
        Ok(PathBuf::from(path))
    }
}

#[derive(Default)]
struct ItemResult {
    result: Option<HRESULT>,
    failure: Option<HRESULT>,
    destination: Option<PathBuf>,
}

#[implement(IFileOperationProgressSink)]
struct Progress {
    source: IShellItem,
    state: Arc<Mutex<ItemResult>>,
    cancel: Arc<AtomicBool>,
}
impl Progress {
    fn check(&self) -> windows::core::Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(windows::core::Error::from_hresult(
                COPYENGINE_E_USER_CANCELLED,
            ))
        } else {
            Ok(())
        }
    }
    fn record(&self, result: HRESULT, original: Ref<'_, IShellItem>, item: Ref<'_, IShellItem>) {
        if let Some(mut state) = self.state.lock().or_else(atlas_model::sync::recover).ok() {
            if result.is_err() || result == COPYENGINE_S_USER_IGNORED {
                state.failure.get_or_insert(result);
            }
            // Windows invokes the same sink for descendants. Keep the top-level
            // destination for Undo instead of the last copied child path. The
            // Shell may expand 8.3 names or change path casing in callbacks.
            if original.as_ref().is_some_and(|item| unsafe {
                item.Compare(&self.source, SICHINT_CANONICAL.0 as u32)
                    .is_ok_and(|order| order == 0)
            }) {
                state.result = Some(result);
                state.destination = item.as_ref().and_then(|item| shell_path(item).ok());
            }
        }
    }
}
impl IFileOperationProgressSink_Impl for Progress_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        self.check()
    }
    fn FinishOperations(&self, _result: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _name: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.check()
    }
    fn PostRenameItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _name: &PCWSTR,
        result: HRESULT,
        new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.record(result, _item, new_item);
        Ok(())
    }
    fn PreMoveItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _folder: Ref<'_, IShellItem>,
        _name: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.check()
    }
    fn PostMoveItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _folder: Ref<'_, IShellItem>,
        _name: &PCWSTR,
        result: HRESULT,
        new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.record(result, _item, new_item);
        Ok(())
    }
    fn PreCopyItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _folder: Ref<'_, IShellItem>,
        _name: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.check()
    }
    fn PostCopyItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        _folder: Ref<'_, IShellItem>,
        _name: &PCWSTR,
        result: HRESULT,
        new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.record(result, _item, new_item);
        Ok(())
    }
    fn PreDeleteItem(&self, _flags: u32, _item: Ref<'_, IShellItem>) -> windows::core::Result<()> {
        self.check()
    }
    fn PostDeleteItem(
        &self,
        _flags: u32,
        _item: Ref<'_, IShellItem>,
        result: HRESULT,
        new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.record(result, _item, new_item);
        Ok(())
    }
    fn PreNewItem(
        &self,
        _flags: u32,
        _folder: Ref<'_, IShellItem>,
        _name: &PCWSTR,
    ) -> windows::core::Result<()> {
        self.check()
    }
    fn PostNewItem(
        &self,
        _flags: u32,
        _folder: Ref<'_, IShellItem>,
        _name: &PCWSTR,
        _template: &PCWSTR,
        _attributes: u32,
        result: HRESULT,
        new_item: Ref<'_, IShellItem>,
    ) -> windows::core::Result<()> {
        self.record(result, _folder, new_item);
        Ok(())
    }
    fn UpdateProgress(&self, _total: u32, _sofar: u32) -> windows::core::Result<()> {
        self.check()
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

fn error_kind(error: &windows::core::Error) -> OpErrorKind {
    match error.code().0 as u32 {
        0x800704C7 | 0x80270000 | 0x80270001 | 0x80004004 => OpErrorKind::Cancelled,
        code if code >> 16 == 0x8007 => atlas_core::fs::transfer::io_error_kind(
            &std::io::Error::from_raw_os_error((code & 0xffff) as i32),
        ),
        _ => OpErrorKind::Msg(error.to_string()),
    }
}

/// Windows supplies UAC UI and an Explorer undo record for operations that can
/// be undone. Permanent delete intentionally omits both recycle and undo flags.
/// `interactive=false` prohibits elevation/dialogs and is used only by fixtures.
pub(crate) fn execute(
    operation: Operation,
    owner: Option<isize>,
    cancel: Arc<AtomicBool>,
    interactive: bool,
) -> Result<Option<PathBuf>, OpError> {
    let source = operation.source().to_path_buf();
    // Tokio's pool can reuse a thread initialized as MTA by another subsystem.
    // A fresh thread guarantees STA ownership and keeps COM objects/apartment
    // teardown together; no shell interface crosses a thread boundary.
    std::thread::Builder::new()
        .name("atlas-shell-file-operation".into())
        .spawn(move || execute_sta(operation, owner, cancel, interactive))
        .map_err(|error| OpError {
            path: source.clone(),
            kind: OpErrorKind::Msg(error.to_string()),
        })?
        .join()
        .unwrap_or_else(|_| {
            Err(OpError {
                path: source,
                kind: OpErrorKind::Msg("Windows shell operation worker failed".into()),
            })
        })
}

fn execute_sta(
    operation: Operation,
    owner: Option<isize>,
    cancel: Arc<AtomicBool>,
    interactive: bool,
) -> Result<Option<PathBuf>, OpError> {
    let source = operation.source().to_path_buf();
    let result = (|| -> windows::core::Result<Option<PathBuf>> {
        if cancel.load(Ordering::Relaxed) {
            return Err(windows::core::Error::from_hresult(
                COPYENGINE_E_USER_CANCELLED,
            ));
        }
        // A fresh blocking thread can enter an STA; changed apartment mode is a
        // real error, never silently followed by an unbalanced CoUninitialize.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        let _apartment = Apartment;
        let operation_api: IFileOperation =
            unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL)? };
        let mut flags = FOF_NOCONFIRMMKDIR | FOFX_EARLYFAILURE;
        let private = atlas_core::vault::live::is_private(operation.source())
            || match &operation {
                Operation::Copy { destination, .. }
                | Operation::Move { destination, .. }
                | Operation::Rename { destination, .. } => {
                    atlas_core::vault::live::is_private(destination)
                }
                Operation::Delete { .. } => false,
            };
        if !private && !matches!(operation, Operation::Delete { recycle: false, .. }) {
            flags |= FOF_ALLOWUNDO | FOFX_ADDUNDORECORD;
        }
        if matches!(operation, Operation::Delete { recycle: true, .. }) {
            flags |= FOFX_RECYCLEONDELETE | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING;
        }
        if interactive {
            flags |= FOFX_SHOWELEVATIONPROMPT;
        } else {
            flags |= FOF_SILENT | FOF_NOERRORUI | FOF_NOCONFIRMATION;
        }
        match &operation {
            Operation::Copy {
                conflict: Resolution::Rename,
                ..
            }
            | Operation::Move {
                conflict: Resolution::Rename,
                ..
            } => flags |= FOF_RENAMEONCOLLISION,
            Operation::Copy {
                conflict: Resolution::Overwrite,
                ..
            }
            | Operation::Move {
                conflict: Resolution::Overwrite,
                ..
            } => flags |= FOF_NOCONFIRMATION,
            _ => {}
        }
        unsafe {
            operation_api.SetOperationFlags(flags)?;
            if let Some(owner) = owner {
                operation_api.SetOwnerWindow(HWND(owner as *mut _))?;
            }
        }
        let source_wide = wide(source.as_os_str());
        let item: IShellItem =
            unsafe { SHCreateItemFromParsingName(PCWSTR(source_wide.as_ptr()), None)? };
        let state = Arc::new(Mutex::new(ItemResult::default()));
        let sink: IFileOperationProgressSink = Progress {
            source: item.clone(),
            state: state.clone(),
            cancel,
        }
        .into();
        unsafe {
            let copying = matches!(operation, Operation::Copy { .. });
            match operation {
                Operation::Copy {
                    destination,
                    conflict,
                    ..
                }
                | Operation::Move {
                    destination,
                    conflict,
                    ..
                } => {
                    if conflict == Resolution::Skip && destination.exists() {
                        return Err(windows::core::Error::from_hresult(HRESULT::from_win32(183)));
                    }
                    let parent = destination.parent().ok_or_else(|| {
                        windows::core::Error::from_hresult(HRESULT::from_win32(3))
                    })?;
                    let folder_wide = wide(parent.as_os_str());
                    let folder: IShellItem =
                        SHCreateItemFromParsingName(PCWSTR(folder_wide.as_ptr()), None)?;
                    let name = wide(destination.file_name().unwrap_or_default());
                    if copying {
                        operation_api.CopyItem(&item, &folder, PCWSTR(name.as_ptr()), &sink)?;
                    } else {
                        operation_api.MoveItem(&item, &folder, PCWSTR(name.as_ptr()), &sink)?;
                    }
                }
                Operation::Rename { destination, .. } => {
                    if source.parent() != destination.parent() {
                        return Err(windows::core::Error::from_hresult(HRESULT::from_win32(87)));
                    }
                    let name = wide(destination.file_name().unwrap_or_default());
                    operation_api.RenameItem(&item, PCWSTR(name.as_ptr()), &sink)?;
                }
                Operation::Delete { .. } => operation_api.DeleteItem(&item, &sink)?,
            }
            operation_api.PerformOperations()?;
            if operation_api.GetAnyOperationsAborted()?.as_bool() {
                return Err(windows::core::Error::from_hresult(
                    COPYENGINE_E_USER_CANCELLED,
                ));
            }
        }
        let state = state
            .lock()
            .map_err(|_| windows::core::Error::from_hresult(HRESULT::from_win32(1)))?;
        let result = state
            .failure
            .or(state.result)
            .ok_or_else(|| windows::core::Error::from_hresult(HRESULT::from_win32(1)))?;
        result.ok()?;
        if result == COPYENGINE_S_USER_IGNORED {
            return Err(windows::core::Error::from_hresult(
                COPYENGINE_E_USER_CANCELLED,
            ));
        }
        Ok(state.destination.clone())
    })();
    result.map_err(|error| OpError {
        path: source,
        kind: error_kind(&error),
    })
}

pub(crate) fn transfer_operation(
    pair: &atlas_core::fs::transfer::TransferPair,
    kind: TransferKind,
) -> Operation {
    if kind == TransferKind::Copy {
        Operation::Copy {
            source: pair.src.clone(),
            destination: pair.dst.clone(),
            conflict: pair.on_conflict,
        }
    } else {
        Operation::Move {
            source: pair.src.clone(),
            destination: pair.dst.clone(),
            conflict: pair.on_conflict,
        }
    }
}

/// Preserve the optimized/journaled transfer engine. Escalate only a denied
/// whole item that has not already produced a destination or partial subtree.
pub(crate) async fn transfer_with_fallback(
    pairs: Vec<atlas_core::fs::transfer::TransferPair>,
    kind: TransferKind,
    origin: atlas_model::ops::TransferOrigin,
    id: u64,
    tx: flume::Sender<atlas_model::app_state::BgResult>,
    cancel: Arc<AtomicBool>,
    affected_tabs: Vec<atlas_model::pane::TabId>,
    owner: Option<isize>,
) {
    use atlas_model::app_state::BgResult;
    let original = pairs.clone();
    let (bridge, rx) = flume::unbounded();
    let core = atlas_core::fs::transfer::execute(
        pairs,
        kind,
        origin,
        id,
        bridge,
        cancel.clone(),
        affected_tabs,
    );
    let forward = async move {
        while let Ok(event) = rx.recv_async().await {
            if let BgResult::TransferComplete {
                id,
                kind,
                origin,
                mut report,
                affected_tabs,
            } = event
            {
                if !report.cancelled && !cancel.load(Ordering::Relaxed) {
                    for pair in &original {
                        if !report.errors.iter().any(|error| {
                            (error.path == pair.src || error.path == pair.dst)
                                && error.kind == OpErrorKind::PermissionDenied
                        }) || pair.dst.exists()
                        {
                            continue;
                        }
                        let operation = transfer_operation(pair, kind);
                        let flag = cancel.clone();
                        match tokio::task::spawn_blocking(move || {
                            execute(operation, owner, flag, true)
                        })
                        .await
                        {
                            Ok(Ok(Some(destination))) => {
                                report.errors.retain(|error| {
                                    !((error.path == pair.src || error.path == pair.dst)
                                        && error.kind == OpErrorKind::PermissionDenied)
                                });
                                report.completed.push((pair.src.clone(), destination));
                            }
                            Ok(Ok(None)) => {}
                            Ok(Err(error)) => {
                                report.errors.retain(|error| {
                                    !((error.path == pair.src || error.path == pair.dst)
                                        && error.kind == OpErrorKind::PermissionDenied)
                                });
                                report.cancelled |= error.kind == OpErrorKind::Cancelled;
                                report.errors.push(error);
                            }
                            Err(error) => report.errors.push(OpError {
                                path: pair.src.clone(),
                                kind: OpErrorKind::Msg(error.to_string()),
                            }),
                        }
                        if report.cancelled {
                            break;
                        }
                    }
                }
                let _ = tx.send(BgResult::TransferComplete {
                    id,
                    kind,
                    origin,
                    report,
                    affected_tabs,
                });
                break;
            }
            if tx.send(event).is_err() {
                break;
            }
        }
    };
    tokio::join!(core, forward);
}

pub(crate) async fn delete_files(
    paths: Vec<PathBuf>,
    recycle: bool,
    id: u64,
    tx: flume::Sender<atlas_model::app_state::BgResult>,
    cancel: Arc<AtomicBool>,
    affected_tabs: Vec<atlas_model::pane::TabId>,
    owner: Option<isize>,
) {
    use atlas_model::app_state::BgResult;
    let mut errors = Vec::new();
    let mut completed = Vec::new();
    let total = paths.len() as u64;
    for (index, path) in paths.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            errors.push(OpError {
                path,
                kind: OpErrorKind::Cancelled,
            });
            break;
        }
        if recycle && atlas_core::fs::paths::wsl_distro_name(&path).is_some() {
            errors.push(OpError {
                path,
                kind: OpErrorKind::TrashUnsupported,
            });
            continue;
        }
        let _ = tx.send(BgResult::OpProgress {
            id,
            done: index as u64,
            total,
            bytes_done: 0,
            bytes_total: 0,
            current_file: path.clone(),
        });
        let operation = Operation::Delete {
            source: path.clone(),
            recycle,
        };
        let flag = cancel.clone();
        let result =
            tokio::task::spawn_blocking(move || execute(operation, owner, flag, true)).await;
        match result {
            Ok(Ok(_)) => completed.push(path),
            Ok(Err(error)) => {
                let cancelled = error.kind == OpErrorKind::Cancelled;
                errors.push(error);
                if cancelled {
                    break;
                }
            }
            Err(error) => errors.push(OpError {
                path,
                kind: OpErrorKind::Msg(error.to_string()),
            }),
        }
    }
    if !completed.is_empty() {
        let _ = tx.send(BgResult::PathsDeleted(completed));
    }
    let _ = tx.send(BgResult::OpComplete {
        id,
        result: if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        },
        affected_tabs,
    });
}

pub(crate) async fn rename_file(
    from: PathBuf,
    to: PathBuf,
    id: u64,
    tx: flume::Sender<atlas_model::app_state::BgResult>,
    affected_tabs: Vec<atlas_model::pane::TabId>,
    owner: Option<isize>,
) {
    let operation = Operation::Rename {
        source: from.clone(),
        destination: to,
    };
    let result = tokio::task::spawn_blocking(move || {
        execute(operation, owner, Arc::new(AtomicBool::new(false)), true)
    })
    .await;
    let result = match result {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(vec![error]),
        Err(error) => Err(vec![OpError {
            path: from,
            kind: OpErrorKind::Msg(error.to_string()),
        }]),
    };
    let _ = tx.send(atlas_model::app_state::BgResult::OpComplete {
        id,
        result,
        affected_tabs,
    });
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    #[test]
    fn shortcut_collision_preserves_existing_link_and_resolves_target() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source with spaces.txt");
        std::fs::write(&source, b"shortcut target").unwrap();
        let first = create_shortcut(source.clone(), fixture.0.clone()).unwrap();
        let bytes = std::fs::read(&first).unwrap();
        let second = create_shortcut(source.clone(), fixture.0.clone()).unwrap();
        assert_ne!(first, second);
        assert!(second
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(" (2).lnk"));
        assert_eq!(std::fs::read(&first).unwrap(), bytes);
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap();
            let _apartment = Apartment;
            for path in [first, second] {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_ALL).unwrap();
                let persistence: windows::Win32::System::Com::IPersistFile = link.cast().unwrap();
                persistence
                    .Load(
                        PCWSTR(wide(path.as_os_str()).as_ptr()),
                        windows::Win32::System::Com::STGM_READ,
                    )
                    .unwrap();
                let mut buffer = [0u16; 32768];
                link.GetPath(&mut buffer, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
                    .unwrap();
                let length = buffer.iter().position(|&unit| unit == 0).unwrap();
                let resolved = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
                assert_eq!(resolved, source);
            }
        }
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "atlas-ifileoperation-{}-{stamp}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            // Shell results use long paths even when TEMP contains RUNNER~1.
            let mut buffer = [0u16; 32768];
            let length = unsafe {
                windows::Win32::Storage::FileSystem::GetLongPathNameW(
                    PCWSTR(wide(path.as_os_str()).as_ptr()),
                    Some(&mut buffer),
                )
            } as usize;
            assert!(length > 0 && length < buffer.len());
            Self(PathBuf::from(std::ffi::OsString::from_wide(
                &buffer[..length],
            )))
        }
        fn run(&self, operation: Operation) -> Option<PathBuf> {
            assert!(
                operation.source().starts_with(&self.0),
                "Fixture source escaped temporary tree"
            );
            execute(operation, None, Arc::new(AtomicBool::new(false)), false).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn native_copy_accepts_short_source_paths_and_keeps_top_level_result() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source with a long filename.txt");
        std::fs::write(&source, b"short-path fixture").unwrap();
        let mut buffer = [0u16; 32768];
        let length = unsafe {
            windows::Win32::Storage::FileSystem::GetShortPathNameW(
                PCWSTR(wide(source.as_os_str()).as_ptr()),
                Some(&mut buffer),
            )
        } as usize;
        assert!(length > 0 && length < buffer.len());
        let short = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
        let destination = fixture.0.join("short-path-copy.txt");
        assert_eq!(
            execute(
                Operation::Copy {
                    source: short,
                    destination: destination.clone(),
                    conflict: Resolution::Rename,
                },
                None,
                Arc::new(AtomicBool::new(false)),
                false,
            )
            .unwrap(),
            Some(destination.clone())
        );
        assert_eq!(std::fs::read(destination).unwrap(), b"short-path fixture");
        assert!(source.exists());
    }

    #[test]
    fn native_copy_move_rename_delete_and_collision_use_real_windows_shell() {
        let fixture = Fixture::new();
        let root = &fixture.0;
        let source = root.join("source.txt");
        std::fs::write(&source, b"native fixture").unwrap();
        let copied = root.join("copy.txt");
        assert_eq!(
            fixture.run(Operation::Copy {
                source: source.clone(),
                destination: copied.clone(),
                conflict: Resolution::Rename
            }),
            Some(copied.clone())
        );
        assert_eq!(std::fs::read(&copied).unwrap(), b"native fixture");
        let collision = fixture
            .run(Operation::Copy {
                source: source.clone(),
                destination: copied.clone(),
                conflict: Resolution::Rename,
            })
            .unwrap();
        assert_ne!(collision, copied);
        assert!(collision.starts_with(root));
        assert_eq!(std::fs::read(&collision).unwrap(), b"native fixture");
        let moved = root.join("moved.txt");
        assert_eq!(
            fixture.run(Operation::Move {
                source: copied.clone(),
                destination: moved.clone(),
                conflict: Resolution::Rename
            }),
            Some(moved.clone())
        );
        assert!(!copied.exists());
        assert!(moved.exists());
        let renamed = root.join("renamed.txt");
        assert_eq!(
            fixture.run(Operation::Rename {
                source: moved.clone(),
                destination: renamed.clone()
            }),
            Some(renamed.clone())
        );
        assert!(!moved.exists());
        assert!(renamed.exists());
        fixture.run(Operation::Delete {
            source: renamed.clone(),
            recycle: false,
        });
        assert!(!renamed.exists());
        assert!(source.exists());
    }
    #[test]
    fn native_precancel_preserves_source_and_reports_cancellation() {
        let fixture = Fixture::new();
        let path = fixture.0.join("preserve.txt");
        std::fs::write(&path, b"preserve").unwrap();
        let error = execute(
            Operation::Delete {
                source: path.clone(),
                recycle: false,
            },
            None,
            Arc::new(AtomicBool::new(true)),
            false,
        )
        .unwrap_err();
        assert_eq!(error.kind, OpErrorKind::Cancelled);
        assert!(path.exists());
    }

    #[test]
    fn native_shell_owns_sta_when_calling_worker_is_already_mta() {
        unsafe {
            CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED)
                .ok()
                .unwrap();
        }
        let _apartment = Apartment;
        let fixture = Fixture::new();
        let source = fixture.0.join("mta-source.txt");
        let destination = fixture.0.join("sta-copy.txt");
        std::fs::write(&source, b"apartment-safe fixture").unwrap();
        assert_eq!(
            fixture.run(Operation::Copy {
                source,
                destination: destination.clone(),
                conflict: Resolution::Rename
            }),
            Some(destination)
        );
    }
    #[test]
    fn native_skip_collision_preserves_both_files() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source.txt");
        let destination = fixture.0.join("target.txt");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&destination, b"target").unwrap();
        let error = execute(
            Operation::Copy {
                source: source.clone(),
                destination: destination.clone(),
                conflict: Resolution::Skip,
            },
            None,
            Arc::new(AtomicBool::new(false)),
            false,
        )
        .unwrap_err();
        assert_eq!(error.kind, OpErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&source).unwrap(), b"source");
        assert_eq!(std::fs::read(&destination).unwrap(), b"target");
    }

    #[test]
    fn native_directory_copy_recurses_and_overwrite_obeys_resolved_policy() {
        let fixture = Fixture::new();
        let source = fixture.0.join("folder");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(source.join("nested/Unicode-ñ.txt"), b"directory fixture").unwrap();
        let destination = fixture.0.join("folder-copy");
        assert_eq!(
            fixture.run(Operation::Copy {
                source: source.clone(),
                destination: destination.clone(),
                conflict: Resolution::Rename
            }),
            Some(destination.clone())
        );
        assert_eq!(
            std::fs::read(destination.join("nested/Unicode-ñ.txt")).unwrap(),
            b"directory fixture"
        );
        let original = fixture.0.join("original.txt");
        let replace = fixture.0.join("replace.txt");
        std::fs::write(&original, b"new value").unwrap();
        std::fs::write(&replace, b"old value").unwrap();
        assert_eq!(
            fixture.run(Operation::Copy {
                source: original,
                destination: replace.clone(),
                conflict: Resolution::Overwrite
            }),
            Some(replace.clone())
        );
        assert_eq!(std::fs::read(replace).unwrap(), b"new value");
        fixture.run(Operation::Delete {
            source: destination.clone(),
            recycle: false,
        });
        assert!(!destination.exists());
        assert!(source.exists());
    }

    #[test]
    fn native_trash_returns_fixture_payload_and_restores_only_that_item() {
        let fixture = Fixture::new();
        let source = fixture.0.join("trash-fixture.txt");
        std::fs::write(&source, b"fixture recycle and restore").unwrap();
        let recycled = fixture
            .run(Operation::Delete {
                source: source.clone(),
                recycle: true,
            })
            .expect("Shell did not report recycled payload");
        assert!(!source.exists());
        assert_eq!(
            std::fs::read(&recycled).unwrap(),
            b"fixture recycle and restore"
        );
        let name = recycled.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("$R"),
            "Unexpected recycle payload format: {recycled:?}"
        );
        let metadata = recycled.with_file_name(format!("$I{}", &name[2..]));
        assert!(metadata.exists());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, rx) = flume::unbounded();
        rt.block_on(atlas_core::fs::recycle_bin::restore_items(
            vec![(recycled.clone(), metadata.clone(), source.clone())],
            1,
            tx,
            vec![],
        ));
        assert!(rx.try_iter().any(|event| matches!(
            event,
            atlas_model::app_state::BgResult::OpComplete { result: Ok(()), .. }
        )));
        assert_eq!(
            std::fs::read(source).unwrap(),
            b"fixture recycle and restore"
        );
        assert!(!recycled.exists());
        assert!(!metadata.exists());
    }
}
