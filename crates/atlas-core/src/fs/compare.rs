//! Compare two folder trees by name, size and modification time.
//!
//! Read-only. Folders that exist on one side only are reported once (not
//! expanded), symlinks and junctions are compared as entries but never
//! followed, and the walk stops at a row limit so a huge tree stays usable.
use atlas_model::compare::{CompareEntry, CompareReport, CompareRow, CompareSide, CompareStatus};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

/// FAT stores times with 2 s precision and some SMB servers round them.
const TIME_TOLERANCE: Duration = Duration::from_secs(2);
pub const MAX_ROWS: usize = 50_000;

struct Item {
    name: String,
    directory: bool,
    link: bool,
    entry: CompareEntry,
}

fn list(dir: &Path, errors: &mut u64) -> BTreeMap<String, Item> {
    let mut items = BTreeMap::new();
    let Ok(read) = std::fs::read_dir(dir) else {
        *errors += 1;
        return items;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata() else {
            *errors += 1;
            continue;
        };
        let link = meta.file_type().is_symlink() || reparse_point(&meta);
        let directory = meta.is_dir();
        // Windows names are case-insensitive: `Foto.jpg` and `foto.JPG` match.
        let key = if cfg!(windows) {
            name.to_lowercase()
        } else {
            name.clone()
        };
        items.insert(
            key,
            Item {
                name,
                directory,
                link,
                entry: CompareEntry {
                    bytes: (!directory).then(|| meta.len()),
                    modified: meta.modified().ok(),
                },
            },
        );
    }
    items
}

fn reparse_point(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

fn same_time(a: Option<SystemTime>, b: Option<SystemTime>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let delta = a.duration_since(b).unwrap_or_else(|e| e.duration());
            delta <= TIME_TOLERANCE
        }
        (None, None) => true,
        _ => false,
    }
}

fn newer(a: Option<SystemTime>, b: Option<SystemTime>) -> Option<CompareSide> {
    match (a, b) {
        (Some(a), Some(b)) if !same_time(Some(a), Some(b)) => Some(if a > b {
            CompareSide::Left
        } else {
            CompareSide::Right
        }),
        _ => None,
    }
}

/// Compare `left` with `right`. With `recursive`, folders present on both
/// sides are compared too. `progress` receives a partial report now and then.
pub fn compare(
    left: &Path,
    right: &Path,
    recursive: bool,
    cancel: &AtomicBool,
    mut progress: impl FnMut(&CompareReport),
) -> CompareReport {
    let mut report = CompareReport {
        left: left.to_owned(),
        right: right.to_owned(),
        ..Default::default()
    };
    let mut queue: Vec<PathBuf> = vec![PathBuf::new()];
    let mut last = std::time::Instant::now();
    while let Some(relative) = queue.pop() {
        if cancel.load(Ordering::Acquire) {
            report.cancelled = true;
            break;
        }
        let mut errors = 0;
        let mut a = list(&left.join(&relative), &mut errors);
        let b = list(&right.join(&relative), &mut errors);
        report.errors += errors;
        for (key, right_item) in b {
            let left_item = a.remove(&key);
            let name = left_item
                .as_ref()
                .map_or(&right_item.name, |l| &l.name)
                .clone();
            let path = relative.join(&name);
            match left_item {
                None => report.rows.push(CompareRow {
                    relative: path,
                    directory: right_item.directory,
                    status: CompareStatus::RightOnly,
                    left: None,
                    right: Some(right_item.entry),
                    newer: None,
                }),
                Some(left_item) if left_item.directory && right_item.directory => {
                    if recursive && !left_item.link && !right_item.link {
                        queue.push(path);
                    }
                }
                Some(left_item) => {
                    let same = left_item.directory == right_item.directory
                        && left_item.entry.bytes == right_item.entry.bytes
                        && same_time(left_item.entry.modified, right_item.entry.modified);
                    report.rows.push(CompareRow {
                        relative: path,
                        directory: left_item.directory,
                        status: if same {
                            CompareStatus::Same
                        } else {
                            CompareStatus::Changed
                        },
                        newer: if same {
                            None
                        } else {
                            newer(left_item.entry.modified, right_item.entry.modified)
                        },
                        left: Some(left_item.entry),
                        right: Some(right_item.entry),
                    });
                }
            }
        }
        for (_, left_item) in a {
            report.rows.push(CompareRow {
                relative: relative.join(&left_item.name),
                directory: left_item.directory,
                status: CompareStatus::LeftOnly,
                left: Some(left_item.entry),
                right: None,
                newer: None,
            });
        }
        if report.rows.len() >= MAX_ROWS {
            report.truncated = true;
            break;
        }
        if last.elapsed() > Duration::from_millis(250) {
            progress(&report);
            last = std::time::Instant::now();
        }
    }
    report.rows.sort_by(|a, b| a.relative.cmp(&b.relative));
    report.finished = true;
    progress(&report);
    report
}

