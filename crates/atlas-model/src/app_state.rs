#![allow(dead_code)] // reason: retained API surface not exercised by every GPUI flow yet
use crate::layout_tree::{Container, Tile, TileId, Tree};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::file_entry::FileEntry;
use crate::pane::{TabData, TabId};
use crate::preview_cache::PreviewResult;
use crate::profiles::{ProfileManager, ProfileUiState};
use crate::settings::Settings;
use crate::tab::{LoadState, TabState};

/// A metadata-ready directory delta, produced off the UI thread.
#[derive(Debug, Clone)]
pub enum FsChange {
    Upsert(FileEntry),
    Remove(PathBuf),
    Rename { from: PathBuf, entry: FileEntry },
}

/// How a native SMB request (credential prompt, map, disconnect) ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkOutcome {
    /// Windows accepted the credentials for the share.
    Connected,
    /// A drive letter (`Z:`) now points at the share.
    Mapped(String),
    /// The drive or connection was removed.
    Disconnected(String),
    /// The user closed the Windows dialog.
    Cancelled,
    /// Win32 error code (when known) and its system message.
    Failed { code: Option<i32>, message: String },
}

pub enum BgResult {
    /// A native SMB request finished. `tab` is reloaded (or navigated to
    /// `navigate`) on success; `target` is the share or drive it concerned.
    Network {
        tab: Option<TabId>,
        target: String,
        navigate: Option<PathBuf>,
        outcome: NetworkOutcome,
    },
    IconKeysReady(Vec<String>),
    DirTypeLabels { tab: TabId, op_id: u64, labels: Vec<String> },
    /// Payload stays in the isolated conversion session; this wakes the UI drain.
    ConversionWake,
    /// Private-folder payload stays in the session-only UI channel.
    VaultWake,
    WatcherDisabled(PathBuf),
    VolumeSnapshot(Vec<(PathBuf, u32)>),
    PreviewDelayElapsed { tab: TabId, request: crate::preview_cache::PreviewRequest, generation: u64 },
    DirListTiming { tab: crate::pane::TabId, op_id: u64, elapsed: std::time::Duration },
    /// Created/accessed stamps of a listing, for grouping by those dates.
    GroupTimes { tab: TabId, version: u64, times: Vec<crate::grouping::EntryTimes> },
    DirListExclusions { tab: TabId, op_id: u64, has_excluded_items: bool },
    DirListPreview {
        tab: TabId,
        op_id: u64,
        entries: Vec<FileEntry>,
    },
    DirListed {
        tab: TabId,
        op_id: u64,
        entries: Vec<FileEntry>,
    },
    DirListError {
        tab: TabId,
        op_id: u64,
        error: String,
    },
    MillerListed {
        tab: TabId,
        location: crate::location::Location,
        op_id: u64,
        entries: Vec<FileEntry>,
        has_excluded_items: bool,
    },
    TreeListed {
        tab: TabId,
        path: PathBuf,
        entries: Vec<FileEntry>,
    },
    PreviewMetadataReady {
        tab: TabId,
        path: PathBuf,
        properties: Vec<(String, String)>,
    },
    PreviewReady {
        tab: TabId,
        path: PathBuf,
        result: PreviewResult,
    },
    MultiPreviewThumbnailReady {
        tab: TabId,
        key: crate::preview_cache::MultiPreviewKey,
        path: PathBuf,
        rgba: Vec<u8>,
        width: usize,
        height: usize,
    },
    /// A downscaled grid thumbnail finished decoding off-thread. `rgba` empty == the
    /// decode failed (cached as a permanent miss so it isn't re-requested).
    /// Windows Shell file-type icon rendered at several tiers (32/64/128/256 px,
    /// each `(rgba, w, h)`). Empty == the Shell had nothing for this key.
    ShellIconReady {
        key: PathBuf,
        tiers: Vec<(Vec<u8>, usize, usize)>,
    },
    GridThumbnailReady {
        path: PathBuf,
        rgba: Vec<u8>,
        width: usize,
        height: usize,
    },
    /// Decode was skipped because the path left the visible viewport before the
    /// blocking task ran. Remove from thumb_textures so the cell can re-request.
    GridThumbnailSkipped {
        path: PathBuf,
    },

