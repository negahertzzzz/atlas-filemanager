use atlas_model::app_state::BgResult;
use atlas_model::file_entry::{FileEntry, FileKind, RecycleBinInfo, SortColumn, SortDir};
use atlas_model::ops::{OpError, OpErrorKind};
use atlas_model::pane::TabId;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub async fn list_recycle_bin(sort_col: SortColumn, sort_dir: SortDir) -> Vec<FileEntry> {
    tokio::task::spawn_blocking(move || read_recycle_bin(&sort_col, &sort_dir))
        .await
        .unwrap_or_default()
}

pub fn read_recycle_bin(sort_col: &SortColumn, sort_dir: &SortDir) -> Vec<FileEntry> {
    list_sync(sort_col, sort_dir)
}

/// Lightweight status for sidebar feedback. Called on a blocking worker.
#[cfg(not(windows))]
pub fn contains_items() -> Option<bool> {
    for dir in accessible_bin_dirs() {
        match std::fs::read_dir(dir.join("files")) {
            Ok(mut entries) => match entries.next() {
                Some(Ok(_)) => return Some(true),
                Some(Err(_)) => return None,
                None => {}
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some(false)
}

fn list_sync(sort_col: &SortColumn, sort_dir: &SortDir) -> Vec<FileEntry> {
    #[cfg(windows)]
    let shell_metadata = atlas_win_space::recycle::metadata().unwrap_or_default();
    let mut entries = Vec::new();
    for rb_dir in accessible_bin_dirs() {
        #[cfg(windows)]
        {
            let Ok(rd) = std::fs::read_dir(&rb_dir) else {
                continue;
            };
            for entry in rd.flatten() {
                let i_path = entry.path();
                let fname = i_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !fname.starts_with("$I") {
                    continue;
                }
                let r_path = rb_dir.join(format!("$R{}", &fname[2..]));
                if !r_path.exists() {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&i_path) else {
                    continue;
                };
                let Some(mut meta) = parse_i_file(&bytes) else {
                    continue;
                };
                if let Some(native) = shell_metadata.get(&r_path) {
                    if let Some(name) = meta.original_path.file_name() {
                        meta.original_path = native.deleted_from.join(name);
                    }
                    meta.deleted_at = native.deleted_at;
                }
                let original_name = meta
                    .original_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| fname.clone());
                let kind = if r_path.is_dir() {
                    FileKind::Dir
                } else {
                    FileKind::File
                };
                entries.push(FileEntry::from(atlas_model::file_entry::FileEntryData {
                    name: original_name.into(),
                    path: r_path,
                    kind,
                    size: Some(meta.original_size),
                    modified: Some(meta.deleted_at),
                    _is_hidden: false,
                    _is_symlink: false,
                    archive_root: None,
                    recycle_info: Some(RecycleBinInfo {
                        original_path: meta.original_path,
                        deleted_at: meta.deleted_at,
                        i_file: i_path,
                    }),
                }));
            }
        }
        #[cfg(not(windows))]
        {
            let info_dir = rb_dir.join("info");
            let files_dir = rb_dir.join("files");
            let Ok(rd) = std::fs::read_dir(&info_dir) else {
                continue;
            };
            for entry in rd.flatten() {
                let i_path = entry.path();
                let fname = i_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !fname.ends_with(".trashinfo") {
                    continue;
                }
                let base_name = &fname[..fname.len() - ".trashinfo".len()];
                let r_path = files_dir.join(base_name);
                if !r_path.exists() {
                    continue;
                }
                let Ok(content) = std::fs::read_to_string(&i_path) else {
                    continue;
                };
                let Some(mut meta) = parse_trashinfo(&content) else {
                    continue;
                };
                let kind = if r_path.is_dir() {
                    FileKind::Dir
                } else {
                    FileKind::File
                };
                if let Ok(fs_meta) = std::fs::metadata(&r_path) {
                    meta.original_size = fs_meta.len();
                }
                let original_name = meta
                    .original_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| base_name.to_string());
                entries.push(FileEntry::from(atlas_model::file_entry::FileEntryData {
                    name: original_name.into(),
                    path: r_path,
                    kind,
                    size: Some(meta.original_size),
                    modified: Some(meta.deleted_at),
                    _is_hidden: false,
                    _is_symlink: false,
                    archive_root: None,
                    recycle_info: Some(RecycleBinInfo {
                        original_path: meta.original_path,
                        deleted_at: meta.deleted_at,
                        i_file: i_path,
                    }),
                }));
            }
        }
    }
    crate::fs::list::sort_entries(&mut entries, sort_col, sort_dir, true);
    entries
}

struct Meta {
    original_path: PathBuf,
    original_size: u64,
    deleted_at: SystemTime,
}

/// Decode percent-encoded characters in a string according to RFC 2396.
#[cfg(any(not(windows), test))]
fn percent_decode(s: &str) -> String {
    let mut bytes = Vec::with_capacity(s.len());
    let mut chars = s.as_bytes().iter();
    while let Some(&b) = chars.next() {
        if b == b'%' {
            if let (Some(&h1), Some(&h2)) = (chars.next(), chars.next()) {
                if let (Some(d1), Some(d2)) = (from_hex_digit(h1), from_hex_digit(h2)) {
                    bytes.push((d1 << 4) | d2);
                    continue;
                }
                bytes.push(b'%');
                bytes.push(h1);
                bytes.push(h2);
                continue;
            }
        }
        bytes.push(b);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(any(not(windows), test))]
const fn from_hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse ISO-8601 deletion date formatted as `YYYY-MM-DDThh:mm:ss`.
#[cfg(any(not(windows), test))]
fn parse_deletion_date(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    // Byte-indexed below; non-ASCII input would split a char and panic.
    if s.len() < 19 || !s.as_bytes()[..19].is_ascii() {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-'
        || b[7] != b'-'
        || (b[10] != b'T' && b[10] != b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let year: i64 = s[0..4].parse().ok()?;
    let month: u32 = s[5..7].parse().ok()?;
    let day: u32 = s[8..10].parse().ok()?;
    let hour: u64 = s[11..13].parse().ok()?;
    let min: u64 = s[14..16].parse().ok()?;
    let sec: u64 = s[17..19].parse().ok()?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour >= 24
        || min >= 60
        || sec >= 60
    {
        return None;
    }

    let is_leap_year = |y: i64| (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
    let mut total_days: i64 = 0;
    if year >= 1970 {
        for y in 1970..year {
            total_days += if is_leap_year(y) { 366 } else { 365 };
        }
    } else {
        for y in year..1970 {
            total_days -= if is_leap_year(y) { 366 } else { 365 };
        }
    }
    let days_in_months = [
        31,
        if is_leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for m in 0..(month as usize - 1) {
        total_days += days_in_months[m];
    }
    total_days += (day as i64) - 1;

    let total_secs = total_days * 86_400 + (hour as i64) * 3600 + (min as i64) * 60 + (sec as i64);
    if total_secs >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(total_secs as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(total_secs.unsigned_abs()))
    }
}

/// Parse FreeDesktop.org `.trashinfo` metadata file.
#[cfg(any(not(windows), test))]
fn parse_trashinfo(data: &str) -> Option<Meta> {
    let mut path = None;
    let mut deleted_at = None;
    for line in data.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("Path=") {
            let decoded = percent_decode(val);
            path = Some(PathBuf::from(decoded));
        } else if let Some(val) = line.strip_prefix("DeletionDate=") {
            deleted_at = parse_deletion_date(val);
        }
    }
    let original_path = path?;
    let deleted_at = deleted_at.unwrap_or_else(SystemTime::now);
    Some(Meta {
        original_path,
        original_size: 0,
        deleted_at,
    })
}

/// Parse a Windows `$I` recycle bin metadata file.
/// Layout (Vista+, version 1): [8B version][8B original size][8B FILETIME][UTF-16LE path, NUL-term]
/// Version 2 adds a 4-byte path-length prefix before the path (path starts at offset 28).
#[cfg(windows)]
fn parse_i_file(data: &[u8]) -> Option<Meta> {
    if data.len() < 24 {
        return None;
    }
    let version = u64::from_le_bytes(data[0..8].try_into().ok()?);
    if version != 1 && version != 2 {
        return None;
    }
    let original_size = u64::from_le_bytes(data[8..16].try_into().ok()?);
    let filetime = u64::from_le_bytes(data[16..24].try_into().ok()?);
    let deleted_at = filetime_to_system_time(filetime);

    let path_start: usize = if version == 2 { 28 } else { 24 };
    if data.len() <= path_start {
        return None;
    }
    let utf16: Vec<u16> = data[path_start..]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    let original_path = PathBuf::from(String::from_utf16_lossy(&utf16));

    Some(Meta {
        original_path,
        original_size,
        deleted_at,
    })
}

#[cfg(windows)]
fn filetime_to_system_time(ft: u64) -> SystemTime {
    if ft == 0 {
        return SystemTime::now();
    }
    // FILETIME: 100-ns ticks since 1601-01-01; UNIX_EPOCH: 1970-01-01
    let unix_secs = (ft / 10_000_000).saturating_sub(11_644_473_600);
    // A corrupt `$I` file can hold any FILETIME; overflowing `SystemTime`
    // would panic while listing the bin.
    UNIX_EPOCH
        .checked_add(Duration::from_secs(unix_secs))
        .unwrap_or_else(SystemTime::now)
}

/// Restore a file from the recycle bin to its original location.
/// Moves the recycled file back to `original_path` and deletes the metadata file.
pub async fn restore(
    r_path: PathBuf,
    i_path: PathBuf,
    original_path: PathBuf,
) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        if let Some(parent) = original_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Err(e) = std::fs::rename(&r_path, &original_path) {
            if r_path.is_file() {
                std::fs::copy(&r_path, &original_path)?;
                std::fs::remove_file(&r_path)?;
            } else {
                return Err(e.into());
            }
        }
        let _ = std::fs::remove_file(&i_path);
        Ok(())
    })
    .await?
}

/// Restore a batch of recycled items (`$R` / payload file, metadata, original path)
/// back to their original locations, reporting per-item failures via `OpComplete`.
pub async fn restore_items(
    items: Vec<(PathBuf, PathBuf, PathBuf)>,
    op_id: u64,
    tx: flume::Sender<BgResult>,
    affected_tabs: Vec<TabId>,
) {
    let mut errors: Vec<OpError> = Vec::new();
    let mut restored = Vec::new();
    for (r_path, i_path, original_path) in items {
        if original_path.exists() {
            errors.push(OpError {
                path: original_path,
                kind: OpErrorKind::AlreadyExists,
            });
            continue;
        }
        if let Err(e) = restore(r_path, i_path, original_path.clone()).await {
            let kind = e.downcast_ref::<std::io::Error>().map_or_else(
                || OpErrorKind::Msg(e.to_string()),
                super::transfer::io_error_kind,
            );
            errors.push(OpError {
                path: original_path,
                kind,
            });
        } else {
            restored.push(original_path);
        }
    }
    if !restored.is_empty() {
        let _ = tx.send(BgResult::PathsRestored(restored));
    }
    let result = if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    };
    let _ = tx.send(BgResult::OpComplete {
        id: op_id,
        result,
        affected_tabs,
    });
}

