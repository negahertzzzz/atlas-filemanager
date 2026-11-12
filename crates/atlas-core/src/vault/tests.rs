use super::*;

const PASSWORD: &str = "a long test passphrase";

#[test]
fn replacement_preserves_old_open_inode_until_its_last_handle_is_released() {
    let (_temp, folder) = fixture();
    let p = profile();
    let mut s = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    let old = s.resolve(Path::new("personal.txt")).unwrap().unwrap();
    let new = s.create_entry(None, "Save.tmp", false).unwrap();
    s.write_at(&new, 0, b"new revision").unwrap();
    s.rename_entry(&new, None, "personal.txt", true).unwrap();
    assert_eq!(&*s.read(&old, 1024).unwrap(), b"confidential content");
    s.write_at(&old, 0, b"OLD").unwrap();
    assert_eq!(&s.read(&old, 1024).unwrap()[..3], b"OLD");
    assert_eq!(
        s.resolve(Path::new("personal.txt")).unwrap(),
        Some(new.clone())
    );
    assert_eq!(&*s.read(&new, 1024).unwrap(), b"new revision");
    s.release_entry(&old);
    assert!(s.entry(&old).is_none());
}
fn profile() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let folder = temp.path().join("Private records");
    fs::create_dir_all(folder.join("Nested empty folder")).unwrap();
    fs::write(folder.join("personal.txt"), b"confidential content").unwrap();
    fs::write(folder.join("empty.bin"), []).unwrap();
    fs::write(
        folder.join("Nested empty folder").join("large.bin"),
        vec![137; CHUNK * 2 + 97],
    )
    .unwrap();
    (temp, folder)
}

#[cfg(windows)]
#[test]
fn locked_names_survive_restart_edits_and_grant_changes_without_plaintext_metadata() {
    use atlas_model::file_entry::{FileEntryData, FileKind};
    let (_temp, folder) = fixture();
    let p = profile();
    let other = profile();
    let mut session = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    let root = session.root().to_path_buf();
    let row = || {
        vec![FileEntryData {
            name: root.file_name().unwrap().to_string_lossy().as_ref().into(),
            path: root.clone(),
            kind: FileKind::Dir,
            size: None,
            modified: None,
            _is_hidden: true,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        }
        .into()]
    };
    let listed = filter_entries(row(), &p, "Protected folder");
    assert_eq!(listed[0].name.as_str(), "Private records");
    assert_eq!(
        listed[0].path(),
        root,
        "display names must retain the physical path"
    );
    assert!(filter_entries(row(), &other, "Protected folder").is_empty());
    assert!(display_name(&root, &other).is_none());
    session
        .change_access(
            PASSWORD,
            Some("a different long password"),
            vec![p.clone(), other.clone()],
            &p,
        )
        .unwrap();
    session
        .create_entry(None, "another secret.txt", false)
        .unwrap();
    drop(session);
    assert_eq!(
        display_name(&root, &other).as_deref(),
        Some("Private records")
    );
    let header = fs::read_to_string(root.join(HEADER)).unwrap();
    for secret in [
        "Private records",
        "another secret.txt",
        PASSWORD,
        "a different long password",
    ] {
        assert!(!header.contains(secret));
    }
    let mut envelope = read_envelope(&root).unwrap();
    let attacker = profile();
    envelope.profiles.push(attacker.clone());
    write_envelope(&root, &envelope).unwrap();
    assert!(
        display_name(&root, &attacker).is_none(),
        "tampered grants must not reveal the label"
    );
    envelope.profiles.pop();
    write_envelope(&root, &envelope).unwrap();
    let mut session = unlock(&root, "a different long password", &p).unwrap();
    session
        .change_access("a different long password", None, vec![p.clone()], &p)
        .unwrap();
    drop(session);
    assert!(filter_entries(row(), &other, "Protected folder").is_empty());
    assert_eq!(display_name(&root, &p).as_deref(), Some("Private records"));
}

#[cfg(windows)]
#[test]
fn legacy_or_damaged_locked_name_is_recovered_only_after_successful_unlock() {
    let (_temp, folder) = fixture();
    let p = profile();
    let session = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    let root = session.root().to_path_buf();
    drop(session);
    // Old headers have no optional field at all. A failed password must not migrate them.
    let mut header: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(HEADER)).unwrap()).unwrap();
    header.as_object_mut().unwrap().remove("sealed_name");
    fs::write(root.join(HEADER), serde_json::to_vec(&header).unwrap()).unwrap();
    assert!(display_name(&root, &p).is_none());
    assert!(unlock(&root, "incorrect password", &p).is_err());
    assert!(read_envelope(&root).unwrap().sealed_name.is_empty());
    drop(unlock(&root, PASSWORD, &p).unwrap());
    assert_eq!(display_name(&root, &p).as_deref(), Some("Private records"));
    let mut envelope = read_envelope(&root).unwrap();
    envelope.sealed_name[0] ^= 1;
    write_envelope(&root, &envelope).unwrap();
    assert!(display_name(&root, &p).is_none());
    let session = unlock(&root, PASSWORD, &p).unwrap();
    assert_eq!(session.name(), "Private records");
    drop(session);
    assert_eq!(display_name(&root, &p).as_deref(), Some("Private records"));
}

