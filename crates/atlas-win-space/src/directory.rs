//! Metadata-only bulk enumeration and cancellable directory notifications.
//! All platform handles stay here; safe consumers never need raw Win32 access.
use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_NOTIFY_ENUM_DIR, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::*;

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

pub fn attributes_require_hydration(attributes: u32) -> bool {
    attributes & (0x0040_0000 | 0x0000_1000 | 0x0004_0000) != 0
}

/// CLOUD through CLOUD_F share every bit except the provider slot. OneDrive's
/// older placeholder tag predates this family and must also be guarded.
pub fn is_cloud_reparse_tag(tag: u32) -> bool {
    tag & !0x0000_f000 == 0x9000_001a || tag == 0x8000_0021
}

pub fn is_cloud_placeholder(path: &Path) -> bool {
    let attributes = unsafe { GetFileAttributesW(wide(path).as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES || attributes_require_hydration(attributes) {
        return true;
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return false;
    }
    // Inspect the reparse point itself. Never follow it or open a data stream.
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return true;
    }
    let handle = OwnedHandle(handle);
    let mut info: FILE_ATTRIBUTE_TAG_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    };
    ok == 0 || is_cloud_reparse_tag(info.ReparseTag)
}

#[derive(Debug)]
pub struct DirectoryEntry {
    pub name: std::ffi::OsString,
    pub attributes: u32,
    pub size: u64,
    pub modified: i64,
    pub file_id: [u8; 16],
    pub reparse_tag: u32,
}

#[repr(C)]
struct IoStatus {
    status: usize,
    information: usize,
}

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryDirectoryFile(
        file: HANDLE,
        event: HANDLE,
        apc: *const c_void,
        context: *const c_void,
        status: *mut IoStatus,
        buffer: *mut c_void,
        length: u32,
        class: u32,
        single: u8,
        name: *const c_void,
        restart: u8,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.0 != 0 && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// 64 KiB, FileIdExtdDirectoryInformation (128-bit ID, reparse tag and cached
/// timestamps). No per-entry stat calls and no data-stream handles are opened.
pub fn enumerate(path: &Path, mut emit: impl FnMut(DirectoryEntry)) -> io::Result<()> {
    enumerate_while(path, |entry| {
        emit(entry);
        true
    })
}

/// Cancellation is checked between native batches (at most 64 KiB), without
/// issuing a separate metadata query for every file.
pub fn enumerate_while(
    path: &Path,
    mut emit: impl FnMut(DirectoryEntry) -> bool,
) -> io::Result<()> {
    let handle = unsafe {
        CreateFileW(
            wide(path).as_ptr(),
            FILE_LIST_DIRECTORY | 0x0010_0000,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = OwnedHandle(handle);
    // u64 alignment is required for the native directory record headers.
    let mut buffer = vec![0u64; 64 * 1024 / 8];
    let mut first = true;
    loop {
        let mut status = IoStatus {
            status: 0,
            information: 0,
        };
        let result = unsafe {
            NtQueryDirectoryFile(
                handle.0,
                0,
                null(),
                null(),
                &mut status,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 8) as u32,
                60,
                0,
                null(),
                u8::from(first),
            )
        };
        first = false;
        if result as u32 == 0x8000_0006 {
            return Ok(());
        } // STATUS_NO_MORE_FILES
        if result < 0 {
            return Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(result) } as i32,
            ));
        }
        if status.information == 0 {
            return Ok(());
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<u8>(),
                status.information.min(buffer.len() * 8),
            )
        };
        let mut keep_going = true;
        parse_directory_records(bytes, &mut |entry| {
            if keep_going {
                keep_going = emit(entry);
            }
        })?;
        if !keep_going {
            return Ok(());
        }
    }
}

