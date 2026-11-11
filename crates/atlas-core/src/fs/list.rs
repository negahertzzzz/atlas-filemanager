use atlas_model::pane::FolderOrder;
use atlas_model::file_entry::{FileEntry, FileKind, SortColumn, SortDir};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use unicode_casefold::UnicodeCaseFold;

fn cached_attributes(metadata: Option<&std::fs::Metadata>) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.map_or(0, MetadataExt::file_attributes)
    }
    #[cfg(not(windows))]
    {
        u32::from(metadata.is_some_and(|metadata| metadata.permissions().readonly()))
    }
}

/// Unicode default case folding with unsigned natural numeric ordering. Runs
/// larger than u64 remain correctly ordered without overflowing or allocating.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut a = a.case_fold().peekable();
    let mut b = b.case_fold().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut az = 0;
                let mut bz = 0;
                while a.peek() == Some(&'0') {
                    a.next();
                    az += 1;
                }
                while b.peek() == Some(&'0') {
                    b.next();
                    bz += 1;
                }
                let ac = a.clone();
                let bc = b.clone();
                let (mut an, mut bn) = (0usize, 0usize);
                let (mut av, mut bv) = (Some(0u64), Some(0u64));
                while let Some(c) = a.peek().copied().filter(char::is_ascii_digit) {
                    an += 1;
                    av = av.and_then(|v| v.checked_mul(10)?.checked_add(u64::from(c as u8 - b'0')));
                    a.next();
                }
                while let Some(c) = b.peek().copied().filter(char::is_ascii_digit) {
                    bn += 1;
                    bv = bv.and_then(|v| v.checked_mul(10)?.checked_add(u64::from(c as u8 - b'0')));
                    b.next();
                }
                let ord = match (av, bv) {
                    (Some(av), Some(bv)) => av.cmp(&bv),
                    _ => an.cmp(&bn).then_with(|| ac.take(an).cmp(bc.take(bn))),
                }
                .then_with(|| az.cmp(&bz));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.cmp(&y);
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
        }
    }
}

pub async fn list_dir(
    path: &Path,
    show_hidden: bool,
    sort_col: SortColumn,
    sort_dir: SortDir,
    folder_order: impl Into<FolderOrder>,
) -> anyhow::Result<Vec<FileEntry>> {
    let folder_order = folder_order.into();
    list_dir_with_preview(path, show_hidden, sort_col, sort_dir, folder_order, |_| {}).await
}

pub async fn list_branch_dir_with_preview(
    path: &Path,
    show_hidden: bool,
    sort_col: SortColumn,
    sort_dir: SortDir,
    preview: impl Fn(Vec<FileEntry>) + Send + 'static,
) -> anyhow::Result<Vec<FileEntry>> {
    let dir = crate::fs::paths::normalize_wsl_unc_path(path.to_path_buf());
    anyhow::ensure!(!crate::vault::is_private_path(&dir), "vault_access_denied");
    let entries = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<FileEntry>> {
        crate::fs::paths::ensure_wsl_ready(&dir)?;
        let mut entries = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(dir);

        while let Some(current_dir) = queue.pop_front() {
            if super::scan_policy::is_scan_excluded(&current_dir)
                || super::preview::is_cloud_placeholder(&current_dir) { continue; }
            let directory = atlas_model::file_entry::EntryDirectory::new(current_dir.clone());
            let read_dir = match std::fs::read_dir(&current_dir) {
                Ok(rd) => rd,
                Err(_) => continue,
            };

            for raw in read_dir {
                let Ok(de) = raw else { continue };
                let path = de.path();
                if !show_hidden && crate::util::display::is_hidden(&path) {
                    continue;
                }
                let Ok(ft) = de.file_type() else { continue };
                if ft.is_dir() {
                    if ft.is_symlink() {
                        continue;
                    }
                    queue.push_back(path);
                    continue;
                }
                if ft.is_symlink() && path.is_dir() {
                    continue;
                }
                let is_symlink = ft.is_symlink();
                let kind = if is_symlink {
                    FileKind::Symlink
                } else {
                    FileKind::File
                };
                let meta = de.metadata().ok();
                let size = ft
                    .is_file()
                    .then(|| meta.as_ref().map(|m| m.len()))
                    .flatten();
                let modified = meta.as_ref().and_then(|m| m.modified().ok());
                let name = path.file_name().map_or_else(
                    || path.to_string_lossy().to_string(),
                    |n| n.to_string_lossy().to_string(),
                );

                let is_hidden = crate::util::display::is_hidden(&path);
                entries.push(FileEntry::in_directory(atlas_model::file_entry::FileEntryData {
                    name: name.into(),
                    path,
                    kind,
                    size,
                    modified,
                    _is_hidden: is_hidden,
                    _is_symlink: is_symlink,
                    archive_root: None,
                    recycle_info: None,
                }, directory.clone()).with_native_metadata(cached_attributes(meta.as_ref()), 0));
                if entries.len() % 256 == 0 {
                    let mut snapshot = entries.clone();
                    sort_entries(&mut snapshot, &sort_col, &sort_dir, false);
                    preview(snapshot);
                }
            }
        }
        sort_entries(&mut entries, &sort_col, &sort_dir, false);
        Ok(entries)
    })
    .await??;
    Ok(entries)
}

