#![allow(dead_code)]
// reason: retained API surface not exercised by every GPUI flow yet
// reason: small JSON config blobs read at startup / written on settings change;
// one-shot and tiny, not the gpui render hot path the sync-fs ban targets.
#![allow(clippy::disallowed_methods)]
//! On-disk persistence. Plain JSON files under the platform config dir. Each blob
//! is its own file so a single corrupt section can't take the rest down (a failed
//! parse falls back to `Default`).

use atlas_model::pane::TabData;
use atlas_model::profiles::ProfileManager;
use atlas_model::settings::Settings;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::Write;
use std::path::PathBuf;

const APP_NAME: &str = "atlas";

pub fn config_dir() -> PathBuf {
    // A separate profile directory makes diagnostic/UI-lab runs reproducible
    // without overwriting the user's session or preferences.
    if let Some(path) = std::env::var_os("ATLAS_CONFIG_DIR").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    dirs::config_dir().map_or_else(|| PathBuf::from("."), |p| p.join(APP_NAME))
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("atlas")
}

/// Directory watched for user-supplied theme JSON files (gpui-component
/// `ThemeSet` format). Created on demand by the theme watcher.
pub fn themes_dir() -> PathBuf {
    config_dir().join("themes")
}

/// Save a custom ThemeSet atomically. The filename is a single JSON basename;
/// bundled presets are always preserved.
pub fn save_custom_theme(file_name: &str, theme: &impl Serialize) -> anyhow::Result<()> {
    save_custom_theme_in(&themes_dir(), file_name, theme)
}

fn save_custom_theme_in(
    directory: &std::path::Path,
    file_name: &str,
    theme: &impl Serialize,
) -> anyhow::Result<()> {
    let path = std::path::Path::new(file_name);
    if path.file_name().and_then(std::ffi::OsStr::to_str) != Some(file_name)
        || path.extension().and_then(std::ffi::OsStr::to_str) != Some("json")
        || file_name.eq_ignore_ascii_case("atlas-themes.json")
        || file_name.contains(['/', '\\'])
    {
        anyhow::bail!("invalid custom theme filename");
    }
    write_atomic(
        &directory.join(file_name),
        &serde_json::to_vec_pretty(theme)?,
    )?;
    Ok(())
}

/// Read a JSON blob, falling back to `Default` if it's missing or unparseable.
fn read_or_default<T: DeserializeOwned + Default>(file: &str) -> T {
    read_opt(file).unwrap_or_default()
}

/// Read an optional JSON blob — `None` if the file is missing or unparseable.
fn read_opt<T: DeserializeOwned>(file: &str) -> Option<T> {
    let path = config_dir().join(file);
    let bytes = std::fs::read(&path).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(e) => {
            tracing::warn!("failed to parse {}: {e}", path.display());
            None
        }
    }
}

fn write_json<T: Serialize>(file: &str, value: &T) {
    let dir = config_dir();
    match serde_json::to_vec_pretty(value) {
        Ok(json) => {
            if let Err(e) = write_atomic(&dir.join(file), &json) {
                tracing::warn!("failed to write {file}: {e}");
            }
        }
        Err(e) => tracing::warn!("failed to serialize {file}: {e}"),
    }
}

/// Serialize writers across processes and replace the destination only after all
/// bytes reach disk. A crash leaves the previous JSON intact, never half a blob.
pub(crate) fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(windows)]
    let _guard = atlas_win_space::persistence::ConfigWriteGuard::acquire()?;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing config directory"))?;
    std::fs::create_dir_all(dir)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".atlas-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    #[cfg(windows)]
    {
        // Even a delete-sharing reader can briefly race MoveFileEx/antivirus.
        // Retain the same complete temporary file and retry the atomic replace;
        // never delete/truncate the destination to make a rename succeed.
        let mut delay = std::time::Duration::from_millis(2);
        for attempt in 0..8 {
            match temporary.persist(path) {
                Ok(_) => break,
                Err(error)
                    if attempt < 7 && matches!(error.error.raw_os_error(), Some(5 | 32 | 33)) =>
                {
                    temporary = error.file;
                    std::thread::sleep(delay);
                    delay *= 2;
                }
                Err(error) => return Err(error.error),
            }
        }
    }
    #[cfg(not(windows))]
    temporary.persist(path).map_err(|error| error.error)?;
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

pub fn load_settings() -> Settings {
    read_or_default("settings.json")
}

pub fn save_settings(settings: &Settings) {
    write_json("settings.json", settings);
}

pub fn load_session() -> Option<atlas_model::layout_tree::Tree<TabData>> {
    let mut tree = read_opt("session.json")?;
    normalize_session_wsl_paths(&mut tree);
    Some(tree)
}

pub fn save_session(tree: &atlas_model::layout_tree::Tree<TabData>) {
    let mut tree = tree.clone();
    scrub_private_tree(&mut tree);
    let volumes = volume_snapshot();
    for (_, tile) in tree.tiles.iter_mut() {
        if let atlas_model::layout_tree::Tile::Pane(tab) = tile {
            tab.reconcile_volume(&volumes);
        }
    }
    write_json("session.json", &tree);
}

pub fn load_profiles() -> ProfileManager {
    let profiles = read_or_default("profiles.json");
    // Persist generated identities immediately, before any vault can reference them.
    save_profiles(&profiles);
    profiles
}