fn parse_directory_records(bytes: &[u8], emit: &mut impl FnMut(DirectoryEntry)) -> io::Result<()> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid native directory record",
        )
    };
    let mut offset = 0usize;
    loop {
        let data = bytes
            .get(offset..)
            .filter(|b| b.len() >= 88)
            .ok_or_else(invalid)?;
        let u32_at = |at| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let next = u32_at(0) as usize;
        let name_len = u32_at(60) as usize;
        let end = 88usize.checked_add(name_len).ok_or_else(invalid)?;
        if !name_len.is_multiple_of(2)
            || end > data.len()
            || (next != 0 && (next < end || next > data.len()))
        {
            return Err(invalid());
        }
        let utf16: Vec<u16> = data[88..end]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        if utf16 != [46] && utf16 != [46, 46] {
            if utf16.iter().any(|unit| matches!(*unit, 0 | 47 | 58 | 92)) {
                return Err(invalid());
            }
            emit(DirectoryEntry {
                name: std::ffi::OsString::from_wide(&utf16),
                attributes: u32_at(56),
                size: i64::from_le_bytes(data[40..48].try_into().unwrap()).max(0) as u64,
                modified: i64::from_le_bytes(data[24..32].try_into().unwrap()),
                file_id: data[72..88].try_into().unwrap(),
                reparse_tag: u32_at(68),
            });
        }
        if next == 0 {
            return Ok(());
        }
        offset = offset.checked_add(next).ok_or_else(invalid)?;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Upsert(PathBuf),
    Remove(PathBuf),
    Rename { from: PathBuf, to: PathBuf },
    Rescan,
    Disabled,
}

fn is_recycle_path(path: &Path) -> bool {
    path.components().any(|part| {
        let name = part.as_os_str().to_string_lossy();
        name.eq_ignore_ascii_case("$Recycle") || name.eq_ignore_ascii_case("$Recycle.Bin")
    })
}

/// Decode ordered FILE_NOTIFY_INFORMATION records; rename halves can span reads.
fn parse_notifications(
    bytes: &[u8],
    root: &Path,
    old: &mut Option<PathBuf>,
) -> io::Result<Vec<Change>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid directory notification");
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let data = bytes
            .get(offset..)
            .filter(|b| b.len() >= 12)
            .ok_or_else(invalid)?;
        let get = |at| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let next = get(0) as usize;
        let len = get(8) as usize;
        let end = 12usize.checked_add(len).ok_or_else(invalid)?;
        if !len.is_multiple_of(2)
            || end > data.len()
            || (next != 0 && (next < end || next > data.len()))
        {
            return Err(invalid());
        }
        let name: Vec<u16> = data[12..end]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        if name.is_empty()
            || name.iter().any(|unit| matches!(*unit, 0 | 47 | 58))
            || name
                .split(|unit| *unit == 92)
                .any(|component| component.is_empty() || component == [46] || component == [46, 46])
        {
            return Err(invalid());
        }
        let path = root.join(std::ffi::OsString::from_wide(&name));
        let action = get(4);
        if action != 5 && action != 4 {
            if let Some(from) = old.take() {
                out.push(Change::Remove(from));
            }
        }
        match action {
            1 | 3 => out.push(Change::Upsert(path)),
            2 => out.push(Change::Remove(path)),
            4 => {
                if let Some(from) = old.replace(path) {
                    out.push(Change::Remove(from));
                }
            }
            5 => out.push(match old.take() {
                Some(from) => Change::Rename { from, to: path },
                None => Change::Upsert(path),
            }),
            _ => out.push(Change::Rescan),
        }
        if next == 0 {
            break;
        }
        offset = offset.checked_add(next).ok_or_else(invalid)?;
    }
    // Preserve moves across the boundary as a removal or insertion. Suppress
    // internal recycle traffic before it reaches the metadata worker.
    Ok(out
        .into_iter()
        .filter_map(|event| match event {
            Change::Upsert(path) if is_recycle_path(&path) => None,
            Change::Remove(path) if is_recycle_path(&path) => None,
            Change::Rename { from, to } => match (is_recycle_path(&from), is_recycle_path(&to)) {
                (true, true) => None,
                (false, true) => Some(Change::Remove(from)),
                (true, false) => Some(Change::Upsert(to)),
                (false, false) => Some(Change::Rename { from, to }),
            },
            event => Some(event),
        })
        .collect())
}