/// What "copy to the other side" should copy from `from`: entries missing on
/// the other side and changed files whose `from` copy is newer, grouped by
/// destination folder.
pub fn copy_plan(report: &CompareReport, from: CompareSide) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let (source_root, target_root, only) = match from {
        CompareSide::Left => (&report.left, &report.right, CompareStatus::LeftOnly),
        CompareSide::Right => (&report.right, &report.left, CompareStatus::RightOnly),
    };
    let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for row in &report.rows {
        let wanted = row.status == only
            || (row.status == CompareStatus::Changed && !row.directory && row.newer == Some(from));
        if !wanted {
            continue;
        }
        let parent = row.relative.parent().unwrap_or(Path::new(""));
        groups
            .entry(target_root.join(parent))
            .or_default()
            .push(source_root.join(&row.relative));
    }
    groups.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, data: &[u8], age: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
        let time = SystemTime::now() - Duration::from_secs(age);
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn differences_are_classified_and_copy_plans_follow_the_newer_side() {
        let dir = tempfile::tempdir().unwrap();
        let (left, right) = (dir.path().join("L"), dir.path().join("R"));
        touch(&left.join("same.txt"), b"abc", 5_000);
        touch(&right.join("same.txt"), b"abc", 5_000);
        touch(&left.join("sub/changed.txt"), b"new content", 10);
        touch(&right.join("sub/changed.txt"), b"old", 9_000);
        touch(&left.join("only-left.txt"), b"x", 100);
        touch(&right.join("only-right/inner.txt"), b"y", 100);

        let cancel = AtomicBool::new(false);
        let report = compare(&left, &right, true, &cancel, |_| {});
        assert!(report.finished && !report.truncated);
        let status = |name: &str| {
            report
                .rows
                .iter()
                .find(|r| r.relative == Path::new(name))
                .map(|r| (r.status, r.newer))
        };
        assert_eq!(status("same.txt"), Some((CompareStatus::Same, None)));
        assert_eq!(
            status(&format!("sub{}changed.txt", std::path::MAIN_SEPARATOR)),
            Some((CompareStatus::Changed, Some(CompareSide::Left)))
        );
        assert_eq!(
            status("only-left.txt"),
            Some((CompareStatus::LeftOnly, None))
        );
        // A one-sided folder is one row, not its contents.
        assert_eq!(status("only-right"), Some((CompareStatus::RightOnly, None)));
        assert!(report
            .rows
            .iter()
            .all(|r| !r.relative.ends_with("inner.txt")));

        let to_right = copy_plan(&report, CompareSide::Left);
        let sources: Vec<_> = to_right.iter().flat_map(|(_, s)| s.clone()).collect();
        assert!(sources.contains(&left.join("only-left.txt")));
        assert!(sources.contains(&left.join("sub").join("changed.txt")));
        assert!(to_right.iter().any(|(dest, _)| dest == &right.join("sub")));
        let to_left = copy_plan(&report, CompareSide::Right);
        assert_eq!(
            to_left,
            vec![(left.clone(), vec![right.join("only-right")])]
        );

        // Without recursion the common folder is not entered.
        let flat = compare(&left, &right, false, &cancel, |_| {});
        assert!(flat
            .rows
            .iter()
            .all(|r| r.relative.parent() == Some(Path::new(""))));
    }
}
