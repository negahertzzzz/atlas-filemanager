//! Explicit, reversible per-user Win+E integration. Startup only reads status.
use std::{
    io,
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::*,
};

const TARGET: &str =
    r"Software\Classes\CLSID\{52205fd8-5dfb-447d-801a-d0b52f2e83e1}\shell\opennewwindow\command";
const BACKUP: &str = r"Software\Atlas\Integration\WinE";
const DELEGATE: &str = "DelegateExecute";

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Value {
    kind: u32,
    bytes: Vec<u8>,
}
impl Value {
    fn text(text: &str) -> Self {
        Self {
            kind: REG_SZ,
            bytes: text
                .encode_utf16()
                .chain(Some(0))
                .flat_map(u16::to_le_bytes)
                .collect(),
        }
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn check(code: u32) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}
impl Key {
    fn open(path: &str, create: bool) -> io::Result<Option<Self>> {
        let mut key = 0;
        let name = wide(path);
        let code = unsafe {
            if create {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    name.as_ptr(),
                    0,
                    null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_READ | KEY_WRITE,
                    null(),
                    &mut key,
                    null_mut(),
                )
            } else {
                RegOpenKeyExW(HKEY_CURRENT_USER, name.as_ptr(), 0, KEY_READ, &mut key)
            }
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(code)?;
        Ok(Some(Self(key)))
    }
    fn get(&self, name: &str) -> io::Result<Option<Value>> {
        let name = wide(name);
        let (mut kind, mut size) = (0, 0);
        let code = unsafe {
            RegQueryValueExW(
                self.0,
                name.as_ptr(),
                null(),
                &mut kind,
                null_mut(),
                &mut size,
            )
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(code)?;
        if size > 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Registry value too large",
            ));
        }
        let mut bytes = vec![0; size as usize];
        check(unsafe {
            RegQueryValueExW(
                self.0,
                name.as_ptr(),
                null(),
                &mut kind,
                bytes.as_mut_ptr(),
                &mut size,
            )
        })?;
        bytes.truncate(size as usize);
        Ok(Some(Value { kind, bytes }))
    }
    fn set(&self, name: &str, value: Option<&Value>) -> io::Result<()> {
        let name = wide(name);
        let code = unsafe {
            match value {
                Some(value) => RegSetValueExW(
                    self.0,
                    name.as_ptr(),
                    0,
                    value.kind,
                    value.bytes.as_ptr(),
                    value.bytes.len() as u32,
                ),
                None => RegDeleteValueW(self.0, name.as_ptr()),
            }
        };
        if code == ERROR_FILE_NOT_FOUND && value.is_none() {
            Ok(())
        } else {
            check(code)
        }
    }
}
fn command(executable: &Path) -> io::Result<Value> {
    let text = executable.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Executable path is not Unicode",
        )
    })?;
    if text.contains('"') || !executable.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid executable path",
        ));
    }
    Ok(Value::text(&format!("\"{text}\"")))
}
fn save_original(backup: &Key, name: &str, original: Option<&Value>) -> io::Result<()> {
    // One binary value preserves both the native registry type and raw bytes.
    let bytes = original
        .map(|value| {
            let mut bytes = value.kind.to_le_bytes().to_vec();
            bytes.extend_from_slice(&value.bytes);
            bytes
        })
        .unwrap_or_default();
    backup.set(
        name,
        Some(&Value {
            kind: REG_BINARY,
            bytes,
        }),
    )
}
fn original(backup: &Key, name: &str) -> io::Result<Option<Value>> {
    let value = backup
        .get(name)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Missing Win+E backup"))?;
    if value.bytes.is_empty() {
        return Ok(None);
    }
    if value.kind != REG_BINARY || value.bytes.len() < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid Win+E backup",
        ));
    }
    Ok(Some(Value {
        kind: u32::from_le_bytes(value.bytes[..4].try_into().unwrap()),
        bytes: value.bytes[4..].to_vec(),
    }))
}
fn owned(target: &Key, backup: &Key) -> io::Result<bool> {
    let expected = backup.get("AtlasCommand")?;
    Ok(expected.is_some()
        && target.get("")? == expected
        && target.get(DELEGATE)? == Some(Value::text("")))
}
pub fn win_e_enabled() -> io::Result<bool> {
    let (Some(target), Some(backup)) = (Key::open(TARGET, false)?, Key::open(BACKUP, false)?)
    else {
        return Ok(false);
    };
    owned(&target, &backup)
}
pub fn set_win_e_enabled(enabled: bool) -> io::Result<()> {
    set_enabled_at(TARGET, BACKUP, &launch_executable()?, enabled)
}

/// Command-line argument the uninstaller runs `atlas.exe` with.
pub const UNINSTALL_ARG: &str = "--remove-shell-integration";

