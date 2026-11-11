//! Native SMB through the Windows Multiple Provider Router (`mpr.dll`) and the
//! LAN Manager share API (`netapi32.dll`).
//!
//! Atlas never sees a network password: interactive connections use the same
//! Windows credential prompt as File Explorer, and Windows stores the answer in
//! Credential Manager when the user ticks "Remember my credentials".
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_CANCELLED, ERROR_MORE_DATA, HANDLE, NO_ERROR};
use windows_sys::Win32::NetworkManagement::NetManagement::NetApiBufferFree;
use windows_sys::Win32::NetworkManagement::WNet::{
    WNetAddConnection3W, WNetCancelConnection2W, WNetCloseEnum, WNetConnectionDialog1W,
    WNetEnumResourceW, WNetGetConnectionW, WNetOpenEnumW, CONNDLG_RO_PATH, CONNDLG_USE_MRU,
    CONNECTDLGSTRUCTW, CONNECT_INTERACTIVE, CONNECT_PROMPT, CONNECT_TEMPORARY,
    CONNECT_UPDATE_PROFILE, NETRESOURCEW, RESOURCETYPE_DISK, RESOURCE_CONNECTED,
    RESOURCE_REMEMBERED,
};
use windows_sys::Win32::Storage::FileSystem::{
    NetShareEnum, SHARE_INFO_1, STYPE_DISKTREE, STYPE_MASK, STYPE_SPECIAL,
};

const MAX_PREFERRED_LENGTH: u32 = u32::MAX;
const NERR_SUCCESS: u32 = 0;

/// A drive letter mapped to an SMB share, connected now or remembered for the
/// next sign-in (shown by Explorer with a red cross).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappedDrive {
    /// `Z:`
    pub local: String,
    /// `\\server\share`
    pub remote: String,
    pub connected: bool,
}

/// A disk share published by a server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    pub remark: String,
    /// Administrative shares such as `C$` and `ADMIN$`.
    pub special: bool,
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

/// # Safety
/// `ptr` must be null or point to a NUL-terminated UTF-16 string.
unsafe fn from_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}

fn win32(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

/// Run a Windows dialog on a single-threaded COM apartment, which the shell
/// parts of the network dialogs (Browse…) require. Call from a dedicated
/// thread, never from a pooled worker that may already be MTA.
pub fn with_sta<T>(dialog: impl FnOnce() -> T) -> T {
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
    // SAFETY: balanced with CoUninitialize below when initialization succeeded.
    let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
    let result = dialog();
    if initialized {
        unsafe { CoUninitialize() };
    }
    result
}

fn enumerate(scope: u32) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut handle: HANDLE = 0;
    // SAFETY: a null NETRESOURCE enumerates the top level; `handle` receives
    // an enumeration handle closed below.
    if unsafe { WNetOpenEnumW(scope, RESOURCETYPE_DISK, 0, null(), &mut handle) } != NO_ERROR {
        return found;
    }
    // 16 KiB is the size Microsoft's own sample uses; the loop grows it on
    // ERROR_MORE_DATA so very long remote names still enumerate.
    let mut buffer: Vec<u64> = vec![0; 2048];
    loop {
        let mut count = u32::MAX;
        let mut bytes = (buffer.len() * 8) as u32;
        // SAFETY: `buffer` is 8-byte aligned (NETRESOURCEW contains pointers)
        // and `bytes` is its true size.
        let status = unsafe {
            WNetEnumResourceW(handle, &mut count, buffer.as_mut_ptr().cast(), &mut bytes)
        };
        if status == ERROR_MORE_DATA && count == u32::MAX {
            buffer.resize((bytes as usize).div_ceil(8) + 1, 0);
            continue;
        }
        if status != NO_ERROR {
            break;
        }
        let resources = buffer.as_ptr().cast::<NETRESOURCEW>();
        for i in 0..count as usize {
            // SAFETY: WNetEnumResourceW wrote `count` records at the start of
            // the buffer; their strings point into the same buffer.
            let resource = unsafe { *resources.add(i) };
            let (local, remote) = unsafe {
                (
                    from_wide(resource.lpLocalName),
                    from_wide(resource.lpRemoteName),
                )
            };
            if !local.is_empty() && !remote.is_empty() {
                found.push((local.to_ascii_uppercase(), remote));
            }
        }
    }
    // SAFETY: handle came from a successful WNetOpenEnumW.
    unsafe { WNetCloseEnum(handle) };
    found
}