    /// Virtual listing of an archive directory completed.
    ArchiveListed {
        tab: TabId,
        op_id: u64,
        /// Archive file on disk.
        archive: PathBuf,
        /// Directory within the archive that was listed (empty = root).
        inner: PathBuf,
        entries: Vec<FileEntry>,
    },
    ArchiveListError {
        tab: TabId,
        op_id: u64,
        error: String,
    },
    SearchInPlaceResults {
        tab: TabId,
        gen: u64,
        entries: Vec<FileEntry>,
    },
    SearchInPlaceError {
        tab: TabId,
        gen: u64,
        error: String,
    },
    /// Fuzzy-filtering a large already-listed folder runs off the UI thread.
    /// `gen` drops results from a query the user has already extended/replaced.
    FolderFilterResults {
        tab: TabId,
        gen: u64,
        query: String,
        visible: Vec<usize>,
        counts: crate::tab::VisibleCounts,
    },
    FsEvent(std::path::PathBuf),
    /// An ordered batch; `FsEvent` is reserved for overflow/recovery rescans.
    FsChanges { directory: PathBuf, changes: Vec<FsChange> },
    OpProgress {
        id: u64,
        done: u64,
        total: u64,
        bytes_done: u64,
        bytes_total: u64,
        current_file: PathBuf,
    },
    OpComplete {
        id: u64,
        /// Per-item failures; a user cancel is one `OpErrorKind::Cancelled` entry.
        result: Result<(), Vec<crate::ops::OpError>>,
        affected_tabs: Vec<TabId>,
    },
    /// Only paths whose delete/trash operation actually succeeded.
    PathsDeleted(Vec<PathBuf>),
    /// Successfully restored destinations; failures arrive separately via OpComplete.
    PathsRestored(Vec<PathBuf>),
    RecycleBinStatus(Option<bool>),
    /// A paste target wrote `Performed DropEffect` onto our clipboard data
    /// object (e.g. Explorer pasting an Atlas cut). `effect` is the raw
    /// `DROPEFFECT` value: 0 = optimized move (target moved the files itself),
    /// 1 = copy, 2 = move (we must delete the sources).
    ClipboardPerformed {
        effect: u32,
        paths: Vec<PathBuf>,
    },
    /// A unified transfer (copy/move via paste, drag-and-drop, or menu) finished —
    /// possibly partially. The report carries per-item errors, skips, the real
    /// `src→dst` pairs for undo, and whether it was cancelled.
    TransferComplete {
        id: u64,
        kind: crate::ops::TransferKind,
        origin: crate::ops::TransferOrigin,
        report: crate::ops::TransferReport,
        affected_tabs: Vec<TabId>,
    },
    /// Full tag-definition list pushed by the tag-db worker after any CRUD.
    TagDefinitions(Vec<crate::tag_cache::TagDef>),
    TagAssignmentComplete {
        tag: crate::tag_cache::TagId,
        paths: Vec<PathBuf>,
        result: Result<(), String>,
    },
    /// Cached tag associations for a set of queried paths.
    TagsForPaths {
        tags: HashMap<PathBuf, Vec<crate::tag_cache::TagId>>,
    },
    /// Paths carrying a given tag, used to populate a virtual tag-results tab.
    PathsForTag {
        tag: crate::tag_cache::TagId,
        paths: Vec<PathBuf>,
    },
    /// A removable device finished mounting at this path (navigate to it).
    DeviceMounted(PathBuf),
    /// A mount/unmount via udisksctl failed.
    DeviceActionFailed(String),
    /// The set of removable devices changed (e.g. after unmount); refresh sidebar.
    DevicesChanged,
    /// A batch of files whose contents matched the goto bar's content search. `gen`
    /// tags the search generation so stale batches (from an older query) are dropped.
    ContentResults {
        gen: u64,
        paths: Vec<PathBuf>,
    },
    GotoServiceResults {
        gen: u64,
        query: String,
        results: Vec<(PathBuf, bool)>,
        complete: bool,
    },
    GotoServiceFailed {
        gen: u64,
        query: String,
    },
    ExtCommandDone {
        ext_id: String,
        error: Option<String>,
        dirs: Vec<PathBuf>,
    },
    /// Remote marketplace registry fetched from R2.
    ExtRegistryFetched(Result<crate::ext_model::RemoteRegistry, String>),
    /// A remote extension finished downloading and installing.
    ExtRemoteInstalled {
        id: String,
        result: Result<(), String>,
    },
    /// A local extension (zip/folder) finished installing on the background thread.
    ExtLocalInstalled(Result<String, String>),
    /// A runtime tool finished installing; retry the preview that needed it.
    ExtToolInstalled {
        tool: String,
        tab: TabId,
        path: PathBuf,
        result: Result<(), String>,
    },
    AppUpdateAvailable {
        version: String,
        url: String,
    },
    /// License activation/deactivation finished; carries the message to show.
    LicenseChanged(Result<String, String>),
    /// A user-requested crash report upload finished; carries the message to show.
    CrashReportSent(Result<String, String>),
    /// A WASM extension requested the fuzzy panel. The background worker blocks on `reply_tx`.
    ExtShowQuickPick {
        items: Vec<String>,
        reply_tx: std::sync::mpsc::Sender<String>,
    },
    // --- cloud extension ---
    /// rclone binary download progress while fetching it on first use.
    #[cfg(feature = "provider-host")]
    CloudRcloneProgress {
        done: u64,
        total: u64,
    },
    /// rclone daemon is up and the provider is ready.
    #[cfg(feature = "provider-host")]
    CloudReady,
    #[cfg(feature = "provider-host")]
    CloudProviderStopped { uninstall: bool, result: Result<(), String> },
    /// A non-fatal cloud error to surface in the UI.
    #[cfg(feature = "provider-host")]
    CloudError(String),
    /// WinFsp download progress `(bytes_done, bytes_total)`.
    #[cfg(all(feature = "provider-host", windows))]
    WinFspProgress {
        done: u64,
        total: u64,
    },
    /// WinFsp installed successfully; retry the pending mount.
    #[cfg(all(feature = "provider-host", windows))]
    WinFspInstalled {
        /// Remote to mount now that WinFsp is available (if any).
        retry_id: Option<crate::cloud_model::RemoteId>,
    },
    /// A remote's backend config was created successfully.
    #[cfg(feature = "provider-host")]
    RemoteConfigured(crate::cloud_model::CloudRemote),
    #[cfg(feature = "provider-host")]
    RemoteConfigFailed {
        id: crate::cloud_model::RemoteId,
        error: String,
    },
    #[cfg(feature = "provider-host")]
    RemoteRemoved(crate::cloud_model::RemoteId),
    /// A remote's runtime (mount/connection) status changed.
    #[cfg(feature = "provider-host")]
    RemoteStatusChanged {
        id: crate::cloud_model::RemoteId,
        status: crate::cloud_model::RemoteStatus,
    },
    /// Full transfer-list snapshot from the Transfer Manager.
    #[cfg(feature = "provider-host")]
    TransferUpdate(crate::cloud_model::TransferSnapshot),
    /// Storage usage `(used, total)` bytes reported for a remote.
    #[cfg(feature = "provider-host")]
    RemoteQuota {
        id: crate::cloud_model::RemoteId,
        used: u64,
        total: u64,
    },
    /// A remote directory's contents changed (e.g. after an upload); re-list any
    /// Access-mode tab showing `(id, path)`.
    #[cfg(feature = "provider-host")]
    RemoteChanged {
        id: crate::cloud_model::RemoteId,
        paths: Vec<String>,
    },
}

