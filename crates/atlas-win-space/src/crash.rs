//! Last-chance crash reports. Everything the fault path needs — file paths, a
//! dedicated writer thread and its events — is prepared at install time.
//!
//! * Native faults (SEH) hand the dump off to the writer thread: the faulting
//!   thread may have no stack left (overflow), so it only signals and waits.
//! * Panics are appended to `panics.log` with message, location and thread.
//!   Release builds unwind, so worker panics are usually contained; only a
//!   panic on the main (UI) thread is fatal and gets a minidump.
//! * A per-process session sentinel lets the next launch detect that the
//!   previous run ended without a clean exit and surface its reports.
mod dbghelp;
mod session;

pub use session::{mark_clean_exit, previous_crash, PreviousCrash};

use std::{
    io::{self, Write as _},
    path::{Path, PathBuf},
    ptr::null,
    sync::{
        atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicU32, Ordering},
        OnceLock,
    },
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
    Storage::FileSystem::{CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ},
    System::{
        Diagnostics::Debug::{
            SetUnhandledExceptionFilter, EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION,
        },
        Threading::{
            CreateEventW, GetCurrentProcessId, GetCurrentThreadId, SetEvent,
            SetThreadStackGuarantee, WaitForSingleObject, INFINITE,
        },
    },
};

static DUMP_PATH: OnceLock<Vec<u16>> = OnceLock::new();
static STACK_PATH: OnceLock<Vec<u16>> = OnceLock::new();
static PANIC_PATH: OnceLock<PathBuf> = OnceLock::new();
static PANIC_LOG: OnceLock<PathBuf> = OnceLock::new();
static VERSION: OnceLock<String> = OnceLock::new();
static WRITING: AtomicBool = AtomicBool::new(false);
static MAIN_THREAD: AtomicU32 = AtomicU32::new(0);
static WRITER_THREAD: AtomicU32 = AtomicU32::new(0);
static REQUEST_EVENT: AtomicIsize = AtomicIsize::new(0);
static DONE_EVENT: AtomicIsize = AtomicIsize::new(0);
static FAULT_THREAD: AtomicU32 = AtomicU32::new(0);
static FAULT_POINTERS: AtomicPtr<EXCEPTION_POINTERS> = AtomicPtr::new(std::ptr::null_mut());

/// The panic log is for diagnosis, not an archive: past this it rotates once.
const PANIC_LOG_LIMIT: u64 = 1024 * 1024;

pub fn install() -> io::Result<()> {
    install_for("unknown")
}

/// Installs the reporters; `version` is stamped into every report.
pub fn install_for(version: &str) -> io::Result<()> {
    install_at(crash_directory()?, version)
}

/// `%LOCALAPPDATA%\Atlas\Crashes`.
pub fn crash_directory() -> io::Result<PathBuf> {
    Ok(std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA"))?
        .join("Atlas")
        .join("Crashes"))
}

fn install_at(directory: PathBuf, version: &str) -> io::Result<()> {
    if DUMP_PATH.get().is_some() {
        return Ok(());
    }
    dbghelp::load()?;
    std::fs::create_dir_all(&directory)?;
    let _ = VERSION.set(version.to_owned());
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = directory.join(format!("atlas-{}-{timestamp}.dmp", std::process::id()));
    let _ = STACK_PATH.set(super::wide(path.with_extension("stack.txt").as_os_str()));
    let _ = PANIC_PATH.set(path.with_extension("panic.txt"));
    let _ = DUMP_PATH.set(super::wide(path.as_os_str()));
    let _ = PANIC_LOG.set(directory.join("panics.log"));
    session::begin(&directory, version);
    unsafe {
        MAIN_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
        // Leaves room for the filter itself to run after a stack overflow.
        let mut guarantee = 64 * 1024;
        SetThreadStackGuarantee(&mut guarantee);
    }
    spawn_writer();
    unsafe {
        SetUnhandledExceptionFilter(Some(unhandled_exception));
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        record_panic(info);
        previous(info);
    }));
    Ok(())
}

/// Pre-spawns the thread that writes native-fault reports, so the fault path
/// needs no stack, allocation or thread creation of its own.
fn spawn_writer() {
    let (request, done) = unsafe {
        (
            CreateEventW(null(), 0, 0, null()),
            CreateEventW(null(), 0, 0, null()),
        )
    };
    if request == 0 || done == 0 {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("atlas-crash-writer".into())
        .stack_size(512 * 1024)
        .spawn(move || {
            WRITER_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
            loop {
                if unsafe { WaitForSingleObject(request, INFINITE) } != WAIT_OBJECT_0 {
                    return;
                }
                unsafe {
                    write_report(
                        FAULT_THREAD.load(Ordering::SeqCst),
                        FAULT_POINTERS.load(Ordering::SeqCst),
                    );
                    SetEvent(done);
                }
            }
        });
    if spawned.is_ok() {
        REQUEST_EVENT.store(request, Ordering::SeqCst);
        DONE_EVENT.store(done, Ordering::SeqCst);
    }
}

fn record_panic(info: &std::panic::PanicHookInfo<'_>) {
    let thread_id = unsafe { GetCurrentThreadId() };
    // A UI-thread panic unwinds into gpui's `extern "system"` callbacks and
    // aborts there. Everywhere else it is contained or ends just that thread.
    let fatal = thread_id == MAIN_THREAD.load(Ordering::SeqCst);
    let thread = std::thread::current();
    let message = info.payload_as_str().unwrap_or("non-string panic payload");
    let location = info.location().map_or_else(
        || "unknown".to_owned(),
        |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
    );
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let report = format!(
        "time: {seconds}\nversion: {}\nthread: {} ({thread_id})\nfatal: {fatal}\nlocation: {location}\nmessage: {message}\n",
        VERSION.get().map_or("unknown", String::as_str),
        thread.name().unwrap_or("<unnamed>"),
    );
    if let Some(log) = PANIC_LOG.get() {
        append_rotating(log, &format!("{report}\n"));
    }
    if fatal && !WRITING.swap(true, Ordering::SeqCst) {
        if let Some(path) = PANIC_PATH.get() {
            let backtrace = std::backtrace::Backtrace::force_capture();
            let _ = std::fs::write(path, format!("{report}\nbacktrace:\n{backtrace}\n"));
        }
        unsafe { write_report(thread_id, null()) };
    }
}

fn append_rotating(path: &Path, text: &str) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > PANIC_LOG_LIMIT) {
        let _ = std::fs::rename(path, path.with_extension("old.log"));
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(text.as_bytes());
    }
}