/// Permanently delete a batch of recycled items (both the payload and the
/// metadata file). Reports per-item failures via `OpComplete`.
pub async fn purge_items(
    items: Vec<(PathBuf, PathBuf)>,
    op_id: u64,
    tx: flume::Sender<BgResult>,
    affected_tabs: Vec<TabId>,
) {
    let result = tokio::task::spawn_blocking(move || {
        let mut errors: Vec<OpError> = Vec::new();
        for (r_path, i_path) in items {
            if let Err(e) = remove_r_file(&r_path) {
                errors.push(OpError {
                    path: r_path,
                    kind: super::transfer::io_error_kind(&e),
                });
                continue;
            }
            let _ = std::fs::remove_file(&i_path);
        }
        errors
    })
    .await
    .unwrap_or_default();
    let result = if result.is_empty() {
        Ok(())
    } else {
        Err(result)
    };
    let _ = tx.send(BgResult::OpComplete {
        id: op_id,
        result,
        affected_tabs,
    });
}

/// Empty every accessible recycle bin: delete all payloads and their
/// metadata. Reports per-item failures via `OpComplete`.
pub async fn empty_bins(op_id: u64, tx: flume::Sender<BgResult>, affected_tabs: Vec<TabId>) {
    empty_recycle_bin(op_id, tx, affected_tabs).await;
}