/// Drive letters mapped to network shares, sorted by letter. Reads the local
/// redirector tables only, so it never waits on a server.
pub fn mapped_drives() -> Vec<MappedDrive> {
    let connected = enumerate(RESOURCE_CONNECTED);
    let mut drives: Vec<MappedDrive> = connected
        .iter()
        .map(|(local, remote)| MappedDrive {
            local: local.clone(),
            remote: remote.clone(),
            connected: true,
        })
        .collect();
    // An elevated process runs in its own logon session: the router neither
    // lists Explorer's connections nor the remembered ones there, but the
    // profile still holds them under `HKCU\Network`.
    let profile = profile_mappings()
        .into_iter()
        .filter(|(local, _)| matches!(drive_type(local), DRIVE_NO_ROOT_DIR | DRIVE_REMOTE));
    for (local, remote) in enumerate(RESOURCE_REMEMBERED).into_iter().chain(profile) {
        if !drives.iter().any(|d| d.local == local) {
            drives.push(MappedDrive {
                local,
                remote,
                connected: false,
            });
        }
    }
    drives.retain(|d| d.local.len() == 2 && d.local.as_bytes()[1] == b':');
    drives.sort_by(|a, b| a.local.cmp(&b.local));
    drives
}

const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOTE: u32 = 4;

fn drive_type(local: &str) -> u32 {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    let root = wide(&format!("{}\\", local.trim_end_matches(['\\', '/'])));
    // SAFETY: `root` is a NUL-terminated drive root; the call reads local state.
    unsafe { GetDriveTypeW(root.as_ptr()) }
}

/// True when `local` (`Z:`) is a drive letter mapped to a network share.
pub fn is_remote_drive(local: &str) -> bool {
    drive_type(local) == DRIVE_REMOTE
}

/// `\\server\share` behind a mapped drive letter (`Z:`), if any.
pub fn remote_of(local: &str) -> Option<String> {
    let letter = local.trim_end_matches(['\\', '/']);
    let local = wide(letter);
    let mut buffer = vec![0u16; 1024];
    let mut len = buffer.len() as u32;
    // SAFETY: `len` is the capacity of `buffer` in UTF-16 units.
    let status = unsafe { WNetGetConnectionW(local.as_ptr(), buffer.as_mut_ptr(), &mut len) };
    // ERROR_CONNECTION_UNAVAIL (1201) still fills in a remembered mapping.
    if status == NO_ERROR || status == 1201 {
        return Some(unsafe { from_wide(buffer.as_ptr()) });
    }
    profile_mappings()
        .into_iter()
        .find(|(drive, _)| drive.eq_ignore_ascii_case(letter))
        .map(|(_, remote)| remote)
}

/// A remembered mapping (`Z:` → `\\server\share`) that is not connected in
/// this process's logon session, e.g. VPN down at sign-in or Atlas running
/// elevated. Opening the drive should reconnect it under the same letter.
pub fn reconnectable(local: &str) -> Option<String> {
    let letter = local.trim_end_matches(['\\', '/']);
    // Connected already, or the letter now belongs to a local disk.
    if drive_type(letter) != DRIVE_NO_ROOT_DIR {
        return None;
    }
    mapped_drives()
        .into_iter()
        .find(|d| !d.connected && d.local.eq_ignore_ascii_case(letter))
        .map(|d| d.remote)
}

