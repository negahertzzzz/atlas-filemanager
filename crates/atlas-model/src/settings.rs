use std::path::PathBuf;
const fn default_thumbnail_cache_mb() -> usize { 256 }

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum CharacterKeyAction {
    #[default]
    TypeInFilter,
    SelectTypedItem,
    TriggerHotkey,
}

/// Which icon set file rows use. `Auto` keeps the historic behaviour: native
/// Windows Shell icons only under the "Atlas Windows" themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum IconStyle {
    #[default]
    Auto,
    /// Native Windows Shell icons (colored, per file type) in every theme.
    Windows,
    /// The installed icon theme (e.g. Flow Icons), or the built-in set.
    Theme,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum ViewMode {
    #[default]
    Details,
    Grid,
    List,
    Miller,
    Tree,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum InspectorPlacement {
    #[default]
    Global,
    ActiveTab,
}

/// The subset of view/layout knobs that are saved *per profile* (sizes, view
/// mode, panel visibility, zoom). [`Settings`] holds the live working copy the UI
/// reads; switching profiles swaps this block in and out via
/// [`Settings::workspace`] / [`Settings::apply_workspace`], so call sites keep
/// reading `settings.*` unchanged.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub show_hidden: bool,
    pub show_sidebar: bool,
    pub sidebar_collapsed: bool,
    pub show_inspector: bool,
    pub inspector_placement: InspectorPlacement,
    pub view_mode: ViewMode,
    pub sidebar_width: f32,
    pub inspector_width: f32,
    pub font_size: f32,
    pub font_family: String,
    pub thumbnail_size: f32,
    pub global_zoom: f32,
    #[serde(default = "default_grid_spacing")]
    pub grid_spacing: f32,
    pub vim_mode: bool,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            show_hidden: false,
            show_sidebar: true,
            sidebar_collapsed: false,
            show_inspector: false,
            inspector_placement: InspectorPlacement::Global,
            view_mode: ViewMode::Details,
            sidebar_width: 220.0,
            inspector_width: 560.0,
            font_size: 13.0,
            font_family: "Geist".to_string(),
            thumbnail_size: 128.0,
            global_zoom: 1.0,
            grid_spacing: default_grid_spacing(),
            vim_mode: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    pub single_click_folders_only: bool,
    pub scroll_overscroll: f32,
    pub character_key_action: CharacterKeyAction,
    pub filter_sticky: bool,
    #[serde(default = "default_thumbnail_cache_mb")]
    pub thumbnail_cache_mb: usize,
    pub show_hidden: bool,
    pub show_sidebar: bool,
    pub sidebar_collapsed: bool,
    pub show_inspector: bool,
    pub inspector_placement: InspectorPlacement,
    pub view_mode: ViewMode,
    pub sidebar_width: f32,
    pub inspector_width: f32,
    pub font_size: f32,
    pub font_family: String,
    pub thumbnail_size: f32,
    /// Legacy global recent folders. Migrated into per-profile prefs at startup.
    pub recent_folders: Vec<PathBuf>,
    pub global_zoom: f32,
    pub language: Option<String>,
    /// When true, all with_animation calls are bypassed: elements render at their
    /// final state immediately. Eliminates the per-frame repaint cost of running
    /// animations across multiple panes.
    pub disable_animations: bool,
    /// Mouse-wheel scroll smoothness, 0.0 (instant, no easing) … 1.0 (long silky
    /// glide). Maps to the gpui scroll handle's ease + per-notch boost. Global, like
    /// `disable_animations`. Trackpad (pixel-precise) input is unaffected.
    pub scroll_smoothness: f32,
    /// Show Atlas' custom top window bar. Turn off when the platform/window manager
    /// already provides chrome and the extra in-app strip is unwanted.
    pub show_titlebar: bool,
    /// When true, evicts thumbnails and shrinks collections when the window is inactive.
    pub eco_mode: bool,
    /// When true, the pane/tab layout is persisted to `session.json` when the
    /// window closes, so the next launch restores it. Off = the last explicit
    /// save (theme change, manual "Save Session" command) still applies.
    pub autosave_session: bool,
    /// Visually de-emphasize panes other than the focused pane.
    pub dim_inactive_panes: bool,
    /// Background opacity for Glass themes (0.20 to 1.00, default 0.80).
    #[serde(default = "default_glass_opacity")]
    pub glass_opacity: f32,
    /// Element spacing / gap in grid view (points, default 16.0).
    #[serde(default = "default_grid_spacing")]
    pub grid_spacing: f32,
    /// True after the first-run setup has been dismissed or completed.
    pub getting_started_seen: bool,
    pub vim_mode: bool,
    pub icon_style: IconStyle,
    /// Convert / Actions radial menu preferences.
    pub ring: RingPrefs,
    /// Built-in Git integration: branch chip on repository folders and the
    /// Git panel. Off until the user turns it on.
    pub git_integration: bool,
    /// Git panel: branches grouped into folders by `/` (`feat/x` under `feat`).
    pub git_branch_tree: bool,
    /// Git panel: changed files grouped into folders instead of a flat list.
    pub git_change_tree: bool,
    /// Most of this PC Atlas may use, in percent (25, 50, 75 or 100). Below
    /// 100 Windows caps Atlas's CPU time and heavy work runs at low priority.
    #[serde(default = "default_cpu_limit")]
    pub cpu_limit: u8,
    /// Look for a new Atlas version when it starts (one small download of
    /// the release manifest; nothing about the user is sent).
    #[serde(default = "default_true")]
    pub check_updates: bool,
}