/// Empty every accessible recycle bin: removes contents of `files/` and `info/`
/// on Linux FreeDesktop trash, or `$R*`/`$I*` payloads on Windows.
pub async fn empty_recycle_bin(op_id: u64, tx: flume::Sender<BgResult>, affected_tabs: Vec<TabId>) {
    let errors = tokio::task::spawn_blocking(|| {
        let mut errors: Vec<OpError> = Vec::new();
        for rb_dir in accessible_bin_dirs() {
            #[cfg(windows)]
            {
                let Ok(rd) = std::fs::read_dir(&rb_dir) else {
                    continue;
                };
                for entry in rd.flatten() {
                    let path = entry.path();
                    let fname = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if !(fname.starts_with("$R") || fname.starts_with("$I")) {
                        continue;
                    }
                    if let Err(e) = remove_r_file(&path) {
                        errors.push(OpError {
                            path,
                            kind: super::transfer::io_error_kind(&e),
                        });
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let files_dir = rb_dir.join("files");
                if let Ok(rd) = std::fs::read_dir(&files_dir) {
                    for entry in rd.flatten() {
                        let path = entry.path();
                        if let Err(e) = remove_r_file(&path) {
                            errors.push(OpError {
                                path,
                                kind: super::transfer::io_error_kind(&e),
                            });
                        }
                    }
                }
                let info_dir = rb_dir.join("info");
                if let Ok(rd) = std::fs::read_dir(&info_dir) {
                    for entry in rd.flatten() {
                        let path = entry.path();
                        if let Err(e) = remove_r_file(&path) {
                            errors.push(OpError {
                                path,
                                kind: super::transfer::io_error_kind(&e),
                            });
                        }
                    }
                }
            }
        }
        errors
    })
    .await
    .unwrap_or_default();
    let result = if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    };
    let _ = tx.send(BgResult::OpComplete {
        id: op_id,
        result,
        affected_tabs,
    });
}

fn remove_r_file(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Enumerate all accessible recycle bin directories.
/// On Windows, enumerates `$Recycle.Bin\<SID>` per drive letter.
/// On Linux, returns the FreeDesktop user trash directory (`$XDG_DATA_HOME/Trash` or `~/.local/share/Trash`).
fn accessible_bin_dirs() -> Vec<PathBuf> {
    let mut result = Vec::new();
    #[cfg(windows)]
    {
        for letter in b'A'..=b'Z' {
            let rb = PathBuf::from(format!("{}:\\$Recycle.Bin", letter as char));
            let Ok(rd) = std::fs::read_dir(&rb) else {
                continue;
            };
            for entry in rd.flatten() {
                let dir = entry.path();
                if dir.is_dir() && std::fs::read_dir(&dir).is_ok() {
                    result.push(dir);
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(trash) = dirs::data_dir().map(|d| d.join("Trash")).or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/Trash"))
        }) {
            result.push(trash);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `$I` files come straight from disk; any byte soup must parse or be
    /// rejected, never panic (extreme FILETIMEs used to overflow SystemTime).
    #[cfg(windows)]
    #[test]
    fn hostile_i_files_never_panic() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for len in 0..96 {
            for round in 0..64 {
                let mut data: Vec<u8> = (0..len).map(|_| next() as u8).collect();
                if len >= 24 {
                    data[..8].copy_from_slice(&(1 + round % 2_u64).to_le_bytes());
                    let filetime = if round % 3 == 0 { u64::MAX } else { next() };
                    data[16..24].copy_from_slice(&filetime.to_le_bytes());
                }
                let _ = parse_i_file(&data);
            }
        }
    }

    #[test]
    fn hostile_deletion_dates_never_panic() {
        for raw in [
            "2é24-01-01T00:00:00",
            "9999-12-31T23:59:59",
            "-999-01-01T00:00:00",
            "0000-01-01T00:00:00",
            "日本語日本語日本語日",
            "2024-13-01T00:00:00",
            "",
        ] {
            let _ = parse_deletion_date(raw);
        }
        assert!(parse_deletion_date("2024-02-29T12:00:00").is_some());
    }

    #[tokio::test]
    async fn restore_reports_only_successful_destinations_and_keeps_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let payload = root.path().join("payload");
        let metadata = root.path().join("metadata");
        let restored = root.path().join("original/file.txt");
        let conflict_payload = root.path().join("conflict-payload");
        let conflict = root.path().join("existing.txt");
        std::fs::write(&payload, b"restored").unwrap();
        std::fs::write(&metadata, b"metadata").unwrap();
        std::fs::write(&conflict_payload, b"keep in bin").unwrap();
        std::fs::write(&conflict, b"keep original").unwrap();
        let (tx, rx) = flume::unbounded();
        restore_items(
            vec![
                (payload.clone(), metadata.clone(), restored.clone()),
                (
                    conflict_payload.clone(),
                    root.path().join("other-metadata"),
                    conflict.clone(),
                ),
            ],
            42,
            tx,
            vec![],
        )
        .await;
        let events: Vec<_> = rx.try_iter().collect();
        assert!(events.iter().any(|event| matches!(event, BgResult::PathsRestored(paths) if paths == &vec![restored.clone()])));
        assert!(events.iter().any(|event| matches!(event, BgResult::OpComplete { id: 42, result: Err(errors), .. } if errors.len() == 1 && errors[0].kind == OpErrorKind::AlreadyExists)));
        assert_eq!(std::fs::read(restored).unwrap(), b"restored");
        assert_eq!(std::fs::read(conflict).unwrap(), b"keep original");
        assert!(conflict_payload.exists());
        assert!(!payload.exists());
        assert!(!metadata.exists());
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(
            percent_decode("/home/user/my%20file.txt"),
            "/home/user/my file.txt"
        );
        assert_eq!(percent_decode("/plain/path.txt"), "/plain/path.txt");
        assert_eq!(percent_decode("foo%2Fbar"), "foo/bar");
    }

    #[test]
    fn test_parse_deletion_date() {
        let date = parse_deletion_date("2026-10-10T12:00:00").unwrap();
        let dur = date.duration_since(UNIX_EPOCH).unwrap();
        assert!(dur.as_secs() > 1_700_000_000);
    }

    #[test]
    fn test_parse_trashinfo() {
        let content =
            "[Trash Info]\nPath=/home/user/test%20doc.txt\nDeletionDate=2026-10-10T12:00:00\n";
        let meta = parse_trashinfo(content).unwrap();
        assert_eq!(meta.original_path, PathBuf::from("/home/user/test doc.txt"));
    }
}