/// A recently closed tab retains every persisted option and its close time.
#[derive(Clone)]
pub struct ClosedTab {
    pub data: TabData,
    pub closed_at: std::time::SystemTime,
}

pub struct AppState {
    pub pane_tree: Tree<TabData>,
    /// Active Pane leaf `TileId` (the individual tab being viewed)
    pub focused_tile: TileId,
    pub closed_tabs: std::collections::VecDeque<ClosedTab>,
    /// Global shell cut paths, including files cut in Explorer.
    pub system_cut_paths: std::collections::HashSet<PathBuf>,
    /// Runtime per-tab state, keyed by Pane leaf `TileId`
    pub tab_states: HashMap<TileId, TabState>,
    pub settings: Settings,
    pub ops: crate::ops::OperationManager,
    pub overlay: crate::overlay::OverlayState,
    pub dragging_pane: Option<TileId>,
    pub drag_pending: bool,
    // Phase 5 — inspector preview
    pub preview: crate::preview_cache::PreviewState,
    pub batch_rename: Option<crate::batch_rename::BatchRenameState>,
    pub profiles: ProfileManager,
    pub profile_ui: ProfileUiState,
    pub sidebar_drop_zone: Vec<StagedDrop>,
    pub tag_cache: crate::tag_cache::TagCache,
    pub ext: crate::ext_state::ExtState,
    #[cfg(feature = "provider-host")]
    pub cloud: crate::cloud_state::CloudState,
}

