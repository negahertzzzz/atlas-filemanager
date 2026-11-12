use super::policy;
use atlas_model::cleanup::{CleanupCandidate, CleanupPlan, CleanupReport, CleanupResult};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn history_dir() -> io::Result<PathBuf> {
    // Keep diagnostics and explicitly isolated Atlas profiles out of the user's
    // normal history. This affects persistence only, never the trusted Temp root.
    if let Some(path) = std::env::var_os("ATLAS_CONFIG_DIR").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path).join("cleanup"));
    }
    dirs::data_local_dir()
        .map(|p| p.join("Atlas/cleanup"))
        .ok_or_else(|| io::Error::other("cleanup_unavailable"))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn prepare_plan(
    volume_root: PathBuf,
    candidates: Vec<CleanupCandidate>,
) -> io::Result<CleanupPlan> {
    let temp_root =
        policy::user_temp_root().ok_or_else(|| io::Error::other("cleanup_unavailable"))?;
    prepare_plan_in(volume_root, temp_root, candidates)
}

/// `temp_root` must be one of the registered category roots.
pub fn prepare_plan_in(
    volume_root: PathBuf,
    temp_root: PathBuf,
    candidates: Vec<CleanupCandidate>,
) -> io::Result<CleanupPlan> {
    if !policy::is_cleanup_root(&temp_root) {
        return Err(io::Error::other("cleanup_scope_changed"));
    }
    if !policy::is_drive_root(&volume_root)
        || !temp_root.starts_with(&volume_root)
        || candidates.is_empty()
        || candidates.len() > 20_000
    {
        return Err(io::Error::other("cleanup_scope_changed"));
    }
    let mut ids = HashSet::new();
    for c in &candidates {
        if !policy::valid_temp_candidate(&c.path, &temp_root)
            || !ids.insert((c.identity.volume, c.identity.id))
        {
            return Err(io::Error::other("cleanup_scope_changed"));
        }
    }
    let plan = CleanupPlan {
        version: 1,
        id: uuid::Uuid::new_v4().to_string(),
        created_unix_seconds: now(),
        volume_root,
        temp_root,
        candidates,
    };
    let dir = history_dir()?;
    fs::create_dir_all(&dir)?;
    check_history_budget(&dir)?;
    let mut file = File::options()
        .create_new(true)
        .write(true)
        .open(dir.join(format!("{}.plan.json", plan.id)))?;
    serde_json::to_writer_pretty(&mut file, &plan)?;
    file.sync_all()?;
    Ok(plan)
}

/// Only the in-memory reviewed plan is accepted; manifests are never loaded as
/// instructions. This entry point cannot run arbitrary file or folder deletes.
pub fn execute(
    plan: &CleanupPlan,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
) -> io::Result<CleanupReport> {
    if !policy::is_cleanup_root(&plan.temp_root) {
        return Err(io::Error::other("cleanup_scope_changed"));
    }
    execute_in(
        plan,
        cancel,
        &mut progress,
        &history_dir()?,
        &plan.temp_root,
    )
}

// The trusted root is supplied exclusively by the production Known Folder
// resolver. Tests inject an isolated fixture root; no public API accepts it.
fn execute_in(
    plan: &CleanupPlan,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
    dir: &Path,
    trusted_temp: &Path,
) -> io::Result<CleanupReport> {
    execute_using(plan, cancel, &mut progress, dir, trusted_temp, delete)
}

fn execute_using(
    plan: &CleanupPlan,
    cancel: &AtomicBool,
    mut progress: impl FnMut(usize, usize),
    dir: &Path,
    trusted_temp: &Path,
    mut retire: impl FnMut(&CleanupCandidate) -> io::Result<u64>,
) -> io::Result<CleanupReport> {
    if plan.version != 1
        || plan.id.parse::<uuid::Uuid>().is_err()
        || now().saturating_sub(plan.created_unix_seconds) > 15 * 60
        || plan.created_unix_seconds > now()
        || !policy::is_drive_root(&plan.volume_root)
        || trusted_temp != plan.temp_root
        || !plan.temp_root.starts_with(&plan.volume_root)
        || plan.candidates.is_empty()
        || plan.candidates.len() > 2_000
    {
        return Err(io::Error::other("cleanup_scope_changed"));
    }
    // Fail before any mutation if an execution journal cannot be created. A
    // second invocation of the same plan cannot silently repeat the operation.
    check_history_budget(dir)?;
    let mut journal = File::options()
        .write(true)
        .create_new(true)
        .open(dir.join(format!("{}.journal.jsonl", plan.id)))?;
    let mut report = CleanupReport {
        plan_id: plan.id.clone(),
        results: Vec::new(),
        cancelled: false,
        free_before: free(&plan.volume_root),
        free_after: None,
        error: None,
    };
    let mut seen = HashSet::new();
    for candidate in &plan.candidates {
        if cancel.load(Ordering::Acquire) {
            report.cancelled = true;
            break;
        }
        let valid = policy::valid_temp_candidate(&candidate.path, &plan.temp_root)
            && !crate::vault::is_private_path(&candidate.path)
            && !crate::vault::live::is_private(&candidate.path)
            && seen.insert((candidate.identity.volume, candidate.identity.id))
            && old_enough(candidate.identity.modified);
        if let Err(error) = record(
            &mut journal,
            &serde_json::json!({"starting":candidate.path}),
        ) {
            report.error = Some(error.to_string());
            break;
        }
        let outcome = if valid {
            retire(candidate)
        } else {
            Err(io::Error::other("cleanup_scope_changed"))
        };
        let result = match outcome {
            Ok(bytes) => CleanupResult {
                path: candidate.path.clone(),
                deleted_bytes: bytes,
                error: None,
            },
            Err(e) => CleanupResult {
                path: candidate.path.clone(),
                deleted_bytes: 0,
                error: Some(e.to_string()),
            },
        };
        report.results.push(result);
        if let Err(error) = record(&mut journal, report.results.last().unwrap()) {
            report.error = Some(error.to_string());
            break;
        }
        progress(report.results.len(), plan.candidates.len());
    }
    report.free_after = free(&plan.volume_root);
    let saved = (|| -> io::Result<()> {
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(dir.join(format!("{}.report.json", plan.id)))?;
        serde_json::to_writer_pretty(&mut file, &report)?;
        file.sync_all()
    })();
    if let Err(error) = saved {
        report.error = Some(error.to_string());
    }
    Ok(report)
}