pub async fn list_dir_with_preview(
    path: &Path,
    show_hidden: bool,
    sort_col: SortColumn,
    sort_dir: SortDir,
    folder_order: impl Into<FolderOrder>,
    preview: impl FnOnce(Vec<FileEntry>) + Send + 'static,
) -> anyhow::Result<Vec<FileEntry>> {
    let folder_order = folder_order.into();
    let dir = crate::fs::paths::normalize_wsl_unc_path(path.to_path_buf());
    anyhow::ensure!(!crate::vault::is_private_path(&dir), "vault_access_denied");
    // Native Windows queries return a 64 KiB page of cached metadata per syscall.
    // Other platforms and unsupported providers retain the portable enumerator.
    let entries = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<FileEntry>> {
        crate::fs::paths::ensure_wsl_ready(&dir)?;
        // A bare server path is not a directory: like Explorer, it lists the shares.
        if let Some(server) = crate::fs::network::bare_server(&dir) {
            return match crate::fs::network::share_entries(&server, show_hidden) {
                Ok(mut entries) => {
                    sort_entries(&mut entries, &sort_col, &sort_dir, folder_order);
                    Ok(entries)
                }
                Err(error) => Err(network_error(&dir, error.into())),
            };
        }
        #[cfg(windows)]
        {
            let directory = atlas_model::file_entry::EntryDirectory::new(dir.clone());
            let mut entries = Vec::new();
            let mut preview = Some(preview);
            // Unsupported filesystems/providers transparently retain read_dir.
            let native = atlas_win_space::directory::enumerate(&dir, |raw| {
                let hidden = raw.attributes & (0x2 | 0x4) != 0
                    || raw.name.to_string_lossy().starts_with('.');
                if !show_hidden && hidden && !raw.name.to_string_lossy().starts_with(crate::vault::PREFIX) {
                    return;
                }
                let path = dir.join(&raw.name);
                let name = raw.name.to_string_lossy().into_owned();
                let is_dir = raw.attributes & 0x10 != 0;
                let symlink = matches!(raw.reparse_tag, 0xA000000C | 0xA0000003);
                let modified =
                    raw.modified
                        .checked_sub(116_444_736_000_000_000)
                        .and_then(|ticks| {
                            let magnitude = ticks.unsigned_abs();
                            let duration = std::time::Duration::new(
                                magnitude / 10_000_000,
                                ((magnitude % 10_000_000) * 100) as u32,
                            );
                            if ticks >= 0 {
                                std::time::UNIX_EPOCH.checked_add(duration)
                            } else {
                                std::time::UNIX_EPOCH.checked_sub(duration)
                            }
                        });

                entries.push(FileEntry::in_directory(atlas_model::file_entry::FileEntryData {
                    name: name.into(),
                    path,
                    kind: if symlink {
                        FileKind::Symlink
                    } else if is_dir {
                        FileKind::Dir
                    } else {
                        FileKind::File
                    },
                    size: (!is_dir && !symlink).then_some(raw.size),
                    modified,
                    _is_hidden: hidden,
                    _is_symlink: symlink,
                    archive_root: None,
                    recycle_info: None,
                }, directory.clone()).with_native_reparse_metadata(raw.attributes, u128::from_le_bytes(raw.file_id), raw.reparse_tag));
                if entries.len() == 256 {
                    let mut first = entries.clone();
                    sort_entries(&mut first, &sort_col, &sort_dir, folder_order);
                    if let Some(preview) = preview.take() {
                        preview(first);
                    }
                }
            });
            if native.is_ok() {
                sort_entries(&mut entries, &sort_col, &sort_dir, folder_order);
                return Ok(entries);
            }
            return list_dir_portable(
                &dir,
                show_hidden,
                &sort_col,
                &sort_dir,
                folder_order,
                preview,
            )
            .map_err(|error| network_error(&dir, error));
        }
        #[cfg(not(windows))]
        list_dir_portable(
            &dir,
            show_hidden,
            &sort_col,
            &sort_dir,
            folder_order,
            Some(preview),
        )
    })
    .await??;
    Ok(entries)
}

