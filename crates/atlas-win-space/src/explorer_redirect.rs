//! Opt-in: open Atlas where Windows would open a new File Explorer window.
//!
//! Recent Windows 11 builds route Win+E around the per-user `opennewwindow`
//! override (see [`crate::integration`]), and "Open file location", "Show in
//! folder" and `explorer /select,` never used it. A small agent
//! (`atlas.exe --explorer-redirect`) therefore watches for new Explorer frames
//! with an out-of-context WinEvent hook (no keyboard hook, nothing injected
//! into Explorer), reads their folder and selection through the documented
//! ShellWindows automation, has that Explorer window start Atlas there and
//! closes it.
//!
//! Explorer keeps windows opened while Shift is held, tabs added to an
//! existing window, and every location that is neither a file-system folder
//! nor the Explorer start page (Control Panel, Recycle Bin, Network,
//! archives, search results, ...).
//!
//! The choice is stored per user in `HKCU\Software\Atlas\Integration\Explorer`
//! and the agent starts with Windows through the `Run` key while any part of
//! it is enabled.
use std::{
    cell::RefCell,
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use windows::{
    core::{Interface, BSTR},
    Win32::{
        System::{
            Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED},
            Variant::VARIANT,
        },
        UI::Shell::{
            Folder2, IShellDispatch2, IShellFolderViewDual, IShellWindows, IWebBrowser2,
            ShellWindows,
        },
    },
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, HWND},
    System::{
        Recovery::{
            RegisterApplicationRestart, RESTART_NO_CRASH, RESTART_NO_HANG, RESTART_NO_REBOOT,
        },
        Registry::{
            RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD,
            REG_SZ, RRF_RT_REG_DWORD,
        },
        Threading::{
            CreateEventW, CreateMutexW, OpenEventW, OpenMutexW, ResetEvent, SetEvent,
            EVENT_MODIFY_STATE, INFINITE, SYNCHRONIZATION_SYNCHRONIZE,
        },
    },
    UI::{
        Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
        Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT},
        WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, EnumWindows, GetAncestor,
            GetClassNameW, IsWindow, IsWindowVisible, MsgWaitForMultipleObjects, PeekMessageW,
            PostQuitMessage, RegisterClassW, TranslateMessage, CHILDID_SELF, EVENT_OBJECT_SHOW,
            GA_ROOT, MSG, OBJID_WINDOW, PM_REMOVE, QS_ALLINPUT, WINEVENT_OUTOFCONTEXT,
            WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_ENDSESSION, WM_QUERYENDSESSION, WM_QUIT,
            WNDCLASSW,
        },
    },
};

/// First argument that turns `atlas.exe` into the redirect agent.
pub const AGENT_ARG: &str = "--explorer-redirect";
const CONFIG_KEY: &str = r"Software\Atlas\Integration\Explorer";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Atlas Explorer redirect";
const MUTEX: &str = r"Local\AtlasExplorerRedirect";
const STOP_EVENT: &str = r"Local\AtlasExplorerRedirectStop";
const HOME: &str = "::{F874310E-B6B7-47DC-BC84-B9E6B38F5903}";
const THIS_PC: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";
/// A new frame that has not shown a location by then is left alone.
const GIVE_UP: Duration = Duration::from_secs(5);
/// `SHOpenFolderAndSelectItems` selects just after the folder appears.
const SELECTION_GRACE: Duration = Duration::from_millis(150);
const TICK_MS: u32 = 25;

/// Which new Explorer windows open Atlas instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Redirect {
    /// Win+E and anything else that opens Explorer's start page (Home or
    /// This PC).
    pub start_page: bool,
    /// Folders opened from other apps: "Open file location", "Show in
    /// folder", `explorer /select,`, folder shortcuts, ...
    pub folders: bool,
}

