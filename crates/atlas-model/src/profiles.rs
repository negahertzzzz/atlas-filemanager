#![allow(dead_code)] // reason: retained API surface not exercised by every GPUI flow yet
use crate::pane::TabData;
use std::collections::HashMap;
use std::path::PathBuf;

/// Namespaced, per-profile preference store. The *official* way for features and
/// extensions to persist data scoped to a profile without growing [`Profile`]
/// with one bespoke field per feature.
///
/// Key = stable namespace id (`"icons"`, `"ext:<id>"`, …). Value = arbitrary JSON.
/// Reach for the typed [`ProfilePrefs::get`] / [`ProfilePrefs::set`] helpers, or
/// the [`ProfileScoped`] trait when a feature owns a fixed key + state type.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ProfilePrefs(HashMap<String, serde_json::Value>);

impl ProfilePrefs {
    /// Deserialize the value stored under `key` as `T`. `None` when absent or when
    /// the stored JSON no longer matches `T` (e.g. after a feature schema change),
    /// so callers can fall back to a default without panicking.
    pub fn get<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        serde_json::from_value(self.0.get(key)?.clone()).ok()
    }

    /// Store `val` under `key`, replacing any previous value. A serialization
    /// failure is dropped rather than propagated — prefs are best-effort and must
    /// never take down a save.
    pub fn set<T: serde::Serialize>(&mut self, key: &str, val: &T) {
        if let Ok(v) = serde_json::to_value(val) {
            self.0.insert(key.to_string(), v);
        }
    }

    pub fn remove(&mut self, key: &str) {
        self.0.remove(key);
    }

    pub fn contains(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A feature (or extension) that owns a fixed [`ProfilePrefs`] namespace and a
/// serializable state type. Implement on a zero-sized marker to get ergonomic,
/// typed per-profile persistence via [`Profile::scoped`] / [`Profile::set_scoped`].
///
/// This is the contract the "icon selection" and per-extension variable stores
/// build on, and the recommended entry point for any new persisted-per-profile
/// feature.
pub trait ProfileScoped {
    /// Stable namespace key in [`ProfilePrefs`]. Must be unique across features.
    const KEY: &'static str;
    /// Serializable state persisted under [`Self::KEY`].
    type State: serde::Serialize + serde::de::DeserializeOwned + Default;
}

/// Persisted preferences for the preview / inspector panel, scoped per profile.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PreviewPrefs {
    /// Show line numbers in the code editor. Defaults to `false`.
    #[serde(default)]
    pub show_line_numbers: bool,
    /// Open files in edit mode (editable) by default. Defaults to `false`.
    #[serde(default)]
    pub edit_mode: bool,
}

impl Default for PreviewPrefs {
    fn default() -> Self {
        Self {
            show_line_numbers: false,
            edit_mode: false,
        }
    }
}

/// Marker for [`ProfileScoped`] — stores [`PreviewPrefs`] under key `"preview"`.
pub struct PreviewFeature;

impl ProfileScoped for PreviewFeature {
    const KEY: &'static str = "preview";
    type State = PreviewPrefs;
}

/// View/sort remembered for a single directory (see [`FolderViewPrefs`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FolderViewPref {
    pub view_mode: crate::settings::ViewMode,
    pub sort_col: crate::file_entry::SortColumn,
    pub sort_dir: crate::file_entry::SortDir,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<FolderColumns>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FolderColumns {
    pub ratios: [f32; 4],
    pub order: [String; 3],
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct FolderViewRecord {
    location: Bookmark,
    pref: FolderViewPref,
    #[serde(default)]
    available: bool,
}

/// LRU-capped `path -> FolderViewPref` map, most-recently-used first. Lets each
/// folder remember its own view mode / sort without growing [`Profile`] or
/// [`ProfilePrefs`] unbounded as the user browses.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FolderViewPrefs {
    entries: Vec<(PathBuf, FolderViewPref)>,
    #[serde(default)]
    records: Vec<FolderViewRecord>,
}

impl FolderViewPrefs {
    const CAP: usize = 200;

    pub fn remove_subtree(&mut self, root: &std::path::Path) {
        self.entries.retain(|(path, _)| !path.starts_with(root));
        self.records
            .retain(|record| !record.location.path.starts_with(root));
    }