unsafe extern "system" fn unhandled_exception(exception: *const EXCEPTION_POINTERS) -> i32 {
    if WRITING.swap(true, Ordering::SeqCst) {
        return 1;
    }
    let thread = unsafe { GetCurrentThreadId() };
    let request = REQUEST_EVENT.load(Ordering::SeqCst);
    if request != 0 && thread != WRITER_THREAD.load(Ordering::SeqCst) {
        FAULT_THREAD.store(thread, Ordering::SeqCst);
        FAULT_POINTERS.store(exception.cast_mut(), Ordering::SeqCst);
        unsafe {
            SetEvent(request);
            if WaitForSingleObject(DONE_EVENT.load(Ordering::SeqCst), 30_000) == WAIT_OBJECT_0 {
                return 1;
            }
        }
    }
    // No writer (or it hung): best effort on the faulting thread.
    unsafe { write_report(thread, exception) };
    1 // Never continue executing at the fault.
}

/// Writes the minidump and symbolized stack for `thread`. With a null
/// `exception` (panic path) it must run on the panicking thread itself.
unsafe fn write_report(thread: u32, exception: *const EXCEPTION_POINTERS) {
    if let Some(path) = DUMP_PATH.get() {
        let file = unsafe { create_new(path) };
        if file != INVALID_HANDLE_VALUE {
            let info = MINIDUMP_EXCEPTION_INFORMATION {
                ThreadId: thread,
                ExceptionPointers: exception.cast_mut(),
                ClientPointers: 0,
            };
            unsafe {
                dbghelp::dump(
                    file,
                    GetCurrentProcessId(),
                    if exception.is_null() { null() } else { &info },
                );
                CloseHandle(file);
            }
        }
    }
    if let Some(path) = STACK_PATH.get() {
        let file = unsafe { create_new(path) };
        if file != INVALID_HANDLE_VALUE {
            unsafe {
                dbghelp::stack(file, exception);
                CloseHandle(file);
            }
        }
    }
}

unsafe fn create_new(path: &[u16]) -> windows_sys::Win32::Foundation::HANDLE {
    unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ,
            null(),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_fault_child() {
        let Some(directory) = std::env::var_os("ATLAS_CRASH_TEST_DIR") else {
            return;
        };
        install_at(directory.into(), "test").unwrap();
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(
                windows_sys::Win32::System::Diagnostics::Debug::SEM_NOGPFAULTERRORBOX,
            );
            windows_sys::Win32::System::Diagnostics::Debug::RaiseException(
                0xc0000005,
                1,
                0,
                std::ptr::null(),
            );
        }
    }

    /// The overflow case is the one that used to leave empty reports.
    #[test]
    fn stack_overflow_child() {
        let Some(directory) = std::env::var_os("ATLAS_CRASH_TEST_OVERFLOW_DIR") else {
            return;
        };
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(
                windows_sys::Win32::System::Diagnostics::Debug::SEM_NOGPFAULTERRORBOX,
            );
        }
        // Run on a thread with a small stack so the overflow is quick; the
        // test harness thread is not the install thread, which is fine here.
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                install_at(directory.into(), "test").unwrap();
                #[inline(never)]
                #[allow(unconditional_recursion)] // the overflow is the point
                fn recurse(depth: u64, sink: &mut [u8; 4096]) -> u64 {
                    let mut local = [0u8; 4096];
                    local[(depth % 4096) as usize] = depth as u8;
                    sink[0] = std::hint::black_box(local)[0];
                    depth + recurse(depth + 1, std::hint::black_box(sink))
                }
                let mut sink = [0u8; 4096];
                std::hint::black_box(recurse(0, &mut sink));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    /// Runs `test` in a child process that crashes, then checks its report.
    fn assert_child_report(test: &str, env: &str) {
        let directory = std::env::temp_dir().join(format!(
            "atlas-crash-test-{}-{}",
            std::process::id(),
            env.len()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(directory.clone());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(env, &directory)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let files: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        let dump = files
            .iter()
            .find(|path| path.extension().is_some_and(|extension| extension == "dmp"))
            .expect("minidump file");
        assert_eq!(&std::fs::read(dump).unwrap()[..4], b"MDMP");
        let stack = files
            .iter()
            .find(|path| path.to_string_lossy().ends_with("stack.txt"))
            .expect("stack file");
        assert!(std::fs::read_to_string(stack).unwrap().starts_with("0x"));
    }

    #[test]
    fn child_native_fault_writes_minidump_and_stack_without_opening_ui() {
        assert_child_report("crash::tests::native_fault_child", "ATLAS_CRASH_TEST_DIR");
    }

    #[test]
    fn child_stack_overflow_still_writes_a_full_report() {
        assert_child_report(
            "crash::tests::stack_overflow_child",
            "ATLAS_CRASH_TEST_OVERFLOW_DIR",
        );
    }
}