/// Tag SMB failures that new credentials could fix so the UI can show the
/// Windows credential prompt instead of a dead "Access denied" pane.
fn network_error(dir: &Path, error: anyhow::Error) -> anyhow::Error {
    if crate::fs::network::wants_credentials(dir, &error) {
        anyhow::anyhow!(crate::fs::network::CREDENTIALS_REQUIRED)
    } else {
        error
    }
}

fn list_dir_portable(
    dir: &Path,
    show_hidden: bool,
    sort_col: &SortColumn,
    sort_dir: &SortDir,
    folder_order: FolderOrder,
    mut preview: Option<impl FnOnce(Vec<FileEntry>)>,
) -> anyhow::Result<Vec<FileEntry>> {
    let directory = atlas_model::file_entry::EntryDirectory::new(dir.to_path_buf());
    let mut entries = Vec::new();
    for raw in std::fs::read_dir(&dir)? {
        let Ok(de) = raw else { continue };
        let path = de.path();
        if !show_hidden && crate::util::display::is_hidden(&path) && !crate::vault::is_private_path(&path) {
            continue;
        }
        let Ok(ft) = de.file_type() else { continue };
        let is_symlink = ft.is_symlink();
        let kind = if ft.is_dir() {
            FileKind::Dir
        } else if is_symlink {
            FileKind::Symlink
        } else {
            FileKind::File
        };
        let meta = de.metadata().ok();
        let size = ft
            .is_file()
            .then(|| meta.as_ref().map(|m| m.len()))
            .flatten();
        let modified = meta.as_ref().and_then(|m| m.modified().ok());
        let name = path.file_name().map_or_else(
            || path.to_string_lossy().to_string(),
            |n| n.to_string_lossy().to_string(),
        );

        let is_hidden = crate::util::display::is_hidden(&path) || {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                meta.as_ref()
                    .is_some_and(|meta| meta.file_attributes() & (0x2 | 0x4) != 0)
            }
            #[cfg(not(windows))]
            {
                false
            }
        };
        if !show_hidden && is_hidden && !crate::vault::is_private_path(&path) {
            continue;
        }
        entries.push(FileEntry::in_directory(atlas_model::file_entry::FileEntryData {
            name: name.into(),
            path,
            kind,
            size,
            modified,
            _is_hidden: is_hidden,
            _is_symlink: is_symlink,
            archive_root: None,
            recycle_info: None,
        }, directory.clone()).with_native_metadata(cached_attributes(meta.as_ref()), 0));
        if entries.len() == 256 {
            let mut first = entries.clone();
            sort_entries(&mut first, &sort_col, &sort_dir, folder_order);
            if let Some(preview) = preview.take() {
                preview(first);
            }
        }
    }
    sort_entries(&mut entries, &sort_col, &sort_dir, folder_order);
    Ok(entries)
}

/// Build a sorted listing from an explicit set of paths (used by virtual tabs such as
/// tag results, where entries come from many different directories rather than one
/// `read_dir`). Missing/unreadable paths are silently skipped.
pub async fn entries_for_paths(
    paths: &[PathBuf],
    show_hidden: bool,
    sort_col: SortColumn,
    sort_dir: SortDir,
    folder_order: impl Into<FolderOrder>,
) -> Vec<FileEntry> {
    let folder_order = folder_order.into();
    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        if !show_hidden && crate::util::display::is_hidden(path) {
            continue;
        }
        if let Some(file_entry) = entry_for_path(path).await {
            if !show_hidden && file_entry.is_hidden() {
                continue;
            }
            entries.push(file_entry);
        }
    }
    sort_entries(&mut entries, &sort_col, &sort_dir, folder_order);
    entries
}

/// Stat a single path and build its [`FileEntry`]. Returns `None` if it can't be read.
/// Mirrors the per-entry logic of [`list_dir`]: metadata is read without following
/// symlinks, so a symlink is classified as `Symlink` even when it points at a dir.
pub async fn entry_for_path(path: &Path) -> Option<FileEntry> {
    let meta = tokio::fs::symlink_metadata(path).await.ok()?;
    let is_symlink = meta.file_type().is_symlink();

    let name = path.file_name().map_or_else(
        || path.to_string_lossy().to_string(),
        |n| n.to_string_lossy().to_string(),
    );

    let kind = if meta.is_dir() {
        FileKind::Dir
    } else if is_symlink {
        FileKind::Symlink
    } else {
        FileKind::File
    };

    let size = meta.is_file().then_some(meta.len());
    let modified = meta.modified().ok();


    // Watcher deltas must never invoke shell extensions or hydrate placeholders.

    Some(FileEntry::from(atlas_model::file_entry::FileEntryData {
        name: name.into(),
        path: path.to_path_buf(),
        kind,
        size,
        modified,
        _is_hidden: {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes() & (0x2 | 0x4) != 0 || crate::util::display::is_hidden(path)
            }
            #[cfg(not(windows))]
            {
                crate::util::display::is_hidden(path)
            }
        },
        _is_symlink: is_symlink,
        archive_root: None,
        recycle_info: None,
    }).with_native_metadata(cached_attributes(Some(&meta)), 0))
}

