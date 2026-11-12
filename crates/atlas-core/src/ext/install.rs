// reason: extension install/uninstall is a one-shot user action off the gpui
// render thread; the sync-fs ban targets blocking the UI foreground.
#![allow(clippy::disallowed_methods)]
//! Install / uninstall extensions on disk. An extension is a directory under
//! [`super::extensions_dir`] containing an `extension.toml`. Installing accepts a
//! `.zip` bundle or a folder; uninstalling removes the directory.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};

use super::manifest::Manifest;
pub use super::manifest::{is_valid_extension_id, validate_extension_id};

/// Resolve and validate the target directory for an extension ID (SEC-01).
///
/// Ensures the ID is well-formed ([a-z0-9_-]+), contains no traversal components,
/// and that `extensions_dir().join(id)` resides strictly inside `extensions_dir()`.
pub fn resolve_extension_dir(id: &str) -> anyhow::Result<PathBuf> {
    validate_extension_id(id)?;
    let base = super::extensions_dir();
    let dest = base.join(id);

    // Lexical containment checks
    if !dest.starts_with(&base) || dest == base {
        anyhow::bail!(
            "Extension path {:?} does not reside inside extensions directory {:?}",
            dest,
            base
        );
    }
    let rel = dest.strip_prefix(&base)?;
    let comps: Vec<_> = rel.components().collect();
    if comps.len() != 1 || !matches!(comps[0], std::path::Component::Normal(_)) {
        anyhow::bail!("Extension ID {:?} produces invalid path components", id);
    }

    // Canonical check if base directory already exists
    if let Ok(canon_base) = base.canonicalize() {
        if dest.exists() {
            if let Ok(canon_dest) = dest.canonicalize() {
                if !canon_dest.starts_with(&canon_base) || canon_dest == canon_base {
                    anyhow::bail!(
                        "Canonical extension path {:?} escapes extensions directory {:?}",
                        canon_dest,
                        canon_base
                    );
                }
            }
        }
    }

    Ok(dest)
}

/// Validate `extension.toml` in `dir` and return its parsed manifest.
fn read_manifest(dir: &Path) -> anyhow::Result<Manifest> {
    let text =
        std::fs::read_to_string(dir.join("extension.toml")).context("extension.toml not found")?;
    Manifest::parse(&text)
}

/// Install from a folder by copying it to `<extensions>/<id>`.
pub fn install_folder(src: &Path) -> anyhow::Result<String> {
    let manifest = read_manifest(src)?;
    let dest = resolve_extension_dir(&manifest.id)?;
    copy_dir(src, &dest)?;
    Ok(manifest.id)
}

