//! Exercise the last-chance handler in an isolated child process.
#![cfg(windows)]
#![allow(unsafe_code)]

/// Kept out of line: this package builds tests optimized, and an inlined
/// frame would resolve to the harness closure instead of this file.
#[inline(never)]
fn raise_native_exception() {
    unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(0x0001 | 0x0002);
        windows_sys::Win32::System::Diagnostics::Debug::RaiseException(
            0xE0424242,
            1,
            0,
            std::ptr::null(),
        );
    }
    // Work after the call stops it compiling to a tail jump, which would
    // drop this frame from the stack.
    std::hint::black_box(());
}

#[test]
fn crash_child() {
    if std::env::var_os("ATLAS_TEST_NATIVE_CRASH").is_none() {
        return;
    }
    atlas_win_space::crash::install().unwrap();
    raise_native_exception();
}

#[test]
fn native_exception_produces_a_minidump_in_isolated_app_data() {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "atlas-minidump-test-{}-{stamp}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--nocapture"])
        .env("ATLAS_TEST_NATIVE_CRASH", "1")
        .env("LOCALAPPDATA", &root)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "child must terminate on native exception"
    );
    let files: Vec<_> = std::fs::read_dir(root.join("Atlas/Crashes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    // The unclean-exit sentinel is expected: the child never exits cleanly.
    let (sentinels, files): (Vec<_>, Vec<_>) = files
        .into_iter()
        .partition(|path| path.to_string_lossy().ends_with(".lock"));
    assert_eq!(sentinels.len(), 1, "child must leave its session sentinel");
    let dumps: Vec<_> = files
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "dmp"))
        .collect();
    assert_eq!(dumps.len(), 1);
    let dump = std::fs::read(dumps[0]).unwrap();
    assert!(dump.len() > 32, "dump must include its header and streams");
    assert_eq!(&dump[..4], b"MDMP");
    // Only remove files created by this test; no recursive directory operations.
    let stacks: Vec<_> = files
        .iter()
        .filter(|path| path.to_string_lossy().ends_with(".stack.txt"))
        .collect();
    assert_eq!(stacks.len(), 1);
    let stack = std::fs::read_to_string(stacks[0]).unwrap();
    assert!(stack.starts_with("0x"));
    assert!(stack.contains("module=0x"));
    if cfg!(debug_assertions) {
        assert!(
            stack.contains("raise_native_exception"),
            "local PDB must resolve the faulting test: {stack}"
        );
        assert!(
            stack.contains("crash_dump.rs:"),
            "source filename and line must be resolved: {stack}"
        );
    }
    assert_eq!(files.len(), 2);
    for file in files.into_iter().chain(sentinels) {
        std::fs::remove_file(file).unwrap();
    }
    std::fs::remove_dir(root.join("Atlas/Crashes")).unwrap();
    std::fs::remove_dir(root.join("Atlas")).unwrap();
    std::fs::remove_dir(root).unwrap();
}
