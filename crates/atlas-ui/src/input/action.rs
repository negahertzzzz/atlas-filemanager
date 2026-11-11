use atlas_model::file_entry::{SortColumn, SortDir};
use atlas_model::location::Location;
use atlas_model::pane::{PaneId, TabId};
use atlas_model::settings::ViewMode;
use atlas_model::tag_cache::TagId;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy)]
pub enum SelectionMove {
    Previous,
    Next,
    Left,
    Right,
    First,
    Last,
    PageUp,
    PageDown,
}

// reason: several variants are constructed only under other-platform cfg
// (e.g. Mount/UnmountDevice from the Linux udisks sidebar), so per-cfg dead-code
// analysis flags them on Windows. Keep the allow scoped to the enum.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum Action {
    /// Navigate to a *filesystem path*, reinterpreted by tab context: inside a
    /// Remote tab the path is treated as a remote path (used by goto results).
    /// For an explicit target use `NavigateLocation`.
    Navigate {
        tab: TabId,
        path: PathBuf,
    },
    NavigateLocation {
        tab: TabId,
        location: Location,
    },
    NavigateBack(TabId),
    NavigateForward(TabId),
    NavigateUp(TabId),
    EditLocation(TabId),
    NewTab(PaneId),
    NewTabFocused,
    NewTabWithPath {
        pane: PaneId,
        path: PathBuf,
    },
    CloseTab(TabId),
    CloseFocusedTab,
    ReopenClosedTab,
    DetachFocusedTab,
    FocusTab(TabId),
    SplitHorizontalFocused,
    SplitVerticalFocused,
    CloseFocusedPane,
    FocusNextPane,
    SelectLeftPanel,
    SelectRightPanel,
    SelectPanelAbove,
    SelectPanelBelow,
    EvenPanelSiblings,
    EvenAllPanes,
    /// Move the focused tab into the nearest visible panel in this direction.
    MoveTabToPanel(crate::ui::tab_reorder::PanelDirection),
    ToggleFolderViewRecursive,
    FocusNextTab,
    FocusPreviousTab,
    FocusTabIndex(usize),
    MoveFocusedTabLeft,
    MoveFocusedTabRight,
    Open(Vec<PathBuf>),
    Copy(Vec<PathBuf>),
    Cut(Vec<PathBuf>),
    Paste {
        dest: PathBuf,
    },
    /// Resolve the pending transfer-conflict dialog. `None` cancels.
    ResolveTransferConflict(Option<atlas_model::ops::ConflictPolicy>),
    /// Fully-specified transfer (used by the OLE drop queue, which knows the
    /// origin and kind); `Move`/`CopyTo` below are convenience forms.
    Transfer(atlas_model::ops::TransferRequest),
    Move {
        paths: Vec<PathBuf>,
        dest: PathBuf,
    },
    CopyTo {
        paths: Vec<PathBuf>,
        dest: PathBuf,
    },
    Delete {
        paths: Vec<PathBuf>,
        to_trash: bool,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    CreateFolder {
        parent: PathBuf,
        name: String,
    },
    CreateFolderFocused,
    CreateFile {
        parent: PathBuf,
        name: String,
    },
    CreateFileFocused,
    RenameFocused,
    OpenSelectedInNewWindow,
    OpenPathInNewWindow(PathBuf),
    OpenSelectedDirInRightSplit,
    OpenSelectedDirInBottomSplit,
    OpenDirInRightSplit {
        pane: TabId,
        path: PathBuf,
    },
    OpenDirInBottomSplit {
        pane: TabId,
        path: PathBuf,
    },
    OpenTerminal {
        dir: PathBuf,
    },
    /// Compress `paths` into a new zip next to the first item (auto-named,
    /// uniqued against collisions).
    CompressToZip(Vec<PathBuf>),
    // ── Recycle Bin ──
    /// Restore the focused pane's selected recycled items to their original paths.
    RestoreSelectedFromRecycleBin,
    /// Permanently delete the focused pane's selected recycled items ($R + $I).
    PurgeSelectedFromRecycleBin,
    /// Permanently delete everything in every accessible recycle bin.
    EmptyRecycleBin,
    Select {
        tile: TabId,
        idx: usize,
        shift: bool,
        ctrl: bool,
    },
    Activate {
        tile: TabId,
        idx: usize,
    },
    SelectAll,
    MoveSelection {
        target: SelectionMove,
        extend: bool,
    },
    MoveCursor(SelectionMove),
    ToggleSelectionCursor,
    OpenPropertiesFocused,
    OpenCursor,
    OpenSelectedDirsInNewTabs,
    InvertSelection,
    MillerInit {
        tile: TabId,
        location: Location,
    },
    MillerSelect {
        tile: TabId,
        column: usize,
        idx: usize,
        shift: bool,
        ctrl: bool,
    },
    MillerActivate {
        tile: TabId,
        column: usize,
        idx: usize,
    },
    MillerToggleSelect {
        tile: TabId,
        column: usize,
        idx: usize,
    },
    ToggleHidden,
    ToggleVimMode,
    SetViewMode(ViewMode),
    /// Switch the active profile's theme by registered theme name.
    SetTheme(String),
    PreviewTheme(String),
    /// Windows "Map Network Drive" dialog (native SMB).
    MapNetworkDrive,
    /// Compare the focused pane's folder with the neighbouring pane's folder.
    CompareFolders,
    /// Git panel for the focused pane's folder (when Git integration is on).
    OpenGit,
    /// Swap between the active profile's light and dark theme.
    ToggleThemeAppearance,
    /// Follow (or stop following) the system light/dark appearance.
    ToggleThemeFollowSystem,
    SetSort {
        col: SortColumn,
        dir: SortDir,
    },
    SetGroupBy(atlas_model::tab::GroupBy),
    ToggleGroup { tab: TabId, id: String },
    /// Open (`false`) or close (`true`) every group of the focused pane.
    SetAllGroupsCollapsed(bool),
    OpenGotoBar,
    ClosePalette,
    OpenPalette,
    SubmitExtPalette(String),
    /// Pin/unpin a command-palette command (by stable id) to the "Quick" section.
    ToggleCommandPin(String),
    /// Begin recording the next keypress as a command-palette shortcut.
    /// (Cancel/set/remove happen via direct methods in `input::shortcuts`.)
    StartCommandShortcutCapture(String),
    OpenSearchInPlace,
    CloseSearchInPlace,
    ToggleSearchInSubfolders,
    // ── Per-pane bottom-bar toggles ──
    ToggleFoldersFirst(TabId),
    SetFolderOrder(TabId, atlas_model::pane::FolderOrder),
    ToggleShowFiles(TabId),
    ToggleShowFolders(TabId),
    ToggleBranchView(TabId),
    ToggleTreeExpand(PathBuf),
    ToggleFoldersFirstFocused,
    ToggleShowFilesFocused,
    ToggleShowFoldersFocused,
    ToggleBranchViewFocused,
    CloseOverlays,
    ToggleInspector,
    ToggleSidebar,
    ToggleSidebarCollapsed,
    OpenBatchRename,
    OpenConverter,
    OpenSettings,
    OpenSettingsAbout,
    /// Open Settings with this setting (registry id) selected.
    OpenSettingAt(&'static str),
    /// Flip a switch setting (registry id) without opening Settings.
    ToggleSetting(&'static str),
    /// Activate the Polar license key currently on the clipboard.
    ActivateLicense,
    /// Release this device's license activation so the key can move to another PC.
    DeactivateLicense,
    /// Reveal the crash-report folder (newest report selected when there is one).
    OpenCrashReports,
    /// Upload the previous session's crash report (text only, paths redacted).
    SendCrashReport,
    /// Open the Premium purchase options.
    GetPremium,
    OpenGettingStarted,
    OpenGuide(usize),
    CloseGuide,
    SetGuideTopic(usize),
    /// Close the guide and watch the user practise the topic's steps.
    StartGuideCoach(usize),
    StopGuideCoach,
    /// Download + activate an Open VSX icon theme (`icon-manager.download-*` command id).
    DownloadIconTheme(String),
    StartGettingStarted,
    CloseGettingStarted,
    OpenExtManager,
    /// Manually persist settings/session/profiles now, regardless of the
    /// autosave-on-quit setting.
    SaveSession,
    /// Toggle the space-analyzer overlay. Currently unreachable: no palette
    /// entry or shortcut constructs it — re-wire when the analyzer UI lands.
    #[allow(dead_code)]
    ToggleAnalyzer,
    Refresh(TabId),
    CopyPathToClipboard(PathBuf),
    CopyPathsToClipboard(Vec<PathBuf>),
    #[cfg(windows)]
    PasteShortcutFocused,
    Undo,
    Redo,
    SwitchProfile(usize),
    /// Create a new profile with the given name, seeded from the current view, and
    /// switch to it.
    CreateProfile(String),
    /// Rename the active profile.
    RenameActiveProfile(String),
    /// Rename a specific profile.
    RenameProfile {
        index: usize,
        name: String,
    },
    /// Open the rename-active-profile dialog (seeded with the current name).
    OpenRenameProfile,
    /// Open the rename dialog for a specific profile.
    OpenRenameProfileAt(usize),
    DuplicateProfile(usize),
    DeleteActiveProfile,
    DeleteProfile(usize),
    ToggleSidebarSection(usize),
    MoveSidebarSectionUp(usize),
    MoveSidebarSectionDown(usize),
    SetSidebarSectionHidden {
        index: usize,
        hidden: bool,
    },
    ResetSidebarSections,
    RemoveBookmark(usize),
    AddBookmark {
        name: String,
        path: PathBuf,
    },
    // ── Tags ──
    /// Create a new tag. `profile = None` makes it global (visible in every profile).
    CreateTag {
        name: String,
        color: [u8; 3],
        profile: Option<String>,
    },
    RenameTag {
        tag: TagId,
        name: String,
    },
    OpenCreateTagOverlay {
        global: bool,
    },
    OpenRenameTagOverlay {
        tag: TagId,
        name: String,
        color: [u8; 3],
        global: bool,
    },
    CloseCreateTagOverlay,
    DeleteTag(TagId),
    AssignTagToFiles {
        tag: TagId,
        paths: Vec<PathBuf>,
    },
    UnassignTagFromFiles {
        tag: TagId,
        paths: Vec<PathBuf>,
    },
    /// Open a virtual tab listing every file carrying `tag`.
    OpenTagResults {
        pane: PaneId,
        tag: TagId,
    },
    OpenSpaceLens {
        pane: PaneId,
        path: PathBuf,
    },
    // ── Removable disks (udisks2) ──
    /// Mount a block device (e.g. `/dev/sdb1`) via udisksctl, then navigate to it.
    MountDevice(String),
    /// Unmount/eject a block device via udisksctl.
    UnmountDevice(String),
    // ── Zoom ──
    ZoomIn,
    ZoomOut,
    ZoomReset,
    PaneContentZoomIn,
    PaneContentZoomOut,
    // ── Archives ──
    /// Navigate inside an archive, entering the `inner` directory within it.
    /// Pass `inner = PathBuf::new()` to open at the archive root.
    NavigateIntoArchive {
        tab: TabId,
        archive: PathBuf,
        inner: PathBuf,
    },
    /// Extract the selected virtual entries (`archive_root` + `inner_path` pairs)
    /// to the given destination directory.
    ExtractArchiveTo {
        tab: TabId,
        items: Vec<(PathBuf, PathBuf)>,
        dest: PathBuf,
    },
    // ── Cloud / network mounting (feature `cloud`) ──
    #[cfg(feature = "provider-host")]
    Cloud(crate::app::cloud::CloudAction),
    // ── Extensions ──
    Ext(crate::app::ext::ExtAction),
    // ── Preview editor ──
    /// Save the current preview editor's content back to its source file.
    SavePreviewFile,
    /// Toggle the preview editor between read-only (disabled) and editable mode.
    /// The preference is persisted per profile.
    TogglePreviewEditMode,
    /// Toggle line-number gutter in the preview code editor.
    /// The preference is persisted per profile.
    TogglePreviewLineNumbers,
    /// Zoom in on the previewed image (same factor as scroll-zoom).
    PreviewZoomIn,
    /// Zoom out on the previewed image.
    PreviewZoomOut,
    /// Reset the previewed image to fit-in-panel zoom and center pan.
    PreviewZoomFit,
}