/// A lost notification buffer invalidates rename pairing, but the directory
/// handle remains usable. Unsupported/closed handles are a terminal failure.
fn notifications_after_read(
    bytes: &[u8],
    error: Option<u32>,
    root: &Path,
    old: &mut Option<PathBuf>,
) -> io::Result<Vec<Change>> {
    if let Some(error) = error {
        *old = None;
        if error != ERROR_NOTIFY_ENUM_DIR {
            return Err(io::Error::from_raw_os_error(error as i32));
        }
        return Ok(vec![Change::Rescan]);
    }
    if bytes.is_empty() {
        *old = None;
        return Ok(vec![Change::Rescan]);
    }
    match parse_notifications(bytes, root, old) {
        Ok(changes) => Ok(changes),
        Err(_) => {
            *old = None;
            Ok(vec![Change::Rescan])
        }
    }
}

pub struct DirectoryWatcher {
    stop: HANDLE,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for DirectoryWatcher {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::SetEvent(self.stop);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        unsafe {
            CloseHandle(self.stop);
        }
    }
}

struct WatchContext {
    directory: *mut HANDLE,
    registered: HANDLE,
    pending: *mut windows_sys::Win32::System::IO::OVERLAPPED,
    removed: bool,
    resume: bool,
    complete: bool,
}

#[repr(C)]
struct DeviceHandle {
    size: u32,
    kind: u32,
    reserved: u32,
    handle: HANDLE,
    notify: isize,
    guid: [u8; 16],
    name_offset: i32,
    data: [u8; 1],
}

unsafe extern "system" fn device_window(window: isize, msg: u32, wp: usize, lp: isize) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    if msg == WM_DEVICECHANGE && matches!(wp, 0x8001 | 0x8002 | 0x8004) {
        let ptr = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut WatchContext;
        if !ptr.is_null() {
            let context = unsafe { &mut *ptr };
            // Ignore unrelated device/volume broadcasts delivered to the window.
            if lp == 0
                || unsafe { *(lp as *const u32) } < std::mem::size_of::<DeviceHandle>() as u32
            {
                return unsafe { DefWindowProcW(window, msg, wp, lp) };
            }
            let device = unsafe { &*(lp as *const DeviceHandle) };
            if device.kind != 6 || device.handle != context.registered {
                return unsafe { DefWindowProcW(window, msg, wp, lp) };
            }
            if wp == 0x8002 {
                context.resume = true;
                return 1;
            } // DBT_DEVICEQUERYREMOVEFAILED
            if wp == 0x8004 {
                context.complete = true;
            } // DBT_DEVICEREMOVECOMPLETE
            context.removed = true;
            unsafe {
                let handle = *context.directory;
                if handle != INVALID_HANDLE_VALUE {
                    windows_sys::Win32::System::IO::CancelIoEx(handle, null());
                    if !context.pending.is_null() {
                        let mut done = 0;
                        windows_sys::Win32::System::IO::GetOverlappedResult(
                            handle,
                            context.pending,
                            &mut done,
                            1,
                        );
                    }
                    CloseHandle(handle);
                    *context.directory = INVALID_HANDLE_VALUE;
                }
            }
            return 1;
        }
    }
    unsafe { DefWindowProcW(window, msg, wp, lp) }
}

impl DirectoryWatcher {
    pub fn watch(path: &Path, emit: impl FnMut(Vec<Change>) + Send + 'static) -> io::Result<Self> {
        Self::watch_with_subtree(path, false, emit)
    }