/// Undo what Atlas changed in the user's shell before its files go away: the
/// Win+E override, the Explorer redirect agent and its autostart. Best effort,
/// since an uninstaller has no one to report errors to.
pub fn remove_for_uninstall() {
    let _ = set_win_e_enabled(false);
    let _ = crate::explorer_redirect::disable_and_wait(std::time::Duration::from_secs(3));
}
/// MSIX installs live under `WindowsApps`, which Explorer cannot launch
/// directly; the package's execution alias is the stable launchable path.
pub(crate) fn launch_executable() -> io::Result<std::path::PathBuf> {
    if is_packaged() {
        let local = std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))?;
        let alias = Path::new(&local).join(r"Microsoft\WindowsApps\atlas.exe");
        if alias.exists() {
            return Ok(alias);
        }
    }
    std::env::current_exe()
}
pub fn is_packaged() -> bool {
    use windows_sys::Win32::{
        Foundation::ERROR_INSUFFICIENT_BUFFER, Storage::Packaging::Appx::GetCurrentPackageFullName,
    };
    let mut len = 0;
    unsafe {
        GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) == ERROR_INSUFFICIENT_BUFFER
    }
}
fn set_enabled_at(
    target_path: &str,
    backup_path: &str,
    executable: &Path,
    enabled: bool,
) -> io::Result<()> {
    let backup = Key::open(backup_path, enabled)?;
    let Some(backup) = backup else {
        return Ok(());
    };
    let target = Key::open(target_path, true)?.expect("created registry key");
    let existing_owner = backup.get("AtlasCommand")?.is_some();
    if existing_owner && !owned(&target, &backup)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Win+E was changed by another application; its command was preserved",
        ));
    }
    if enabled {
        let command = command(executable)?;
        if !existing_owner {
            save_original(&backup, "Command", target.get("")?.as_ref())?;
            save_original(&backup, "Delegate", target.get(DELEGATE)?.as_ref())?;
        }
        let old_command = target.get("")?;
        let old_delegate = target.get(DELEGATE)?;
        let old_owner = backup.get("AtlasCommand")?;
        let result = (|| {
            backup.set("AtlasCommand", Some(&command))?;
            target.set("", Some(&command))?;
            target.set(DELEGATE, Some(&Value::text("")))
        })();
        if let Err(error) = result {
            target.set("", old_command.as_ref())?;
            target.set(DELEGATE, old_delegate.as_ref())?;
            backup.set("AtlasCommand", old_owner.as_ref())?;
            return Err(error);
        }
    } else if existing_owner {
        let old_command = original(&backup, "Command")?;
        let old_delegate = original(&backup, "Delegate")?;
        let installed = target.get("")?;
        target.set("", old_command.as_ref())?;
        if let Err(error) = target.set(DELEGATE, old_delegate.as_ref()) {
            target.set("", installed.as_ref())?;
            return Err(error);
        }
        // Only remove our private backup after both original values are restored.
        check(unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(backup_path).as_ptr()) })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_registry_round_trip_preserves_types_and_newer_owner() {
        let root = format!(r"Software\Atlas\Tests\WinE-{}", std::process::id());
        let target_path = format!(r"{root}\target");
        let backup_path = format!(r"{root}\backup");
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                unsafe {
                    RegDeleteTreeW(HKEY_CURRENT_USER, wide(&self.0).as_ptr());
                }
            }
        }
        let _cleanup = Cleanup(root);
        let target = Key::open(&target_path, true).unwrap().unwrap();
        let original = Value {
            kind: REG_EXPAND_SZ,
            bytes: Value::text("%SystemRoot%\\explorer.exe").bytes,
        };
        target.set("", Some(&original)).unwrap();
        let exe = Path::new(r"C:\Atlas Test\atlas.exe");
        set_enabled_at(&target_path, &backup_path, exe, true).unwrap();
        set_enabled_at(&target_path, &backup_path, exe, true).unwrap();
        assert_eq!(target.get("").unwrap(), Some(command(exe).unwrap()));
        set_enabled_at(&target_path, &backup_path, exe, false).unwrap();
        assert_eq!(target.get("").unwrap(), Some(original));
        assert_eq!(target.get(DELEGATE).unwrap(), None);
        set_enabled_at(&target_path, &backup_path, exe, true).unwrap();
        let other = Value::text("other.exe");
        target.set("", Some(&other)).unwrap();
        assert!(set_enabled_at(&target_path, &backup_path, exe, false).is_err());
        assert_eq!(target.get("").unwrap(), Some(other));
    }
}
/// Call on an initialized COM apartment after the startup shell work settles.
pub fn free_unused_com_libraries() {
    unsafe {
        windows::Win32::System::Com::CoFreeUnusedLibraries();
    }
}

pub fn double_click_interval_ms() -> u32 {
    unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }
}