#[derive(Clone)]
pub struct StagedDrop {
    pub path: PathBuf,
    pub from_self: bool,
}

impl AppState {
    pub fn remember_closed_tab(&mut self, data: TabData) {
        if self.closed_tabs.len() == 32 {
            self.closed_tabs.pop_front();
        }
        self.closed_tabs.push_back(ClosedTab {
            data,
            closed_at: std::time::SystemTime::now(),
        });
    }

    pub fn container_contains_pane(&self, container_id: TileId, pane_id: TileId) -> bool {
        if container_id == pane_id {
            return true;
        }
        match self.pane_tree.tiles.get(container_id) {
            Some(Tile::Container(Container::Tabs(tabs))) => tabs.children.contains(&pane_id),
            Some(Tile::Container(Container::Linear(linear))) => linear
                .children
                .iter()
                .any(|c| self.container_contains_pane(*c, pane_id)),
            _ => false,
        }
    }

    pub fn new(settings: Settings) -> (Self, crate::pane::PaneId) {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let tab_data = TabData::with_view(
            home,
            settings.view_mode.clone(),
            settings.thumbnail_size,
            settings.show_inspector,
        );

        let mut tree: Tree<TabData> = Tree::empty();
        let pane_tile_id = tree.tiles.insert_pane(tab_data);
        let tabs_id = tree.tiles.insert_tab_tile(vec![pane_tile_id]);
        tree.root = Some(tabs_id);

        let mut tab_states = HashMap::new();
        tab_states.insert(pane_tile_id, TabState::default());

        let state = Self::build(tree, pane_tile_id, tab_states, settings);
        (state, pane_tile_id)
    }

    pub fn active_tab_state(&self) -> Option<&TabState> {
        self.tab_states.get(&self.focused_tile)
    }