/// Install from a `.zip` whose root contains `extension.toml`.
pub fn install_zip(zip_path: &Path) -> anyhow::Result<String> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    // Extract to a temp dir first, then validate + move into place.
    let tmp = std::env::temp_dir().join(format!("fp-ext-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&tmp)?;
    if let Err(error) = extract_bundle(&mut archive, &tmp) {
        std::fs::remove_dir_all(&tmp).ok();
        return Err(error.context("unzip extension"));
    }

    // The manifest may be at the zip root or in a single top-level folder.
    let root = if tmp.join("extension.toml").is_file() {
        tmp.clone()
    } else {
        std::fs::read_dir(&tmp)?
            .flatten()
            .map(|e| e.path())
            .find(|p| p.join("extension.toml").is_file())
            .ok_or_else(|| anyhow!("no extension.toml in zip"))?
    };

    let manifest = read_manifest(&root)?;
    let dest = resolve_extension_dir(&manifest.id)?;
    if dest.exists() {
        std::fs::remove_dir_all(&dest).ok();
    }
    copy_dir(&root, &dest)?;
    std::fs::remove_dir_all(&tmp).ok();
    Ok(manifest.id)
}

/// Remove an installed extension by id. Builtins (no on-disk dir) are a no-op.
pub fn uninstall(id: &str) -> anyhow::Result<()> {
    let dir = resolve_extension_dir(id)?;
    if dir.is_dir() {
        std::fs::remove_dir_all(&dir).context("remove extension dir")?;
    }
    Ok(())
}

/// Official bundled extensions that Atlas no longer ships. Their features
/// moved into the app (native previews, the Actions ring), so a stale copy
/// would only duplicate menu entries.
pub const RETIRED_BUNDLED: [&str; 2] = ["file-tools", "document-previews"];

/// Move retired official extensions into `extensions/.trash/`. Only folders
/// whose manifest is still the official one (same id, "Atlas Team") move; a
/// user's own extension that reused the id is left alone. Never fails startup:
/// on any error the folder stays and keeps loading exactly as before.
pub fn retire_bundled() -> Vec<String> {
    retire_in(&super::extensions_dir(), &RETIRED_BUNDLED)
}

/// Runs once per id: `.retired` records handled ids, so an extension the user
/// deliberately reinstalls afterwards (e.g. from the marketplace) stays put.
fn retire_in(base: &Path, ids: &[&str]) -> Vec<String> {
    let marker = base.join(".retired");
    let done = std::fs::read_to_string(&marker).unwrap_or_default();
    let mut handled: Vec<String> = done.lines().map(str::to_owned).collect();
    let mut retired = Vec::new();
    for id in ids {
        if handled.iter().any(|h| h == id) {
            continue;
        }
        match retire_one(base, id) {
            Ok(moved) => {
                if moved {
                    retired.push(id.to_string());
                }
                handled.push(id.to_string());
            }
            // Not recorded: retried on the next start.
            Err(error) => tracing::warn!("could not retire extension {id}: {error:#}"),
        }
    }
    if handled.len() != done.lines().count() {
        let _ = std::fs::create_dir_all(base);
        if let Err(error) = std::fs::write(
            &marker,
            handled.join(
                "
",
            ) + "
",
        ) {
            tracing::warn!("could not record retired extensions: {error}");
        }
    }
    retired
}

fn retire_one(base: &Path, id: &str) -> anyhow::Result<bool> {
    validate_extension_id(id)?;
    let dir = base.join(id);
    if !dir.is_dir() {
        return Ok(false);
    }
    let Ok(text) = std::fs::read_to_string(dir.join("extension.toml")) else {
        return Ok(false);
    };
    let official = Manifest::parse(&text)
        .is_ok_and(|manifest| manifest.id == id && manifest.author == "Atlas Team");
    if !official {
        return Ok(false);
    }
    let trash = base.join(".trash");
    std::fs::create_dir_all(&trash)?;
    let target = trash.join(format!("{id}_{}", uuid::Uuid::new_v4().simple()));
    std::fs::rename(&dir, &target).context("move retired extension to trash")?;
    Ok(true)
}

/// Write/update a manifest-only bundled extension on disk.
pub fn ensure_bundled(id: &str, manifest_toml: &str) -> anyhow::Result<()> {
    let dir = resolve_extension_dir(id)?;
    let manifest_path = dir.join("extension.toml");
    std::fs::create_dir_all(&dir)?;
    let current = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    if current != manifest_toml {
        std::fs::write(&manifest_path, manifest_toml)?;
    }
    Ok(())
}

/// Seed or update one official bundled asset when its packaged bytes change.
pub fn ensure_asset(id: &str, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = resolve_extension_dir(id)?;
    let path = dir.join(name);
    let current = std::fs::read(&path).unwrap_or_default();
    if current != bytes {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

/// Seed the official icon provider from the application package on first use.
/// This runs on the extension worker, preserving confined sidecar execution.
pub(crate) fn ensure_packaged_icon_provider(directory: &Path) -> anyhow::Result<()> {
    let filename = if cfg!(windows) {
        "atlas-icon-extension.exe"
    } else {
        "atlas-icon-extension"
    };
    let source = std::env::current_exe()?
        .parent()
        .context("application has no directory")?
        .join(filename);
    let target = directory.join(filename);
    if !source.is_file() && target.is_file() {
        return Ok(()); // Independently installed provider in a development build.
    }
    copy_packaged_program(&source, &target)
}

fn copy_packaged_program(source: &Path, target: &Path) -> anyhow::Result<()> {
    use sha2::{Digest as _, Sha256};
    use std::io::Write as _;
    let bytes = std::fs::read(source).context(
        "Packaged icon provider is missing; reinstall Atlas or the official icon extension",
    )?;
    if std::fs::read(target).is_ok_and(|current| Sha256::digest(&current) == Sha256::digest(&bytes))
    {
        return Ok(());
    }
    let directory = target
        .parent()
        .context("provider has no destination directory")?;
    std::fs::create_dir_all(directory)?;
    let mut staged = tempfile::NamedTempFile::new_in(directory)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    std::fs::set_permissions(staged.path(), std::fs::metadata(source)?.permissions())?;
    staged
        .persist(target)
        .map_err(|error| error.error)
        .context("Cannot update the icon provider; close other Atlas operations and retry")?;
    Ok(())
}

/// Seed a bundled extension and copy any bundled assets that are not already present.
pub fn ensure_bundled_with_assets(id: &str, manifest_toml: &str, src: &Path) -> anyhow::Result<()> {
    ensure_bundled(id, manifest_toml)?;
    let dir = resolve_extension_dir(id)?;
    if src.is_dir() {
        copy_dir_missing(src, &dir)?;
    }
    Ok(())
}

/// Recursively copy `src` into `dest`.
/// Upper bounds for an extension bundle, so a hostile zip cannot fill the disk.
const MAX_BUNDLE_ENTRIES: usize = 20_000;
const MAX_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;

/// Extract `archive` into `dest` one entry at a time. Unlike
/// `ZipArchive::extract`, this never creates links and never writes outside
/// `dest`: names that escape it (`..`, absolute paths, drive prefixes) and
/// symbolic-link entries are rejected, and the expanded size is capped.
fn extract_bundle<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    dest: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        archive.len() <= MAX_BUNDLE_ENTRIES,
        "extension bundle has too many files"
    );
    let mut written = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        anyhow::ensure!(!entry.is_symlink(), "extension bundle contains a link");
        // `enclosed_name` turns "/x" or "C:/x" into "x"; the stored name is
        // checked as well, so such entries are refused instead of relocated.
        let stored = entry.name()?.into_owned();
        let relative = entry
            .enclosed_name()
            .and(crate::fs::archive::sanitize::sanitize_inner_path(
                Path::new(&stored),
            ))
            .ok_or_else(|| anyhow!("extension bundle contains an unsafe path"))?;
        let target = dest.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let remaining = MAX_BUNDLE_BYTES.saturating_sub(written);
        let mut out = std::fs::File::create(&target)?;
        // Read one byte past the budget to detect a size field that lies.
        let copied = std::io::copy(
            &mut std::io::Read::take(&mut entry, remaining + 1),
            &mut out,
        )?;
        anyhow::ensure!(copied <= remaining, "extension bundle is too large");
        written += copied;
    }
    Ok(())
}