impl Redirect {
    pub const fn any(self) -> bool {
        self.start_page || self.folders
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn read_flag(name: &str) -> bool {
    let (key, value) = (wide(CONFIG_KEY), wide(name));
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    code == 0 && data != 0
}

fn write_flag(name: &str, on: bool) -> io::Result<()> {
    let (key, value) = (wide(CONFIG_KEY), wide(name));
    let data = u32::from(on);
    check(unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            REG_DWORD,
            (&data as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    })
}

fn check(code: u32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

/// Whether the choice was ever stored ([`set`] writes both flags).
pub fn configured() -> bool {
    let (key, value) = (wide(CONFIG_KEY), wide("StartPage"));
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    code == 0
}

/// The user's current choice.
pub fn current() -> Redirect {
    Redirect {
        start_page: read_flag("StartPage"),
        folders: read_flag("Folders"),
    }
}

/// Store the choice, register or remove the agent's autostart, and start or
/// stop the agent so the change applies at once.
pub fn set(redirect: Redirect) -> io::Result<()> {
    write_flag("StartPage", redirect.start_page)?;
    write_flag("Folders", redirect.folders)?;
    if redirect.any() {
        // Packaged (MSIX) writes to HKCU are virtualized, so a Run value would
        // never be seen; there Atlas starts the agent itself on launch.
        if !crate::integration::is_packaged() {
            set_autostart(true)?;
        }
        ensure_agent()?;
    } else {
        set_autostart(false)?;
        stop_agent();
    }
    Ok(())
}

/// Turn everything off: used when the user disables it and by the uninstaller.
pub fn disable() -> io::Result<()> {
    set(Redirect::default())
}

/// [`disable`], then give a running agent up to `timeout` to exit so an
/// uninstaller can delete the executable it runs from.
pub fn disable_and_wait(timeout: Duration) -> io::Result<()> {
    let result = disable();
    let deadline = Instant::now() + timeout;
    while agent_running() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    result
}

fn set_autostart(on: bool) -> io::Result<()> {
    let (key, value) = (wide(RUN_KEY), wide(RUN_VALUE));
    if on {
        let exe = crate::integration::launch_executable()?;
        let exe = exe.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Executable path is not Unicode",
            )
        })?;
        let command = wide(&format!("\"{exe}\" {AGENT_ARG}"));
        check(unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        })
    } else {
        let code = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
        if code == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            check(code)
        }
    }
}

fn agent_running() -> bool {
    let name = wide(MUTEX);
    let handle = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, name.as_ptr()) };
    if handle == 0 {
        return false;
    }
    unsafe { CloseHandle(handle) };
    true
}

/// Start the agent when it is enabled but not running (Atlas calls this on
/// launch, so a crashed or packaged agent comes back).
pub fn ensure_agent() -> io::Result<()> {
    if !current().any() || agent_running() {
        return Ok(());
    }
    let exe = crate::integration::launch_executable()?;
    std::process::Command::new(exe)
        .arg(AGENT_ARG)
        .spawn()
        .map(drop)
}

fn stop_agent() {
    let name = wide(STOP_EVENT);
    let handle = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
    if handle != 0 {
        unsafe {
            SetEvent(handle);
            CloseHandle(handle);
        }
    }
}

/// What a new Explorer window shows, read through its automation object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Shown {
    /// `Folder.Self.Path`: a file-system path, or `::{CLSID}` for shell
    /// places such as Home.
    location: String,
    file_system_folder: bool,
    selected: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Leave,
    /// Open Atlas; `None` opens it as a plain launch would.
    Open(Option<PathBuf>),
}

fn decide(redirect: Redirect, shift: bool, shown: &Shown) -> Decision {
    if shift {
        return Decision::Leave;
    }
    let start_page =
        shown.location.eq_ignore_ascii_case(HOME) || shown.location.eq_ignore_ascii_case(THIS_PC);
    if start_page {
        return if redirect.start_page {
            Decision::Open(None)
        } else {
            Decision::Leave
        };
    }
    if redirect.folders && shown.file_system_folder && !shown.location.is_empty() {
        return Decision::Open(Some(
            shown
                .selected
                .clone()
                .unwrap_or_else(|| PathBuf::from(&shown.location)),
        ));
    }
    Decision::Leave
}