    pub fn retain_paths(&mut self, mut keep: impl FnMut(&std::path::Path) -> bool) {
        self.entries.retain(|(path, _)| keep(path));
        self.records.retain(|record| keep(&record.location.path));
    }

    pub fn get_exact(&self, path: &std::path::Path) -> Option<FolderViewPref> {
        self.entries
            .iter()
            .map(|(p, v)| (p, v))
            .chain(
                self.records
                    .iter()
                    .filter(|r| r.available)
                    .map(|r| (&r.location.path, &r.pref)),
            )
            .find(|(p, _)| *p == path)
            .map(|(_, pref)| pref.clone())
    }

    pub fn get(&self, path: &std::path::Path) -> Option<FolderViewPref> {
        let candidates = self.entries.iter().map(|(p, v)| (p, v)).chain(
            self.records
                .iter()
                .filter(|record| record.available)
                .map(|r| (&r.location.path, &r.pref)),
        );
        candidates
            .clone()
            .find(|(p, _)| *p == path)
            .or_else(|| {
                candidates
                    .filter(|(p, pref)| pref.recursive && path.starts_with(p))
                    .max_by_key(|(p, _)| p.components().count())
            })
            .map(|(_, pref)| pref.clone())
    }

    /// Insert/update `path`, bumping it to most-recently-used and evicting the
    /// oldest entry past [`Self::CAP`].
    pub fn set(&mut self, path: PathBuf, pref: FolderViewPref) {
        if let Some(record) = self
            .records
            .iter_mut()
            .find(|r| r.available && r.location.path == path)
        {
            record.pref = pref;
            return;
        }
        self.entries.retain(|(p, _)| *p != path);
        self.entries.insert(0, (path, pref));
        self.entries.truncate(Self::CAP);
    }

    /// Migrate old path keys, bind to a partition serial and repair drive-letter
    /// changes. Disconnected volumes keep their settings without applying them
    /// to a different disk that happens to reuse the same letter.
    pub fn reconcile_volumes(&mut self, volumes: &[(PathBuf, u32)]) -> bool {
        let before = self.clone();
        for record in &mut self.records {
            record.location.reconcile_volume(volumes);
            record.available = volumes.iter().any(|(root, serial)| {
                record.location.volume_serial == Some(*serial)
                    && same_drive(root, &record.location.path)
            });
        }
        let mut unbound = Vec::new();
        for (path, pref) in self.entries.drain(..) {
            let mut location = Bookmark::new(String::new(), path.clone());
            location.reconcile_volume(volumes);
            if location.volume_serial.is_none() {
                unbound.push((path, pref));
                continue;
            }
            self.records.retain(|r| {
                r.location.volume_serial != location.volume_serial
                    || r.location.path != location.path
            });
            self.records.insert(
                0,
                FolderViewRecord {
                    location,
                    pref,
                    available: true,
                },
            );
        }
        self.entries = unbound;
        self.records.truncate(Self::CAP);
        *self != before
    }
}

fn same_drive(a: &std::path::Path, b: &std::path::Path) -> bool {
    let (a, b) = (a.to_string_lossy(), b.to_string_lossy());
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() >= 3 && b.len() >= 3 && a[1] == b':' && b[1] == b':' && a[0].eq_ignore_ascii_case(&b[0])
}

/// Marker for [`ProfileScoped`] — stores [`FolderViewPrefs`] under key `"folder_view"`.
pub struct FolderViewFeature;

impl ProfileScoped for FolderViewFeature {
    const KEY: &'static str = "folder_view";
    type State = FolderViewPrefs;
}

/// Marker for [`ProfileScoped`] — stores recent folders for one profile.
pub struct RecentFoldersFeature;

impl ProfileScoped for RecentFoldersFeature {
    const KEY: &'static str = "recent_folders";
    type State = Vec<PathBuf>;
}

/// Name of the theme applied to new profiles. Must match a theme registered in
/// `ThemeRegistry` (see `crates/atlas-ui/assets/themes/atlas-themes.json`).
pub const DEFAULT_THEME: &str = "Atlas Light Glass";