pub const fn default_cpu_limit() -> u8 {
    100
}

const fn default_true() -> bool {
    true
}

/// Convert / Actions ring preferences. Unknown action ids (from a newer or
/// older build) are ignored by the UI, so a stale list never breaks loading.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RingPrefs {
    /// Open on the Actions side instead of Convert.
    pub open_actions: bool,
    /// Visible actions, in ring order (ids from `convert::actions::Action::id`).
    pub actions: Vec<String>,
    /// Longest edge for "Resize", in pixels.
    pub resize_edge: u32,
    /// JPEG quality for "Compress".
    pub compress_quality: u8,
}

pub const RING_ACTION_IDS: [&str; 6] = [
    "resize",
    "rotate-right",
    "rotate-left",
    "strip-metadata",
    "compress",
    "compress-pdf",
];

impl Default for RingPrefs {
    fn default() -> Self {
        Self {
            open_actions: false,
            actions: RING_ACTION_IDS.iter().map(|id| (*id).to_string()).collect(),
            resize_edge: 1920,
            compress_quality: 80,
        }
    }
}

impl RingPrefs {
    pub fn shows(&self, id: &str) -> bool {
        self.actions.iter().any(|action| action == id)
    }
    /// Show/hide `id`, keeping the canonical order for re-added actions.
    pub fn set_shown(&mut self, id: &str, shown: bool) {
        self.actions.retain(|action| action != id);
        if shown {
            let rank = |a: &str| RING_ACTION_IDS.iter().position(|x| *x == a).unwrap_or(usize::MAX);
            let at = self
                .actions
                .iter()
                .position(|action| rank(action) > rank(id))
                .unwrap_or(self.actions.len());
            self.actions.insert(at, id.to_string());
        }
    }
}

pub fn default_glass_opacity() -> f32 {
    0.80
}

pub fn default_grid_spacing() -> f32 {
    16.0
}

impl Settings {
    pub fn scroll_overscroll_fraction(&self) -> f32 {
        if self.scroll_overscroll.is_finite() { self.scroll_overscroll.clamp(0.0, 0.9) } else { 0.4 }
    }
    pub fn single_click_opens_folder(&self, is_dir: bool, clicks: usize, modified: bool) -> bool {
        self.single_click_folders_only && is_dir && clicks == 1 && !modified
    }
    /// Snapshot the per-profile view/layout knobs out of the live settings.
    pub fn workspace(&self) -> Workspace {
        Workspace {
            show_hidden: self.show_hidden,
            show_sidebar: self.show_sidebar,
            sidebar_collapsed: self.sidebar_collapsed,
            show_inspector: self.show_inspector,
            inspector_placement: self.inspector_placement.clone(),
            view_mode: self.view_mode.clone(),
            sidebar_width: self.sidebar_width,
            inspector_width: self.inspector_width,
            font_size: self.font_size,
            font_family: self.font_family.clone(),
            thumbnail_size: self.thumbnail_size,
            global_zoom: self.global_zoom,
            grid_spacing: self.grid_spacing,
            vim_mode: self.vim_mode,
        }
    }

    /// Overlay a profile's saved knobs onto the live settings. Truly global
    /// fields (`language`, `disable_animations`) are left intact.
    pub fn apply_workspace(&mut self, ws: &Workspace) {
        self.show_hidden = ws.show_hidden;
        self.show_sidebar = ws.show_sidebar;
        self.sidebar_collapsed = ws.sidebar_collapsed;
        self.show_inspector = ws.show_inspector;
        self.inspector_placement = ws.inspector_placement.clone();
        self.view_mode = ws.view_mode.clone();
        self.sidebar_width = ws.sidebar_width;
        self.inspector_width = ws.inspector_width;
        self.font_size = ws.font_size;
        self.font_family.clone_from(&ws.font_family);
        self.thumbnail_size = ws.thumbnail_size;
        self.global_zoom = ws.global_zoom;
        self.grid_spacing = ws.grid_spacing;
        self.vim_mode = ws.vim_mode;
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            single_click_folders_only: false,
            scroll_overscroll: 0.4,
            character_key_action: CharacterKeyAction::TypeInFilter,
            icon_style: IconStyle::Auto,
            filter_sticky: false,
            thumbnail_cache_mb: default_thumbnail_cache_mb(),
            show_hidden: false,
            show_sidebar: true,
            sidebar_collapsed: false,
            show_inspector: false,
            inspector_placement: InspectorPlacement::Global,
            view_mode: ViewMode::Details,
            sidebar_width: 220.0,
            inspector_width: 560.0,
            font_size: 13.0,
            font_family: "Geist".to_string(),
            thumbnail_size: 128.0,
            recent_folders: Vec::new(),
            global_zoom: 1.0,
            language: None,
            disable_animations: false,
            scroll_smoothness: 0.6,
            show_titlebar: true,
            eco_mode: true,
            autosave_session: true,
            dim_inactive_panes: false,
            glass_opacity: default_glass_opacity(),
            grid_spacing: default_grid_spacing(),
            getting_started_seen: false,
            vim_mode: false,
            ring: RingPrefs::default(),
            git_integration: false,
            git_branch_tree: true,
            git_change_tree: false,
            cpu_limit: default_cpu_limit(),
            check_updates: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scroll_margin_defaults_to_forty_percent_and_rejects_invalid_extents() {
        let mut settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.scroll_overscroll_fraction(), 0.4);
        for (value, expected) in [(-1.0, 0.0), (0.0, 0.0), (0.4, 0.4), (5.0, 0.9), (f32::NAN, 0.4)] {
            settings.scroll_overscroll = value;
            assert_eq!(settings.scroll_overscroll_fraction(), expected);
        }
    }