/// Quote one argument for `CommandLineToArgvW`-style parsing. Paths cannot
/// contain `"`, but a trailing `\` (as in `C:\`) must be doubled.
fn quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.ends_with('\\') {
        format!("\"{text}\\\"")
    } else {
        format!("\"{text}\"")
    }
}

/// Entry point for `atlas.exe --explorer-redirect`; `false` for any other
/// command line.
pub fn try_run() -> bool {
    if std::env::args_os()
        .nth(1)
        .is_none_or(|arg| arg != AGENT_ARG)
    {
        return false;
    }
    let name = wide(MUTEX);
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if mutex == 0 || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return true;
    }
    if current().any() {
        let _ = run_agent();
    }
    unsafe { CloseHandle(mutex) };
    true
}

struct Pending {
    hwnd: HWND,
    since: Instant,
    shift: bool,
    located: Option<Instant>,
}

thread_local! {
    static SEEN: RefCell<HashSet<HWND>> = RefCell::new(HashSet::new());
    static PENDING: RefCell<Vec<Pending>> = const { RefCell::new(Vec::new()) };
}

fn is_explorer_frame(hwnd: HWND) -> bool {
    let mut class = [0u16; 32];
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
    len > 0 && String::from_utf16_lossy(&class[..len as usize]) == "CabinetWClass"
}

unsafe extern "system" fn on_window_shown(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    if object != OBJID_WINDOW || child != CHILDID_SELF as i32 || hwnd == 0 {
        return;
    }
    if GetAncestor(hwnd, GA_ROOT) != hwnd || !is_explorer_frame(hwnd) {
        return;
    }
    let new = SEEN.with(|seen| {
        let mut seen = seen.borrow_mut();
        if seen.len() > 256 {
            seen.retain(|&known| IsWindow(known) != 0);
        }
        seen.insert(hwnd)
    });
    if new {
        let shift = GetAsyncKeyState(i32::from(VK_SHIFT)) < 0;
        PENDING.with(|pending| {
            pending.borrow_mut().push(Pending {
                hwnd,
                since: Instant::now(),
                shift,
                located: None,
            });
        });
    }
}

unsafe extern "system" fn remember_existing(hwnd: HWND, _: isize) -> i32 {
    if IsWindowVisible(hwnd) != 0 && is_explorer_frame(hwnd) {
        SEEN.with(|seen| seen.borrow_mut().insert(hwnd));
    }
    1
}

unsafe extern "system" fn agent_window(
    hwnd: HWND,
    message: u32,
    wparam: usize,
    lparam: isize,
) -> isize {
    match message {
        // Restart Manager (installer updates) and log-off ask through these.
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION if wparam != 0 => {
            PostQuitMessage(0);
            0
        }
        WM_CLOSE => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn run_agent() -> windows::core::Result<()> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let stop_name = wide(STOP_EVENT);
    let stop = unsafe { CreateEventW(std::ptr::null(), 1, 0, stop_name.as_ptr()) };
    if stop == 0 {
        return Err(windows::core::Error::from_win32());
    }
    unsafe { ResetEvent(stop) };
    // A hidden top-level window lets Restart Manager close the agent during
    // an update instead of the installer failing on a locked atlas.exe.
    let class = wide("AtlasExplorerRedirect");
    let window_class = WNDCLASSW {
        lpfnWndProc: Some(agent_window),
        lpszClassName: class.as_ptr(),
        ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        RegisterClassW(&window_class);
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
            std::ptr::null(),
        );
        let restart = wide(AGENT_ARG);
        RegisterApplicationRestart(
            restart.as_ptr(),
            RESTART_NO_CRASH | RESTART_NO_HANG | RESTART_NO_REBOOT,
        );
        EnumWindows(Some(remember_existing), 0);
    }
    let hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_SHOW,
            0,
            Some(on_window_shown),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    'agent: loop {
        let busy = PENDING.with(|pending| !pending.borrow().is_empty());
        let wait = unsafe {
            MsgWaitForMultipleObjects(
                1,
                &stop,
                0,
                if busy { TICK_MS } else { INFINITE },
                QS_ALLINPUT,
            )
        };
        if wait == 0 {
            break;
        }
        let mut message: MSG = unsafe { std::mem::zeroed() };
        while unsafe { PeekMessageW(&mut message, 0, 0, 0, PM_REMOVE) } != 0 {
            if message.message == WM_QUIT {
                break 'agent;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if PENDING.with(|pending| !pending.borrow().is_empty()) {
            let redirect = current();
            if !redirect.any() {
                break;
            }
            process_pending(redirect);
        }
    }
    unsafe {
        if hook != 0 {
            UnhookWinEvent(hook);
        }
        CloseHandle(stop);
    }
    Ok(())
}