/// Persistent mappings of the signed-in user (`HKCU\Network\<letter>`,
/// value `RemotePath`), the list Windows restores at sign-in.
fn profile_mappings() -> Vec<(String, String)> {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
    };
    let mut found = Vec::new();
    let mut key: HKEY = 0;
    let path = wide("Network");
    // SAFETY: `path` is NUL-terminated; `key` is closed below.
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, KEY_READ, &mut key) } != 0 {
        return found;
    }
    let value = wide("RemotePath");
    for index in 0.. {
        let mut name = [0u16; 64];
        let mut name_len = name.len() as u32;
        // SAFETY: `name_len` is the capacity of `name` in UTF-16 units.
        let status = unsafe {
            RegEnumKeyExW(
                key,
                index,
                name.as_mut_ptr(),
                &mut name_len,
                null(),
                null_mut(),
                null_mut(),
                null_mut(),
            )
        };
        if status != 0 {
            break;
        }
        let letter = String::from_utf16_lossy(&name[..name_len as usize]);
        if letter.len() != 1 || !letter.as_bytes()[0].is_ascii_alphabetic() {
            continue;
        }
        let mut subkey = name[..name_len as usize].to_vec();
        subkey.push(0);
        let mut buffer = vec![0u16; 1024];
        let mut bytes = (buffer.len() * 2) as u32;
        // SAFETY: `bytes` is the byte size of `buffer`; the value is a string.
        let status = unsafe {
            RegGetValueW(
                key,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
                null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        if status == 0 {
            let remote = unsafe { from_wide(buffer.as_ptr()) };
            if remote.starts_with(r"\\") {
                found.push((format!("{}:", letter.to_ascii_uppercase()), remote));
            }
        }
    }
    // SAFETY: `key` came from a successful RegOpenKeyExW.
    unsafe { RegCloseKey(key) };
    found
}

/// Show the Windows "Map Network Drive" dialog, the one File Explorer uses
/// (drive letter, "Reconnect at sign-in", "Connect using different
/// credentials"). `remote` pre-fills and locks the folder field.
///
/// Returns the mapped drive (`Z:`) or `None` when the user cancelled.
pub fn map_drive_dialog(owner: isize, remote: Option<&str>) -> io::Result<Option<String>> {
    let mut remote_wide = remote.map(wide);
    let mut resource: NETRESOURCEW = unsafe { std::mem::zeroed() };
    resource.dwType = RESOURCETYPE_DISK;
    if let Some(remote) = remote_wide.as_mut() {
        resource.lpRemoteName = remote.as_mut_ptr();
    }
    let mut dialog = CONNECTDLGSTRUCTW {
        cbStructure: std::mem::size_of::<CONNECTDLGSTRUCTW>() as u32,
        hwndOwner: owner,
        lpConnRes: &mut resource,
        dwFlags: if remote.is_some() {
            CONNDLG_RO_PATH
        } else {
            CONNDLG_USE_MRU
        },
        dwDevNum: 0,
    };
    // SAFETY: every pointer in `dialog` outlives the modal call.
    let status = unsafe { WNetConnectionDialog1W(&mut dialog) };
    match status {
        NO_ERROR => Ok((dialog.dwDevNum >= 1 && dialog.dwDevNum <= 26)
            .then(|| format!("{}:", char::from(b'A' + (dialog.dwDevNum - 1) as u8)))),
        // The dialog returns -1 when the user presses Cancel.
        u32::MAX | ERROR_CANCELLED => Ok(None),
        code => Err(win32(code)),
    }
}

/// Connect to `remote` (`\\server\share`), optionally on a drive letter.
///
/// `prompt` forces the Windows credential dialog even when cached
/// credentials exist (used after the server refused them); otherwise Windows
/// uses the signed-in user and only asks when it must.
pub fn connect(
    owner: isize,
    remote: &str,
    local: Option<&str>,
    prompt: bool,
    persist: bool,
) -> io::Result<()> {
    let mut remote_wide = wide(remote);
    let mut local_wide = local.map(wide);
    let mut resource: NETRESOURCEW = unsafe { std::mem::zeroed() };
    resource.dwType = RESOURCETYPE_DISK;
    resource.lpRemoteName = remote_wide.as_mut_ptr();
    if let Some(local) = local_wide.as_mut() {
        resource.lpLocalName = local.as_mut_ptr();
    }
    let mut flags = CONNECT_INTERACTIVE;
    if prompt {
        flags |= CONNECT_PROMPT;
    }
    flags |= if persist && local.is_some() {
        CONNECT_UPDATE_PROFILE
    } else {
        CONNECT_TEMPORARY
    };
    // SAFETY: null user/password make Windows use the signed-in user or ask.
    let status = unsafe { WNetAddConnection3W(owner, &resource, null(), null(), flags) };
    match status {
        NO_ERROR => Ok(()),
        code => Err(win32(code)),
    }
}

/// Disconnect a mapped drive (`Z:`) or a deviceless `\\server\share`
/// connection and forget it for future sign-ins. Without `force`, Windows
/// refuses while files on the share are open (`ERROR_OPEN_FILES`, 2401).
pub fn disconnect(name: &str, force: bool) -> io::Result<()> {
    let name = wide(name.trim_end_matches(['\\', '/']));
    // SAFETY: `name` is NUL-terminated.
    let status =
        unsafe { WNetCancelConnection2W(name.as_ptr(), CONNECT_UPDATE_PROFILE, i32::from(force)) };
    match status {
        NO_ERROR => Ok(()),
        code => Err(win32(code)),
    }
}

/// Disk shares published by `server` (`\\server` or `server`).
pub fn shares(server: &str) -> io::Result<Vec<Share>> {
    let server = format!(
        r"\\{}",
        server
            .trim_start_matches(['\\', '/'])
            .trim_end_matches(['\\', '/'])
    );
    let server = wide(&server);
    let mut buffer: *mut u8 = null_mut();
    let (mut read, mut total, mut resume) = (0u32, 0u32, 0u32);
    let mut shares = Vec::new();
    loop {
        // SAFETY: NetShareEnum allocates `buffer`; it is released below.
        let status = unsafe {
            NetShareEnum(
                server.as_ptr(),
                1,
                &mut buffer,
                MAX_PREFERRED_LENGTH,
                &mut read,
                &mut total,
                &mut resume,
            )
        };
        if status != NERR_SUCCESS && status != ERROR_MORE_DATA {
            if !buffer.is_null() {
                unsafe { NetApiBufferFree(buffer.cast()) };
            }
            return Err(win32(status));
        }
        let records = buffer.cast::<SHARE_INFO_1>();
        for i in 0..read as usize {
            // SAFETY: NetShareEnum wrote `read` SHARE_INFO_1 records.
            let info = unsafe { *records.add(i) };
            if info.shi1_type & STYPE_MASK != STYPE_DISKTREE {
                continue;
            }
            shares.push(Share {
                name: unsafe { from_wide(info.shi1_netname) },
                remark: unsafe { from_wide(info.shi1_remark) },
                special: info.shi1_type & STYPE_SPECIAL != 0,
            });
        }
        if !buffer.is_null() {
            unsafe { NetApiBufferFree(buffer.cast()) };
            buffer = null_mut();
        }
        if status != ERROR_MORE_DATA {
            break;
        }
    }
    shares.sort_by_key(|s| s.name.to_lowercase());
    Ok(shares)
}

/// Windows error codes after which asking for different credentials can help.
pub const fn credentials_may_help(code: i32) -> bool {
    matches!(
        code,
        5       // ERROR_ACCESS_DENIED
        | 86    // ERROR_INVALID_PASSWORD
        | 1244  // ERROR_NOT_AUTHENTICATED
        | 1272  // guest access blocked by policy: a real account works
        | 1326  // ERROR_LOGON_FAILURE
        | 1907  // ERROR_PASSWORD_MUST_CHANGE
        | 2202 // NERR_BadUsername
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_codes_cover_the_explorer_prompt_cases() {
        for code in [5, 86, 1244, 1326] {
            assert!(credentials_may_help(code));
        }
        // Unreachable servers and missing shares must not trigger a prompt.
        for code in [2, 3, 53, 67, 1219, 1231] {
            assert!(!credentials_may_help(code));
        }
    }

    #[test]
    fn enumeration_never_fails_without_network() {
        // Works on any machine: the redirector tables may simply be empty.
        let drives = mapped_drives();
        assert!(drives.iter().all(|d| d.local.ends_with(':')));
    }
}