/// Quick, non-recursive stats for a folder-hover tooltip (Explorer-style): immediate
/// children only, no descent into subfolders. Fast enough to run synchronously from
/// the tooltip closure, on demand, only while actually hovering.
pub struct FolderHoverStats {
    pub created: Option<std::time::SystemTime>,
    pub subfolders: usize,
    pub subfiles: usize,
    pub size: u64,
}

pub fn folder_hover_stats(path: &Path) -> Option<FolderHoverStats> {
    let created = std::fs::metadata(path).ok().and_then(|m| m.created().ok());
    let mut subfolders = 0usize;
    let mut subfiles = 0usize;
    let mut size = 0u64;
    for entry in std::fs::read_dir(path).ok()?.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            subfolders += 1;
        } else if ft.is_file() {
            subfiles += 1;
            size += entry.metadata().map_or(0, |m| m.len());
        }
    }
    Some(FolderHoverStats {
        created,
        subfolders,
        subfiles,
        size,
    })
}

pub fn sort_entries(
    entries: &mut [FileEntry],
    col: &SortColumn,
    dir: &SortDir,
    folder_order: impl Into<FolderOrder>,
) {
    let folder_order = folder_order.into();
    entries.sort_by(|a, b| {
        if folder_order != FolderOrder::Mixed {
            let kind_ord = match (&a.kind(), &b.kind()) {
                (FileKind::Dir, FileKind::Dir) => std::cmp::Ordering::Equal,
                (FileKind::Dir, _) => std::cmp::Ordering::Less,
                (_, FileKind::Dir) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            };
            let kind_ord = if folder_order == FolderOrder::FoldersLast { kind_ord.reverse() } else { kind_ord };
            if kind_ord != std::cmp::Ordering::Equal {
                return kind_ord;
            }
        }

        let ord = match col {
            SortColumn::Name | SortColumn::Kind => natural_cmp(&a.name, &b.name),
            SortColumn::Size => a.size().cmp(&b.size()),
            SortColumn::Modified => a.modified.cmp(&b.modified),
            SortColumn::Extension => a.extension().cmp(&b.extension()),
        }
        .then_with(|| natural_cmp(&a.name, &b.name))
        .then_with(|| a.name.cmp(&b.name));

        if *dir == SortDir::Desc {
            ord.reverse()
        } else {
            ord
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_folder_orders_respect_ascending_and_descending_sort() {
        use FolderOrder::{FoldersFirst, FoldersLast, Mixed};
        for (order, direction, expected) in [
            (FoldersFirst, SortDir::Asc, ["b-dir", "d-dir", "a-file", "c-file"]),
            (FoldersLast, SortDir::Asc, ["a-file", "c-file", "b-dir", "d-dir"]),
            (Mixed, SortDir::Asc, ["a-file", "b-dir", "c-file", "d-dir"]),
            (FoldersFirst, SortDir::Desc, ["d-dir", "b-dir", "c-file", "a-file"]),
            (FoldersLast, SortDir::Desc, ["c-file", "a-file", "d-dir", "b-dir"]),
            (Mixed, SortDir::Desc, ["d-dir", "c-file", "b-dir", "a-file"]),
        ] {
            let mut entries = vec![entry("c-file", FileKind::File), entry("d-dir", FileKind::Dir),
                entry("a-file", FileKind::File), entry("b-dir", FileKind::Dir)];
            sort_entries(&mut entries, &SortColumn::Name, &direction, order);
            assert_eq!(entries.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(), expected);
        }
    }

    #[tokio::test]
    async fn native_listing_applies_folder_order_to_preview_and_final_batches() {
        let fixture = tempfile::tempdir().unwrap();
        std::fs::create_dir(fixture.path().join("z-folder")).unwrap();
        for index in 0..260 {
            std::fs::write(fixture.path().join(format!("file-{index:03}")), []).unwrap();
        }
        for order in FolderOrder::ALL {
            let preview = std::sync::Arc::new(std::sync::Mutex::new(None));
            let output = preview.clone();
            let final_batch = list_dir_with_preview(fixture.path(), true, SortColumn::Name, SortDir::Asc,
                order, move |batch| *output.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(batch)).await.unwrap();
            let first_batch = preview.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take().unwrap();
            assert_eq!(first_batch.len(), 256);
            assert_eq!(final_batch.len(), 261);
            for batch in [&first_batch, &final_batch] {
                let mut sorted = batch.clone();
                sort_entries(&mut sorted, &SortColumn::Name, &SortDir::Asc, order);
                assert_eq!(batch.iter().map(|e| e.path()).collect::<Vec<_>>(), sorted.iter().map(|e| e.path()).collect::<Vec<_>>());
            }
            assert_eq!(final_batch.iter().position(|entry| entry.kind() == FileKind::Dir),
                Some(if order == FolderOrder::FoldersFirst { 0 } else { 260 }));
        }
    }

    #[test]
    fn natural_order_handles_u64_overflow_zeros_and_unicode_casefold() {
        use std::cmp::Ordering::{Equal, Less};
        assert_eq!(natural_cmp("file2.txt", "file10.txt"), Less);
        assert_eq!(natural_cmp("file2", "file02"), Less);
        assert_eq!(
            natural_cmp("x18446744073709551615", "x18446744073709551616"),
            Less
        );
        assert_eq!(
            natural_cmp("x9999999999999999999999", "x10000000000000000000000"),
            Less
        );
        assert_eq!(natural_cmp("Straße2", "STRASSE2"), Equal);
        assert_eq!(natural_cmp("Σ1", "ς1"), Equal);
    }

    #[test]
    fn descending_natural_sort_keeps_folder_order() {
        let mut entries = vec![
            entry("file2", FileKind::File),
            entry("folder3", FileKind::Dir),
            entry("file10", FileKind::File),
        ];
        sort_entries(&mut entries, &SortColumn::Name, &SortDir::Desc, true);
        assert_eq!(
            entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            ["folder3", "file10", "file2"]
        );
    }

    fn entry(name: &str, kind: FileKind) -> FileEntry {
        FileEntry::from(atlas_model::file_entry::FileEntryData {
            name: name.into(),
            path: PathBuf::from(name),
            kind,
            size: None,
            modified: None,
            _is_hidden: false,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        })
    }

    #[test]
    fn folder_order_toggle_controls_dir_grouping() {
        let mut entries = vec![
            entry("zeta", FileKind::File),
            entry("alpha", FileKind::File),
            entry("midway", FileKind::Dir),
        ];
        sort_entries(&mut entries, &SortColumn::Name, &SortDir::Asc, true);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["midway", "alpha", "zeta"]);

        sort_entries(&mut entries, &SortColumn::Name, &SortDir::Asc, false);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha", "midway", "zeta"]);
    }

    #[tokio::test]
    async fn large_directory_emits_preview_before_full_listing() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..300 {
            std::fs::File::create(dir.path().join(format!("{i:03}.txt"))).unwrap();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let entries = list_dir_with_preview(
            dir.path(),
            true,
            SortColumn::Name,
            SortDir::Asc,
            true,
            move |preview| tx.send(preview.len()).unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(rx.recv().unwrap(), 256);
        assert_eq!(entries.len(), 300);
    }

    #[tokio::test]
    async fn branch_view_flattens_tree_and_ignores_dir_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub1/sub2")).unwrap();
        std::fs::File::create(root.join("root_file.txt")).unwrap();
        std::fs::File::create(root.join("sub1/file_b.txt")).unwrap();
        std::fs::File::create(root.join("sub1/sub2/file_c.txt")).unwrap();

        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(root, root.join("sub1/loop_link"));
        }

        let entries =
            list_branch_dir_with_preview(root, true, SortColumn::Name, SortDir::Asc, |_| {})
                .await
                .unwrap();

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["file_b.txt", "file_c.txt", "root_file.txt"]);
        assert!(entries.iter().all(|e| !matches!(e.kind(), FileKind::Dir)));
    }

    #[tokio::test]
    async fn branch_view_streams_previews_every_256() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for i in 0..300 {
            std::fs::File::create(root.join("sub").join(format!("item_{i:03}.txt"))).unwrap();
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let entries = list_branch_dir_with_preview(
            root,
            true,
            SortColumn::Name,
            SortDir::Asc,
            move |preview| {
                let _ = tx.send(preview.len());
            },
        )
        .await
        .unwrap();

        assert_eq!(entries.len(), 300);
        let preview_len = rx.recv().unwrap();
        assert_eq!(preview_len, 256);
    }
}