    pub fn watch_with_subtree(
        path: &Path,
        recursive: bool,
        mut emit: impl FnMut(Vec<Change>) + Send + 'static,
    ) -> io::Result<Self> {
        use windows_sys::Win32::System::Threading::*;
        let directory = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                0,
            )
        };
        if directory == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let stop = unsafe { CreateEventW(null(), 1, 0, null()) };
        if stop == 0 {
            unsafe {
                CloseHandle(directory);
            }
            return Err(io::Error::last_os_error());
        }
        let path = path.to_path_buf();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("atlas-directory-watch".into())
            .spawn(move || {
                use windows_sys::Win32::System::IO::*;
                use windows_sys::Win32::UI::WindowsAndMessaging::*;
                let mut directory = OwnedHandle(directory);
                let event = OwnedHandle(unsafe { CreateEventW(null(), 1, 0, null()) });
                if event.0 == 0 {
                    emit(vec![Change::Rescan]);
                    return;
                }
                let class: Vec<u16> = "AtlasDirectoryDeviceWatcher\0".encode_utf16().collect();
                let mut wc: WNDCLASSW = unsafe { std::mem::zeroed() };
                wc.lpfnWndProc = Some(device_window);
                wc.lpszClassName = class.as_ptr();
                unsafe {
                    RegisterClassW(&wc);
                }
                // A hidden top-level window receives handle-specific removal queries.
                let window = unsafe {
                    CreateWindowExW(
                        0,
                        class.as_ptr(),
                        class.as_ptr(),
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                        null(),
                    )
                };
                if window == 0 {
                    emit(vec![Change::Rescan]);
                    return;
                }
                let mut context = WatchContext {
                    directory: &raw mut directory.0,
                    registered: directory.0,
                    pending: null_mut(),
                    removed: false,
                    resume: false,
                    complete: false,
                };
                unsafe {
                    SetWindowLongPtrW(window, GWLP_USERDATA, (&raw mut context) as isize);
                }
                let mut filter: DeviceHandle = unsafe { std::mem::zeroed() };
                filter.size = std::mem::size_of::<DeviceHandle>() as u32;
                filter.kind = 6;
                filter.handle = directory.0;
                let mut notification =
                    unsafe { RegisterDeviceNotificationW(window, (&raw const filter).cast(), 0) };
                if notification.is_null() {
                    unsafe {
                        SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                        DestroyWindow(window);
                    }
                    emit(vec![Change::Rescan]);
                    return;
                }
                let mut buffer = vec![0u32; 32 * 1024 / 4];
                let mut old: Option<PathBuf> = None;
                let mut ready = Some(ready_tx);
                let mut rescan_after_arm = false;
                'read: loop {
                    context.pending = null_mut();
                    if context.complete {
                        break;
                    }
                    if context.removed {
                        // DispatchMessage invokes device_window, which changes
                        // these flags through the registered stack context.
                        #[allow(clippy::while_immutable_condition)]
                        while !context.resume && !context.complete {
                            let result = unsafe {
                                MsgWaitForMultipleObjectsEx(
                                    1,
                                    &stop,
                                    u32::MAX,
                                    QS_ALLINPUT,
                                    MWMO_INPUTAVAILABLE,
                                )
                            };
                            if result == 0 || result == u32::MAX {
                                break 'read;
                            }
                            let mut msg: MSG = unsafe { std::mem::zeroed() };
                            while unsafe { PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) } != 0 {
                                unsafe {
                                    TranslateMessage(&msg);
                                    DispatchMessageW(&msg);
                                }
                            }
                        }
                        if context.complete {
                            break;
                        }
                        directory.0 = unsafe {
                            CreateFileW(
                                wide(&path).as_ptr(),
                                FILE_LIST_DIRECTORY,
                                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                                null(),
                                OPEN_EXISTING,
                                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                                0,
                            )
                        };
                        if directory.0 == INVALID_HANDLE_VALUE {
                            emit(vec![Change::Rescan]);
                            break;
                        }
                        unsafe {
                            if !notification.is_null() {
                                UnregisterDeviceNotification(notification);
                            }
                        }
                        filter.handle = directory.0;
                        context.registered = directory.0;
                        notification = unsafe {
                            RegisterDeviceNotificationW(window, (&raw const filter).cast(), 0)
                        };
                        if notification.is_null() {
                            emit(vec![Change::Rescan]);
                            break;
                        }
                        context.removed = false;
                        context.resume = false;
                        old = None;
                        rescan_after_arm = true;
                    }
                    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
                    overlapped.hEvent = event.0;
                    context.pending = &raw mut overlapped;
                    unsafe {
                        ResetEvent(event.0);
                    }
                    let started = unsafe {
                        ReadDirectoryChangesW(
                            directory.0,
                            buffer.as_mut_ptr().cast(),
                            (buffer.len() * 4) as u32,
                            i32::from(recursive),
                            FILE_NOTIFY_CHANGE_FILE_NAME
                                | FILE_NOTIFY_CHANGE_DIR_NAME
                                | FILE_NOTIFY_CHANGE_ATTRIBUTES
                                | FILE_NOTIFY_CHANGE_SIZE
                                | FILE_NOTIFY_CHANGE_LAST_WRITE,
                            null_mut(),
                            &mut overlapped,
                            None,
                        )
                    };
                    if started == 0 {
                        emit(vec![Change::Disabled]);
                        break;
                    }
                    // A successful constructor guarantees the first read is armed.
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(());
                    }
                    if rescan_after_arm {
                        emit(vec![Change::Rescan]);
                        rescan_after_arm = false;
                    }
                    loop {
                        let handles = [stop, event.0];
                        let result = unsafe {
                            MsgWaitForMultipleObjectsEx(
                                2,
                                handles.as_ptr(),
                                if old.is_some() { 80 } else { u32::MAX },
                                QS_ALLINPUT,
                                MWMO_INPUTAVAILABLE,
                            )
                        };
                        if context.removed {
                            continue 'read;
                        }
                        if result == 258 {
                            // WAIT_TIMEOUT: moved outside the watched directory.
                            if let Some(from) = old.take() {
                                if !is_recycle_path(&from) {
                                    emit(vec![Change::Remove(from)]);
                                }
                            }
                            continue;
                        }
                        if result == 0 || result == u32::MAX {
                            unsafe {
                                CancelIoEx(directory.0, &overlapped);
                            }
                            let mut done = 0;
                            unsafe {
                                GetOverlappedResult(directory.0, &overlapped, &mut done, 1);
                            }
                            break 'read;
                        }
                        if result == 1 {
                            break;
                        }
                        let mut msg: MSG = unsafe { std::mem::zeroed() };
                        while unsafe { PeekMessageW(&mut msg, 0, 0, 0, PM_REMOVE) } != 0 {
                            unsafe {
                                TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                        if context.removed {
                            continue 'read;
                        }
                    }
                    let mut count = 0;
                    let error =
                        if unsafe { GetOverlappedResult(directory.0, &overlapped, &mut count, 0) }
                            == 0
                        {
                            Some(unsafe { GetLastError() })
                        } else {
                            None
                        };
                    let bytes = if error.is_some() {
                        &[][..]
                    } else {
                        unsafe {
                            std::slice::from_raw_parts(
                                buffer.as_ptr().cast::<u8>(),
                                (count as usize).min(buffer.len() * 4),
                            )
                        }
                    };
                    match notifications_after_read(bytes, error, &path, &mut old) {
                        Ok(changes) if !changes.is_empty() => emit(changes),
                        Ok(_) => {}
                        Err(_) => {
                            emit(vec![Change::Disabled]);
                            break;
                        }
                    };
                }
                unsafe {
                    // DestroyWindow can synchronously dispatch messages: detach the
                    // stack context before the outstanding OVERLAPPED goes out of scope.
                    SetWindowLongPtrW(window, GWLP_USERDATA, 0);
                    if !notification.is_null() {
                        UnregisterDeviceNotification(notification);
                    }
                    if window != 0 {
                        DestroyWindow(window);
                    }
                }
            });
        match thread {
            Ok(thread) => {
                if ready_rx.recv().is_ok() {
                    Ok(Self {
                        stop,
                        thread: Some(thread),
                    })
                } else {
                    let _ = thread.join();
                    unsafe {
                        CloseHandle(stop);
                    }
                    Err(io::Error::other("Could not arm native directory watcher"))
                }
            }
            Err(error) => {
                unsafe {
                    CloseHandle(stop);
                    CloseHandle(directory);
                }
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_buffers_rescan_and_rearm_without_pairing_a_stale_rename() {
        let root = Path::new("C:\\test");
        for error in [None, Some(ERROR_NOTIFY_ENUM_DIR)] {
            let mut pending = None;
            notifications_after_read(&notification(4, "old.txt"), None, root, &mut pending)
                .unwrap();
            assert!(pending.is_some());
            assert_eq!(
                notifications_after_read(&[], error, root, &mut pending).unwrap(),
                vec![Change::Rescan]
            );
            assert!(pending.is_none());
            // The same watch can continue immediately after either Windows
            // overflow form; the next new-name half is an insertion, not a
            // spurious rename from before the lost buffer.
            assert_eq!(
                notifications_after_read(&notification(5, "new.txt"), None, root, &mut pending)
                    .unwrap(),
                vec![Change::Upsert(root.join("new.txt"))]
            );
        }
    }

    #[test]
    fn watch_completion_separates_fatal_handles_malformed_buffers_and_ignored_recycle_traffic() {
        let root = Path::new("C:\\test");
        let mut pending = Some(root.join("old.txt"));
        assert!(notifications_after_read(&[], Some(6), root, &mut pending).is_err()); // ERROR_INVALID_HANDLE
        assert!(pending.is_none());
        pending = Some(root.join("old.txt"));
        assert_eq!(
            notifications_after_read(&[0; 11], None, root, &mut pending).unwrap(),
            vec![Change::Rescan]
        );
        assert!(pending.is_none());
        assert!(notifications_after_read(
            &notification(3, r"$Recycle.Bin\trash"),
            None,
            root,
            &mut pending
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            notifications_after_read(&notification(3, "normal.txt"), None, root, &mut pending)
                .unwrap(),
            vec![Change::Upsert(root.join("normal.txt"))]
        );
    }

    #[test]
    fn cloud_tag_family_and_legacy_onedrive_are_guarded() {
        for provider in 0..16 {
            assert!(is_cloud_reparse_tag(0x9000_001a | provider << 12));
        }
        assert!(is_cloud_reparse_tag(0x8000_0021));
        for tag in [0, 0xa000_000c, 0xa000_0003, 0x9000_001b, 0x9001_001a] {
            assert!(!is_cloud_reparse_tag(tag));
        }
    }

    #[test]
    fn recycle_notifications_are_ignored_but_boundary_moves_are_preserved() {
        let root = Path::new("C:\\");
        assert!(
            parse_notifications(&notification(3, r"$recycle.bin\sid\trash"), root, &mut None)
                .unwrap()
                .is_empty()
        );
        assert!(
            parse_notifications(&notification(2, r"$Recycle\trash"), root, &mut None)
                .unwrap()
                .is_empty()
        );
        let mut old = None;
        parse_notifications(&notification(4, "document.txt"), root, &mut old).unwrap();
        assert_eq!(
            parse_notifications(&notification(5, r"$Recycle.Bin\trash"), root, &mut old).unwrap(),
            vec![Change::Remove(root.join("document.txt"))]
        );
        parse_notifications(&notification(4, r"$Recycle.Bin\trash"), root, &mut old).unwrap();
        assert_eq!(
            parse_notifications(&notification(5, "restored.txt"), root, &mut old).unwrap(),
            vec![Change::Upsert(root.join("restored.txt"))]
        );
        assert!(!is_recycle_path(Path::new(r"C:\$Recycle.Bin-backup\file")));
    }
    #[test]
    fn recursive_notifications_accept_nested_relative_paths_without_traversal() {
        let root = Path::new(r"C:\root");
        assert!(
            matches!(parse_notifications(&notification(3, r"a\b\file.bin"), root, &mut None).unwrap().as_slice(),
            [Change::Upsert(path)] if path == &root.join(r"a\b\file.bin"))
        );
        for path in [
            r"..\file",
            r"a\..\file",
            r"\rooted",
            r"a\\file",
            r"C:\file",
            r"a/../file",
        ] {
            assert!(
                parse_notifications(&notification(3, path), root, &mut None).is_err(),
                "{path}"
            );
        }
    }
    #[test]
    fn guard_checks_offline_and_recall_flags_only() {
        for flag in [0x1000, 0x40000, 0x400000] {
            assert!(attributes_require_hydration(flag));
        }
        assert!(!attributes_require_hydration(
            FILE_ATTRIBUTE_NORMAL | FILE_ATTRIBUTE_REPARSE_POINT
        ));
    }
    #[test]
    fn metadata_guard_observes_real_offline_file_attributes() {
        let path = std::env::temp_dir().join(format!("atlas-offline-{}.txt", std::process::id()));
        std::fs::write(&path, b"metadata-only guard").unwrap();
        assert!(!is_cloud_placeholder(&path));
        assert_ne!(
            unsafe { SetFileAttributesW(wide(&path).as_ptr(), FILE_ATTRIBUTE_OFFLINE) },
            0
        );
        assert!(is_cloud_placeholder(&path));
        assert_ne!(
            unsafe { SetFileAttributesW(wide(&path).as_ptr(), FILE_ATTRIBUTE_NORMAL) },
            0
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            is_cloud_placeholder(&path),
            "Attribute errors must fail closed"
        );
    }
    #[test]
    fn malformed_buffers_are_rejected_without_out_of_bounds_reads() {
        assert!(parse_directory_records(&[0; 87], &mut |_| {}).is_err());
        assert!(parse_notifications(&[0; 11], Path::new("C:\\"), &mut None).is_err());
        assert!(parse_notifications(
            &notification(1, "..\\escape.txt"),
            Path::new("C:\\root"),
            &mut None
        )
        .is_err());
    }
    fn notification(action: u32, name: &str) -> Vec<u8> {
        let name: Vec<u16> = name.encode_utf16().collect();
        let mut data = vec![0u8; 12];
        data[4..8].copy_from_slice(&action.to_le_bytes());
        data[8..12].copy_from_slice(&((name.len() * 2) as u32).to_le_bytes());
        for c in name {
            data.extend_from_slice(&c.to_le_bytes());
        }
        data
    }
    #[test]
    fn rename_pair_can_span_reads_without_losing_identity() {
        let root = Path::new("C:\\root");
        let mut old = None;
        assert!(
            parse_notifications(&notification(4, "old.txt"), root, &mut old)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            parse_notifications(&notification(5, "new.txt"), root, &mut old).unwrap(),
            [Change::Rename {
                from: root.join("old.txt"),
                to: root.join("new.txt")
            }]
        );
    }
    #[test]
    fn nt_query_matches_portable_enumeration_and_metadata() {
        let root = std::env::temp_dir().join(format!("atlas-native-list-{}", std::process::id()));
        std::fs::create_dir_all(root.join("folder")).unwrap();
        std::fs::write(root.join("Straße2.txt"), b"native metadata").unwrap();
        let mut native = Vec::new();
        enumerate(&root, |entry| native.push(entry)).unwrap();
        let mut names: Vec<_> = native.iter().map(|e| e.name.clone()).collect();
        names.sort();
        let mut portable: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        portable.sort();
        assert_eq!(names, portable);
        let file = native.iter().find(|e| e.name == "Straße2.txt").unwrap();
        assert_eq!(file.size, 15);
        assert!(file.modified > 116_444_736_000_000_000);
        assert_ne!(file.file_id, [0; 16]);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_watch_reports_create_rename_delete_and_releases_handle() {
        let root = std::env::temp_dir().join(format!("atlas-native-watch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = DirectoryWatcher::watch(&root, move |batch| {
            let _ = tx.send(batch);
        })
        .unwrap();
        // Wait until the first overlapped read is registered by observing a probe.
        let a = root.join("a.txt");
        let b = root.join("b.txt");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            std::fs::write(&a, b"a").unwrap();
            if rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_ok()
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
        }
        std::fs::rename(&a, &b).unwrap();
        let mut renamed = false;
        while !renamed && std::time::Instant::now() < deadline {
            renamed = rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .map(|batch| {
                    batch.iter().any(
                        |event| matches!(event,Change::Rename { from,to } if from==&a && to==&b),
                    )
                })
                .unwrap_or(false);
        }
        assert!(renamed);
        std::fs::remove_file(&b).unwrap();
        let mut removed = false;
        while !removed && std::time::Instant::now() < deadline {
            removed = rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .map(|batch| batch.contains(&Change::Remove(b.clone())))
                .unwrap_or(false);
        }
        assert!(removed);
        // Exercise Windows' actual message path: query removal must release the
        // directory before returning TRUE, and a failed eject must arm a new read.
        let class: Vec<u16> = "AtlasDirectoryDeviceWatcher\0".encode_utf16().collect();
        let window = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW(class.as_ptr(), null())
        };
        assert_ne!(window, 0);
        let closed_handle = unsafe {
            let context = windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                window,
                windows_sys::Win32::UI::WindowsAndMessaging::GWLP_USERDATA,
            ) as *const WatchContext;
            *(*context).directory
        };
        let mut query: DeviceHandle = unsafe { std::mem::zeroed() };
        query.size = std::mem::size_of::<DeviceHandle>() as u32;
        query.kind = 6;
        query.handle = closed_handle;
        let accepted = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                window,
                0x219,
                0x8001,
                (&raw const query) as isize,
            )
        };
        assert_eq!(accepted, 1);
        let mut information = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { GetFileInformationByHandle(closed_handle, &mut information) },
            0,
            "Query-remove kept watcher directory handle open"
        );
        let exclusive = unsafe {
            CreateFileW(
                wide(&root).as_ptr(),
                FILE_LIST_DIRECTORY,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                0,
            )
        };
        // Other processes may hold the folder, so exclusive-open is advisory.
        unsafe {
            if exclusive != INVALID_HANDLE_VALUE {
                CloseHandle(exclusive);
            }
            windows_sys::Win32::UI::WindowsAndMessaging::SendMessageW(
                window,
                0x219,
                0x8002,
                (&raw const query) as isize,
            );
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut resumed = false;
        while !resumed && std::time::Instant::now() < deadline {
            resumed = rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .map(|batch| batch.contains(&Change::Rescan))
                .unwrap_or(false);
        }
        assert!(resumed, "Failed eject did not restore directory watching");
        std::fs::write(&a, b"resumed").unwrap();
        let mut upsert = false;
        while !upsert && std::time::Instant::now() < deadline {
            upsert = rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .map(|batch| batch.contains(&Change::Upsert(a.clone())))
                .unwrap_or(false);
        }
        assert!(upsert);
        let outside = root.with_extension("moved-out");
        std::fs::rename(&a, &outside).unwrap();
        let mut moved_out = false;
        while !moved_out && std::time::Instant::now() < deadline {
            moved_out = rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .map(|batch| batch.contains(&Change::Remove(a.clone())))
                .unwrap_or(false);
        }
        assert!(moved_out);
        std::fs::remove_file(&outside).unwrap();
        let start = std::time::Instant::now();
        drop(watcher);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        std::fs::remove_dir(&root).unwrap();
    }
}
