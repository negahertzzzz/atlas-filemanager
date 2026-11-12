//! Windows virtual folder. Only ciphertext is persisted by filesystem callbacks.
use super::{live, Entry, Session};
use anyhow::Result;
use std::{
    ffi::c_void,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::Instant,
};
use windows::Win32::Foundation::*;
use winfsp::{
    filesystem::*,
    host::{FileSystemHost, VolumeParams},
    U16CStr,
};

struct Shared {
    session: Mutex<Option<Session>>,
    files: AtomicUsize,
    closing: AtomicBool,
    handles: Mutex<std::collections::HashMap<String, usize>>,
    activity: Mutex<Instant>,
}
struct PrivateFs(Arc<Shared>);
pub struct Mounted {
    host: Option<FileSystemHost<PrivateFs>>,
    shared: Arc<Shared>,
    path: PathBuf,
    root: PathBuf,
    name: String,
}
pub struct Handle {
    id: Option<String>,
    directory: bool,
    buffer: DirBuffer,
}

impl Mounted {
    pub fn start(session: Session) -> std::result::Result<Self, (Session, String)> {
        if let Err(error) = atlas_win_space::vault_driver::load() {
            return Err((session, error.to_string()));
        }
        // The installed library was loaded by absolute path, before delayed imports are called.
        if winfsp::winfsp_init().is_err() {
            return Err((session, "vault_driver_missing".into()));
        }
        let root = session.root().to_path_buf();
        let name = session.name().to_owned();
        let path = crate::util::config::data_dir()
            .join(live::NAMESPACE)
            .join(uuid::Uuid::new_v4().simple().to_string());
        let _ = session.collect_orphans();
        let shared = Arc::new(Shared {
            session: Mutex::new(Some(session)),
            files: AtomicUsize::new(0),
            closing: AtomicBool::new(false),
            handles: Mutex::new(Default::default()),
            activity: Mutex::new(Instant::now()),
        });
        let result = (|| -> Result<FileSystemHost<PrivateFs>> {
            std::fs::create_dir_all(path.parent().unwrap())?;
            let mut params = VolumeParams::new();
            params
                .filesystem_name("AtlasPrivate")
                .sector_size(4096)
                .sectors_per_allocation_unit(1)
                .max_component_length(255)
                .case_sensitive_search(false)
                .case_preserved_names(true)
                .unicode_on_disk(true)
                .persistent_acls(false)
                .reparse_points(false)
                .named_streams(false)
                .file_info_timeout(0)
                .flush_and_purge_on_cleanup(true);
            let mut host = FileSystemHost::<PrivateFs>::new(params, PrivateFs(shared.clone()))?;
            host.mount(&path)?;
            host.start()?;
            Ok(host)
        })();
        match result {
            Ok(host) => {
                live::register(path.clone(), name.clone());
                Ok(Self {
                    host: Some(host),
                    shared,
                    path,
                    root,
                    name,
                })
            }
            Err(error) => Err((
                shared
                    .session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .unwrap(),
                format!("vault_mount_failed: {error:#}"),
            )),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn has_open_files(&self) -> bool {
        self.shared.files.load(Ordering::Acquire) != 0
    }
    pub fn prepare_lock(&self) -> bool {
        let _guard = self
            .shared
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.shared.closing.store(true, Ordering::Release);
        if self.has_open_files() {
            self.shared.closing.store(false, Ordering::Release);
            false
        } else {
            true
        }
    }
    pub fn idle_for(&self) -> std::time::Duration {
        self.shared
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .elapsed()
    }
    pub fn touch(&self) {
        *self
            .shared
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }
    pub fn into_session(mut self) -> Session {
        self.detach();
        self.shared
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap()
    }
    fn detach(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        if let Some(mut host) = self.host.take() {
            host.unmount();
            host.stop();
            drop(host);
        }
        live::unregister(&self.path);
        // WinFsp removes its reparse mount point. Remove only the empty residue.
        let _ = std::fs::remove_dir(&self.path);
    }
}
impl Drop for Mounted {
    fn drop(&mut self) {
        self.detach();
        self.shared
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

/// Explicit UI action only. Pin the official release and checksum before MSI execution.
pub async fn install_driver() -> Result<()> {
    let code = crate::util::winfsp::install(|_, _| {})
        .await
        .map_err(|error| anyhow::anyhow!("vault_driver_install_failed: {error}"))?;
    if matches!(code, Some(1641 | 3010)) {
        anyhow::bail!(
            "WinFsp was installed. Restart Windows, then open Atlas and unlock this folder again."
        );
    }
    if code == Some(1602) {
        anyhow::bail!("WinFsp installation was cancelled. Your encrypted folder is unchanged; you can retry later.");
    }
    anyhow::ensure!(
        code == Some(0),
        "vault_driver_install_failed: {}",
        code.unwrap_or(-1)
    );
    Ok(())
}

impl PrivateFs {
    fn session(&self) -> winfsp::Result<MutexGuard<'_, Option<Session>>> {
        let session = self.0.session.lock().map_err(|_| STATUS_DEVICE_NOT_READY)?;
        if session.is_none() || self.0.closing.load(Ordering::Acquire) {
            return Err(STATUS_DEVICE_NOT_READY.into());
        }
        Ok(session)
    }
    fn handle(&self, id: Option<String>, directory: bool) -> Handle {
        if let Some(id) = &id {
            *self
                .0
                .handles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(id.clone())
                .or_default() += 1;
        }
        if !directory {
            self.0.files.fetch_add(1, Ordering::AcqRel);
        }
        Handle {
            id,
            directory,
            buffer: DirBuffer::new(),
        }
    }
}
fn mapped<T>(result: anyhow::Result<T>) -> winfsp::Result<T> {
    result.map_err(|error| {
        let msg = format!("{error:#}");
        let status = if msg.contains("vault_missing_entry") {
            STATUS_OBJECT_NAME_NOT_FOUND
        } else if msg.contains("vault_name_exists") {
            STATUS_OBJECT_NAME_COLLISION
        } else if msg.contains("vault_directory_not_empty") {
            STATUS_DIRECTORY_NOT_EMPTY
        } else if msg.contains("vault_damaged") {
            STATUS_DATA_ERROR
        } else if msg.contains("vault_invalid") || msg.contains("vault_not_file") {
            STATUS_OBJECT_NAME_INVALID
        } else {
            STATUS_ACCESS_DENIED
        };
        status.into()
    })
}
fn info(entry: Option<&Entry>) -> FileInfo {
    let stamp = entry
        .map_or(0, |e| e.modified)
        .saturating_add(11_644_473_600)
        .saturating_mul(10_000_000);
    let directory = entry.is_none_or(|e| e.directory);
    let size = entry.map_or(0, |e| e.size);
    FileInfo {
        file_attributes: 0x2000
            | (if directory { 0x10 } else { 0x20 })
            | entry.map_or(0, |e| e.attributes & 0x27),
        file_size: size,
        allocation_size: size.div_ceil(4096) * 4096,
        creation_time: stamp,
        last_write_time: stamp,
        last_access_time: stamp,
        change_time: stamp,
        index_number: entry
            .and_then(|e| u64::from_str_radix(&e.id[..16], 16).ok())
            .unwrap_or(1),
        ..Default::default()
    }
}
fn resolve(s: &Session, path: &U16CStr) -> winfsp::Result<Option<String>> {
    mapped(s.resolve(Path::new(&path.to_string_lossy())))
}
fn split_destination(s: &Session, path: &U16CStr) -> winfsp::Result<(Option<String>, String)> {
    let value = path.to_string_lossy();
    let path = Path::new(&value);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(STATUS_OBJECT_NAME_INVALID)?
        .to_owned();
    let parent = mapped(s.resolve(path.parent().unwrap_or(Path::new("\\"))))?;
    Ok((parent, name))
}

impl FileSystemContext for PrivateFs {
    type FileContext = Handle;
    fn get_security_by_name(
        &self,
        name: &U16CStr,
        _: Option<&mut [c_void]>,
        _: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let guard = self.session()?;
        let s = guard.as_ref().unwrap();
        let id = resolve(s, name)?;
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: 0,
            attributes: info(id.as_deref().and_then(|id| s.entry(id))).file_attributes,
        })
    }
    fn open(
        &self,
        name: &U16CStr,
        options: u32,
        _: u32,
        out: &mut OpenFileInfo,
    ) -> winfsp::Result<Handle> {
        *self
            .0
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        let guard = self.session()?;
        let s = guard.as_ref().unwrap();
        let id = resolve(s, name)?;
        let file = info(id.as_deref().and_then(|id| s.entry(id)));
        let directory = file.file_attributes & 0x10 != 0;
        if options & 1 != 0 && !directory {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        if options & 0x40 != 0 && directory {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        *out.as_mut() = file;
        Ok(self.handle(id, directory))
    }
    fn close(&self, handle: Handle) {
        let last = if let Some(id) = &handle.id {
            let mut handles = self
                .0
                .handles
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(count) = handles.get_mut(id) {
                *count = count.saturating_sub(1);
            }
            if handles.get(id) == Some(&0) {
                handles.remove(id);
                true
            } else {
                false
            }
        } else {
            false
        };
        if last {
            if let (Some(id), Ok(mut guard)) = (&handle.id, self.session()) {
                guard.as_mut().unwrap().release_entry(id);
            }
        }
        if !handle.directory {
            self.0.files.fetch_sub(1, Ordering::AcqRel);
        }
    }
    fn create(
        &self,
        name: &U16CStr,
        options: u32,
        _: u32,
        attrs: u32,
        _: Option<&[c_void]>,
        _: u64,
        _: Option<&[u8]>,
        reparse: bool,
        out: &mut OpenFileInfo,
    ) -> winfsp::Result<Handle> {
        if reparse {
            return Err(STATUS_ACCESS_DENIED.into());
        }
        let mut guard = self.session()?;
        let s = guard.as_mut().unwrap();
        let (parent, name) = split_destination(s, name)?;
        let directory = options & 1 != 0;
        let id = mapped(s.create_entry(parent.as_deref(), &name, directory))?;
        if attrs != 0 {
            mapped(s.set_metadata(&id, attrs, None))?;
        }
        *out.as_mut() = info(s.entry(&id));
        Ok(self.handle(Some(id), directory))
    }
    fn read(&self, handle: &Handle, buffer: &mut [u8], offset: u64) -> winfsp::Result<u32> {
        *self
            .0
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        let guard = self.session()?;
        let s = guard.as_ref().unwrap();
        let id = handle.id.as_deref().ok_or(STATUS_FILE_IS_A_DIRECTORY)?;
        Ok(mapped(s.read_range(id, offset, buffer))? as u32)
    }
    fn write(
        &self,
        handle: &Handle,
        buffer: &[u8],
        mut offset: u64,
        eof: bool,
        constrained: bool,
        out: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        *self
            .0
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        let mut guard = self.session()?;
        let s = guard.as_mut().unwrap();
        let id = handle.id.as_deref().ok_or(STATUS_FILE_IS_A_DIRECTORY)?;
        let size = s.entry(id).ok_or(STATUS_OBJECT_NAME_NOT_FOUND)?.size;
        if eof {
            offset = size;
        }
        let n = if constrained {
            buffer.len().min(size.saturating_sub(offset) as usize)
        } else {
            buffer.len()
        };
        mapped(s.write_at(id, offset, &buffer[..n]))?;
        *out = info(s.entry(id));
        Ok(n as u32)
    }
    fn get_file_info(&self, handle: &Handle, out: &mut FileInfo) -> winfsp::Result<()> {
        let guard = self.session()?;
        let s = guard.as_ref().unwrap();
        if handle.id.as_deref().is_some_and(|id| s.entry(id).is_none()) {
            return Err(STATUS_OBJECT_NAME_NOT_FOUND.into());
        }
        *out = info(handle.id.as_deref().and_then(|id| s.entry(id)));
        Ok(())
    }
    fn get_volume_info(&self, out: &mut VolumeInfo) -> winfsp::Result<()> {
        let guard = self.session()?;
        let s = guard.as_ref().unwrap();
        out.total_size = fs2::total_space(s.root()).unwrap_or(0);
        out.free_size = fs2::available_space(s.root()).unwrap_or(0);
        out.set_volume_label("Atlas Private");
        Ok(())
    }
    fn flush(&self, handle: Option<&Handle>, out: &mut FileInfo) -> winfsp::Result<()> {
        // Every write acknowledges only after encrypted objects and manifest are synced.
        if let Some(handle) = handle {
            self.get_file_info(handle, out)?;
        }
        Ok(())
    }
    fn read_directory(
        &self,
        handle: &Handle,
        _: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        *self
            .0
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
        if !handle.directory {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        if let Ok(lock) = handle.buffer.acquire(marker.is_none(), None) {
            let guard = self.session()?;
            let s = guard.as_ref().unwrap();
            for entry in s.children(handle.id.as_deref()) {
                let mut dir = DirInfo::<1024>::new();
                *dir.file_info_mut() = info(Some(&entry));
                dir.set_name(&entry.name)?;
                lock.write(&mut dir)?;
            }
        }
        Ok(handle.buffer.read(marker, buffer))
    }
    fn set_file_size(
        &self,
        handle: &Handle,
        size: u64,
        allocation: bool,
        out: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let mut guard = self.session()?;
        let s = guard.as_mut().unwrap();
        let id = handle.id.as_deref().ok_or(STATUS_FILE_IS_A_DIRECTORY)?;
        if !allocation || size < s.entry(id).ok_or(STATUS_OBJECT_NAME_NOT_FOUND)?.size {
            mapped(s.resize(id, size))?;
        }
        *out = info(s.entry(id));
        Ok(())
    }
    fn overwrite(
        &self,
        handle: &Handle,
        attrs: u32,
        _: bool,
        _: u64,
        _: Option<&[u8]>,
        out: &mut FileInfo,
    ) -> winfsp::Result<()> {
        self.set_file_size(handle, 0, false, out)?;
        self.set_basic_info(handle, attrs, 0, 0, 0, 0, out)
    }
    fn set_basic_info(
        &self,
        handle: &Handle,
        attrs: u32,
        _: u64,
        _: u64,
        written: u64,
        _: u64,
        out: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let mut guard = self.session()?;
        let s = guard.as_mut().unwrap();
        if let Some(id) = &handle.id {
            let attrs = if attrs == u32::MAX {
                s.entry(id).ok_or(STATUS_OBJECT_NAME_NOT_FOUND)?.attributes
            } else {
                attrs
            };
            let modified = if written > 116_444_736_000_000_000 {
                Some(written / 10_000_000 - 11_644_473_600)
            } else {
                None
            };
            mapped(s.set_metadata(id, attrs, modified))?;
        }
        *out = info(handle.id.as_deref().and_then(|id| s.entry(id)));
        Ok(())
    }
    fn rename(
        &self,
        handle: &Handle,
        _: &U16CStr,
        new_name: &U16CStr,
        replace: bool,
    ) -> winfsp::Result<()> {
        let mut guard = self.session()?;
        let s = guard.as_mut().unwrap();
        let id = handle.id.as_deref().ok_or(STATUS_ACCESS_DENIED)?;
        let (parent, name) = split_destination(s, new_name)?;
        mapped(s.rename_entry(id, parent.as_deref(), &name, replace))
    }
    fn set_delete(&self, handle: &Handle, _: &U16CStr, delete: bool) -> winfsp::Result<()> {
        if handle.id.is_none() {
            return Err(STATUS_ACCESS_DENIED.into());
        }
        if delete && handle.directory {
            let guard = self.session()?;
            if !guard
                .as_ref()
                .unwrap()
                .children(handle.id.as_deref())
                .is_empty()
            {
                return Err(STATUS_DIRECTORY_NOT_EMPTY.into());
            }
        }
        Ok(())
    }
    fn cleanup(&self, handle: &Handle, _: Option<&U16CStr>, flags: u32) {
        if flags & 1 != 0 {
            if let (Some(id), Ok(mut guard)) = (&handle.id, self.session()) {
                if let Err(error) = guard.as_mut().unwrap().delete_entry(id) {
                    tracing::warn!("Private delete failed: {error}");
                }
            }
        }
    }
}
