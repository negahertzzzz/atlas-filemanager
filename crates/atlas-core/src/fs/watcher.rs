use atlas_model::app_state::{BgResult, FsChange};
use std::path::{Path, PathBuf};

pub struct WatcherHandle {
    #[cfg(windows)]
    _native: atlas_win_space::directory::DirectoryWatcher,
    #[cfg(not(windows))]
    _watcher: notify::RecommendedWatcher,
    pub watched_path: PathBuf,
}

#[derive(Debug)]
enum Change {
    Upsert(PathBuf),
    Remove(PathBuf),
    Rename(PathBuf, PathBuf),
    Rescan,
}

fn coalesce_rename_chains(events: Vec<Change>) -> Vec<Change> {
    let renamed: std::collections::HashSet<_> = events
        .iter()
        .filter_map(|event| match event {
            Change::Rename(from, _) => Some(from.clone()),
            _ => None,
        })
        .collect();
    let mut result = Vec::new();
    for event in events {
        match event {
            // An earlier write can no longer be statted at its old name; the
            // rename carries final metadata and retains the original identity.
            Change::Upsert(path) if renamed.contains(&path) => {},
            Change::Rename(from, to) => {
                if let Some(Change::Rename(_, destination)) = result.iter_mut().rev().find(|previous| matches!(previous, Change::Rename(_, previous_to) if previous_to==&from)) {
                    *destination = to;
                } else { result.push(Change::Rename(from,to)); }
            }
            other => result.push(other),
        }
    }
    result
}

pub fn watch_path(path: &Path, tx: flume::Sender<BgResult>) -> anyhow::Result<WatcherHandle> {
    watch_path_with_mode(path, tx, false)
}

pub fn watch_path_recursive(
    path: &Path,
    tx: flume::Sender<BgResult>,
) -> anyhow::Result<WatcherHandle> {
    watch_path_with_mode(path, tx, true)
}