#[test]
fn mutable_blocks_roundtrip_sparse_write_truncate_and_atomic_replace() {
    let (temp, folder) = fixture();
    let p = profile();
    let mut session = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    let id = session.resolve(Path::new("personal.txt")).unwrap().unwrap();
    session.write_at(&id, 13, b"changed").unwrap();
    assert_eq!(&*session.read(&id, 1024).unwrap(), b"confidential changed");
    let original_block = session.entry(&id).unwrap().blocks.as_ref().unwrap()[0].clone();
    session
        .write_at(&id, CHUNK as u64 + 9, b"secret tail")
        .unwrap();
    let mut gap = [7; 24];
    session.read_range(&id, CHUNK as u64 - 4, &mut gap).unwrap();
    assert_eq!(&gap[..13], &[0; 13]);
    assert_eq!(&gap[13..], b"secret tail");
    assert_eq!(
        session.entry(&id).unwrap().blocks.as_ref().unwrap()[0],
        original_block
    );
    session.resize(&id, 5).unwrap();
    session.resize(&id, 40).unwrap();
    let bytes = session.read(&id, 1024).unwrap();
    assert_eq!(&bytes[..5], b"confi");
    assert!(bytes[5..].iter().all(|b| *b == 0));
    let root = session.root().to_path_buf();
    drop(session);
    let session = unlock(&root, PASSWORD, &p).unwrap();
    assert_eq!(&*session.read(&id, 1024).unwrap(), &*bytes);
    let export = temp.path().join("mutable export");
    session.export(None, &export, &Control::default()).unwrap();
    assert_eq!(&fs::read(export.join("personal.txt")).unwrap(), &*bytes);
}

#[test]
fn namespace_operations_preserve_identity_and_reject_cycles_and_nonempty_deletes() {
    let (_temp, folder) = fixture();
    let p = profile();
    let mut s = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    let dir = s.create_entry(None, "New folder", true).unwrap();
    let file = s.create_entry(Some(&dir), "draft.txt", false).unwrap();
    s.write_at(&file, 0, b"private editing").unwrap();
    assert!(s.delete_entry(&dir).is_err());
    assert!(s.rename_entry(&dir, Some(&dir), "cycle", false).is_err());
    s.rename_entry(&file, None, "Renamed.txt", false).unwrap();
    assert_eq!(
        s.resolve(Path::new("renamed.TXT")).unwrap(),
        Some(file.clone())
    );
    assert_eq!(&*s.read(&file, 1024).unwrap(), b"private editing");
    s.delete_entry(&dir).unwrap();
    s.delete_entry(&file).unwrap();
    assert!(s.resolve(Path::new("Renamed.txt")).is_err());
}

#[test]
fn copy_on_write_does_not_reuse_nonces_and_detects_modified_blocks() {
    let (_temp, folder) = fixture();
    let p = profile();
    let mut s = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    let id = s.create_entry(None, "saved.txt", false).unwrap();
    s.write_at(&id, 0, b"private content").unwrap();
    let block = s.entry(&id).unwrap().blocks.as_ref().unwrap()[0]
        .clone()
        .unwrap();
    let first = fs::read(s.root().join(&block)).unwrap();
    s.write_at(&id, 0, b"private content").unwrap();
    let block2 = s.entry(&id).unwrap().blocks.as_ref().unwrap()[0]
        .clone()
        .unwrap();
    let second = fs::read(s.root().join(&block2)).unwrap();
    assert_ne!(block, block2);
    assert_ne!(first, second);
    assert!(!s.root().join(block).exists());
    let mut broken = second;
    broken[24] ^= 1;
    fs::write(s.root().join(block2), broken).unwrap();
    assert!(s.read(&id, 1024).is_err());
}