fn process_pending(redirect: Redirect) {
    let pending = PENDING.with(|pending| std::mem::take(&mut *pending.borrow_mut()));
    let mut keep = Vec::new();
    for mut item in pending {
        if unsafe { IsWindow(item.hwnd) } == 0 || item.since.elapsed() > GIVE_UP {
            continue;
        }
        match handle(&mut item, redirect) {
            Ok(true) => {}
            Ok(false) | Err(_) => keep.push(item),
        }
    }
    PENDING.with(|pending| pending.borrow_mut().extend(keep));
}

/// `Ok(true)` once the window is dealt with (opened in Atlas or left alone),
/// `Ok(false)` while Explorer is still setting it up.
fn handle(item: &mut Pending, redirect: Redirect) -> windows::core::Result<bool> {
    let windows: IShellWindows = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)? };
    let mut browsers = Vec::new();
    for index in 0..unsafe { windows.Count()? } {
        let Ok(dispatch) = (unsafe { windows.Item(&VARIANT::from(index)) }) else {
            continue;
        };
        let Ok(browser) = dispatch.cast::<IWebBrowser2>() else {
            continue;
        };
        if unsafe { browser.HWND() }.is_ok_and(|frame| frame.0 == item.hwnd) {
            browsers.push(browser);
        }
    }
    let browser = match browsers.len() {
        // Not registered yet.
        0 => return Ok(false),
        1 => browsers.remove(0),
        // A frame with tabs: closing it would close the user's other tabs.
        _ => return Ok(true),
    };
    let Some(shown) = read_shown(&browser) else {
        return Ok(false);
    };
    if shown.location.is_empty() {
        return Ok(false);
    }
    if shown.file_system_folder && shown.selected.is_none() {
        let located = *item.located.get_or_insert_with(Instant::now);
        if located.elapsed() < SELECTION_GRACE {
            return Ok(false);
        }
    }
    if let Decision::Open(target) = decide(redirect, item.shift, &shown) {
        open_in_atlas(&browser, target.as_deref());
    }
    Ok(true)
}

fn read_shown(browser: &IWebBrowser2) -> Option<Shown> {
    unsafe {
        let view: IShellFolderViewDual = browser.Document().ok()?.cast().ok()?;
        let folder: Folder2 = view.Folder().ok()?.cast().ok()?;
        let place = folder.Self_().ok()?;
        let location = place.Path().ok()?.to_string();
        let file_system_folder = place.IsFileSystem().is_ok_and(|flag| flag.as_bool())
            && place.IsFolder().is_ok_and(|flag| flag.as_bool());
        let selected = view.SelectedItems().ok().and_then(|items| {
            if items.Count().ok()? < 1 {
                return None;
            }
            let first = items.Item(&VARIANT::from(0)).ok()?;
            if !first.IsFileSystem().ok()?.as_bool() {
                return None;
            }
            Some(PathBuf::from(first.Path().ok()?.to_string()))
        });
        Some(Shown {
            location,
            file_system_folder,
            selected,
        })
    }
}

