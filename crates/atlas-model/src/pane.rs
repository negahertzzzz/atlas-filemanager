use crate::file_entry::{SortColumn, SortDir};
use crate::location::Location;
use crate::settings::ViewMode;
use crate::tag_cache::TagId;
use std::path::PathBuf;

pub type PaneId = crate::layout_tree::TileId;
pub type TabId = crate::layout_tree::TileId;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum FolderOrder {
    #[default]
    FoldersFirst,
    FoldersLast,
    Mixed,
}

impl FolderOrder {
    pub const ALL: [Self; 3] = [Self::FoldersFirst, Self::FoldersLast, Self::Mixed];
    pub const fn next(self) -> Self {
        match self {
            Self::FoldersFirst => Self::FoldersLast,
            Self::FoldersLast => Self::Mixed,
            Self::Mixed => Self::FoldersFirst,
        }
    }
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::FoldersFirst => "folder_order_first",
            Self::FoldersLast => "folder_order_last",
            Self::Mixed => "folder_order_mixed",
        }
    }
    pub fn label(self) -> &'static str {
        self.label_key()
    }
}

impl From<bool> for FolderOrder {
    fn from(first: bool) -> Self {
        if first {
            Self::FoldersFirst
        } else {
            Self::Mixed
        }
    }
}

// Kept privately for compat deserialization of sessions saved before Location migration.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Deserialize)]
enum TabSource {
    #[default]
    Directory,
    Tag(TagId),
    #[cfg(feature = "provider-host")]
    Remote {
        id: String,
        path: String,
    },
    Archive {
        inner: PathBuf,
    },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TabData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_serial: Option<u32>,
    #[serde(skip)]
    pub breadcrumb_trail: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details_columns: Option<crate::profiles::FolderColumns>,
    #[serde(default)]
    pub group_by: crate::grouping::GroupBy,
    #[serde(default)]
    pub collapsed_groups: std::collections::BTreeSet<String>,
    pub location: Location,
    pub sort_col: SortColumn,
    pub sort_dir: SortDir,
    pub history: Vec<Location>,
    pub history_forward: Vec<Location>,
    #[serde(default)]
    pub view_mode: ViewMode,
    #[serde(default = "default_thumbnail_size")]
    pub thumbnail_size: f32,
    #[serde(default)]
    pub preview_enabled: bool,
    #[serde(default)]
    pub folder_order: FolderOrder,
    #[serde(default = "default_true")]
    pub show_files: bool,
    #[serde(default = "default_true")]
    pub show_folders: bool,
    #[serde(default)]
    pub is_branch_view: bool,
}

/// Compat deserializer: accepts both the new format (`location: Location`) and the
/// old format (`path + source + history: Vec<PathBuf>`).
#[derive(serde::Deserialize)]
struct TabDataRaw {
    #[serde(default)]
    volume_serial: Option<u32>,
    #[serde(default)]
    details_columns: Option<crate::profiles::FolderColumns>,
    #[serde(default)]
    group_by: crate::grouping::GroupBy,
    #[serde(default)]
    collapsed_groups: std::collections::BTreeSet<String>,
    location: Option<Location>,
    path: Option<PathBuf>,
    #[serde(default)]
    source: TabSource,
    #[serde(default, deserialize_with = "deser_history")]
    history: Vec<Location>,
    #[serde(default, deserialize_with = "deser_history")]
    history_forward: Vec<Location>,
    #[serde(default)]
    sort_col: SortColumn,
    #[serde(default)]
    sort_dir: SortDir,
    #[serde(default)]
    view_mode: ViewMode,
    #[serde(default = "default_thumbnail_size")]
    thumbnail_size: f32,
    #[serde(default)]
    preview_enabled: bool,
    #[serde(default = "default_true")]
    folders_first: bool,
    #[serde(default)]
    folder_order: Option<FolderOrder>,
    #[serde(default = "default_true")]
    show_files: bool,
    #[serde(default = "default_true")]
    show_folders: bool,
    #[serde(default)]
    is_branch_view: bool,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum LocationOrPath {
    Location(Location),
    Path(PathBuf),
}

fn deser_history<'de, D>(d: D) -> Result<Vec<Location>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    Vec::<LocationOrPath>::deserialize(d).map(|v| {
        v.into_iter()
            .map(|item| match item {
                LocationOrPath::Location(l) => l,
                LocationOrPath::Path(p) => Location::Directory(p),
            })
            .collect()
    })
}