#[test]
fn v1_unlock_upgrades_authenticated_envelope_and_retains_legacy_contents() {
    let (_temp, folder) = fixture();
    let p = profile();
    let mut s = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    s.envelope = read_envelope(s.root()).unwrap();
    s.envelope.version = 1;
    let derived = derive(PASSWORD, &s.envelope.salt).unwrap();
    s.envelope.wrapped_key = seal(&derived, s.key.as_slice(), &s.envelope.aad().unwrap()).unwrap();
    write_envelope(s.root(), &s.envelope).unwrap();
    let root = s.root().to_path_buf();
    drop(s);
    let mut s = unlock(&root, PASSWORD, &p).unwrap();
    assert_eq!(s.envelope.version, 2);
    let id = s.resolve(Path::new("personal.txt")).unwrap().unwrap();
    assert_eq!(&*s.read(&id, 1024).unwrap(), b"confidential content");
    s.write_at(&id, 0, b"CONFIDENTIAL").unwrap();
    assert_eq!(&*s.read(&id, 1024).unwrap(), b"CONFIDENTIAL content");
}

#[test]
fn round_trip_names_contents_large_and_empty_files_never_persist_in_plaintext() {
    let (temp, folder) = fixture();
    let p = profile();
    let session = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    assert!(!folder.exists());
    let root = session.root().to_path_buf();
    assert!(root
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with(PREFIX));
    for file in fs::read_dir(&root).unwrap() {
        let file = file.unwrap();
        if file.file_name() == "lock" {
            continue;
        }
        let bytes = fs::read(file.path()).unwrap();
        for secret in [
            "Private records",
            "personal.txt",
            "confidential content",
            PASSWORD,
        ] {
            assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_bytes()));
        }
    }
    let id = session
        .children(None)
        .into_iter()
        .find(|e| e.name == "personal.txt")
        .unwrap()
        .id
        .clone();
    assert_eq!(&*session.read(&id, 1024).unwrap(), b"confidential content");
    session.verify(&Control::default()).unwrap();
    drop(session);
    let session = unlock(&root, PASSWORD, &p).unwrap();
    let export = temp.path().join("Export");
    session.export(None, &export, &Control::default()).unwrap();
    assert_eq!(
        fs::read(export.join("personal.txt")).unwrap(),
        b"confidential content"
    );
    assert_eq!(fs::read(export.join("empty.bin")).unwrap().len(), 0);
    assert_eq!(
        fs::read(export.join("Nested empty folder/large.bin")).unwrap(),
        vec![137; CHUNK * 2 + 97]
    );
    assert!(session.export(None, &export, &Control::default()).is_err());
    let (restored, removed) = session.restore(&Control::default()).unwrap();
    assert!(removed);
    assert_eq!(restored, folder);
    assert!(!root.exists());
    assert_eq!(
        fs::read(folder.join("personal.txt")).unwrap(),
        b"confidential content"
    );
}

#[test]
fn grants_are_authenticated_password_change_revokes_old_password_and_import_commits() {
    let (temp, folder) = fixture();
    let p = profile();
    let other = profile();
    let mut session = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    let root = session.root().to_path_buf();
    assert!(!visible_to(&root, &other));
    assert!(
        unlock(&root, PASSWORD, &p).is_err(),
        "exclusive lock must prevent a second session"
    );
    assert!(session
        .change_access("wrong", None, vec![p.clone()], &p)
        .is_err());
    session
        .change_access(
            PASSWORD,
            Some("a different long password"),
            vec![p.clone(), other.clone()],
            &p,
        )
        .unwrap();
    assert!(visible_to(&root, &other));
    let imported = temp.path().join("import.txt");
    fs::write(&imported, "new content").unwrap();
    session
        .import(std::slice::from_ref(&imported), None, &Control::default())
        .unwrap();
    assert!(imported.exists(), "import is copy-in");
    assert!(session
        .import(&[imported], None, &Control::default())
        .is_err());
    drop(session);
    assert!(unlock(&root, PASSWORD, &p).is_err());
    let session = unlock(&root, "a different long password", &other).unwrap();
    assert!(session
        .children(None)
        .iter()
        .any(|e| e.name == "import.txt"));
    drop(session);
    let mut envelope = read_envelope(&root).unwrap();
    let attacker = profile();
    envelope.profiles.push(attacker.clone());
    write_envelope(&root, &envelope).unwrap();
    assert!(unlock(&root, "a different long password", &attacker).is_err());
}

