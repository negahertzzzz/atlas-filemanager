//! Unclean-exit detection. Each process owns `session-<pid>.lock` until a clean
//! exit removes it. A leftover sentinel whose process is gone means that run
//! died; if it left reports behind, the next launch surfaces them.
//!
//! Sentinels without reports (killed from Task Manager, power loss, OS
//! shutdown) are discarded silently — they are not evidence of an Atlas bug.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::SystemTime,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, STILL_ACTIVE},
    System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
};

/// Newest reports kept on disk; older dump groups are pruned at startup.
const KEEP_DUMPS: usize = 10;

/// What the previous, crashed run left behind.
#[derive(Debug, Clone)]
pub struct PreviousCrash {
    pub version: String,
    pub ended_after: SystemTime,
    /// Dump, stack and panic files of that run, newest first.
    pub reports: Vec<PathBuf>,
    /// `location` + `message` of a fatal panic, when the crash was one.
    pub summary: Option<String>,
}

static PREVIOUS: OnceLock<Option<PreviousCrash>> = OnceLock::new();
static SENTINEL: OnceLock<PathBuf> = OnceLock::new();

/// The crash of the previous run, if it had one. Valid after `crash::install`.
pub fn previous_crash() -> Option<&'static PreviousCrash> {
    PREVIOUS.get().and_then(Option::as_ref)
}

/// Removes this process's sentinel. Call on every orderly shutdown path.
pub fn mark_clean_exit() {
    if let Some(path) = SENTINEL.get() {
        let _ = std::fs::remove_file(path);
    }
}

pub(super) fn begin(directory: &Path, version: &str) {
    let own_pid = std::process::id();
    let mut crash = None;
    for (path, pid) in sentinels(directory) {
        if pid == own_pid || process_alive(pid) {
            continue;
        }
        let previous_version = std::fs::read_to_string(&path).unwrap_or_default();
        let started = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let _ = std::fs::remove_file(&path);
        let reports = reports_of(directory, pid);
        if reports.is_empty() {
            continue;
        }
        let summary = reports
            .iter()
            .find(|p| p.to_string_lossy().ends_with(".panic.txt"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|text| summarize(&text));
        crash = Some(PreviousCrash {
            version: previous_version.trim().to_owned(),
            ended_after: started.unwrap_or(SystemTime::UNIX_EPOCH),
            reports,
            summary,
        });
    }
    prune(directory);
    let sentinel = directory.join(format!("session-{own_pid}.lock"));
    let _ = std::fs::write(&sentinel, version);
    let _ = SENTINEL.set(sentinel);
    let _ = PREVIOUS.set(crash);
}

fn sentinels(directory: &Path) -> Vec<(PathBuf, u32)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let pid = name
                .strip_prefix("session-")?
                .strip_suffix(".lock")?
                .parse()
                .ok()?;
            Some((entry.path(), pid))
        })
        .collect()
}

/// Report files are named `atlas-<pid>-<nanos>.*`.
fn reports_of(directory: &Path, pid: u32) -> Vec<PathBuf> {
    let prefix = format!("atlas-{pid}-");
    let mut reports: Vec<_> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        // Empty stack/dump files are what a failed write leaves; not useful.
        .filter(|entry| entry.metadata().is_ok_and(|m| m.len() > 0))
        .map(|entry| entry.path())
        .collect();
    reports.sort_by_key(|p| std::cmp::Reverse(modified(p)));
    reports
}

fn summarize(panic_report: &str) -> String {
    let field = |key: &str| {
        panic_report
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .map(str::trim)
            .unwrap_or_default()
            .to_owned()
    };
    let (message, location) = (field("message:"), field("location:"));
    if location.is_empty() {
        message
    } else {
        format!("{message} ({location})")
    }
}

fn process_alive(pid: u32) -> bool {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            return false;
        }
        let mut code = 0u32;
        let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(handle);
        alive
    }
}

fn modified(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Keeps the newest `KEEP_DUMPS` report groups (one group per crashed run).
fn prune(directory: &Path) {
    let mut dumps: Vec<_> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.extension().is_some_and(|e| e == "dmp"))
        .collect();
    if dumps.len() <= KEEP_DUMPS {
        return;
    }
    dumps.sort_by_key(|p| std::cmp::Reverse(modified(p)));
    for dump in &dumps[KEEP_DUMPS..] {
        let stem = dump.with_extension("");
        for suffix in ["dmp", "stack.txt", "panic.txt"] {
            let _ = std::fs::remove_file(stem.with_extension(suffix));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_joins_message_and_location() {
        let text = "time: 1\nlocation: src/a.rs:3:9\nmessage: index out of bounds\n";
        assert_eq!(summarize(text), "index out of bounds (src/a.rs:3:9)");
    }

    #[test]
    fn dead_sentinel_with_reports_is_reported_once() {
        let dir = std::env::temp_dir().join(format!("atlas-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // PID 0 is never a live user process.
        std::fs::write(dir.join("session-0.lock"), "1.2.3").unwrap();
        std::fs::write(
            dir.join("atlas-0-5.panic.txt"),
            "location: x.rs:1:1\nmessage: boom\n",
        )
        .unwrap();
        std::fs::write(dir.join("atlas-0-5.stack.txt"), "").unwrap();
        // A dead sentinel with no reports is dropped silently.
        std::fs::write(dir.join("session-1.lock"), "1.2.3").unwrap();

        let mut found = None;
        for (path, pid) in sentinels(&dir) {
            if pid == 0 {
                found = Some(reports_of(&dir, pid));
                let _ = std::fs::remove_file(path);
            }
        }
        let reports = found.expect("sentinel for pid 0");
        assert_eq!(reports.len(), 1, "empty stack file must be ignored");
        assert!(reports[0].to_string_lossy().ends_with(".panic.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