/// Dark counterpart of [`DEFAULT_THEME`], used by light/dark switching until
/// the profile has picked a dark theme of its own.
pub const DEFAULT_DARK_THEME: &str = "Atlas Graphite Glass";

fn default_theme() -> String {
    DEFAULT_THEME.to_string()
}

/// Light/dark pairing for a profile: the light and the dark theme it uses,
/// and whether Atlas follows the system's light/dark appearance. Choosing any
/// theme remembers it for its own appearance, so switching back and forth
/// always lands on the user's last pick.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ThemeAppearancePrefs {
    #[serde(default)]
    pub follow_system: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<String>,
}

impl ThemeAppearancePrefs {
    /// The theme to show for a light (`dark == false`) or dark appearance.
    pub fn theme_for(&self, dark: bool) -> String {
        let (chosen, fallback) = if dark {
            (&self.dark, DEFAULT_DARK_THEME)
        } else {
            (&self.light, DEFAULT_THEME)
        };
        chosen.clone().unwrap_or_else(|| fallback.to_string())
    }

    /// Remember `name` as the theme for its appearance. Returns whether the
    /// stored pairing changed (and so needs saving).
    pub fn remember(&mut self, name: &str, dark: bool) -> bool {
        let slot = if dark {
            &mut self.dark
        } else {
            &mut self.light
        };
        if slot.as_deref() == Some(name) {
            return false;
        }
        *slot = Some(name.to_string());
        true
    }
}

/// Marker for [`ProfileScoped`] — stores [`ThemeAppearancePrefs`].
pub struct ThemeAppearanceFeature;