fn watch_path_with_mode(
    path: &Path,
    tx: flume::Sender<BgResult>,
    recursive: bool,
) -> anyhow::Result<WatcherHandle> {
    anyhow::ensure!(
        !super::scan_policy::is_scan_excluded(path)
            || crate::vault::live::mount_root(path).is_some(),
        "Background watching is disabled for this system directory"
    );
    anyhow::ensure!(
        !super::preview::is_cloud_placeholder(path),
        "Background watching is disabled for an offline cloud directory"
    );
    let directory = path.to_path_buf();
    let watched_path = directory.clone();
    let (events_tx, events_rx) = std::sync::mpsc::channel::<Vec<Change>>();
    #[cfg(windows)]
    let failure_tx = tx.clone();
    #[cfg(windows)]
    let failure_path = directory.clone();
    #[cfg(windows)]
    let native = atlas_win_space::directory::DirectoryWatcher::watch_with_subtree(
        path,
        recursive,
        move |events| {
            use atlas_win_space::directory::Change as Native;
            let events = events
                .into_iter()
                .filter_map(|event| match event {
                    Native::Upsert(path) => Some(Change::Upsert(path)),
                    Native::Remove(path) => Some(Change::Remove(path)),
                    Native::Rename { from, to } => Some(Change::Rename(from, to)),
                    Native::Rescan => Some(Change::Rescan),
                    Native::Disabled => {
                        let _ = failure_tx.send(BgResult::WatcherDisabled(failure_path.clone()));
                        None
                    }
                })
                .collect();
            let _ = events_tx.send(events);
        },
    )?;
    #[cfg(not(windows))]
    let watcher = {
        use notify::{
            event::{EventKind, ModifyKind, RenameMode},
            RecursiveMode, Watcher,
        };
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let events = match result {
                    Ok(event) if event.need_rescan() => vec![Change::Rescan],
                    Ok(event) => match event.kind {
                        EventKind::Remove(_) => {
                            event.paths.into_iter().map(Change::Remove).collect()
                        }
                        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
                            if event.paths.len() == 2 =>
                        {
                            vec![Change::Rename(
                                event.paths[0].clone(),
                                event.paths[1].clone(),
                            )]
                        }
                        EventKind::Access(_) => return,
                        _ => event.paths.into_iter().map(Change::Upsert).collect(),
                    },
                    Err(_) => vec![Change::Rescan],
                };
                let _ = events_tx.send(events);
            })?;
        watcher.watch(
            path,
            if recursive {
                RecursiveMode::Recursive
            } else {
                RecursiveMode::NonRecursive
            },
        )?;
        watcher
    };
    std::thread::Builder::new()
        .name("atlas-watch-metadata".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(runtime) = runtime else {
                let _ = tx.send(BgResult::FsEvent(directory));
                return;
            };
            while let Ok(mut events) = events_rx.recv() {
                // Coalesce write bursts while preserving rename/removal order.
                while let Ok(more) = events_rx.recv_timeout(std::time::Duration::from_millis(30)) {
                    events.extend(more);
                    if events.len() >= 2048 {
                        break;
                    }
                }
                if events.iter().any(|event| matches!(event, Change::Rescan)) {
                    if tx.send(BgResult::FsEvent(directory.clone())).is_err() {
                        break;
                    }
                    continue;
                }
                // A panic resolving one batch must not end the watcher thread;
                // fall back to a full rescan, which needs no per-event state.
                let changes = crate::util::guard::contain("fs-watcher", || {
                    runtime.block_on(async {
                        let mut changes = Vec::new();
                        for event in coalesce_rename_chains(events) {
                            let path = match &event {
                                Change::Upsert(path) | Change::Remove(path) => Some(path),
                                Change::Rename(_, to) => Some(to),
                                Change::Rescan => None,
                            };
                            if path.is_some_and(|path| {
                                super::scan_policy::is_scan_excluded(path)
                                    && crate::vault::live::mount_root(path).is_none()
                            }) {
                                continue;
                            }
                            match event {
                                Change::Remove(path) => changes.push(FsChange::Remove(path)),
                                Change::Upsert(path) => {
                                    match super::list::entry_for_path(&path).await {
                                        Some(entry) => changes.push(FsChange::Upsert(entry)),
                                        None if !path.exists() => {
                                            changes.push(FsChange::Remove(path))
                                        }
                                        None => {}
                                    }
                                }
                                Change::Rename(from, to) => {
                                    match super::list::entry_for_path(&to).await {
                                        Some(entry) => {
                                            changes.push(FsChange::Rename { from, entry })
                                        }
                                        None => changes.push(FsChange::Remove(from)),
                                    }
                                }
                                Change::Rescan => unreachable!(),
                            }
                        }
                        changes
                    })
                });
                let Ok(changes) = changes else {
                    if tx.send(BgResult::FsEvent(directory.clone())).is_err() {
                        break;
                    }
                    continue;
                };
                if !changes.is_empty()
                    && tx
                        .send(BgResult::FsChanges {
                            directory: directory.clone(),
                            changes,
                        })
                        .is_err()
                {
                    break;
                }
            }
        })?;
    Ok(WatcherHandle {
        #[cfg(windows)]
        _native: native,
        #[cfg(not(windows))]
        _watcher: watcher,
        watched_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn burst_renames_keep_original_identity_and_final_metadata_path() {
        let a = PathBuf::from("a");
        let b = PathBuf::from("b");
        let c = PathBuf::from("c");
        let events = coalesce_rename_chains(vec![
            Change::Upsert(a.clone()),
            Change::Rename(a.clone(), b.clone()),
            Change::Upsert(b.clone()),
            Change::Rename(b, c.clone()),
        ]);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0],Change::Rename(from,to) if from==&a && to==&c));
    }
    fn wait_for(rx: &flume::Receiver<BgResult>, predicate: impl Fn(&FsChange) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(BgResult::FsChanges { changes, .. }) if changes.iter().any(&predicate) => return,
                Ok(BgResult::FsEvent(path)) => panic!("Unexpected recovery rescan: {path:?}"),
                _ => {}
            }
        }
        panic!("Expected surgical watcher delta was not received");
    }
    #[test]
    fn create_modify_rename_remove_emit_ready_metadata_without_rescanning() {
        let root = tempfile::tempdir().unwrap();
        let (tx, rx) = flume::unbounded();
        let watcher = watch_path(root.path(), tx).unwrap();
        let a = root.path().join("a.txt");
        let b = root.path().join("b.txt");
        std::fs::write(&a, b"first").unwrap();
        wait_for(
            &rx,
            |change| matches!(change,FsChange::Upsert(entry) if entry.path()==a && entry.size()==Some(5)),
        );
        std::fs::write(&a, b"modified data").unwrap();
        wait_for(
            &rx,
            |change| matches!(change,FsChange::Upsert(entry) if entry.path()==a && entry.size()==Some(13)),
        );
        std::fs::rename(&a, &b).unwrap();
        wait_for(
            &rx,
            |change| matches!(change,FsChange::Rename { from,entry } if from==&a && entry.path()==b),
        );
        std::fs::remove_file(&b).unwrap();
        wait_for(
            &rx,
            |change| matches!(change,FsChange::Remove(path) if path==&b),
        );
        drop(watcher);
    }
}