#[test]
fn wrong_profile_tampering_truncation_and_cancellation_fail_closed() {
    let (temp, folder) = fixture();
    let p = profile();
    let control = Control::default();
    control.cancel();
    assert!(protect(&folder, PASSWORD, vec![p.clone()], &p, &control).is_err());
    assert!(folder.join("personal.txt").exists());
    let session = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    let root = session.root().to_path_buf();
    let entry = session
        .children(None)
        .into_iter()
        .find(|e| e.name == "personal.txt")
        .unwrap();
    let object = root.join(&entry.id);
    let original = fs::read(&object).unwrap();
    let mut damaged = original.clone();
    damaged[35] ^= 1;
    fs::write(&object, damaged).unwrap();
    assert!(session.read(&entry.id, 1024).is_err());
    let export = temp.path().join("failed-export");
    assert!(session.export(None, &export, &Control::default()).is_err());
    assert!(!export.exists());
    fs::write(&object, &original[..original.len() - 1]).unwrap();
    assert!(session.read(&entry.id, 1024).is_err());
    let mut appended = original;
    appended.push(0);
    fs::write(&object, appended).unwrap();
    assert!(session.read(&entry.id, 1024).is_err());
    drop(session);
    assert!(unlock(&root, PASSWORD, &profile()).is_err());
    assert!(unlock(&root, "incorrect password", &p).is_err());
}

#[test]
fn invalid_names_cycles_and_recovery_paths_are_rejected() {
    for name in [
        "..",
        ".",
        "../escape",
        "C:\\secret",
        "a/b",
        "CON",
        "nul.txt",
        "file.",
        "file ",
    ] {
        assert!(validate_name(name).is_err(), "{name}");
    }
    assert!(is_private_path(Path::new(
        "ordinary/.ATLAS-VAULT-opaque/child"
    )));
    let mut manifest = Manifest {
        name: "safe".into(),
        entries: vec![Entry {
            id: "a".repeat(32),
            parent: Some("a".repeat(32)),
            name: "cycle".into(),
            directory: true,
            size: 0,
            modified: 0,
            digest: [0; 32],
            blocks: None,
            attributes: 0,
        }],
    };
    assert!(validate_manifest(&manifest).is_err());
    manifest.entries[0].parent = None;
    assert!(validate_manifest(&manifest).is_ok());
}

#[cfg(windows)]
#[test]
fn windows_hard_links_and_alternate_streams_are_rejected_without_touching_originals() {
    let (temp, folder) = fixture();
    let p = profile();
    let outside = temp.path().join("outside.txt");
    fs::write(&outside, b"outside data").unwrap();
    fs::hard_link(&outside, folder.join("linked.txt")).unwrap();
    assert!(protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).is_err());
    assert!(folder.join("personal.txt").exists());
    fs::remove_file(folder.join("linked.txt")).unwrap();
    fs::write(
        folder.join("personal.txt:private-stream"),
        b"must not be discarded",
    )
    .unwrap();
    assert!(protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).is_err());
    assert_eq!(
        fs::read(folder.join("personal.txt:private-stream")).unwrap(),
        b"must not be discarded"
    );
    assert_eq!(fs::read(outside).unwrap(), b"outside data");
}

#[test]
fn listings_mask_only_authorized_roots_and_direct_payload_navigation_is_denied() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (temp, folder) = fixture();
    let p = profile();
    let session = protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).unwrap();
    let root = session.root().to_path_buf();
    drop(session);
    let entries = runtime
        .block_on(crate::fs::list::list_dir(
            temp.path(),
            false,
            atlas_model::file_entry::SortColumn::Name,
            atlas_model::file_entry::SortDir::Asc,
            atlas_model::pane::FolderOrder::default(),
        ))
        .unwrap();
    let allowed = filter_entries(entries.clone(), &p, "Protected folder");
    // Windows can recover the original label through account-bound DPAPI;
    // other platforms use the generic label while the vault is locked.
    let expected_name = if cfg!(windows) {
        "Private records"
    } else {
        "Protected folder"
    };
    assert!(allowed
        .iter()
        .any(|e| e.path() == root && e.name == expected_name && !e.is_hidden()));
    assert!(filter_entries(entries, &profile(), "Protected folder").is_empty());
    assert!(runtime
        .block_on(crate::fs::list::list_dir(
            &root,
            true,
            atlas_model::file_entry::SortColumn::Name,
            atlas_model::file_entry::SortDir::Asc,
            atlas_model::pane::FolderOrder::default()
        ))
        .is_err());
    assert!(crate::fs::scan_policy::is_scan_excluded(
        &root.join("child")
    ));
}

#[cfg(unix)]
#[test]
fn symbolic_and_hard_links_do_not_get_followed_or_destroyed() {
    let (temp, folder) = fixture();
    let p = profile();
    let outside = temp.path().join("outside");
    fs::write(&outside, "keep me").unwrap();
    std::os::unix::fs::symlink(&outside, folder.join("link")).unwrap();
    assert!(protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).is_err());
    fs::remove_file(folder.join("link")).unwrap();
    fs::hard_link(&outside, folder.join("link")).unwrap();
    assert!(protect(&folder, PASSWORD, vec![p.clone()], &p, &Control::default()).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"keep me");
}