impl ProfileScoped for ThemeAppearanceFeature {
    const KEY: &'static str = "theme_appearance";
    type State = ThemeAppearancePrefs;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SectionKind {
    Places,
    Bookmarks,
    Tags,
    Linux,
    Drives,
    Recent,
    /// Cloud / network remotes (feature `cloud`). Renders nothing without it.
    #[serde(other)]
    Cloud,
}

impl SectionKind {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Places => "Places",
            Self::Bookmarks => "Bookmarks",
            Self::Tags => "Tags",
            Self::Linux => "Linux",
            Self::Drives => "Drives",
            Self::Recent => "Recent",
            Self::Cloud => "Network",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SidebarSection {
    pub kind: SectionKind,
    pub collapsed: bool,
    #[serde(default)]
    pub hidden: bool,
}

impl SidebarSection {
    pub const fn new(kind: SectionKind) -> Self {
        Self {
            kind,
            collapsed: false,
            hidden: false,
        }
    }
}

pub fn default_sections() -> Vec<SidebarSection> {
    vec![
        SidebarSection::new(SectionKind::Places),
        SidebarSection::new(SectionKind::Bookmarks),
        SidebarSection::new(SectionKind::Tags),
        #[cfg(windows)]
        SidebarSection::new(SectionKind::Linux),
        SidebarSection::new(SectionKind::Drives),
        #[cfg(feature = "provider-host")]
        SidebarSection::new(SectionKind::Cloud),
        SidebarSection::new(SectionKind::Recent),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub path: PathBuf,
    /// Stable identity of the Windows partition; absent in legacy/non-Windows saves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_serial: Option<u32>,
}

impl Bookmark {
    pub const fn new(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            volume_serial: None,
        }
    }

    /// Reconcile against a complete snapshot of mounted drive-letter volumes.
    /// Never attach a missing bookmark to a different partition reusing its letter.
    pub fn reconcile_volume(&mut self, volumes: &[(PathBuf, u32)]) -> bool {
        let original = self.path.to_string_lossy();
        let bytes = original.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || !matches!(bytes[2], b'\\' | b'/')
        {
            return false;
        }
        let current = volumes.iter().find(|(root, _)| {
            root.to_string_lossy()
                .as_bytes()
                .first()
                .is_some_and(|letter| letter.eq_ignore_ascii_case(&bytes[0]))
        });
        let Some(serial) = self.volume_serial else {
            self.volume_serial = current.map(|(_, serial)| *serial);
            return self.volume_serial.is_some();
        };
        if current.is_some_and(|(_, mounted)| *mounted == serial) {
            return false;
        }
        let mut matches = volumes.iter().filter(|(_, mounted)| *mounted == serial);
        let Some((root, _)) = matches.next() else {
            return false;
        };
        // Cloned partitions can share a serial. Preserve the bookmark until unique.
        if matches.next().is_some() {
            return false;
        }
        let Some(letter) = root.to_string_lossy().chars().next() else {
            return false;
        };
        #[cfg(windows)]
        {
            use std::os::windows::ffi::{OsStrExt, OsStringExt};
            let mut units: Vec<_> = self.path.as_os_str().encode_wide().collect();
            units[0] = letter as u16;
            self.path = PathBuf::from(std::ffi::OsString::from_wide(&units));
        }
        #[cfg(not(windows))]
        {
            self.path = PathBuf::from(format!("{letter}{}", &original[1..]));
        }
        true
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Tag {
    pub name: String,
    /// RGB color
    pub color: [u8; 3],
    /// If true this tag lives in `global_tags` rather than a profile
    pub global: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    /// Stable identity for private-folder grants; names and vector indices can change.
    #[serde(default = "new_profile_id")]
    pub id: String,
    pub name: String,
    pub bookmarks: Vec<Bookmark>,
    pub tags: Vec<Tag>,
    /// Saved pane/tab tree for this profile. None = fresh session on first switch.
    #[serde(default)]
    pub tree: Option<crate::layout_tree::Tree<TabData>>,
    /// Sidebar section order and collapsed state, per profile.
    #[serde(default = "default_sections")]
    pub sidebar_sections: Vec<SidebarSection>,
    /// Visual theme for this profile, by registered theme name. Legacy enum
    /// values (e.g. "Dark") deserialize as plain strings and are normalized to a
    /// registered theme at apply time, falling back to [`DEFAULT_THEME`].
    #[serde(default = "default_theme")]
    pub theme: String,
    /// When true, the goto bar searches inside file *contents* (grep) instead of
    /// fuzzy-matching folder names. Persisted per profile.
    #[serde(default)]
    pub search_in_content: bool,
    /// Command-palette favorites: stable command ids pinned to the "Quick"
    /// section. Scoped per profile.
    #[serde(default)]
    pub pinned_commands: Vec<String>,
    /// Per-profile view/layout knobs (sizes, view mode, panel visibility, zoom).
    /// Swapped in/out of the live [`crate::settings::Settings`] on profile switch.
    #[serde(default)]
    pub workspace: crate::settings::Workspace,
    /// Namespaced store for feature/extension state scoped to this profile. The
    /// official extension point — see [`ProfilePrefs`] / [`ProfileScoped`].
    #[serde(default)]
    pub prefs: ProfilePrefs,
}

impl Profile {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: new_profile_id(),
            name: name.into(),
            bookmarks: Vec::new(),
            tags: Vec::new(),
            tree: None,
            sidebar_sections: default_sections(),
            theme: default_theme(),
            search_in_content: false,
            pinned_commands: Vec::new(),
            workspace: crate::settings::Workspace::default(),
            prefs: ProfilePrefs::default(),
        }
    }

    /// Read this profile's persisted state for feature `S`, defaulting when unset
    /// or unparseable.
    pub fn scoped<S: ProfileScoped>(&self) -> S::State {
        self.prefs.get::<S::State>(S::KEY).unwrap_or_default()
    }

    /// Persist this profile's state for feature `S` under its namespace.
    pub fn set_scoped<S: ProfileScoped>(&mut self, state: &S::State) {
        self.prefs.set(S::KEY, state);
    }
}

fn new_profile_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod private_identity_tests {
    use super::*;
    #[test]
    fn identity_survives_rename_and_serde_and_legacy_profiles_get_distinct_ids() {
        let mut profile = Profile::new("Personal");
        let id = profile.id.clone();
        profile.name = "Renamed".into();
        let serialized = serde_json::to_value(&profile).unwrap();
        let restored: Profile = serde_json::from_value(serialized.clone()).unwrap();
        assert_eq!(restored.id, id);
        let mut legacy = serialized;
        legacy.as_object_mut().unwrap().remove("id");
        let first: Profile = serde_json::from_value(legacy.clone()).unwrap();
        let second: Profile = serde_json::from_value(legacy).unwrap();
        assert_ne!(first.id, second.id);
        assert_ne!(first.id, id);
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProfileManager {
    pub profiles: Vec<Profile>,
    pub active_idx: usize,
    pub global_tags: Vec<Tag>,
}

impl Default for ProfileManager {
    fn default() -> Self {
        Self {
            profiles: vec![Profile::new("Default")],
            active_idx: 0,
            global_tags: Vec::new(),
        }
    }
}

impl ProfileManager {
    /// Update every profile, including inactive profiles, after device changes.
    pub fn reconcile_bookmark_volumes(&mut self, volumes: &[(PathBuf, u32)]) -> bool {
        self.reconcile_volume_paths(volumes).changed
    }

    /// Include saved tabs and recent folders, retaining identities while a disk
    /// is disconnected. Return real path changes separately from first binding.
    pub fn reconcile_volume_paths(&mut self, volumes: &[(PathBuf, u32)]) -> VolumeReconciliation {
        let mut result = VolumeReconciliation::default();
        for profile in &mut self.profiles {
            for bookmark in &mut profile.bookmarks {
                let old = bookmark.path.clone();
                result.changed |= bookmark.reconcile_volume(volumes);
                if old != bookmark.path {
                    result.moved.push((old, bookmark.path.clone()));
                }
            }
            if let Some(tree) = &mut profile.tree {
                for (_, tile) in tree.tiles.iter_mut() {
                    if let crate::layout_tree::Tile::Pane(tab) = tile {
                        let old = tab.volume_path().map(std::path::Path::to_path_buf);
                        result.changed |= tab.reconcile_volume(volumes);
                        if let (Some(old), Some(new)) = (old, tab.volume_path()) {
                            if old != new {
                                result.moved.push((old, new.to_path_buf()));
                            }
                        }
                    }
                }
            }
            let mut recent = profile.scoped::<RecentFoldersFeature>();
            let saved: HashMap<PathBuf, u32> = profile
                .prefs
                .get("recent_folder_volumes")
                .unwrap_or_default();
            let mut identities = HashMap::new();
            for path in &mut recent {
                let old = path.clone();
                let mut identity = Bookmark::new(String::new(), old.clone());
                identity.volume_serial = saved.get(&old).copied();
                result.changed |= identity.reconcile_volume(volumes);
                *path = identity.path;
                if let Some(serial) = identity.volume_serial {
                    identities.insert(path.clone(), serial);
                }
                if old != *path {
                    result.moved.push((old, path.clone()));
                }
            }
            profile.set_scoped::<RecentFoldersFeature>(&recent);
            profile.prefs.set("recent_folder_volumes", &identities);
            let mut views = profile.scoped::<FolderViewFeature>();
            result.changed |= views.reconcile_volumes(volumes);
            profile.set_scoped::<FolderViewFeature>(&views);
        }
        result
    }

    pub fn active(&self) -> &Profile {
        &self.profiles[self.active_idx]
    }

    pub fn active_mut(&mut self) -> &mut Profile {
        &mut self.profiles[self.active_idx]
    }

    /// Ensure every profile exposes the Network/Cloud sidebar section. Profiles
    /// persisted before the cloud feature existed lack it, which would otherwise
    /// hide all configured remotes. Inserts it just before "Recent".
    #[cfg(feature = "provider-host")]
    pub fn ensure_cloud_section(&mut self) {
        for p in &mut self.profiles {
            if p.sidebar_sections
                .iter()
                .any(|s| s.kind == SectionKind::Cloud)
            {
                continue;
            }
            let pos = p
                .sidebar_sections
                .iter()
                .position(|s| s.kind == SectionKind::Recent)
                .unwrap_or(p.sidebar_sections.len());
            p.sidebar_sections
                .insert(pos, SidebarSection::new(SectionKind::Cloud));
        }
    }

    /// Profiles saved before the WSL/Linux section existed lack it.
    #[cfg(windows)]
    pub fn ensure_linux_section(&mut self) {
        for p in &mut self.profiles {
            if p.sidebar_sections
                .iter()
                .any(|s| s.kind == SectionKind::Linux)
            {
                continue;
            }
            let pos = p
                .sidebar_sections
                .iter()
                .position(|s| s.kind == SectionKind::Drives)
                .unwrap_or(p.sidebar_sections.len());
            p.sidebar_sections
                .insert(pos, SidebarSection::new(SectionKind::Linux));
        }
    }
}

#[derive(Default)]
pub struct VolumeReconciliation {
    pub changed: bool,
    pub moved: Vec<(PathBuf, PathBuf)>,
}

/// Transient UI state for the sidebar profile/bookmark/tag forms.
/// Not persisted.
#[derive(Default)]
pub struct ProfileUiState {
    pub adding_bookmark: bool,
    pub new_bookmark_name: String,
    pub adding_tag: bool,
    pub creating_tag: bool,
    pub editing_tag: Option<crate::tag_cache::TagId>,
    pub new_tag_global: bool,
    pub new_tag_name: String,
    pub new_tag_color: [u8; 3],
    /// Targets captured when creating from a file's tag picker; empty for sidebar creation.
    pub new_tag_paths: Vec<PathBuf>,
    pub confirming_delete_profile: Option<usize>,
    pub atlas_launcher_profiles_open: bool,
}

pub const PRESET_COLORS: &[[u8; 3]] = &[
    [239, 68, 68],   // red
    [249, 115, 22],  // orange
    [234, 179, 8],   // yellow
    [34, 197, 94],   // green
    [59, 130, 246],  // blue
    [168, 85, 247],  // purple
    [236, 72, 153],  // pink
    [148, 163, 184], // slate
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_tabs_and_recents_follow_their_volume_after_disconnect_and_letter_reuse() {
        use crate::{layout_tree::Tree, pane::TabData, settings::ViewMode};
        let mut profiles = ProfileManager::default();
        profiles.profiles.push(Profile::new("Inactive"));
        let profile = &mut profiles.profiles[1];
        let mut tree = Tree::empty();
        let tab = tree.tiles.insert_pane(TabData::with_view(
            PathBuf::from(r"E:\Music"),
            ViewMode::List,
            128.0,
            false,
        ));
        tree.root = Some(tree.tiles.insert_tab_tile(vec![tab]));
        profile.tree = Some(tree);
        profile.set_scoped::<RecentFoldersFeature>(&vec![PathBuf::from(r"E:\Music\日本語")]);
        assert!(
            profiles
                .reconcile_volume_paths(&[(PathBuf::from(r"E:\"), 42)])
                .changed
        );
        let json = serde_json::to_string(&profiles).unwrap();
        let mut profiles: ProfileManager = serde_json::from_str(&json).unwrap();
        // Another disk reuses E: while the original USB is disconnected.
        let disconnected = profiles.reconcile_volume_paths(&[(PathBuf::from(r"E:\"), 99)]);
        assert!(disconnected.moved.is_empty());
        let changed = profiles
            .reconcile_volume_paths(&[(PathBuf::from(r"E:\"), 99), (PathBuf::from(r"F:\"), 42)]);
        assert_eq!(changed.moved.len(), 2);
        let profile = &profiles.profiles[1];
        assert_eq!(
            profile.scoped::<RecentFoldersFeature>(),
            [PathBuf::from(r"F:\Music\日本語")]
        );
        let restored = profile.tree.as_ref().unwrap().tiles.get_pane(&tab).unwrap();
        assert_eq!(
            restored.location,
            crate::location::Location::Directory(PathBuf::from(r"F:\Music"))
        );
        assert_eq!(restored.volume_serial, Some(42));
        // Capture survives a second roundtrip and can follow another letter.
        let mut profiles: ProfileManager =
            serde_json::from_str(&serde_json::to_string(&profiles).unwrap()).unwrap();
        assert_eq!(
            profiles
                .reconcile_volume_paths(&[(PathBuf::from(r"G:\"), 42)])
                .moved
                .len(),
            2
        );
    }

    fn folder_pref(mode: crate::settings::ViewMode, recursive: bool) -> FolderViewPref {
        FolderViewPref {
            view_mode: mode,
            sort_col: crate::file_entry::SortColumn::Name,
            sort_dir: crate::file_entry::SortDir::Asc,
            recursive,
            columns: None,
        }
    }

    #[test]
    fn folder_views_follow_the_partition_and_do_not_leak_to_a_reused_letter() {
        let mut prefs = FolderViewPrefs::default();
        prefs.set(
            PathBuf::from(r"E:\photos"),
            folder_pref(crate::settings::ViewMode::Grid, true),
        );
        prefs.reconcile_volumes(&[(PathBuf::from(r"E:\"), 42)]);
        let json = serde_json::to_string(&prefs).unwrap();
        let mut prefs: FolderViewPrefs = serde_json::from_str(&json).unwrap();
        prefs.reconcile_volumes(&[(PathBuf::from(r"E:\"), 99)]);
        assert!(prefs.get(std::path::Path::new(r"E:\photos")).is_none());
        prefs.set(
            PathBuf::from(r"E:\photos"),
            folder_pref(crate::settings::ViewMode::List, false),
        );
        prefs.reconcile_volumes(&[(PathBuf::from(r"E:\"), 99), (PathBuf::from(r"H:\"), 42)]);
        assert_eq!(
            prefs
                .get(std::path::Path::new(r"E:\photos"))
                .unwrap()
                .view_mode,
            crate::settings::ViewMode::List
        );
        assert_eq!(
            prefs
                .get(std::path::Path::new(r"H:\photos"))
                .unwrap()
                .view_mode,
            crate::settings::ViewMode::Grid
        );
    }

    #[test]
    fn closest_recursive_folder_wins_and_exact_children_override_it() {
        let mut prefs = FolderViewPrefs::default();
        prefs.set(
            PathBuf::from("/a"),
            folder_pref(crate::settings::ViewMode::Grid, true),
        );
        prefs.set(
            PathBuf::from("/a/b"),
            folder_pref(crate::settings::ViewMode::List, true),
        );
        assert_eq!(
            prefs.get(std::path::Path::new("/a/b/c")).unwrap().view_mode,
            crate::settings::ViewMode::List
        );
        prefs.set(
            PathBuf::from("/a/b/c"),
            folder_pref(crate::settings::ViewMode::Details, false),
        );
        assert_eq!(
            prefs.get(std::path::Path::new("/a/b/c")).unwrap().view_mode,
            crate::settings::ViewMode::Details
        );
        assert!(prefs.get(std::path::Path::new("/ab")).is_none());
        assert!(prefs.get_exact(std::path::Path::new("/a/b/c/d")).is_none());
        assert_eq!(
            prefs
                .get(std::path::Path::new("/a/b/c/d"))
                .unwrap()
                .view_mode,
            crate::settings::ViewMode::List
        );
    }

    #[test]
    fn bookmark_captures_serial_and_follows_volume_to_a_new_letter() {
        let mut bookmark = Bookmark::new("USB".into(), PathBuf::from(r"E:\photos\旅行"));
        assert!(bookmark.reconcile_volume(&[(PathBuf::from(r"E:\"), 123)]));
        let json = serde_json::to_string(&bookmark).unwrap();
        let mut restored: Bookmark = serde_json::from_str(&json).unwrap();
        assert!(
            restored.reconcile_volume(&[(PathBuf::from(r"E:\"), 99), (PathBuf::from(r"F:\"), 123)])
        );
        assert_eq!(restored.path, PathBuf::from(r"F:\photos\旅行"));
        assert_eq!(restored.volume_serial, Some(123));
        assert!(!restored.reconcile_volume(&[(PathBuf::from(r"F:\"), 123)]));
    }

    #[test]
    fn disconnected_or_ambiguous_volumes_never_retarget_bookmarks() {
        let mut bookmark = Bookmark::new("USB".into(), PathBuf::from(r"E:\photos"));
        bookmark.volume_serial = Some(123);
        assert!(!bookmark.reconcile_volume(&[(PathBuf::from(r"E:\"), 99)]));
        assert!(!bookmark
            .reconcile_volume(&[(PathBuf::from(r"F:\"), 123), (PathBuf::from(r"G:\"), 123)]));
        assert_eq!(bookmark.path, PathBuf::from(r"E:\photos"));
    }

    #[test]
    fn legacy_bookmarks_and_network_paths_remain_compatible() {
        let mut bookmark: Bookmark =
            serde_json::from_str(r#"{"name":"Old","path":"E:\\stuff"}"#).unwrap();
        assert_eq!(bookmark.volume_serial, None);
        assert!(bookmark.reconcile_volume(&[(PathBuf::from(r"E:\"), 123)]));
        let mut network = Bookmark::new("Network".into(), PathBuf::from(r"\\server\share"));
        assert!(!network.reconcile_volume(&[(PathBuf::from(r"E:\"), 123)]));
    }

    #[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
    struct IconSel {
        theme_id: Option<String>,
    }

    struct IconFeature;
    impl ProfileScoped for IconFeature {
        const KEY: &'static str = "icons";
        type State = IconSel;
    }

    #[test]
    fn prefs_round_trip_typed() {
        let mut prefs = ProfilePrefs::default();
        assert!(prefs.is_empty());
        let val = IconSel {
            theme_id: Some("flow-icons".into()),
        };
        prefs.set("icons", &val);
        assert!(prefs.contains("icons"));
        assert_eq!(prefs.get::<IconSel>("icons"), Some(val));
    }

    #[test]
    fn prefs_get_absent_is_none() {
        let prefs = ProfilePrefs::default();
        assert_eq!(prefs.get::<IconSel>("icons"), None);
    }

    #[test]
    fn prefs_schema_mismatch_is_none_not_panic() {
        let mut prefs = ProfilePrefs::default();
        prefs.set("icons", &"a bare string, not IconSel");
        assert_eq!(prefs.get::<IconSel>("icons"), None);
    }

    #[test]
    fn scoped_helpers_default_then_persist() {
        let mut profile = Profile::new("Test");
        // Unset → default.
        assert_eq!(profile.scoped::<IconFeature>(), IconSel::default());
        profile.set_scoped::<IconFeature>(&IconSel {
            theme_id: Some("material".into()),
        });
        assert_eq!(
            profile.scoped::<IconFeature>().theme_id.as_deref(),
            Some("material")
        );
    }

    #[test]
    fn theme_appearance_pairs_light_and_dark_per_profile() {
        let mut profile = Profile::new("Theme");
        let prefs = profile.scoped::<ThemeAppearanceFeature>();
        assert!(!prefs.follow_system);
        assert_eq!(prefs.theme_for(false), DEFAULT_THEME);
        assert_eq!(prefs.theme_for(true), DEFAULT_DARK_THEME);

        let mut prefs = prefs;
        assert!(prefs.remember("Atlas Nord", true));
        assert!(
            !prefs.remember("Atlas Nord", true),
            "unchanged picks need no save"
        );
        assert!(prefs.remember("Atlas Rosé", false));
        prefs.follow_system = true;
        profile.set_scoped::<ThemeAppearanceFeature>(&prefs);

        let json = serde_json::to_string(&profile).unwrap();
        let restored: Profile = serde_json::from_str(&json).unwrap();
        let restored = restored.scoped::<ThemeAppearanceFeature>();
        assert_eq!(restored, prefs);
        assert_eq!(restored.theme_for(true), "Atlas Nord");
        assert_eq!(restored.theme_for(false), "Atlas Rosé");
    }

    #[test]
    fn profile_prefs_survive_serde() {
        let mut profile = Profile::new("Persisted");
        profile.set_scoped::<IconFeature>(&IconSel {
            theme_id: Some("flow".into()),
        });
        let json = serde_json::to_string(&profile).unwrap();
        let back: Profile = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.scoped::<IconFeature>().theme_id.as_deref(),
            Some("flow")
        );
    }

    #[test]
    fn legacy_profile_without_prefs_deserializes() {
        // A profile JSON predating the `prefs` field must still load (serde default).
        let legacy = r#"{"name":"Old","bookmarks":[],"tags":[]}"#;
        let p: Profile = serde_json::from_str(legacy).unwrap();
        assert_eq!(p.name, "Old");
        assert!(p.prefs.is_empty());
    }
}