/// Have the Explorer window start Atlas, then close it. Explorer owns the
/// foreground at this point, so Atlas comes up in front; a launch from this
/// background agent would open behind the other windows.
fn open_in_atlas(browser: &IWebBrowser2, target: Option<&Path>) {
    let Ok(exe) = crate::integration::launch_executable() else {
        return;
    };
    let arguments = target.map(quote).unwrap_or_default();
    let through_explorer = unsafe {
        (|| -> windows::core::Result<()> {
            let view: IShellFolderViewDual = browser.Document()?.cast()?;
            let shell: IShellDispatch2 = view.Application()?.cast()?;
            shell.ShellExecute(
                &BSTR::from(exe.to_string_lossy().as_ref()),
                &VARIANT::from(arguments.as_str()),
                &VARIANT::default(),
                &VARIANT::from("open"),
                &VARIANT::from(1i32),
            )
        })()
    };
    if through_explorer.is_err() {
        let mut command = std::process::Command::new(&exe);
        if let Some(target) = target {
            command.arg(target);
        }
        let _ = command.spawn();
    }
    let _ = unsafe { browser.Quit() };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(location: &str, file_system_folder: bool, selected: Option<&str>) -> Shown {
        Shown {
            location: location.into(),
            file_system_folder,
            selected: selected.map(PathBuf::from),
        }
    }

    #[test]
    fn start_page_and_folders_follow_their_own_switch() {
        let both = Redirect {
            start_page: true,
            folders: true,
        };
        let home = shown(&HOME.to_lowercase(), false, None);
        assert_eq!(decide(both, false, &home), Decision::Open(None));
        assert_eq!(
            decide(
                Redirect {
                    start_page: false,
                    ..both
                },
                false,
                &home
            ),
            Decision::Leave
        );
        let pc = shown(THIS_PC, false, None);
        assert_eq!(decide(both, false, &pc), Decision::Open(None));
        let folder = shown(r"C:\Windows", true, Some(r"C:\Windows\win.ini"));
        assert_eq!(
            decide(both, false, &folder),
            Decision::Open(Some(PathBuf::from(r"C:\Windows\win.ini")))
        );
        assert_eq!(
            decide(
                Redirect {
                    folders: false,
                    ..both
                },
                false,
                &folder
            ),
            Decision::Leave
        );
        let plain = shown(r"\\server\share\docs", true, None);
        assert_eq!(
            decide(both, false, &plain),
            Decision::Open(Some(PathBuf::from(r"\\server\share\docs")))
        );
    }

    #[test]
    fn shift_and_shell_places_stay_in_explorer() {
        let both = Redirect {
            start_page: true,
            folders: true,
        };
        let folder = shown(r"C:\Windows", true, None);
        assert_eq!(decide(both, true, &folder), Decision::Leave);
        assert_eq!(
            decide(both, true, &shown(HOME, false, None)),
            Decision::Leave
        );
        let control_panel = shown("::{26EE0668-A00A-44D7-9371-BEB064C98683}", false, None);
        assert_eq!(decide(both, false, &control_panel), Decision::Leave);
        let archive = shown(r"C:\Downloads\photos.zip\2024", false, None);
        assert_eq!(decide(both, false, &archive), Decision::Leave);
        assert_eq!(decide(Redirect::default(), false, &folder), Decision::Leave);
    }

    #[test]
    fn arguments_survive_command_line_parsing() {
        assert_eq!(quote(Path::new(r"C:\")), r#""C:\\""#);
        assert_eq!(
            quote(Path::new(r"C:\Program Files\a b.txt")),
            r#""C:\Program Files\a b.txt""#
        );
        assert_eq!(
            quote(Path::new(r"\\server\share\")),
            r#""\\server\share\\""#
        );
    }
}