impl From<TabDataRaw> for TabData {
    fn from(r: TabDataRaw) -> Self {
        let location = match r.location {
            Some(loc) => loc,
            None => {
                let path = r.path.unwrap_or_default();
                match r.source {
                    TabSource::Directory => Location::Directory(path),
                    TabSource::Archive { inner } => Location::Archive {
                        archive: path,
                        inner,
                    },
                    TabSource::Tag(id) => Location::Tag(id),
                    #[cfg(feature = "provider-host")]
                    TabSource::Remote { id, path: rpath } => Location::Remote { id, path: rpath },
                }
            }
        };
        // Sanitize corrupted states where both are false
        let (show_files, show_folders) = if !r.show_files && !r.show_folders {
            (true, true)
        } else {
            (r.show_files, r.show_folders)
        };
        Self {
            volume_serial: r.volume_serial,
            breadcrumb_trail: None,
            details_columns: r.details_columns,
            group_by: r.group_by,
            collapsed_groups: r.collapsed_groups,
            location,
            sort_col: r.sort_col,
            sort_dir: r.sort_dir,
            history: r.history,
            history_forward: r.history_forward,
            view_mode: r.view_mode,
            thumbnail_size: r.thumbnail_size,
            preview_enabled: r.preview_enabled,
            folder_order: r.folder_order.unwrap_or_else(|| r.folders_first.into()),
            show_files,
            show_folders,
            is_branch_view: r.is_branch_view,
        }
    }
}

impl<'de> serde::Deserialize<'de> for TabData {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        TabDataRaw::deserialize(d).map(Into::into)
    }
}

impl TabData {
    pub fn volume_path(&self) -> Option<&std::path::Path> {
        match &self.location {
            Location::Directory(path) | Location::Archive { archive: path, .. } => Some(path),
            _ => None,
        }
    }

    pub fn with_view(
        path: PathBuf,
        view_mode: ViewMode,
        thumbnail_size: f32,
        preview_enabled: bool,
    ) -> Self {
        Self {
            volume_serial: None,
            breadcrumb_trail: None,
            details_columns: None,
            group_by: crate::grouping::GroupBy::None,
            collapsed_groups: std::collections::BTreeSet::new(),
            location: Location::Directory(path),
            sort_col: SortColumn::default(),
            sort_dir: SortDir::default(),
            history: Vec::new(),
            history_forward: Vec::new(),
            view_mode,
            thumbnail_size,
            preview_enabled,
            folder_order: FolderOrder::default(),
            show_files: true,
            show_folders: true,
            is_branch_view: false,
        }
    }

    /// Pure snapshot reconciliation; the platform queries volumes off-thread.
    /// Archive tabs track the physical archive rather than its virtual members.
    pub fn reconcile_volume(&mut self, volumes: &[(PathBuf, u32)]) -> bool {
        let path = match &mut self.location {
            Location::Directory(path) | Location::Archive { archive: path, .. } => path,
            _ => {
                self.volume_serial = None;
                return false;
            }
        };
        let mut identity = crate::profiles::Bookmark::new(String::new(), path.clone());
        identity.volume_serial = self.volume_serial;
        let changed = identity.reconcile_volume(volumes);
        self.volume_serial = identity.volume_serial;
        *path = identity.path;
        changed
    }
}