fn record(file: &mut File, value: &impl serde::Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut *file, value)?;
    writeln!(file)?;
    file.sync_all()
}

fn check_history_budget(dir: &Path) -> io::Result<()> {
    let mut bytes = 0u64;
    let mut files = 0usize;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            bytes = bytes.saturating_add(metadata.len());
            files += 1;
        }
        // Preserve history instead of silently deleting it. The next operation
        // requires the user to archive it once this local budget is reached.
        if bytes >= 64 * 1024 * 1024 || files >= 768 {
            return Err(io::Error::other("cleanup_history_full"));
        }
    }
    Ok(())
}

fn old_enough(filetime: u64) -> bool {
    let seconds = filetime / 10_000_000;
    seconds.checked_sub(11_644_473_600).is_some_and(|time| {
        now()
            .checked_sub(time)
            .is_some_and(|age| Duration::from_secs(age) >= Duration::from_secs(7 * 24 * 3600))
    })
}
fn delete(candidate: &CleanupCandidate) -> io::Result<u64> {
    #[cfg(windows)]
    {
        atlas_win_space::cleanup::delete_verified(candidate)
    }
    #[cfg(not(windows))]
    {
        let _ = candidate;
        Err(io::Error::other("cleanup_unavailable"))
    }
}
fn free(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        atlas_win_space::cleanup::capacity(path).map(|(_, free)| free)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn age_check_rejects_future_and_invalid_timestamps() {
        assert!(!old_enough(0));
        assert!(!old_enough((now() + 11_644_473_600 + 1) * 10_000_000));
        assert!(old_enough(
            (now() + 11_644_473_600 - 8 * 24 * 3600) * 10_000_000
        ));
    }

    #[cfg(windows)]
    #[test]
    fn invalid_or_expired_plans_fail_before_journaling_or_retirement() {
        let history = tempfile::tempdir().unwrap();
        let root = std::env::current_dir().unwrap();
        let candidate = CleanupCandidate {
            path: root.join("old.tmp"),
            ancestors: Vec::new(),
            identity: atlas_model::cleanup::FileIdentity {
                volume: 1,
                id: [1; 16],
                bytes: 4,
                links: 1,
                modified: (now() + 11_644_473_600 - 8 * 24 * 3600) * 10_000_000,
            },
        };
        let valid = CleanupPlan {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            created_unix_seconds: now(),
            volume_root: root.ancestors().last().unwrap().to_owned(),
            temp_root: root.clone(),
            candidates: vec![candidate],
        };
        let invalid = (0..6).map(|case| {
            let mut plan = valid.clone();
            match case {
                0 => plan.created_unix_seconds = now() - 901,
                1 => plan.created_unix_seconds = now() + 60,
                2 => plan.version = 2,
                3 => plan.id = "../injected".into(),
                4 => plan.candidates.clear(),
                _ => plan.candidates.resize(2_001, plan.candidates[0].clone()),
            }
            plan
        });
        for plan in invalid {
            assert!(execute_using(
                &plan,
                &AtomicBool::new(false),
                |_, _| {},
                history.path(),
                &root,
                |_| panic!("invalid plan invoked backend")
            )
            .is_err());
        }
        assert_eq!(fs::read_dir(history.path()).unwrap().count(), 0);
    }
    #[cfg(windows)]
    #[test]
    fn plan_journal_cancellation_and_replay_are_independent_of_windows_handles() {
        let fixture = tempfile::Builder::new()
            .prefix("atlas-cleanup-workflow-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let file = fixture.path().join("reviewed.tmp");
        let sentinel = fixture.path().join("keep.txt");
        fs::write(&file, b"old").unwrap();
        fs::write(&sentinel, b"keep").unwrap();
        let root = fixture.path().to_owned();
        let candidate = CleanupCandidate {
            path: file.clone(),
            ancestors: Vec::new(),
            identity: atlas_model::cleanup::FileIdentity {
                volume: 1,
                id: [7; 16],
                bytes: 3,
                modified: (now() + 11_644_473_600 - 8 * 24 * 3600) * 10_000_000,
                links: 1,
            },
        };
        let mut plan = CleanupPlan {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            created_unix_seconds: now(),
            volume_root: root.ancestors().last().unwrap().to_owned(),
            temp_root: root.clone(),
            candidates: vec![candidate],
        };
        let cancelled = execute_using(
            &plan,
            &AtomicBool::new(true),
            |_, _| {},
            &root,
            &root,
            |_| panic!("cancelled plan invoked filesystem backend"),
        )
        .unwrap();
        assert!(cancelled.cancelled);
        assert!(file.exists());
        assert!(execute_using(
            &plan,
            &AtomicBool::new(false),
            |_, _| {},
            &root,
            &root,
            |_| panic!("replayed plan invoked filesystem backend")
        )
        .is_err());
        plan.id = uuid::Uuid::new_v4().to_string();
        let mut calls = 0;
        let report = execute_using(
            &plan,
            &AtomicBool::new(false),
            |_, _| {},
            &root,
            &root,
            |candidate| {
                assert_eq!(candidate.path, file);
                calls += 1;
                fs::remove_file(&candidate.path)?;
                Ok(3)
            },
        )
        .unwrap();
        assert_eq!(calls, 1);
        assert!(!file.exists());
        assert!(report.error.is_none());
        assert_eq!(report.results[0].deleted_bytes, 3);
        assert_eq!(fs::read(&sentinel).unwrap(), b"keep");
        let persisted: CleanupReport = serde_json::from_slice(
            &fs::read(root.join(format!("{}.report.json", plan.id))).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted.results.len(), 1);
        plan.id = uuid::Uuid::new_v4().to_string();
        assert!(execute_using(
            &plan,
            &AtomicBool::new(false),
            |_, _| {},
            &root,
            &root.join("wrong-root"),
            |_| panic!("out-of-scope plan invoked backend")
        )
        .is_err());
    }
    #[cfg(windows)]
    #[test]
    fn reviewed_fixture_is_deleted_once_and_cancelled_fixture_survives() {
        let fixture = tempfile::Builder::new()
            .prefix("atlas-cleanup-test-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let file = fixture.path().join("reviewed.tmp");
        fs::write(&file, b"old temporary file").unwrap();
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(8 * 24 * 3600))
            .unwrap();
        let candidate = atlas_win_space::cleanup::inspect(&file).unwrap();
        let temp_root = fixture.path().to_owned();
        let plan = CleanupPlan {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            created_unix_seconds: now(),
            volume_root: temp_root.ancestors().last().unwrap().to_owned(),
            temp_root,
            candidates: vec![candidate],
        };
        let cancel = AtomicBool::new(true);
        let cancelled =
            execute_in(&plan, &cancel, |_, _| {}, fixture.path(), &plan.temp_root).unwrap();
        assert!(cancelled.cancelled);
        assert!(cancelled.results.is_empty());
        assert!(file.exists());
        // A cancelled plan is consumed too; replay cannot silently perform work.
        cancel.store(false, Ordering::Release);
        assert!(execute_in(&plan, &cancel, |_, _| {}, fixture.path(), &plan.temp_root).is_err());
        let mut fresh = plan.clone();
        fresh.id = uuid::Uuid::new_v4().to_string();
        let report =
            execute_in(&fresh, &cancel, |_, _| {}, fixture.path(), &plan.temp_root).unwrap();
        assert_eq!(report.results.len(), 1);
        assert!(
            report.results[0].error.is_none(),
            "{:?}",
            report.results[0].error
        );
        assert!(!file.exists());
        assert!(report.error.is_none());
        assert!(fixture
            .path()
            .join(format!("{}.report.json", fresh.id))
            .is_file());
    }
    #[cfg(windows)]
    #[test]
    fn fresh_or_changed_files_are_reported_without_deletion() {
        let fixture = tempfile::Builder::new()
            .prefix("atlas-cleanup-test-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let file = fixture.path().join("keep.tmp");
        fs::write(&file, b"keep").unwrap();
        let candidate = atlas_win_space::cleanup::inspect(&file).unwrap();
        let temp_root = fixture.path().to_owned();
        let plan = CleanupPlan {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            created_unix_seconds: now(),
            volume_root: temp_root.ancestors().last().unwrap().to_owned(),
            temp_root,
            candidates: vec![candidate],
        };
        let report = execute_in(
            &plan,
            &AtomicBool::new(false),
            |_, _| {},
            fixture.path(),
            &plan.temp_root,
        )
        .unwrap();
        assert!(report.results[0].error.is_some());
        assert_eq!(fs::read(&file).unwrap(), b"keep");
        assert!(execute_in(
            &plan,
            &AtomicBool::new(false),
            |_, _| {},
            fixture.path(),
            &fixture.path().join("another-root")
        )
        .is_err());
    }
}