    #[test]
    fn single_click_never_launches_files_or_modified_folder_clicks() {
        let mut settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(!settings.single_click_opens_folder(true, 1, false));
        settings.single_click_folders_only = true;
        assert!(settings.single_click_opens_folder(true, 1, false));
        assert!(!settings.single_click_opens_folder(false, 1, false));
        assert!(!settings.single_click_opens_folder(true, 1, true));
        assert!(!settings.single_click_opens_folder(true, 2, false));
        let saved = serde_json::to_string(&settings).unwrap();
        assert!(serde_json::from_str::<Settings>(&saved).unwrap().single_click_folders_only);
    }

    #[test]
    fn ring_prefs_load_from_old_and_partial_settings() {
        let old: Settings = serde_json::from_str(r#"{"font_size": 13.0}"#).unwrap();
        assert_eq!(old.ring, RingPrefs::default());
        let partial: Settings =
            serde_json::from_str(r#"{"ring": {"actions": ["compress", "future-action"]}}"#).unwrap();
        assert_eq!(partial.ring.actions, ["compress", "future-action"]);
        assert_eq!(partial.ring.resize_edge, 1920);
        let mut ring = partial.ring;
        ring.set_shown("resize", true);
        ring.set_shown("compress", false);
        assert!(ring.shows("resize") && !ring.shows("compress"));
        assert_eq!(ring.actions[0], "resize");
    }

    #[test]
    fn settings_defaults_glass_opacity_when_absent() {
        let json = r#"{"font_size": 13.0}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!((s.glass_opacity - 0.80).abs() < 1e-4);
    }

    #[test]
    fn settings_preserves_custom_glass_opacity() {
        let json = r#"{"glass_opacity": 0.55}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!((s.glass_opacity - 0.55).abs() < 1e-4);
    }

    #[test]
    fn settings_defaults_grid_spacing_when_absent() {
        let json = r#"{"font_size": 13.0}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!((s.grid_spacing - 16.0).abs() < 1e-4);
    }

    #[test]
    fn settings_preserves_custom_grid_spacing() {
        let json = r#"{"grid_spacing": 24.0}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!((s.grid_spacing - 24.0).abs() < 1e-4);
    }

    #[test]
    fn settings_defaults_vim_mode_to_false() {
        let json = r#"{"font_size": 13.0}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(!s.vim_mode);

        let ws: Workspace = serde_json::from_str(json).unwrap();
        assert!(!ws.vim_mode);
    }

    #[test]
    fn settings_preserves_vim_mode() {
        let json = r#"{"vim_mode": true}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert!(s.vim_mode);

        let ws: Workspace = serde_json::from_str(json).unwrap();
        assert!(ws.vim_mode);
    }

    #[test]
    fn git_integration_is_off_until_turned_on() {
        assert!(!Settings::default().git_integration);
        let old: Settings = serde_json::from_str(r#"{"font_size": 13.0}"#).unwrap();
        assert!(!old.git_integration);
        let on: Settings = serde_json::from_str(r#"{"git_integration": true}"#).unwrap();
        assert!(on.git_integration);
        assert!(on.git_branch_tree && !on.git_change_tree);
    }

    #[test]
    fn resource_limit_and_update_check_default_to_previous_behaviour() {
        let old: Settings = serde_json::from_str(r#"{"font_size": 13.0}"#).unwrap();
        assert_eq!(old.cpu_limit, 100);
        assert!(old.check_updates);
        let set: Settings = serde_json::from_str(r#"{"cpu_limit": 50, "check_updates": false}"#).unwrap();
        assert_eq!(set.cpu_limit, 50);
        assert!(!set.check_updates);
    }

    #[test]
    fn workspace_roundtrip_vim_mode() {
        let mut s = Settings::default();
        assert!(!s.vim_mode);
        s.vim_mode = true;
        let ws = s.workspace();
        assert!(ws.vim_mode);

        let mut s2 = Settings::default();
        s2.apply_workspace(&ws);
        assert!(s2.vim_mode);
    }
}