/// Copy a folder tree. Links and junctions are skipped, so an extension folder
/// cannot pull in files from elsewhere on the disk.
fn copy_dir(src: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            continue;
        }
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn copy_dir_missing(src: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir_missing(&from, &to)?;
        } else if !to.exists() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_extraction_refuses_paths_that_escape() {
        use std::io::Write as _;
        let build = |name: &str| {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"x").unwrap();
            writer.finish().unwrap().into_inner()
        };
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let mut good =
            zip::ZipArchive::new(std::io::Cursor::new(build("ext/extension.toml"))).unwrap();
        extract_bundle(&mut good, &dest).unwrap();
        assert!(dest.join("ext").join("extension.toml").is_file());
        for name in ["../evil.txt", "/absolute.txt", "C:/drive.txt"] {
            let mut bad = zip::ZipArchive::new(std::io::Cursor::new(build(name))).unwrap();
            assert!(
                extract_bundle(&mut bad, &dest).is_err(),
                "{name} was extracted"
            );
        }
        assert!(!root.path().join("evil.txt").exists());
    }

    #[test]
    fn packaged_provider_seeds_a_clean_profile_and_replaces_an_old_build() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("packaged.exe");
        let target = directory
            .path()
            .join("fresh-profile/extensions/icon-manager/provider.exe");
        std::fs::write(&source, b"new-build").unwrap();
        copy_packaged_program(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new-build");
        std::fs::write(&target, b"old-build-with-a-longer-payload").unwrap();
        copy_packaged_program(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new-build");
        copy_packaged_program(&source, &target).unwrap();
        assert_eq!(
            std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn retired_official_extensions_move_to_trash_and_the_rest_stay() {
        let base = tempfile::tempdir().unwrap();
        let write = |id: &str, author: &str, extra: &[(&str, &[u8])]| {
            let dir = base.path().join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("extension.toml"),
                format!(
                    "id = \"{id}\"
name = \"X\"
author = \"{author}\"
kind = \"declarative\"
"
                ),
            )
            .unwrap();
            for (name, bytes) in extra {
                std::fs::write(dir.join(name), bytes).unwrap();
            }
        };
        write(
            "file-tools",
            "Atlas Team",
            &[("plugin.wasm", b" asm"), ("icon.svg", b"<svg/>")],
        );
        write("document-previews", "Someone Else", &[]);
        write("media-thumbs", "Atlas Team", &[]);
        // Broken manifest: never touched, never an error.
        std::fs::create_dir_all(base.path().join("broken")).unwrap();
        std::fs::write(base.path().join("broken/extension.toml"), "id = [").unwrap();

        let ids = ["file-tools", "document-previews", "broken", "missing"];
        assert_eq!(retire_in(base.path(), &ids), ["file-tools"]);
        assert!(!base.path().join("file-tools").exists());
        assert!(
            base.path().join("document-previews").is_dir(),
            "third-party reuse of the id stays"
        );
        assert!(base.path().join("media-thumbs").is_dir());
        assert!(base.path().join("broken").is_dir());
        let trashed: Vec<_> = std::fs::read_dir(base.path().join(".trash"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(trashed.len(), 1);
        assert!(trashed[0].path().join("plugin.wasm").is_file());
        // Second boot: nothing left to do.
        assert!(retire_in(base.path(), &ids).is_empty());
        // A deliberate reinstall after the migration is respected.
        write("file-tools", "Atlas Team", &[]);
        assert!(retire_in(base.path(), &ids).is_empty());
        assert!(base.path().join("file-tools").is_dir());
    }

    #[test]
    fn test_resolve_extension_dir_rejects_path_traversal() {
        let dangerous_ids = [
            "../../../.ssh",
            "../etc/passwd",
            "/absolute/path",
            "nested/../../secret",
            "extension:name",
            "ext;rm -rf",
            "..",
            ".",
            "sub/dir",
            "sub\\dir",
        ];

        for id in &dangerous_ids {
            assert!(
                resolve_extension_dir(id).is_err(),
                "Extension ID '{id}' must be rejected for path traversal or invalid characters"
            );
            assert!(
                uninstall(id).is_err(),
                "Uninstalling with ID '{id}' must be rejected"
            );
        }

        let valid_ids = ["simple-ext", "my_extension_123", "theme-dark"];
        for id in &valid_ids {
            let res = resolve_extension_dir(id);
            assert!(res.is_ok(), "Valid ID '{id}' must be accepted");
            let path = res.unwrap();
            assert!(
                path.starts_with(crate::ext::extensions_dir()),
                "Path must reside within extensions_dir"
            );
        }
    }
}