    pub fn active_preview_request(&self) -> Option<crate::preview_cache::PreviewRequest> {
        use crate::preview_cache::{MultiPreviewKey, PreviewRequest, MULTI_PREVIEW_LIMIT};

        let tab = self.focused_tile;
        let (view, folder) = match self.pane_tree.tiles.get(tab) {
            Some(Tile::Pane(p)) => (p.view_mode.clone(), p.location.fs_dir().map(Path::to_path_buf)),
            _ => return None,
        };
        let ts = self.tab_states.get(&tab)?;
        let selected = ts.selected_entries(&view);
        #[cfg(feature = "provider-host")]
        if let Some(Tile::Pane(pane)) = self.pane_tree.tiles.get(tab) {
            if let crate::location::Location::Remote { id, .. } = &pane.location {
                return if let [entry] = selected.as_slice() {
                    Some(PreviewRequest::Single(crate::cloud_model::preview_key(id, ts.last_nav_op_id, &entry.path())))
                } else { None };
            }
        }
        if let [entry] = selected.as_slice() {
            return Some(entry_to_preview_request(entry));
        }
        if selected.is_empty() {
            // Nothing selected: preview the folder being browsed (Finder-style).
            return folder.map(PreviewRequest::Single);
        }

        let mut hasher = DefaultHasher::new();
        let mut paths = Vec::with_capacity(MULTI_PREVIEW_LIMIT);
        let selected_count = selected.len();
        for entry in selected {
            entry.path().hash(&mut hasher);
            let is_thumbable = entry
                .extension()
                .as_deref()
                .is_some_and(|ext| {
                    crate::preview_cache::is_image_ext(ext)
                        || crate::preview_cache::is_audio_ext(ext)
                        || crate::preview_cache::is_model_ext(ext)
                        || crate::preview_cache::is_video_ext(ext)
                });
            if paths.len() < MULTI_PREVIEW_LIMIT && is_thumbable {
                paths.push(entry.path().clone());
            }
        }

        Some(PreviewRequest::Multiple {
            key: MultiPreviewKey {
                fingerprint: hasher.finish(),
                selected_count,
            },
            paths,
        })
    }

    pub fn active_tab_state_mut(&mut self) -> Option<&mut TabState> {
        self.tab_states.get_mut(&self.focused_tile)
    }