const fn default_thumbnail_size() -> f32 {
    128.0
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_order_migrates_legacy_booleans_and_roundtrips_all_three_states() {
        for (old, expected) in [
            (true, FolderOrder::FoldersFirst),
            (false, FolderOrder::Mixed),
        ] {
            let tab: TabData =
                serde_json::from_str(&format!(r#"{{"path":"/files","folders_first":{old}}}"#))
                    .unwrap();
            assert_eq!(tab.folder_order, expected);
        }
        for order in FolderOrder::ALL {
            let mut tab = TabData::with_view("/files".into(), ViewMode::Details, 128.0, false);
            tab.folder_order = order;
            let json = serde_json::to_value(&tab).unwrap();
            assert!(json.get("folders_first").is_none());
            assert_eq!(
                serde_json::from_value::<TabData>(json)
                    .unwrap()
                    .folder_order,
                order
            );
        }
        assert_eq!(FolderOrder::FoldersFirst.next(), FolderOrder::FoldersLast);
        assert_eq!(FolderOrder::FoldersLast.next(), FolderOrder::Mixed);
        assert_eq!(FolderOrder::Mixed.next(), FolderOrder::FoldersFirst);
    }

    #[test]
    fn persists_view_per_tab() {
        let mut tab = TabData::with_view(PathBuf::from("/tmp"), ViewMode::Grid, 192.0, true);
        tab.group_by = crate::grouping::GroupBy::Size;
        tab.collapsed_groups.insert("size:huge".into());
        let json = serde_json::to_string(&tab).unwrap();
        let restored: TabData = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.view_mode, ViewMode::Grid);
        assert!((restored.thumbnail_size - 192.0).abs() < f32::EPSILON);
        assert!(restored.preview_enabled);
        assert_eq!(restored.group_by, crate::grouping::GroupBy::Size);
        assert_eq!(restored.collapsed_groups, tab.collapsed_groups);
    }

    #[test]
    fn restores_tabs_saved_before_preview_was_per_tab() {
        // Old format: path + source (default Directory) without preview_enabled.
        let json = r#"{
            "path":"/tmp",
            "sort_col":"Name",
            "sort_dir":"Asc",
            "history":[],
            "history_forward":[],
            "view_mode":"Details",
            "thumbnail_size":128.0
        }"#;
        let restored: TabData = serde_json::from_str(json).unwrap();
        assert!(!restored.preview_enabled);
        assert_eq!(restored.group_by, crate::grouping::GroupBy::None);
        assert!(restored.collapsed_groups.is_empty());
        assert_eq!(restored.folder_order, FolderOrder::FoldersFirst);
        assert!(restored.show_files);
        assert!(restored.show_folders);
        assert_eq!(
            restored.location,
            Location::Directory(PathBuf::from("/tmp"))
        );
    }

    #[test]
    fn deserializes_new_location_format() {
        let json = r#"{
            "location": {"Directory": "/home/user/docs"},
            "sort_col": "Name",
            "sort_dir": "Asc",
            "history": [{"Directory": "/home/user"}],
            "history_forward": [],
            "view_mode": "Details",
            "thumbnail_size": 128.0,
            "preview_enabled": false
        }"#;
        let td: TabData = serde_json::from_str(json).unwrap();
        assert_eq!(
            td.location,
            Location::Directory(PathBuf::from("/home/user/docs"))
        );
        assert_eq!(td.history.len(), 1);
    }

    #[test]
    fn old_history_vec_pathbuf_upgrades_to_location() {
        let json = r#"{
            "path": "/home/user/docs",
            "source": "Directory",
            "sort_col": "Name",
            "sort_dir": "Asc",
            "history": ["/home/user", "/home"],
            "history_forward": [],
            "view_mode": "Details",
            "thumbnail_size": 128.0,
            "preview_enabled": false
        }"#;
        let td: TabData = serde_json::from_str(json).unwrap();
        assert_eq!(
            td.location,
            Location::Directory(PathBuf::from("/home/user/docs"))
        );
        assert_eq!(td.history.len(), 2);
        assert!(matches!(td.history[0], Location::Directory(_)));
        assert!(!td.is_branch_view);
    }

    #[test]
    fn branch_view_serializes_and_deserializes() {
        let mut tab = TabData::with_view(
            PathBuf::from("/home/user/docs"),
            ViewMode::Details,
            128.0,
            false,
        );
        tab.is_branch_view = true;
        let json = serde_json::to_string(&tab).unwrap();
        assert!(json.contains("\"is_branch_view\":true"));
        let restored: TabData = serde_json::from_str(&json).unwrap();
        assert!(restored.is_branch_view);
    }
}