pub fn save_profiles(profiles: &ProfileManager) {
    let mut profiles = profiles.clone();
    use atlas_model::profiles::{FolderViewFeature, RecentFoldersFeature};
    for profile in &mut profiles.profiles {
        if let Some(tree) = &mut profile.tree {
            scrub_private_tree(tree);
        }
        profile
            .bookmarks
            .retain(|bookmark| !crate::vault::live::is_private(&bookmark.path));
        let mut recent = profile.scoped::<RecentFoldersFeature>();
        recent.retain(|path| !crate::vault::live::is_private(path));
        profile.set_scoped::<RecentFoldersFeature>(&recent);
        let mut views = profile.scoped::<FolderViewFeature>();
        views.retain_paths(|path| !crate::vault::live::is_private(path));
        profile.set_scoped::<FolderViewFeature>(&views);
    }
    refresh_bookmark_volumes(&mut profiles);
    write_json("profiles.json", &profiles);
}

fn scrub_private_tree(tree: &mut atlas_model::layout_tree::Tree<TabData>) {
    for (_, tile) in tree.tiles.iter_mut() {
        let atlas_model::layout_tree::Tile::Pane(tab) = tile else {
            continue;
        };
        if crate::vault::live::is_private(&tab.location.display_path()) {
            tab.location =
                atlas_model::location::Location::Directory(dirs::home_dir().unwrap_or_default());
            tab.volume_serial = None;
        }
        tab.history
            .retain(|location| !crate::vault::live::is_private(&location.display_path()));
        tab.history_forward
            .retain(|location| !crate::vault::live::is_private(&location.display_path()));
        if tab
            .breadcrumb_trail
            .as_ref()
            .is_some_and(|path| crate::vault::live::is_private(path))
        {
            tab.breadcrumb_trail = None;
        }
    }
}

static VOLUME_SNAPSHOT: std::sync::RwLock<Vec<(PathBuf, u32)>> = std::sync::RwLock::new(Vec::new());

pub fn set_volume_snapshot(volumes: &[(PathBuf, u32)]) {
    if let Ok(mut snapshot) = VOLUME_SNAPSHOT.write() {
        *snapshot = volumes.to_vec();
    }
}

fn volume_snapshot() -> Vec<(PathBuf, u32)> {
    VOLUME_SNAPSHOT
        .read()
        .map(|snapshot| snapshot.clone())
        .unwrap_or_default()
}

/// Apply the last background snapshot; saving never probes disconnected disks.
pub fn refresh_bookmark_volumes(profiles: &mut ProfileManager) -> bool {
    profiles.reconcile_bookmark_volumes(&volume_snapshot())
}

fn normalize_session_wsl_paths(tree: &mut atlas_model::layout_tree::Tree<TabData>) {
    for (_, tile) in tree.tiles.iter_mut() {
        let atlas_model::layout_tree::Tile::Pane(tab) = tile else {
            continue;
        };
        crate::fs::paths::normalize_wsl_location_path(&mut tab.location);
        for location in &mut tab.history {
            crate::fs::paths::normalize_wsl_location_path(location);
        }
        for location in &mut tab.history_forward {
            crate::fs::paths::normalize_wsl_location_path(location);
        }
    }
}

/// Per-extension enabled overrides (id → enabled).
pub fn load_ext_enabled() -> std::collections::HashMap<String, bool> {
    read_or_default("ext_enabled.json")
}

pub fn save_ext_enabled(enabled: &std::collections::HashMap<String, bool>) {
    write_json("ext_enabled.json", enabled);
}

/// Configured cloud remotes (non-secret definitions only; credentials live in
/// rclone's own config). Empty when the feature is off or none are set.
#[cfg(feature = "provider-host")]
pub fn load_cloud_remotes() -> Vec<crate::cloud::CloudRemote> {
    read_or_default("cloud_remotes.json")
}

#[cfg(feature = "provider-host")]
pub fn save_cloud_remotes(remotes: &[crate::cloud::CloudRemote]) {
    write_json("cloud_remotes.json", &remotes.to_vec());
}

#[cfg(test)]
mod tests {
    use super::write_atomic;

    #[test]
    fn custom_theme_save_roundtrips_and_preserves_bundled_presets() {
        let directory = tempfile::tempdir().unwrap();
        let bundled = directory.path().join("atlas-themes.json");
        std::fs::write(&bundled, b"bundled preset").unwrap();
        let theme = serde_json::json!({"name": "Custom", "themes": [{"name": "Custom", "colors": {"accent.background": "#aabbcc"}}]});
        super::save_custom_theme_in(directory.path(), "custom-1.json", &theme).unwrap();
        let restored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.path().join("custom-1.json")).unwrap())
                .unwrap();
        assert_eq!(restored, theme);
        for name in [
            "atlas-themes.json",
            "../outside.json",
            "..\\outside.json",
            "nested/theme.json",
            "custom.tmp",
        ] {
            assert!(super::save_custom_theme_in(directory.path(), name, &theme).is_err());
        }
        assert_eq!(std::fs::read(bundled).unwrap(), b"bundled preset");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn concurrent_config_writers_and_readers_always_observe_complete_json() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        write_atomic(&path, br#"{"writer":0,"payload":"initial"}"#).unwrap();
        std::thread::scope(|scope| {
            for writer in 0..4 {
                let path = &path;
                scope.spawn(move || {
                    for generation in 0..12 {
                        let bytes = serde_json::to_vec(&serde_json::json!({
                            "writer": writer, "generation": generation,
                            "payload": "x".repeat(8192)
                        }))
                        .unwrap();
                        write_atomic(path, &bytes).unwrap();
                    }
                });
            }
            for _ in 0..150 {
                let bytes = std::fs::read(&path).unwrap();
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert!(value.get("writer").is_some());
                assert!(value.get("payload").is_some());
            }
        });
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn failed_atomic_replace_preserves_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("sentinel"), b"preserved").unwrap();
        assert!(write_atomic(&path, b"new").is_err());
        assert_eq!(std::fs::read(path.join("sentinel")).unwrap(), b"preserved");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