    /// Paths of the currently selected entries in the focused tab (handles Miller).
    /// Mirrors what the inspector's `SelectionSummary` displays.
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        let Some(ts) = self.active_tab_state() else {
            return Vec::new();
        };
        let mode = self.active_tab_data().map(|tab| tab.view_mode.clone()).unwrap_or_default();
        ts.selected_entries(&mode).into_iter().map(FileEntry::path).collect()
    }

    /// Like `selected_paths()` but returns `(archive, inner)` pairs for archive
    /// tabs, so callers can implement drag-out virtual (extract on drag).
    /// Returns `None` when the specified tab is not in archive mode.
    pub fn selected_archive_items_for_pane(&self, pane: TileId) -> Option<Vec<(PathBuf, PathBuf)>> {
        let td = match self.pane_tree.tiles.get(pane) {
            Some(Tile::Pane(p)) => Some(p),
            _ => None,
        }?;
        let (archive, _) = td.location.as_archive()?;
        let archive = archive.to_path_buf();
        let Some(ts) = self.tab_states.get(&pane) else {
            return Some(Vec::new());
        };
        let mut selected: Vec<usize> = ts.selection.selected.iter().copied().collect();
        selected.sort_unstable();
        let items = selected
            .into_iter()
            .filter_map(|i| ts.entries.get(i).map(|e| (archive.clone(), e.path().clone())))
            .collect();
        Some(items)
    }

    /// Like `selected_paths()` but returns `(archive, inner)` pairs for archive
    /// tabs, so callers can implement drag-out virtual (extract on drag).
    /// Returns `None` when the focused tab is not in archive mode.
    pub fn selected_archive_items(&self) -> Option<Vec<(PathBuf, PathBuf)>> {
        self.selected_archive_items_for_pane(self.focused_tile)
    }

    pub fn active_tab_data(&self) -> Option<&TabData> {
        match self.pane_tree.tiles.get(self.focused_tile) {
            Some(Tile::Pane(p)) => Some(p),
            _ => None,
        }
    }

    pub fn active_tab_data_mut(&mut self) -> Option<&mut TabData> {
        match self.pane_tree.tiles.get_mut(self.focused_tile) {
            Some(Tile::Pane(p)) => Some(p),
            _ => None,
        }
    }

    pub fn for_each_tab_data_mut(&mut self, mut f: impl FnMut(&mut TabData)) {
        for (_, tile) in self.pane_tree.tiles.iter_mut() {
            if let Tile::Pane(pane) = tile {
                f(pane);
            }
        }
    }

    pub fn any_loading(&self) -> bool {
        self.tab_states
            .values()
            .any(|s| s.load_state == LoadState::Loading)
    }

    /// Find the Tabs container `TileId` that contains the given pane tile.
    pub fn find_tabs_container(&self, tile_id: TileId) -> Option<TileId> {
        for (id, tile) in self.pane_tree.tiles.iter() {
            if let Tile::Container(Container::Tabs(tabs)) = tile {
                if tabs.children.contains(&tile_id) {
                    return Some(*id);
                }
            }
        }
        None
    }

    /// Find any Tabs container in the tree (fallback when `focused_tile` is stale).
    pub fn find_any_tabs_container(&self) -> Option<TileId> {
        self.pane_tree.tiles.iter().find_map(|(id, tile)| {
            if matches!(tile, Tile::Container(Container::Tabs(_))) {
                Some(*id)
            } else {
                None
            }
        })
    }

    /// Find the direct parent container of a tile.
    pub fn find_parent_of(&self, tile_id: TileId) -> Option<TileId> {
        for (id, tile) in self.pane_tree.tiles.iter() {
            let children = match tile {
                Tile::Container(Container::Linear(l)) => l.children.as_slice(),
                Tile::Container(Container::Tabs(t)) => t.children.as_slice(),
                _ => continue,
            };
            if children.contains(&tile_id) {
                return Some(*id);
            }
        }
        None
    }

    /// Restore from a persisted tree. Returns (state, `first_pane_to_load`).
    pub fn from_tree(mut tree: Tree<TabData>, settings: Settings) -> (Self, TileId) {
        tree.sanitize();

        // Collect all Pane leaf IDs to initialize tab_states
        let mut tab_states = HashMap::new();
        let mut first_tile = None;

        for (id, tile) in tree.tiles.iter() {
            if matches!(tile, Tile::Pane(_)) {
                tab_states.insert(*id, TabState::default());
                if first_tile.is_none() {
                    first_tile = Some(*id);
                }
            }
        }

        let focused = first_tile.unwrap_or(TileId(0));
        tree.make_active(|id, _| id == focused);

        let state = Self::build(tree, focused, tab_states, settings);
        (state, focused)
    }

    fn build(
        pane_tree: Tree<TabData>,
        focused_tile: TileId,
        tab_states: HashMap<TileId, TabState>,
        settings: Settings,
    ) -> Self {
        Self {
            pane_tree,
            focused_tile,
            closed_tabs: std::collections::VecDeque::with_capacity(32),
            system_cut_paths: std::collections::HashSet::new(),
            tab_states,
            settings,
            ops: crate::ops::OperationManager::default(),
            overlay: crate::overlay::OverlayState::default(),
            dragging_pane: None,
            preview: crate::preview_cache::PreviewState::default(),
            batch_rename: None,
            profiles: ProfileManager::default(),
            profile_ui: ProfileUiState::default(),
            sidebar_drop_zone: Vec::new(),
            drag_pending: false,
            tag_cache: crate::tag_cache::TagCache::default(),
            ext: crate::ext_state::ExtState::default(),
            #[cfg(feature = "provider-host")]
            cloud: crate::cloud_state::CloudState::default(),
        }
    }

    /// Remove all preview data for a tab.
    pub fn clear_preview(&mut self, tab: TileId) {
        self.preview.clear(tab);
    }

    /// All Pane leaf `TileIds` in the tree (for session restore / refresh all).
    pub fn all_pane_tile_ids(&self) -> Vec<TileId> {
        self.pane_tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| {
                if matches!(tile, Tile::Pane(_)) {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn next_op_id(&self) -> u64 {
        self.ops.next_op_id()
    }
}

fn entry_to_preview_request(
    entry: &crate::file_entry::FileEntry,
) -> crate::preview_cache::PreviewRequest {
    use crate::preview_cache::PreviewRequest;
    if let Some(archive) = entry.archive_root().clone() {
        PreviewRequest::ArchiveSingle {
            archive,
            inner: entry.path().clone(),
        }
    } else {
        PreviewRequest::Single(entry.path().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::AppState;
    use crate::file_entry::{FileEntry, FileKind};
    use crate::preview_cache::{PreviewRequest, MULTI_PREVIEW_LIMIT};
    use crate::settings::Settings;
    use std::path::PathBuf;

    #[test]
    fn recently_closed_tabs_are_bounded_and_keep_full_data_in_lifo_order() {
        let (mut state, _) = AppState::new(Settings::default());
        for index in 0..40 {
            let mut tab = state.active_tab_data().unwrap().clone();
            tab.location = crate::location::Location::Directory(PathBuf::from(format!("folder-{index}")));
            tab.history.push(crate::location::Location::Directory(PathBuf::from("previous")));
            tab.view_mode = crate::settings::ViewMode::Grid;
            tab.thumbnail_size = 224.0;
            state.remember_closed_tab(tab);
        }
        assert_eq!(state.closed_tabs.len(), 32);
        assert_eq!(state.closed_tabs.front().unwrap().data.location,
            crate::location::Location::Directory(PathBuf::from("folder-8")));
        let newest = state.closed_tabs.pop_back().unwrap();
        assert_eq!(newest.data.location, crate::location::Location::Directory(PathBuf::from("folder-39")));
        assert_eq!(newest.data.history.len(), 1);
        assert_eq!(newest.data.view_mode, crate::settings::ViewMode::Grid);
        assert_eq!(newest.data.thumbnail_size, 224.0);
        assert!(newest.closed_at.elapsed().unwrap().as_secs() < 10);
    }

    #[test]
    fn grid_and_miller_multi_selection_produce_complete_drag_paths() {
        use crate::layout_tree::Tile;
        use crate::location::Location;
        use crate::settings::ViewMode;
        use crate::tab::MillerColumn;

        for mode in [ViewMode::Grid, ViewMode::Miller] {
            let (mut state, tile) = AppState::new(Settings::default());
            if let Some(Tile::Pane(pane)) = state.pane_tree.tiles.get_mut(tile) {
                pane.view_mode = mode.clone();
            }
            let tab = state.tab_states.get_mut(&tile).unwrap();
            let entries = vec![
                entry("a.txt"),
                entry("b.txt"),
                entry("c.txt"),
                entry("d.txt"),
            ];
            let selection = if mode == ViewMode::Miller {
                // A different flat selection must not leak into the column drag.
                tab.set_entries(vec![entry("other.txt")].into());
                tab.selection.selected.insert(0);
                let mut column = MillerColumn::new(Location::Directory(PathBuf::from("files")));
                column.entries = entries.into();
                tab.miller.columns.push(column);
                &mut tab.miller.columns[0].selection
            } else {
                tab.set_entries(entries.into());
                &mut tab.selection
            };
            selection.select(0, &[0, 1, 2, 3], false, false);
            selection.select(2, &[0, 1, 2, 3], true, false);
            selection.select(3, &[0, 1, 2, 3], false, true);
            assert_eq!(
                state.selected_paths(),
                ["a.txt", "b.txt", "c.txt", "d.txt"].map(PathBuf::from)
            );
        }
    }

    fn entry(name: &str) -> FileEntry {
        let path = PathBuf::from(name);
        FileEntry::from(crate::file_entry::FileEntryData {
            name: name.into(),
            path,
            kind: FileKind::File,
            size: Some(1),
            modified: None,
            _is_hidden: false,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        })
    }

    #[test]
    fn multi_preview_limits_images_in_visible_order() {
        let (mut state, tile) = AppState::new(Settings::default());
        let tab = state.tab_states.get_mut(&tile).unwrap();
        let total = MULTI_PREVIEW_LIMIT + 4;
        tab.set_entries((0..total).map(|idx| entry(&format!("{idx}.png"))).collect());
        // Selection order is scrambled; thumbnails still follow visible order.
        tab.selection.selected.extend((0..total).rev());

        let Some(PreviewRequest::Multiple { key, paths }) = state.active_preview_request() else {
            panic!("expected a multi-preview request");
        };
        assert_eq!(key.selected_count, total);
        assert_eq!(paths.len(), MULTI_PREVIEW_LIMIT);
        assert_eq!(
            paths,
            (0..MULTI_PREVIEW_LIMIT)
                .map(|idx| PathBuf::from(format!("{idx}.png")))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn tree_and_miller_preview_the_displayed_selection_instead_of_the_root_index() {
        use crate::{layout_tree::Tile, location::Location, settings::ViewMode, tab::MillerColumn};
        let (mut state, tile) = AppState::new(Settings::default());
        let folder = FileEntry::from(crate::file_entry::FileEntryData {
            name: "folder".into(), path: "folder".into(), kind: FileKind::Dir,
            size: None, modified: None, _is_hidden: false, _is_symlink: false,
            archive_root: None, recycle_info: None,
        });
        let tab = state.tab_states.get_mut(&tile).unwrap();
        tab.set_entries(vec![folder.clone(), entry("wrong-root.txt")].into());
        tab.tree.expanded.insert(folder.path());
        tab.tree.children.insert(folder.path(), vec![entry("nested.jpg")].into());
        tab.selection.selected.insert(1);
        if let Some(Tile::Pane(tab)) = state.pane_tree.tiles.get_mut(tile) { tab.view_mode = ViewMode::Tree; }
        assert_eq!(state.active_preview_request(), Some(PreviewRequest::Single("nested.jpg".into())));
        assert_eq!(state.selected_paths(), [PathBuf::from("nested.jpg")]);
        let mut column = MillerColumn::new(Location::Directory("/column".into()));
        column.entries = vec![entry("column.jpg")].into();
        column.selection.selected.insert(0);
        state.tab_states.get_mut(&tile).unwrap().miller.columns.push(column);
        if let Some(Tile::Pane(tab)) = state.pane_tree.tiles.get_mut(tile) { tab.view_mode = ViewMode::Miller; }
        assert_eq!(state.active_preview_request(), Some(PreviewRequest::Single("column.jpg".into())));
        assert_eq!(state.selected_paths(), [PathBuf::from("column.jpg")]);
    }

    #[test]
    fn mixed_selection_schedules_only_image_thumbnails() {
        let (mut state, tile) = AppState::new(Settings::default());
        let tab = state.tab_states.get_mut(&tile).unwrap();
        tab.set_entries(vec![entry("photo.png"), entry("notes.txt"), entry("other.jpg")].into());
        tab.selection.selected.extend([0, 1, 2]);

        let Some(PreviewRequest::Multiple { paths, .. }) = state.active_preview_request() else {
            panic!("expected a multi-preview request");
        };
        assert_eq!(
            paths,
            ["photo.png", "other.jpg"]
                .into_iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn selected_archive_items_for_pane_targets_specified_pane() {
        let (mut state, tile1) = AppState::new(Settings::default());
        let mut td = crate::pane::TabData::with_view(
            PathBuf::from("test.zip"),
            crate::settings::ViewMode::Details,
            128.0,
            false,
        );
        td.location = crate::location::Location::Archive {
            archive: PathBuf::from("test.zip"),
            inner: PathBuf::from(""),
        };
        let tile2 = state.pane_tree.tiles.insert_pane(td);
        state.tab_states.insert(tile2, crate::tab::TabState::default());
        state.focused_tile = tile1;

        let tab2 = state.tab_states.get_mut(&tile2).unwrap();
        tab2.set_entries(vec![entry("inner_file.txt")].into());
        tab2.selection.selected.insert(0);

        let items = state.selected_archive_items_for_pane(tile2).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0], (PathBuf::from("test.zip"), PathBuf::from("inner_file.txt")));
    }
}
