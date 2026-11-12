//! Native SMB through the Windows Multiple Provider Router (`mpr.dll`) and the
//! LAN Manager share API (`netapi32.dll`).
//!
//! Sign-ins use the same Windows credential dialog as File Explorer, but Atlas
//! makes exactly one connection attempt per answer instead of letting Windows
//! retry in a loop, so a wrong password never turns into a burst of failed
//! logons on the server. The password is wiped from memory right after that
//! attempt; when the user ticks "Remember my credentials" it is handed to
//! Credential Manager, like Explorer does.
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_CANCELLED, ERROR_MORE_DATA, HANDLE, NO_ERROR};
use windows_sys::Win32::NetworkManagement::NetManagement::NetApiBufferFree;
use windows_sys::Win32::NetworkManagement::WNet::{
    WNetAddConnection2W, WNetCancelConnection2W, WNetCloseEnum, WNetConnectionDialog1W,
    WNetEnumResourceW, WNetGetConnectionW, WNetOpenEnumW, CONNDLG_RO_PATH, CONNDLG_USE_MRU,
    CONNECTDLGSTRUCTW, CONNECT_TEMPORARY, CONNECT_UPDATE_PROFILE, NETRESOURCEW,
    RESOURCETYPE_DISK, RESOURCE_CONNECTED, RESOURCE_REMEMBERED,
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

/// `\\server\share` behind `local` (`Z:`) when it is a network drive,
/// connected or remembered. Local disks answer `None` from the drive type
/// alone, so this is cheap enough to call for every listing.
pub fn network_remote_of(local: &str) -> Option<String> {
    let letter = local.trim_end_matches(['\\', '/']);
    match drive_type(letter) {
        DRIVE_REMOTE | DRIVE_NO_ROOT_DIR => remote_of(letter),
        _ => None,
    }
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

/// A user name and password typed into the Windows credential dialog. The
/// password lives only as long as one connection attempt and is wiped from
/// memory when this value is dropped.
pub struct Credentials {
    /// NUL-terminated `DOMAIN\user`, `user@domain` or `user`.
    user: Vec<u16>,
    /// NUL-terminated.
    password: Vec<u16>,
    /// The user ticked "Remember my credentials".
    pub save: bool,
}

impl Credentials {
    pub fn user(&self) -> String {
        String::from_utf16_lossy(self.user.split(|&c| c == 0).next().unwrap_or_default())
    }
}

impl Drop for Credentials {
    fn drop(&mut self) {
        wipe(&mut self.password);
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials").field("user", &self.user()).finish_non_exhaustive()
    }
}

/// Overwrite secret UTF-16 text so it does not linger in freed memory.
fn wipe(buffer: &mut [u16]) {
    for unit in buffer.iter_mut() {
        // SAFETY: `unit` is a valid, aligned element of `buffer`.
        unsafe { std::ptr::write_volatile(unit, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Show the Windows credential dialog (the one File Explorer uses) for
/// `target` (`\\server\share`). `refused` is the error of the previous
/// attempt (`1326` shows "The user name or password is incorrect"), or 0.
///
/// Showing the dialog never contacts the server: nothing is attempted until
/// the caller passes the answer to [`connect`]. `Ok(None)` means Cancel.
pub fn prompt_credentials(owner: isize, target: &str, refused: u32) -> io::Result<Option<Credentials>> {
    use windows_sys::Win32::Security::Credentials::{
        CredUIPromptForWindowsCredentialsW, CredUnPackAuthenticationBufferW, CREDUIWIN_CHECKBOX,
        CREDUI_INFOW, CRED_PACK_PROTECTED_CREDENTIALS,
    };
    let caption = wide("Atlas");
    let message = wide(target);
    let info = CREDUI_INFOW {
        cbSize: std::mem::size_of::<CREDUI_INFOW>() as u32,
        hwndParent: owner,
        pszMessageText: message.as_ptr(),
        pszCaptionText: caption.as_ptr(),
        hbmBanner: 0,
    };
    let mut package = 0u32;
    let mut out: *mut std::ffi::c_void = null_mut();
    let mut out_len = 0u32;
    let mut save = 0i32;
    // SAFETY: every pointer is valid for the call; `out` is freed below.
    let status = unsafe {
        CredUIPromptForWindowsCredentialsW(
            &info,
            refused,
            &mut package,
            null(),
            0,
            &mut out,
            &mut out_len,
            &mut save,
            CREDUIWIN_CHECKBOX,
        )
    };
    if status == ERROR_CANCELLED {
        return Ok(None);
    }
    if status != NO_ERROR {
        return Err(win32(status));
    }
    let mut user = vec![0u16; 514];
    let mut domain = vec![0u16; 338];
    let mut password = vec![0u16; 514];
    let unpacked = [CRED_PACK_PROTECTED_CREDENTIALS, 0].into_iter().any(|flags| {
        let (mut user_len, mut domain_len, mut password_len) =
            (user.len() as u32, domain.len() as u32, password.len() as u32);
        // SAFETY: the lengths are the buffer capacities in UTF-16 units.
        unsafe {
            CredUnPackAuthenticationBufferW(
                flags,
                out,
                out_len,
                user.as_mut_ptr(),
                &mut user_len,
                domain.as_mut_ptr(),
                &mut domain_len,
                password.as_mut_ptr(),
                &mut password_len,
            ) != 0
        }
    });
    let error = io::Error::last_os_error();
    // SAFETY: CredUI allocated `out_len` bytes at `out` with CoTaskMemAlloc.
    unsafe {
        std::ptr::write_bytes(out.cast::<u8>(), 0, out_len as usize);
        windows::Win32::System::Com::CoTaskMemFree(Some(out.cast_const()));
    }
    if !unpacked {
        wipe(&mut password);
        return Err(error);
    }
    let text = |buffer: &[u16]| buffer.iter().take_while(|&&c| c != 0).copied().collect::<Vec<u16>>();
    let (name, domain) = (text(&user), text(&domain));
    let mut user: Vec<u16> = if domain.is_empty() {
        name
    } else {
        domain.into_iter().chain(Some(u16::from(b'\\'))).chain(name).collect()
    };
    user.push(0);
    Ok(Some(Credentials { user, password, save: save != 0 }))
}

/// One sign-in attempt to `remote` (`\\server\share`), optionally on a drive
/// letter. `None` uses the signed-in user or the credentials saved in
/// Credential Manager. Never shows UI and never retries: the caller decides
/// whether a refused attempt may be followed by another.
pub fn connect(remote: &str, local: Option<&str>, credentials: Option<&Credentials>) -> io::Result<()> {
    let mut remote_wide = wide(remote);
    let mut local_wide = local.map(wide);
    let mut resource: NETRESOURCEW = unsafe { std::mem::zeroed() };
    resource.dwType = RESOURCETYPE_DISK;
    resource.lpRemoteName = remote_wide.as_mut_ptr();
    if let Some(local) = local_wide.as_mut() {
        resource.lpLocalName = local.as_mut_ptr();
    }
    let (user, password) = credentials.map_or((null(), null()), |c| (c.user.as_ptr(), c.password.as_ptr()));
    // SAFETY: the strings are NUL-terminated and outlive the call.
    let status = unsafe { WNetAddConnection2W(&resource, password, user, CONNECT_TEMPORARY) };
    match status {
        NO_ERROR => Ok(()),
        code => Err(win32(code)),
    }
}

/// Store `credentials` for `server` in Credential Manager, as File Explorer
/// does for "Remember my credentials", so Windows signs in on its own next
/// time. Call only after they were accepted.
pub fn remember_credentials(server: &str, credentials: &Credentials) -> io::Result<()> {
    use windows_sys::Win32::Security::Credentials::{
        CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_DOMAIN_PASSWORD,
    };
    let mut target = wide(server.trim_matches(['\\', '/']));
    let mut user = credentials.user.clone();
    let secret_len = credentials.password.iter().take_while(|&&c| c != 0).count();
    let mut secret = credentials.password[..secret_len].to_vec();
    let credential = CREDENTIALW {
        Flags: 0,
        Type: CRED_TYPE_DOMAIN_PASSWORD,
        TargetName: target.as_mut_ptr(),
        Comment: null_mut(),
        LastWritten: windows_sys::Win32::Foundation::FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 },
        CredentialBlobSize: (secret.len() * 2) as u32,
        CredentialBlob: secret.as_mut_ptr().cast(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: null_mut(),
        TargetAlias: null_mut(),
        UserName: user.as_mut_ptr(),
    };
    // SAFETY: every pointer in `credential` outlives the call.
    let written = unsafe { CredWriteW(&credential, 0) } != 0;
    let error = io::Error::last_os_error();
    wipe(&mut secret);
    if written { Ok(()) } else { Err(error) }
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
