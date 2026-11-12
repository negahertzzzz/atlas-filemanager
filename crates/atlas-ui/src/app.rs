use atlas_model::clamp::ClampLenient;
use atlas_model::layout_tree::{Container, Tile, TileId};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, ColorExt as _, Entity, FocusHandle, UniformListScrollHandle};
use gpui_component::input::InputState;

use atlas_core::search::nucleo_pool::NucleoPool;
use atlas_model::app_state::{AppState, BgResult};
use atlas_model::pane::TabData;

use atlas_core::util::config::{save_profiles, save_session, save_settings};

pub mod actions;
mod bg;
#[cfg(feature = "provider-host")]
pub mod cloud;
pub mod devices;
mod edition;
pub mod ext;
pub mod fs_sync;
pub(crate) mod git;
pub(crate) mod icon_loader;
pub(crate) mod liquid_glass;
pub mod nav;
#[cfg(windows)]
pub(crate) mod network;
pub mod ops;
pub mod pane_ops;
pub mod profiles;
mod startup;
pub mod theme;
pub(crate) mod vault;

pub struct App {
    pub(crate) vault: Option<vault::Dialog>,
    #[cfg(windows)]
    pub(crate) vault_mount: Option<atlas_core::vault::mount::Mounted>,
    pub(crate) converter: Option<crate::ui::converter::Session>,
    pub(crate) cleanup_panel: Option<Entity<crate::ui::cleanup::CleanupPanel>>,
    /// Folder comparison between the two visible panes.
    pub(crate) compare_panel: Option<Entity<crate::ui::compare::ComparePanel>>,
    /// `(left, right)` waiting for a `Context` to mount the compare panel.
    pub(crate) pending_compare: Option<(std::path::PathBuf, std::path::PathBuf)>,
    /// Git panel of the repository a folder belongs to.
    pub(crate) git_panel: Option<Entity<crate::ui::git_panel::GitPanel>>,
    /// Folder waiting for a window to mount the Git panel.
    pub(crate) pending_git: Option<std::path::PathBuf>,
    /// Cached repository probes for the pane headers' branch chips.
    pub(crate) git_probes: git::GitProbes,
    pub(crate) mounted_volumes: Vec<(PathBuf, u32)>,
    pub(crate) preview_delay: crate::ui::preview_delay::PreviewDelay,
    pub(crate) state: AppState,
    pub(crate) startup_started: Instant,
    pub(crate) startup_first_render_logged: bool,
    pub(crate) close_after_tab_transfer: bool,
    /// The previous crash's report was uploaded (or is uploading) this session.
    pub(crate) crash_report_sent: bool,
    #[cfg(windows)]
    pub(crate) pending_tab_restore:
        Option<(TileId, crate::dnd::windows::ole_clipboard::TabTransfer)>,
    pub(crate) tx: flume::Sender<BgResult>,
    pub(crate) rx: flume::Receiver<BgResult>,
    pub(crate) rt: tokio::runtime::Handle,
    /// Keyboard focus for the whole window; the root element tracks it so the
    /// `Atlas` key context (nav/tab shortcuts) is active.
    pub(crate) focus_handle: FocusHandle,
    /// Per-pane virtualized-list scroll handles (UI-side), so keyboard nav can
    /// scroll the cursor into view.
    pub(crate) scroll_handles: HashMap<TileId, UniformListScrollHandle>,
    pub(crate) miller_scroll_handles: HashMap<(TileId, usize), UniformListScrollHandle>,
    pub(crate) tree_loading: HashSet<(TileId, PathBuf)>,
    /// Runtime filesystem watcher handles keyed by pane tile. Kept out of
    /// `AppState` so model/state does not depend on `fs`.
    pub(crate) watchers: HashMap<TileId, atlas_core::fs::watcher::WatcherHandle>,
    /// Runtime fuzzy-search index. Kept out of model/state so state never owns
    /// search engine internals.
    pub(crate) nucleo: NucleoPool,
    /// Small directory-only index that is never repointed by scoped searches.
    pub(crate) goto_warm_nucleo: NucleoPool,
    /// Directory the goto's nucleo index was built from and whether it is a
    /// scoped (drill-in) index; a path-prefixed query re-scopes when either
    /// differs.
    pub(crate) goto_index_root: Option<(PathBuf, bool)>,
    /// One published, ranked list per query; late index updates cannot move rows.
    pub(crate) goto_snapshot: crate::ui::palette::GotoSnapshot,
    /// Last service-backed Goto query. Avoids repeating named-pipe work on every paint frame.
    pub(crate) goto_service_cache: Option<(
        String,
        std::sync::Arc<Vec<atlas_core::search::nucleo_pool::IndexEntry>>,
        bool,
    )>,
    pub(crate) goto_service_inflight: Option<String>,
    pub(crate) goto_service_failed: Option<String>,
    pub(crate) goto_service_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Pre-fill for the goto input on its next open (Ctrl+L: current dir).
    pub(crate) goto_prefill: Option<String>,
    /// One-shot fuzzy ranker for the command/ext palettes (matcher reused).
    pub(crate) fuzzy_scorer: atlas_core::search::fuzzy::FuzzyScorer,
    /// Scroll state of the open palette's result list (keyboard nav keeps the
    /// selected row in view). Shared: only one palette is open at a time.
    pub(crate) palette_scroll: gpui::ScrollHandle,
    pub(crate) type_filter_scroll: gpui::ScrollHandle,
    pub(crate) settings_scroll: gpui::ScrollHandle,
    pub(crate) getting_started_theme_scroll: gpui::ScrollHandle,
    pub(crate) ext_manager_scroll: gpui::ScrollHandle,
    pub(crate) pane_zoom_sliders: HashMap<TileId, Entity<gpui_component::slider::SliderState>>,
    pub(crate) pane_zoom_slider_subs: HashMap<TileId, gpui::Subscription>,
    /// View-selector popovers whose open state must survive switching render trees.
    pub(crate) open_view_selectors: HashSet<TileId>,
    /// Runtime extension registry. Kept out of `AppState` because loading it scans
    /// extension folders and PATH.
    pub(crate) ext_registry: atlas_core::ext::Registry,
    /// Channel to the `SQLite` tag-db worker (writes fire-and-forget, reads reply via `tx`).
    pub(crate) tag_db_tx: flume::Sender<atlas_core::fs::tag_db::TagDbCmd>,
    /// Channel to the cloud worker (rclone sidecar + transfers). Replies via `tx`.
    #[cfg(feature = "provider-host")]
    pub(crate) cloud_tx: flume::Sender<atlas_core::cloud::CloudCmd>,
    // Phase5: UI-side preview texture caches (state holds only neutral RGBA).
    // Phase5: single-preview texture per tab, keyed by its source path for invalidation.
    pub(crate) preview_textures: HashMap<TileId, (PathBuf, std::sync::Arc<gpui::RenderImage>)>,
    // Phase5: decoded multi-selection thumbnail textures per tab.
    pub(crate) multi_textures: HashMap<TileId, Vec<(PathBuf, std::sync::Arc<gpui::RenderImage>)>>,
    /// Orbiting media thumbnails of the folder preview (own entity: spins without re-rendering App).
    pub(crate) dir_ring: Option<Entity<crate::ui::dir_ring::DirRing>>,
    /// Tree view: flattened rows per pane, rebuilt only when the fingerprint of the
    /// listing/expansion state changes (scrolling re-renders every frame). The last
    /// field lists expanded folders still waiting for their children.
    pub(crate) tree_rows_cache: HashMap<
        TileId,
        (
            u64,
            std::sync::Arc<[(atlas_model::file_entry::FileEntry, usize, bool, bool)]>,
            Vec<PathBuf>,
        ),
    >,
    // Selectable code/text preview editors per tab. Keyed by path/content so the
    // inspector can preserve selection while rendering read-only preview text.

    // Phase6: per-tab image-preview interaction state (scroll=zoom, drag=pan, Space=expand).
    // Pure UI/interaction state, kept out of `AppState` like the other render caches.
    pub(crate) preview_view: HashMap<TileId, PreviewView>,
    pub(crate) inspector_animating: bool,
    pub(crate) last_viewport_size: Option<gpui::Size<gpui::Pixels>>,
    pub(crate) window_resize_suppress_until: Option<Instant>,
    // Transient: last cursor position while panning the preview image.
    pub(crate) preview_drag: Option<gpui::Point<gpui::Pixels>>,
    /// Code/text editor state for the currently focused tab's preview. `None` when the
    /// preview is not a text/code file or has not loaded yet. Recreated when the tab or
    /// selected file changes.
    pub(crate) preview_editor: Option<PreviewEditor>,
    /// State for the open tag-picker overlay. `None` when closed.
    pub(crate) tag_picker_menu: Option<crate::ui::tag_picker_menu::TagPickerState>,
    /// Search input inside the tag-picker overlay.
    pub(crate) tag_picker_input: Option<Entity<InputState>>,
    /// Subscription that notifies on input changes so the tag picker repaints.
    pub(crate) tag_picker_sub: Option<gpui::Subscription>,
    pub(crate) _rubber_escape_sub: Option<gpui::Subscription>,
    /// Scroll handle for the tag-picker list.
    pub(crate) tag_picker_scroll: gpui::ScrollHandle,
    // Keyboard focus for the active overlay (fuzzy bars / modals capture keys here).
    pub(crate) overlay_focus: Option<FocusHandle>,
    pub(crate) batch_rename_field: BatchRenameField,
    // UI-side cache of downscaled grid thumbnails, keyed by file path.
    pub(crate) thumb_textures: crate::ui::thumbnail_cache::ThumbnailCache,
    pub(crate) shell_icon_inflight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) shell_icon_lru: std::collections::VecDeque<std::path::PathBuf>,
    /// Windows Shell file-type / folder icons (multi-resolution). `None` = requested.
    pub(crate) shell_icons: std::collections::HashMap<
        std::path::PathBuf,
        Option<std::sync::Arc<crate::icon_theme::MultiResIcon>>,
    >,
    // Paths with an in-flight *extension* thumbnailer process. Caps how many
    // external processes (ffmpeg/magick) run at once so a big folder can't spawn
    // hundreds. Native image decodes are CPU-only and not throttled here.
    pub(crate) ext_thumb_inflight: std::collections::HashSet<PathBuf>,
    // Viewport-priority cancellation: paths currently visible in the grid.
    // Workers check this before decoding; paths absent are skipped (and removed
    // from thumb_textures so they re-request when scrolled back into view).
    pub(crate) thumb_wanted: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
    /// Per-pane slice of `thumb_wanted` (visible + prefetch cells of each grid).
    pub(crate) thumb_wanted_by_pane: HashMap<atlas_model::pane::TabId, Vec<PathBuf>>,
    // Count of in-flight native image decodes (incremented on spawn, decremented
    // on Ready or Skipped). Caps concurrent decodes so the blocking pool stays
    // available for directory listing and icon rasterisation.
    pub(crate) native_thumb_inflight: usize,
    // Icon theme: parsed VSCode icon-theme index (extension → SVG path) + rasterised
    // textures. `None` when no supported theme was found at startup.
    pub(crate) icon_theme: Option<crate::icon_theme::IconThemeIndex>,
    /// Rasterised icon textures. Key = `icon_theme::icon_key()` result.
    pub(crate) icon_cache: HashMap<String, crate::icon_theme::IconState>,
    /// Default Icons8 Liquid Glass GPU textures for files and folders.
    pub(crate) liquid_glass: liquid_glass::LiquidGlassCache,
    /// The active theme is light; picks icon variants that read on light surfaces.
    pub(crate) light_surfaces: bool,
    /// Dedicated channel for icon rasterisation results. Uses `Arc<RenderImage>` directly
    /// so GPUI types never enter atlas-model's `BgResult` enum.
    pub(crate) icon_tx: flume::Sender<(
        String,
        Option<std::sync::Arc<crate::icon_theme::MultiResIcon>>,
    )>,
    pub(crate) icon_rx: flume::Receiver<(
        String,
        Option<std::sync::Arc<crate::icon_theme::MultiResIcon>>,
    )>,
    /// Cached `FilePanel` entities keyed by pane `TileId`. Reusing entities across
    /// `rebuild_dock_center` calls keeps gpui animation state alive on existing panes
    /// so they don't flash when a new split or close restructures the layout.
    pub(crate) file_panel_cache: HashMap<TileId, Entity<crate::ui::pane_panel::FilePanel>>,
    pub(crate) space_lens: HashMap<TileId, atlas_core::space_lens::SpaceLensState>,
    /// One `PaneTabsView` (tab strip + body) per tabs-like tile, embedded through
    /// `AnyView::cached` (the same pattern Zed uses). Fingerprint invalidation lets
    /// `render_tree` recalculate `pane_tabs_fp` and notify the view only when it
    /// changes, so unchanged pane chrome is reused wholesale.
    pub(crate) pane_tabs_cache: HashMap<TileId, Entity<crate::ui::dock::render::PaneTabsView>>,
    pub(crate) pane_tabs_fp: HashMap<TileId, u64>,
    pub(crate) pane_tabs_bounds: HashMap<TileId, gpui::Bounds<gpui::Pixels>>,
    /// The dock painted a retained FLIP frame last frame (split/close motion).
    pub(crate) dock_flip_last_frame: bool,
    /// Panes still reflowing to their settled geometry, a few per frame.
    pub(crate) dock_settling: bool,
    pub(crate) sidebar_panel: Option<Entity<crate::ui::sidebar::SidebarPanel>>,
    /// Inspector body hosted in its own retained entity. Preview decoding and
    /// pane animations update App frequently, but the inspector only observes
    /// changes to its focused-preview fingerprint.
    pub(crate) inspector_host: Option<Entity<crate::ui::inspector::InspectorHost>>,
    /// The dock tree hosted in its own view (see `ui::dock_host`). Split/close/drag
    /// animations call `request_animation_frame`, which notifies the *current* view;
    /// hosting the dock here keeps that per-frame notify off `App`, so animations no
    /// longer re-render the whole App tree (was the stutter source under many panes).
    pub(crate) dock_host: Option<Entity<crate::ui::dock_host::DockHost>>,
    /// MRU focus history: pane TileIds in the order they last held focus.
    /// `find_focus_after_close` pops entries until it finds a live pane,
    /// giving VS-Code-style "go to previously used pane" on Ctrl+W.
    pub(crate) focus_history: std::collections::VecDeque<TileId>,
    // The open palette hosted in its own view (see `ui::palette_host`). Cached like
    // `dock`; cleared on open/close so each open replays the panel's open animation.
    pub(crate) palette_host: Option<(
        crate::ui::palette::PaletteKind,
        Entity<crate::ui::palette_host::PaletteHost>,
    )>,
    // Set when a palette is dismissed with a fade: keeps its view mounted for one
    // exit animation, then `tick_palette_close` (driven by the 60 Hz drain) tears
    // it down.
    pub(crate) palette_closing: Option<(crate::ui::palette::PaletteKind, Instant)>,
    pub(crate) layout_revision: u64,
    /// Short-lived visual affordances for panes that just entered through a tab
    /// create or split operation. UI-only; never persisted into AppState.
    pub(crate) pane_animations: HashMap<TileId, PaneAnimation>,
    /// One lightweight entity per pane. `PaneContentView` observes its own signal
    /// so only the affected pane re-renders on pane-specific state changes, instead
    /// of every pane re-rendering on every `App::cx.notify()`.
    pub(crate) pane_signals: HashMap<TileId, gpui::Entity<PaneSignal>>,
    /// Last painted bounds per pane, used for RAD-style FLIP layout transitions.
    /// This is intentionally separate from `pane_view_bounds`, which is cleared
    /// every frame for interaction hit-testing.
    pub(crate) pane_layout_bounds: HashMap<TileId, gpui::Bounds<gpui::Pixels>>,
    pub(crate) pane_layout_animations: HashMap<TileId, PaneLayoutAnimation>,
    pub(crate) dock_split_animations: HashMap<TileId, DockSplitAnimation>,
    /// Panes mid close-out animation: kept mounted (and in the dock) until the exit
    /// animation elapses, then the pane is removed. Mirrors `palette_closing`.
    pub(crate) pane_closing: HashMap<TileId, Instant>,
    /// Tabs mid close-out animation inside an existing tab strip. Separate from
    /// `pane_closing` so a tab close never triggers split/pane collapse motion.
    pub(crate) tab_closing: HashMap<TileId, Instant>,
    /// Panes whose column recalculation was deferred while split/close/sidebar
    /// animations are in flight. Flushed once animations settle.
    pub(crate) pending_column_reflow: HashSet<TileId>,
    /// When a pane's fresh listing last appeared (tab create / navigation). Views
    /// stagger-animate item rows in only while within `ITEM_APPEAR_WINDOW` of this,
    /// so scrolling later never re-triggers the appear. Set alongside `appear_seq`.
    pub(crate) pane_items_appear: HashMap<TileId, Instant>,
    pub(crate) pane_view_transitions: HashMap<TileId, crate::ui::views::transition::ViewTransition>,
    // Phase C: rubber-band (select box) drag state, UI-side. `row_rects` holds the
    // window-space rect of each *visible* item, rebuilt every frame via on_prepaint;
    // the active drag intersects its rectangle against them to drive the selection.
    pub(crate) rubber: Option<RubberDrag>,
    pub(crate) pending_folder_click: Option<crate::ui::click_navigation::FolderClick>,
    pub(crate) recent_folder_navigation: Option<crate::ui::click_navigation::NavigationClick>,
    pub(crate) row_rects:
        std::cell::RefCell<HashMap<TileId, Vec<(usize, gpui::Bounds<gpui::Pixels>)>>>,
    pub(crate) miller_row_rects:
        std::cell::RefCell<HashMap<(TileId, usize), Vec<(usize, gpui::Bounds<gpui::Pixels>)>>>,
    pub(crate) details_hit: HashMap<TileId, DetailsHitModel>,
    /// Per-pane circular row slot buffer for Details (shape on miss only).
    pub(crate) details_row_rings: HashMap<TileId, crate::ui::views::details_ring::DetailsRowRing>,
    /// Cross-pane immutable text cache. Panes cloned from the same directory
    /// share shaped Details rows instead of rebuilding one cache per pane.
    pub(crate) details_row_texts: crate::ui::views::details_ring::DetailsRowTextCache,
    pub(crate) grid_hit: HashMap<TileId, GridHitModel>,
    pub(crate) grid_hover: Option<(TileId, usize)>,
    /// Cross-pane cold cache; hot reuse remains in each Grid surface's local ring.
    pub(crate) file_surface_shapes:
        crate::ui::views::grid_surface_element_deprecated::FileSurfaceShapeCache,
    pub(crate) list_hit: HashMap<TileId, GridHitModel>,
    pub(crate) list_hover: Option<(TileId, usize)>,
    pub(crate) grid_flip_states: std::rc::Rc<
        std::cell::RefCell<std::collections::HashMap<atlas_model::pane::TabId, GridFlipState>>,
    >,
    pub(crate) details_hover: Option<(TileId, usize)>,
    pub(crate) miller_column_bounds: HashMap<(TileId, usize), gpui::Bounds<gpui::Pixels>>,
    pub(crate) dropzone_selection: atlas_model::tab::SelectionState<PathBuf>,
    pub(crate) dropzone_rects: Vec<(PathBuf, gpui::Bounds<gpui::Pixels>)>,
    pub(crate) dropzone_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    pub(crate) dropzone_drag_paths: HashSet<PathBuf>,
    pub(crate) dropzone_pending_tags: HashMap<PathBuf, atlas_model::tag_cache::TagId>,
    pub(crate) pending_transfers: std::collections::VecDeque<atlas_model::ops::TransferRequest>,
    pub(crate) operation_feedback:
        std::cell::RefCell<crate::ui::operation_feedback::OperationFeedback>,
    pub(crate) hidden_operation_notices:
        HashMap<TileId, crate::ui::operation_feedback::HiddenResultNotice>,
    pub(crate) toast_stack: crate::ui::overlays::toasts::ToastStack,
    pub(crate) recycle_bin_full: Option<bool>,
    pub(crate) recycle_bin_status_inflight: bool,
    pub(crate) recycle_bin_status_checked: bool,
    pub(crate) recycle_bin_status_dirty: bool,
    pub(crate) recycle_bin_icon_dirty: bool,
    pub(crate) view_selection_pending: HashMap<TileId, crate::ui::view_continuity::ViewSelection>,
    pub(crate) tab_reorder_preview: Option<crate::ui::tab_reorder::TabReorderPreview>,
    pub(crate) view_scroll_pending: HashMap<TileId, (atlas_model::location::Location, PathBuf)>,
    // Phase F: pane drop-zones for the OLE drop target. Each frame's render path
    // pushes one DropZone per visible pane (via on_prepaint) into this vec; at the
    // start of the next frame the vec is committed to `dnd.zones` (Windows only) so
    // the COM vtable can resolve drop positions to the correct pane directory.
    // Kept unconditional so the helper fn compiles on all platforms without cfg noise.
    pub(crate) pending_pane_zones: std::cell::RefCell<Vec<crate::dnd::DropZone>>,
    pub(crate) pending_rename: Option<(TileId, PathBuf, bool)>,
    // Native single-line inputs (gpui-component) for the overlays. Created lazily on
    // open, cleared to `None` on close so each open starts empty. Holding the `Entity`
    // here lets the renderer read `.value()` each frame as the source of truth.
    pub(crate) goto_input: Option<Entity<InputState>>,
    pub(crate) palette_input: Option<Entity<InputState>>,
    pub(crate) ext_palette_input: Option<Entity<InputState>>,
    pub(crate) search_inputs: HashMap<TileId, Entity<InputState>>,
    pub(crate) batch_pattern_input: Option<Entity<InputState>>,
    pub(crate) batch_replacement_input: Option<Entity<InputState>>,
    /// Native input backing the rename-active-profile dialog.
    pub(crate) rename_profile_input: Option<Entity<InputState>>,
    pub(crate) getting_started_profile_input: Option<Entity<InputState>>,
    /// Native input backing create/rename tag.
    pub(crate) create_tag_input: Option<Entity<InputState>>,
    pub(crate) renaming_profile_idx: Option<usize>,
    pub(crate) settings_input: Option<Entity<InputState>>,
    pub(crate) theme_editor: Option<crate::ui::theme_editor::ThemeEditor>,
    pub(crate) theme_fork_error: Option<String>,
    /// Transient state of the settings palette (selection, category, picker).
    pub(crate) settings_ui: crate::ui::settings_palette::SettingsUi,
    /// System font families are relatively expensive to enumerate, so cache
    /// them after settings first opens.
    pub(crate) settings_font_families: Option<std::sync::Arc<Vec<gpui::SharedString>>>,
    pub(crate) ext_manager_input: Option<Entity<InputState>>,
    /// Native inputs for the parameters of the pending extension command.
    pub(crate) ext_prompt_inputs: Vec<Entity<InputState>>,
    /// Inputs for the add-remote dialog; created on open, cleared on close.
    #[cfg(feature = "provider-host")]
    pub(crate) cloud_add_inputs: Option<crate::ui::overlays::add_cloud::CloudAddInputs>,
    /// Input backing the rename-cloud-remote dialog.
    #[cfg(feature = "provider-host")]
    pub(crate) rename_remote_input: Option<Entity<InputState>>,
    /// In-flight inline rename (one row at a time). Holds the native input being edited.
    pub(crate) rename_edit: Option<RenameEdit>,
    /// Mirrors the rename input's edits onto the other selected files.
    pub(crate) batch_input_sub: Option<gpui::Subscription>,
    // Goto bar "search in content": matching file paths from the latest grep, the
    // generation that produced them (stale batches are dropped), and the cancel flag
    // shared with the in-flight background search.
    pub(crate) content_results: Vec<PathBuf>,
    pub(crate) content_gen: u64,
    pub(crate) content_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// When set, the next render applies the active profile's theme to the global
    /// gpui-component `Theme` (deferred because applying needs `&mut App` + window,
    /// which `process_actions` lacks). Starts `true` so the restored profile theme
    /// is applied on first paint.
    pub(crate) theme_dirty: bool,
    /// When set, the next theme apply first re-reads the system light/dark
    /// appearance (startup, profile switch, OS change) for profiles that
    /// follow it.
    pub(crate) theme_follow_pending: bool,
    pub(crate) theme_preview_before: Option<String>,
    /// A Settings theme dropdown is hover-previewing; reverted when it closes.
    pub(crate) settings_theme_previewing: bool,
    /// Settings hover preview of the default view, applied to the focused pane.
    pub(crate) view_preview_before:
        Option<(atlas_model::pane::TabId, atlas_model::settings::ViewMode)>,
    /// Settings hover preview of the icon style (committed value).
    pub(crate) icon_style_preview_before: Option<atlas_model::settings::IconStyle>,
    /// When set, the next render applies `settings.sidebar_width` to the left dock
    /// (per-profile sidebar width). Starts `true` so the restored width is applied
    /// once the dock exists; set again by `switch_profile`.
    pub(crate) sidebar_width_dirty: bool,
    pub(crate) sidebar_render_width: Option<f32>,
    /// Layout the sidebar shows right now. Flips halfway through the width
    /// spring so content never snaps to icons before the panel has narrowed.
    pub(crate) sidebar_visual_collapsed: bool,
    pub(crate) sidebar_width_animating: bool,

    // ── Windows OLE drag-and-drop ──────────────────────────────────────────
    // Raw HWND value (isize). Extracted once in App::new from gpui's
    // HasWindowHandle trait. Stored as isize so it is Send (HWND is !Send).
    // Used only on the main (UI) thread.
    #[cfg(windows)]
    pub(crate) hwnd: Option<isize>,
    /// Native SMB prompts in flight (see `app::network`).
    #[cfg(windows)]
    pub(crate) network: crate::app::network::NetworkUi,
    // DnD sub-system: drop target COM object (kept alive), zone registry
    // (updated each render frame while a drag is active), and the action queue
    // (COM vtable pushes, App drains in drain_dnd_queue).
    #[cfg(windows)]
    pub(crate) dnd: Option<crate::dnd::windows::DndState>,

    // ── Shell context menu (File Pilot–style right-click) ──────────────────
    // STA worker that enumerates and invokes the native Windows shell menu
    // (Open with, Send to, 7-Zip, Properties, …). `None` if the worker failed.
    #[cfg(windows)]
    pub(crate) shell_menu: Option<crate::shell_menu::ShellMenuHandle>,
    // Open right-click menu state: Atlas items + (Windows) native shell items.
    // `None` when closed. Cross-platform; on non-Windows only Atlas items show.
    pub(crate) dropped_in_background: bool,

    // Sidebar interaction model for monolithic custom element rendering
    pub(crate) sidebar_hit_model: Option<SidebarHitModel>,

    pub(crate) was_inactive: bool,
    pub(crate) inactive_since: Option<Instant>,
    pub(crate) eco_cleared: bool,
    pub(crate) ctx_menu: Option<crate::ui::shell_context_menu::CtxMenuState>,
    // Monotonic id: stale `Built` events from a previous open are ignored.
    pub(crate) ctx_menu_seq: u64,
    /// Last-known native items per selection kind + deferred invoke (instant open).
    #[cfg(windows)]
    pub(crate) ctx_native: crate::ui::shell_context_menu::NativeMenuCache,
    pub(crate) suppress_next_shell_ctx_menu: bool,
    /// When `NewFolder` is invoked, stores (tile, path) so the FS-watcher result
    /// can auto-select that entry once it appears in the listing.
    pub(crate) pending_select: Option<(TileId, PathBuf)>,
    /// Batch rename: the new paths to re-select once the renamed listing lands.
    /// Batch rename mirror carets blink from this instant (reset on every edit).
    pub(crate) batch_blink_origin: std::time::Instant,
    pub(crate) batch_blink_visible: bool,
    pub(crate) pending_batch_select: Option<(TileId, Vec<PathBuf>, std::time::Instant)>,
    /// Details-view column widths, shared across all panes.
    pub(crate) col_widths: ColWidths,
    pub(crate) typed_selection: Option<(atlas_model::pane::TabId, String, Instant)>,
    #[cfg(windows)]
    pub(crate) win_e_enabled: bool,
    /// Folders opened by other apps ("Open file location", ...) open in Atlas.
    #[cfg(windows)]
    pub(crate) explorer_folders: bool,
    /// User-facing order for Details metadata columns. Name stays fixed first.
    pub(crate) details_column_order: [DetailsColumn; 3],
    /// Active column-resize drag state (cleared on mouse-up).
    pub(crate) col_resize: Option<ColResizeState>,
    /// Column currently being drag-reordered (for the floating header + source gap).
    /// Cleared on drop / mouse-up.
    pub(crate) col_dragging: Option<DetailsColumn>,
    /// Header-local x of the cursor during a column drag (drives the floating box).
    pub(crate) col_drag_x: f32,
    /// The entire column follows the original pointer grab point in its source pane.
    pub(crate) col_drag_grab_x: f32,
    pub(crate) col_drag_pane: Option<TileId>,
    /// Per-pane active type-category filters for the Details view.
    /// Empty set = no filter. Values are `file_type_label()` strings.
    pub(crate) type_filters: HashMap<TileId, HashSet<String>>,
    /// Per-pane active size-category filters for the Details view.
    /// Empty set = no filter.
    pub(crate) size_filters: HashMap<TileId, HashSet<crate::ui::size_filter_menu::SizeFilter>>,
    pub(crate) file_filter_projections:
        HashMap<TileId, crate::ui::file_filter::FileFilterProjection>,
    // ── Details type-filter overlay ───────────────────────────────────────
    /// State for the open type-filter overlay. `None` when closed.
    pub(crate) type_filter_menu: Option<crate::ui::type_filter_menu::TypeFilterMenuState>,
    /// Search input inside the type-filter overlay.
    pub(crate) type_filter_input: Option<Entity<InputState>>,
    /// Subscription that notifies on input changes so the overlay repaints.
    pub(crate) type_filter_sub: Option<gpui::Subscription>,
    /// State for the open breadcrumb-chevron overlay. `None` when closed.
    pub(crate) breadcrumb_menu: Option<crate::ui::breadcrumb_menu::BreadcrumbMenuState>,
    /// Search input inside the breadcrumb-chevron overlay.
    pub(crate) breadcrumb_menu_input: Option<Entity<InputState>>,
    /// Subscription that notifies on input changes so the overlay repaints.
    pub(crate) breadcrumb_menu_sub: Option<gpui::Subscription>,
    // ── Details size-filter overlay ───────────────────────────────────────
    /// State for the open size-filter overlay. `None` when closed.
    pub(crate) size_filter_menu: Option<crate::ui::size_filter_menu::SizeFilterMenuState>,
    // ── Drag-hover navigation ─────────────────────────────────────────────
    /// Finder-style hover-to-navigate state (updated each animation frame while a drag is live).
    pub(crate) drag_hover: crate::ui::drag_hover::DragHoverState,
    /// Buffer of targets registered by renderers each frame; consumed by `tick_drag_hover`.
    pub(crate) pending_drag_hover_targets: Vec<crate::ui::drag_hover::DragHoverTarget>,
    /// Outgoing OS DnD payload tracking (Wayland / X11 / macOS).
    pub(crate) active_external_drag: Option<crate::ui::drag::ActiveExternalDrag>,
    /// Window-space bounds of each pane view, used for edge-scroll + tag-drop detection.
    pub(crate) pane_view_bounds: HashMap<TileId, gpui::Bounds<gpui::Pixels>>,
    pub(crate) dock_root_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    pub(crate) dock_linear_bounds: HashMap<TileId, gpui::Bounds<gpui::Pixels>>,
    pub(crate) dock_resize: Option<DockResizeState>,
    pub(crate) sidebar_resize: Option<SidebarResizeState>,
    pub(crate) inspector_resize: Option<InspectorResizeState>,
    /// Sidebar pixel-scroll region registered each frame for drag edge-scroll.
    /// The sidebar is a separate entity (not a pane), so it can't use the
    /// row-based pane edge-scroll; this drives `ScrollHandle` by pixels instead.
    pub(crate) sidebar_scroll_region: Option<(gpui::Bounds<gpui::Pixels>, gpui::ScrollHandle)>,
    pub(crate) atlas_launcher_profiles_anchor: gpui::Point<gpui::Pixels>,
    /// Panes whose `location` changed via `navigate_to` but whose `PaneSignal`
    /// has not been notified yet. Flushed by `flush_dirty_panes` from any path
    /// that holds a `Context` after navigating (drops, drag-hover tick).
    pub(crate) dirty_panes: std::collections::HashSet<TileId>,
    pub(crate) dirty_all_panes: bool,
    /// Focused pane as of the last App render. When it differs, both the old
    /// and new pane repaint so the active/inactive selection style follows focus.
    pub(crate) focus_painted: Option<TileId>,
    // ── Command shortcut capture ──────────────────────────────────────────
    /// Non-None while the user is recording a new keyboard shortcut for a palette command.
    pub(crate) command_shortcut_capture: Option<String>,
    pub(crate) command_shortcut_conflict: Option<String>,
    /// Pending key for multi-key sequences in Vim mode (e.g. 'g' waiting for second 'g').
    pub(crate) vim_pending_key: Option<char>,
}

/// Marker entity — one per pane. `PaneContentView` observes this instead of `App`
/// so that only the affected pane re-renders on pane-specific state changes.
pub struct PaneSignal;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneAnimationKind {
    NewTab,
    SplitHorizontal,
    SplitVertical,
}

/// A deferred mutation of the live dock, applied in `render`. `Split` adds a new
/// pane next to `source`; `Remove` drops a pane. Both let `reconcile_from_dock`
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaneAnimation {
    pub(crate) kind: PaneAnimationKind,
    pub(crate) started: Instant,
}

impl PaneAnimation {
    const TTL: Duration = DockSplitAnimation::DURATION;

    pub(crate) fn new(kind: PaneAnimationKind) -> Self {
        Self {
            kind,
            started: Instant::now(),
        }
    }

    pub(crate) fn is_alive(self) -> bool {
        self.started.elapsed() < Self::TTL
    }
}

/// Marker for a pane mid layout transition. The FLIP offsets themselves now
/// live in the gpui paint-offset patch; this only tracks liveness.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PaneLayoutAnimation {
    pub(crate) started: Instant,
}

impl PaneLayoutAnimation {
    const TTL: Duration = Duration::from_millis(crate::ui::motion::PANE_MS);

    pub(crate) fn is_alive(self) -> bool {
        self.started.elapsed() < Self::TTL
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DockSplitAnimation {
    pub(crate) from: Vec<f32>,
    pub(crate) to: Vec<f32>,
    /// Divider extents at interruption, expressed as a fraction of their 5 px
    /// resting width. Retargeting shares alone still jumps a partly open seam.
    pub(crate) from_gap_factors: Vec<f32>,
    /// A close burst shares a bounded deadline instead of retaining every pane
    /// until 180 ms after the user releases the close shortcut.
    pub(crate) close_duration: Option<Duration>,
    pub(crate) started: Instant,
    pub(crate) closing: bool,
    /// Splitter immediately adjacent to the closing slot that should collapse
    /// with it. Keeping the other adjacent splitter alive avoids a final gap
    /// snap when the pane is removed from the tree.
    pub(crate) closing_ix: Option<usize>,
}

impl DockSplitAnimation {
    // Keep the pane geometry and its teardown in one calm motion. The previous
    // 150ms spring left a visible second jump when the 0.01-share slot was
    // finally removed, especially in nested split trees.
    pub(crate) const DURATION: Duration = Duration::from_millis(crate::ui::motion::SPLIT_OPEN_MS);
    pub(crate) const CLOSE_DURATION: Duration =
        Duration::from_millis(crate::ui::motion::SPLIT_CLOSE_MS);

    pub(crate) fn new(to: Vec<f32>, inserted_ix: usize) -> Self {
        if to.is_empty() {
            return Self {
                from: Vec::new(),
                from_gap_factors: Vec::new(),
                close_duration: None,
                to,
                started: Instant::now(),
                closing: false,
                closing_ix: None,
            };
        }
        let mut from = to.clone();
        from.remove(inserted_ix.min(from.len().saturating_sub(1)));
        Self::opening(from, to, inserted_ix)
    }

    pub(crate) fn opening(mut from: Vec<f32>, mut to: Vec<f32>, inserted_ix: usize) -> Self {
        normalize_split_shares_allow_zero(&mut from);
        normalize_split_shares_allow_zero(&mut to);
        let mut from_gap_factors = Self::gap_targets(&from);
        if from.len() + 1 == to.len() && inserted_ix <= from.len() {
            let start = 0.0;
            let old_sum = from.iter().sum::<f32>().max(f32::EPSILON);
            let scale = (to.len() as f32 - start) / old_sum;
            for share in &mut from {
                *share *= scale;
            }
            from.insert(inserted_ix, start);
            from_gap_factors.insert(
                inserted_ix.saturating_sub(1).min(from_gap_factors.len()),
                0.0,
            );
        } else {
            from = to.clone();
            from_gap_factors = Self::gap_targets(&from);
        }
        normalize_split_shares_allow_zero(&mut from);
        Self {
            from,
            to,
            from_gap_factors,
            close_duration: None,
            started: Instant::now(),
            closing: false,
            closing_ix: None,
        }
    }

    pub(crate) fn closing(from: Vec<f32>, closing_ix: usize) -> Self {
        Self::closing_many(from, &[closing_ix])
    }

    pub(crate) fn closing_many(mut from: Vec<f32>, closing_indices: &[usize]) -> Self {
        normalize_split_shares_allow_zero(&mut from);
        let mut to = from.clone();
        for &ix in closing_indices {
            if let Some(share) = to.get_mut(ix) {
                *share = 0.0;
            }
        }
        // Every survivor absorbs space proportionally; an interrupted close
        // retains its visible share in `from` and stays at zero in `to`.
        normalize_split_shares_allow_zero(&mut to);
        let from_gap_factors = Self::gap_targets(&from);
        Self {
            from,
            to,
            from_gap_factors,
            close_duration: None,
            started: Instant::now(),
            closing: true,
            closing_ix: closing_indices.last().copied(),
        }
    }

    pub(crate) fn shares(&self) -> Vec<f32> {
        let eased = self.progress();
        let mut shares = self
            .from
            .iter()
            .zip(&self.to)
            .map(|(from, to)| from + (to - from) * eased)
            .collect::<Vec<_>>();
        normalize_split_shares_allow_zero(&mut shares);
        shares
    }

    pub(crate) fn gap_targets(shares: &[f32]) -> Vec<f32> {
        let mut gaps = vec![1.0; shares.len().saturating_sub(1)];
        for (ix, share) in shares.iter().enumerate() {
            if *share == 0.0 && !gaps.is_empty() {
                let adjacent = ix.min(gaps.len() - 1);
                // A run of closing panes must collapse one distinct gap each.
                // In particular, the last two slots share the same adjacent gap.
                let gap_ix = (0..=adjacent)
                    .rev()
                    .chain(adjacent + 1..gaps.len())
                    .find(|gap| gaps[*gap] != 0.0);
                if let Some(gap_ix) = gap_ix {
                    gaps[gap_ix] = 0.0;
                }
            }
        }
        gaps
    }

    pub(crate) fn gap_factors(&self) -> Vec<f32> {
        let progress = self.progress();
        self.from_gap_factors
            .iter()
            .zip(Self::gap_targets(&self.to))
            .map(|(from, to)| from + (to - from) * progress)
            .collect()
    }

    /// Visual progress for paint-only FLIP. Layout uses `to` immediately; only
    /// the cached scene moves from its previous origin to the final origin.
    pub(crate) fn progress(&self) -> f32 {
        let duration = self.duration();
        if duration.is_zero() {
            return 1.0;
        }
        let t = (self.started.elapsed().as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0);
        if self.closing {
            Self::closing_progress(t)
        } else {
            1.0 - (1.0 - t).powi(3)
        }
    }

    fn duration(&self) -> Duration {
        if self.closing {
            self.close_duration.unwrap_or(Self::CLOSE_DURATION)
        } else {
            Self::DURATION
        }
    }

    pub(crate) fn retarget_close_deadline(&mut self, previous: &Self) {
        if self.closing && previous.closing && !previous.done() {
            self.close_duration = Some(
                previous
                    .duration()
                    .saturating_sub(previous.started.elapsed())
                    .max(Duration::from_millis(32)),
            );
        }
    }

    /// Rebase a live transition when an already covered pane is removed. The
    /// remaining geometry and dividers start at their current painted extents.
    pub(crate) fn remove_child(&mut self, ix: usize) {
        if ix >= self.to.len() {
            return;
        }
        let remaining = self.duration().saturating_sub(self.started.elapsed());
        let mut shares = self.shares();
        let mut gaps = self.gap_factors();
        if !gaps.is_empty() {
            let adjacent = ix.min(gaps.len() - 1);
            let gap = (0..=adjacent)
                .rev()
                .chain(adjacent + 1..gaps.len())
                .find(|gap| gaps[*gap] <= f32::EPSILON)
                .unwrap_or(adjacent);
            gaps.remove(gap);
        }
        shares.remove(ix);
        self.to.remove(ix);
        normalize_split_shares_allow_zero(&mut shares);
        normalize_split_shares_allow_zero(&mut self.to);
        self.from = shares;
        self.from_gap_factors = gaps;
        self.closing_ix = self.to.iter().position(|share| *share == 0.0);
        self.started = Instant::now();
        if self.closing {
            self.close_duration = Some(remaining);
        }
    }

    /// Progress used by both the closing split geometry and its visual treatment.
    /// Keeping this in one place prevents the pane from fading out before its slot
    /// has finished collapsing.
    pub(crate) fn closing_progress(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        // Respond immediately and decelerate into settlement. Reversing the
        // entrance's direction must not leave the exit nearly still for 90 ms.
        1.0 - (1.0 - t).powi(3)
    }

    pub(crate) fn done(&self) -> bool {
        self.started.elapsed() >= self.duration()
    }
}

pub(crate) fn normalize_split_shares(shares: &mut [f32]) {
    if shares.is_empty() {
        return;
    }
    for share in shares.iter_mut() {
        if !share.is_finite() || *share <= 0.0 {
            *share = 1.0;
        }
    }
    let sum = shares.iter().sum::<f32>();
    if sum <= f32::EPSILON {
        shares.fill(1.0);
        return;
    }
    let max = shares.len() as f32;
    let scale = max / sum;
    for share in shares {
        *share = (*share * scale).clamp_lenient(0.01, max);
    }
}

/// Normalize split weights while preserving intentional zero-width slots used
/// during pane reveal/close-out. Regular layout recovery still uses
/// [`normalize_split_shares`], which replaces zeroes with sane defaults.
pub(crate) fn normalize_split_shares_allow_zero(shares: &mut [f32]) {
    if shares.is_empty() {
        return;
    }
    for share in shares.iter_mut() {
        if !share.is_finite() || *share < 0.0 {
            *share = 0.0;
        }
    }
    let sum = shares
        .iter()
        .copied()
        .filter(|share| *share > 0.0)
        .sum::<f32>();
    if sum <= f32::EPSILON {
        shares.fill(1.0);
        return;
    }
    let scale = shares.len() as f32 / sum;
    for share in shares {
        *share = (*share * scale).max(0.0);
    }
}

fn split_spring(t: f32) -> f32 {
    let omega: f32 = 22.0;
    let zeta: f32 = 0.68;
    let wd = omega * (1.0 - zeta * zeta).sqrt();
    let e = (-zeta * omega * t).exp();
    1.0 - e * ((wd * t).cos() + (zeta * omega / wd) * (wd * t).sin())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_animation_normalizes_runaway_shares() {
        let anim = DockSplitAnimation::closing(vec![8_430_453.0, 7_901_766.0, 168_608_530.0], 2);

        assert!(anim.from.iter().all(|share| share.is_finite()));
        assert!(anim.to.iter().all(|share| share.is_finite()));
        assert!(anim.from.iter().all(|share| *share <= 3.0));
        assert!(anim.to.iter().all(|share| *share <= 3.0));
    }

    #[test]
    fn split_animation_replaces_bad_shares() {
        let anim = DockSplitAnimation::opening(vec![f32::NAN], vec![f32::INFINITY, -1.0], 1);

        assert!(anim
            .from
            .iter()
            .chain(&anim.to)
            .all(|share| share.is_finite()));
        assert!(anim.from.iter().all(|share| *share >= 0.0));
        assert!(anim.to.iter().all(|share| *share > 0.0));
        assert_eq!(anim.from[1], 0.0, "new pane starts completely covered");
    }

    #[test]
    fn split_motion_settles_exactly_and_close_finishes_sooner() {
        let mut opening = DockSplitAnimation::new(vec![1.0, 1.0], 1);
        assert_eq!(opening.from[1], 0.0);
        assert!(opening.from[0] > 0.0);
        opening.started = Instant::now() - DockSplitAnimation::DURATION;
        assert!(opening.done());
        assert_eq!(opening.progress(), 1.0);
        assert_eq!(opening.shares(), opening.to);

        let mut closing = DockSplitAnimation::closing(vec![1.0, 1.0], 1);
        assert!(closing.duration() < opening.duration());
        closing.started = Instant::now() - DockSplitAnimation::CLOSE_DURATION;
        assert!(closing.done());
        assert_eq!(closing.shares()[1], 0.0);
        let mut previous = 0.0;
        for step in 0..=100 {
            let progress = DockSplitAnimation::closing_progress(step as f32 / 100.0);
            assert!(progress >= previous && progress <= 1.0);
            previous = progress;
        }
    }

    #[test]
    fn split_close_distributes_space_across_all_survivors() {
        let anim = DockSplitAnimation::closing(vec![1.0, 1.0, 1.0], 2);

        assert!((anim.to[0] - 1.5).abs() < 0.001);
        assert!((anim.to[1] - 1.5).abs() < 0.001);
        assert_eq!(anim.to[2], 0.0);
    }

    #[test]
    fn split_close_preserves_survivor_proportions() {
        let anim = DockSplitAnimation::closing(vec![1.0, 2.0, 3.0], 2);

        assert!((anim.to[1] / anim.to[0] - 2.0).abs() < 0.001);
    }

    #[test]
    fn rapid_close_keeps_pending_panes_and_current_geometry() {
        let from = vec![1.4, 1.1, 0.5, 1.0];
        let anim = DockSplitAnimation::closing_many(from.clone(), &[2, 3]);
        assert_eq!(anim.from, from);
        assert_eq!(anim.to[2], 0.0);
        assert_eq!(anim.to[3], 0.0);
        assert!((anim.to[0] / anim.to[1] - 1.4 / 1.1).abs() < 0.001);
    }

    #[test]
    fn rapid_split_does_not_resurrect_zero_width_panes() {
        let from = vec![2.0, 0.0];
        let anim = DockSplitAnimation::opening(from, vec![1.5, 0.0, 1.5], 2);
        assert_eq!(anim.from, vec![3.0, 0.0, 0.0]);
        assert_eq!(anim.to[1], 0.0);
    }

    #[test]
    fn closing_responds_immediately_and_does_not_wait_until_the_end() {
        assert!(DockSplitAnimation::closing_progress(0.25) > 0.5);
        assert!(DockSplitAnimation::closing_progress(0.5) > 0.85);
        assert_eq!(DockSplitAnimation::closing_progress(1.0), 1.0);
        let mut closing = DockSplitAnimation::closing(vec![1.0, 1.0], 1);
        closing.close_duration = Some(Duration::ZERO);
        assert_eq!(closing.progress(), 1.0);
        assert_eq!(closing.shares(), closing.to);
    }

    #[test]
    fn retargeted_dividers_keep_fractional_width_and_settle_to_zero() {
        let mut closing = DockSplitAnimation::closing_many(vec![1.0, 1.0, 1.0, 1.0], &[2, 3]);
        closing.from_gap_factors = vec![1.0, 0.4, 0.7];
        closing.started = Instant::now() + Duration::from_secs(1);
        assert_eq!(closing.gap_factors(), vec![1.0, 0.4, 0.7]);
        closing.started = Instant::now() - DockSplitAnimation::CLOSE_DURATION;
        assert_eq!(closing.gap_factors(), vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn middle_split_adds_exactly_one_zero_width_divider() {
        let opening = DockSplitAnimation::opening(vec![1.0, 1.0], vec![1.0, 1.0, 1.0], 1);
        assert_eq!(opening.from_gap_factors, vec![0.0, 1.0]);
        assert_eq!(
            DockSplitAnimation::gap_targets(&[0.0, 0.0, 1.0, 1.0]),
            vec![0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn close_bursts_keep_a_bounded_deadline() {
        let mut previous = DockSplitAnimation::closing(vec![1.0, 1.0, 1.0], 2);
        previous.started = Instant::now() - Duration::from_millis(100);
        let mut next = DockSplitAnimation::closing_many(previous.shares(), &[1, 2]);
        next.retarget_close_deadline(&previous);
        assert!(next.duration() <= Duration::from_millis(80));
        assert!(next.duration() >= Duration::from_millis(32));
    }

    #[test]
    fn removing_covered_slot_preserves_the_remaining_geometry() {
        let mut anim = DockSplitAnimation::closing_many(vec![1.8, 1.2, 0.0], &[1, 2]);
        anim.started = Instant::now() + Duration::from_secs(1);
        anim.remove_child(2);
        assert_eq!(anim.from.len(), 2);
        assert_eq!(anim.to.len(), 2);
        assert!((anim.from[0] / anim.from[1] - 1.5).abs() < 0.001);
        assert_eq!(anim.from_gap_factors, vec![1.0]);
        assert_eq!(anim.to[1], 0.0);
    }

    #[test]
    fn list_columns_move_by_one_column_without_wrapping() {
        // 3 rows, 8 items: columns [0,1,2] [3,4,5] [6,7].
        assert_eq!(list_column_left(4, 3), 1);
        assert_eq!(list_column_left(1, 3), 1);
        assert_eq!(list_column_right(1, 8, 3), 4);
        assert_eq!(list_column_right(4, 8, 3), 7);
        assert_eq!(list_column_right(5, 8, 3), 7);
        assert_eq!(list_column_right(7, 8, 3), 7);
        assert_eq!(list_column_right(6, 8, 3), 6);
        assert_eq!(list_column_right(0, 0, 3), 0);
    }

    #[test]
    fn grid_hit_ignores_pitch_gaps() {
        let model = GridHitModel {
            presented: Vec::new(),
            bounds: gpui::Bounds {
                origin: gpui::point(gpui::px(10.0), gpui::px(20.0)),
                size: gpui::size(gpui::px(220.0), gpui::px(120.0)),
            },
            pad_x: 0.0,
            pad_y: 0.0,
            scroll_x: 0.0,
            scroll_y: 0.0,
            cell_w: 100.0,
            cell_h: 80.0,
            cols: 2,
            major_rows: 0,
            pitch_y: 110.0,
            pitch_x: 110.0,
            visible: std::sync::Arc::new(vec![4, 5, 6, 7]),
            start_idx: 0,
            end_idx: 4,
            total_count: 4,
            bands: Default::default(),
        };

        assert_eq!(
            model.hit_entry(gpui::point(gpui::px(15.0), gpui::px(25.0))),
            Some(4)
        );
        assert_eq!(
            model.hit_entry(gpui::point(gpui::px(112.0), gpui::px(25.0))),
            None
        );
        assert_eq!(
            model.hit_entry(gpui::point(gpui::px(15.0), gpui::px(125.0))),
            None
        );
    }

    #[test]
    fn animated_cells_hit_the_painted_identity_including_overlap_and_content_offsets() {
        use gpui::{point, px, size, Bounds};
        let a = Bounds::new(point(px(130.0), px(40.0)), size(px(100.0), px(80.0)));
        let b = Bounds::new(point(px(80.0), px(40.0)), size(px(100.0), px(80.0)));
        let model = GridHitModel {
            presented: vec![(0, a), (1, b)],
            bounds: Bounds::new(point(px(10.0), px(20.0)), size(px(300.0), px(200.0))),
            pad_x: 0.0,
            pad_y: 0.0,
            scroll_x: 0.0,
            scroll_y: 30.0,
            cell_w: 100.0,
            cell_h: 80.0,
            cols: 2,
            major_rows: 0,
            pitch_y: 110.0,
            pitch_x: 110.0,
            visible: std::sync::Arc::new(vec![4, 5]),
            start_idx: 0,
            end_idx: 2,
            total_count: 2,
            bands: Default::default(),
        };
        assert_eq!(model.hit_entry(point(px(150.0), px(50.0))), Some(5));
        assert_eq!(
            model.hit_entry_at_content_offset(point(px(140.0), px(60.0))),
            Some(5)
        );
        assert_eq!(model.hit_entry(point(px(210.0), px(50.0))), Some(4));
        assert_eq!(model.hit_entry(point(px(20.0), px(50.0))), None);
        assert_eq!(model.cell_bounds(0), a);
    }
}

/// User widths of the Details columns. The renderer may shrink these transiently
/// in a narrow pane, but never writes that responsive projection back to prefs.
/// Serialized into ProfilePrefs via `DetailsColumns: ProfileScoped`.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) struct ColWidths {
    /// Name column width (icon + text), in real pixels.
    #[serde(default = "default_name_width")]
    pub(crate) name: f32,
    pub(crate) size: f32,
    pub(crate) modified: f32,
    pub(crate) type_w: f32,
    /// Pane width at which these proportions were edited. Absent in legacy prefs.
    #[serde(default)]
    pub(crate) reference_width: Option<f32>,
}

impl ColWidths {
    pub(crate) fn projected(self, available: f32) -> Self {
        let factor = self
            .reference_width
            .filter(|width| width.is_finite() && *width > 0.0)
            .map_or(1.0, |width| available.max(0.0) / width);
        Self {
            name: self.name * factor,
            size: self.size * factor,
            modified: self.modified * factor,
            type_w: self.type_w * factor,
            reference_width: None,
        }
    }

    pub(crate) fn resize(&mut self, column: &str, pixels: f32, available: f32) {
        if !available.is_finite() || available <= 0.0 {
            return;
        }
        let index = match column {
            "name" => 0,
            "size" => 1,
            "modified" => 2,
            "type" => 3,
            _ => return,
        };
        let projected = self.projected(available);
        let mut values = [
            projected.name,
            projected.size,
            projected.modified,
            projected.type_w,
        ];
        let total: f32 = values.iter().sum();
        if total > 0.0 {
            for value in &mut values {
                *value *= available / total;
            }
        }
        let neighbor = if index < 3 { index + 1 } else { index - 1 };
        let combined = values[index] + values[neighbor];
        let minima: [f32; 4] = [120.0, 46.0, 92.0, 56.0];
        let minimum = minima[index].min(combined * 0.5);
        let maximum = (combined - minima[neighbor]).max(minimum);
        values[index] = pixels.clamp_lenient(minimum, maximum);
        values[neighbor] = combined - values[index];
        self.name = values[0];
        self.size = values[1];
        self.modified = values[2];
        self.type_w = values[3];
        self.reference_width = Some(available);
    }
}

fn default_name_width() -> f32 {
    300.
}

impl Default for ColWidths {
    fn default() -> Self {
        Self {
            name: default_name_width(),
            size: 90.,
            modified: 150.,
            type_w: 110.,
            reference_width: Some(650.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum DetailsColumn {
    Size,
    Modified,
    Type,
}

impl DetailsColumn {
    pub(crate) const DEFAULT_ORDER: [Self; 3] = [Self::Size, Self::Modified, Self::Type];

    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Size => "size",
            Self::Modified => "modified",
            Self::Type => "type",
        }
    }
}

/// Per-profile Details column preferences: widths + visual order. Persisted as a
/// single `ProfileScoped` blob so widths and order travel together.
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) struct DetailsColumnPrefs {
    pub(crate) widths: ColWidths,
    pub(crate) order: [DetailsColumn; 3],
}

impl Default for DetailsColumnPrefs {
    fn default() -> Self {
        Self {
            widths: ColWidths::default(),
            order: DetailsColumn::DEFAULT_ORDER,
        }
    }
}

/// Active column-resize drag state for the Details header.
pub(crate) struct ColResizeState {
    /// Which column: "size", "modified", or "type".
    pub(crate) col: &'static str,
    /// Pane the resize started on — only this pane is re-rendered live (scoped
    /// `notify_pane`); other Details panes catch up on mouse-up.
    pub(crate) pane: TileId,
    pub(crate) start_x: f32,
    pub(crate) start_width: f32,
}

pub(crate) struct DockResizeState {
    pub(crate) container: TileId,
    pub(crate) divider: usize,
    pub(crate) dir: atlas_model::layout_tree::LinearDir,
    pub(crate) start_pos: f32,
    pub(crate) span_px: f32,
    pub(crate) start_shares: Vec<f32>,
}

pub(crate) struct SidebarResizeState {
    pub(crate) start_x: f32,
    pub(crate) start_width: f32,
}

pub(crate) struct InspectorResizeState {
    pub(crate) start_x: f32,
    pub(crate) start_width: f32,
}

/// Per-tab interaction state for the inspector image preview. `zoom` is a multiplier
/// over the fit-to-panel size (1.0 = fit). `expanded` widens the inspector to most of
/// the window (toggled with Space). Reset on each new selection (except `expanded`,
/// which persists so browsing in the big view keeps it open).
#[derive(Clone, Copy)]
pub(crate) struct PreviewView {
    pub(crate) zoom: f32,
    pub(crate) pan_x: f32,
    pub(crate) pan_y: f32,
    pub(crate) expanded: bool,
    pub(crate) details_open: bool,
    pub(crate) viewport: Option<gpui::Bounds<gpui::Pixels>>,
    pub(crate) minimap_drag: bool,
    /// Multi-selection carousel: front card index, pointer-down x while
    /// swiping, current swipe offset and accumulated wheel delta.
    pub(crate) stack_index: usize,
    pub(crate) stack_drag: Option<f32>,
    pub(crate) stack_dx: f32,
    pub(crate) stack_wheel: f32,
}

impl Default for PreviewView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            expanded: false,
            details_open: false,
            viewport: None,
            minimap_drag: false,
            stack_index: 0,
            stack_drag: None,
            stack_dx: 0.0,
            stack_wheel: 0.0,
        }
    }
}

/// Per-tab code/text editor state for the inspector preview. Holds the native
/// `InputState` in code-editor mode, the source path for saving, a dirty flag
/// (set on any change), whether the editor is in editable mode, and the language
/// token (so the toolbar can display it).
pub(crate) struct PreviewEditor {
    /// The gpui-component `InputState` in `code_editor(lang)` mode.
    pub(crate) state: Entity<InputState>,
    /// Absolute path of the file being previewed/edited.
    pub(crate) path: PathBuf,
    /// Most recent text content, kept current by the Change subscription.
    /// Used by `SavePreviewFile` without needing `cx`.
    pub(crate) cached_text: String,
    /// True when the content has changed since last save (or since load).
    pub(crate) dirty: bool,
    /// When false the editor is rendered `.disabled(true)` — read-only with highlight.
    pub(crate) editing: bool,
    /// Current line-number visibility (mirrors the profile pref). The render path
    /// syncs this into the `InputState` when `ln_needs_sync` is set.
    pub(crate) line_numbers: bool,
    /// Set by `TogglePreviewLineNumbers`; cleared by the render after syncing.
    pub(crate) ln_needs_sync: bool,
    /// Change subscription — kept alive as long as the editor is open.
    pub(crate) _sub: gpui::Subscription,
}

/// In-flight inline rename of a single row in the focused pane.
pub struct RenameEdit {
    pub(crate) pane: TileId,
    pub(crate) path: PathBuf,
    pub(crate) input: Entity<InputState>,
    /// When set, the next focus-sync selects this many chars from the start
    /// (to pre-highlight the stem; None after consumed).
    pub(crate) initial_select_chars: Option<u32>,
}

/// In-flight rubber-band selection.
pub struct RubberDrag {
    pub(crate) target: RubberTarget,
    pub(crate) target_bounds: gpui::Bounds<gpui::Pixels>,
    pub(crate) origin: gpui::Point<gpui::Pixels>,
    pub(crate) current: gpui::Point<gpui::Pixels>,
    pub(crate) content_origin: gpui::Point<gpui::Pixels>,
    /// Selection before the drag began (kept when Ctrl/Cmd makes the drag additive).
    pub(crate) base: atlas_model::tab::SelectionState,
    pub(crate) dropzone_base: atlas_model::tab::SelectionState<PathBuf>,
    pub(crate) additive: bool,
    /// True once the cursor has moved at least 4 px from origin. Prevents a plain
    /// click from drawing a marquee or clearing the selection prematurely.
    pub(crate) started: bool,
    /// A selected group collapses only on a click release, never on drag press.
    pub(crate) selected_click: Option<usize>,
    /// Rubber-band scrolling is deliberately stricter than drag-and-drop: the
    /// pointer must move toward an edge after the marquee has started.
    pub(crate) edge_intent: Option<crate::ui::edge_autoscroll::EdgeDirection>,
    pub(crate) edge_intent_origin_y: Option<gpui::Pixels>,
    pub(crate) last_scroll_frame: Option<std::time::Instant>,
}

#[derive(Clone, Debug)]
pub(crate) enum SidebarHit {
    SectionHeader(usize),
    NavRow(std::path::PathBuf),
    VirtualNavRow(atlas_model::location::Location),
    TagRow(atlas_model::tag_cache::TagId),
    DriveRow(std::path::PathBuf),
    CloudRow(std::path::PathBuf),
}

#[derive(Clone, Default)]
pub(crate) struct SidebarHitModel {
    pub(crate) bounds: gpui::Bounds<gpui::Pixels>,
    pub(crate) zones: Vec<(gpui::Bounds<gpui::Pixels>, SidebarHit)>,
}

impl SidebarHitModel {
    pub(crate) fn hit(&self, pos: gpui::Point<gpui::Pixels>) -> Option<SidebarHit> {
        if !self.bounds.contains(&pos) {
            return None;
        }
        for (rect, hit) in &self.zones {
            if rect.contains(&pos) {
                return Some(hit.clone());
            }
        }
        None
    }
}

#[derive(Clone)]
pub(crate) struct DetailsHitModel {
    pub(crate) bounds: gpui::Bounds<gpui::Pixels>,
    pub(crate) scroll_y: f32,
    pub(crate) row_h: f32,
    pub(crate) visible: std::sync::Arc<Vec<usize>>,
    pub(crate) start_idx: usize,
    pub(crate) end_idx: usize,
    pub(crate) total_count: usize,
    /// Content-space row positions while sorting; empty for the O(1) idle path.
    pub(crate) presented_rows: Vec<(usize, f32)>,
}

impl DetailsHitModel {
    pub(crate) fn hit_visible_pos(&self, pos: gpui::Point<gpui::Pixels>) -> Option<usize> {
        if !self.bounds.contains(&pos) {
            return None;
        }
        let y = f32::from(pos.y) - f32::from(self.bounds.top()) + self.scroll_y;
        if y < 0.0 {
            return None;
        }
        if !self.presented_rows.is_empty() {
            return self
                .presented_rows
                .iter()
                .rev()
                .find(|(_, top)| y >= *top && y < *top + self.row_h)
                .map(|(row, _)| *row);
        }
        let visible_pos = (y / self.row_h).floor() as usize;
        (visible_pos >= self.start_idx
            && visible_pos < self.end_idx
            && visible_pos < self.total_count)
            .then_some(visible_pos)
    }

    pub(crate) fn hit_entry(&self, pos: gpui::Point<gpui::Pixels>) -> Option<usize> {
        self.hit_visible_pos(pos).and_then(|visible_pos| {
            self.visible
                .get(visible_pos)
                .copied()
                .filter(|cell| !atlas_model::grouping::is_virtual_cell(*cell))
        })
    }

    pub(crate) fn hit_entry_at_content_offset(
        &self,
        offset: gpui::Point<gpui::Pixels>,
    ) -> Option<usize> {
        let y = f32::from(offset.y);
        if y < 0.0 {
            return None;
        }
        if !self.presented_rows.is_empty() {
            return self
                .presented_rows
                .iter()
                .rev()
                .find(|(_, top)| y >= *top && y < *top + self.row_h)
                .and_then(|(row, _)| {
                    self.visible
                        .get(*row)
                        .copied()
                        .filter(|cell| !atlas_model::grouping::is_virtual_cell(*cell))
                });
        }
        let visible_pos = (y / self.row_h).floor() as usize;
        (visible_pos < self.total_count)
            .then_some(visible_pos)
            .and_then(|visible_pos| {
                self.visible
                    .get(visible_pos)
                    .copied()
                    .filter(|cell| !atlas_model::grouping::is_virtual_cell(*cell))
            })
    }

    pub(crate) fn row_bounds(&self, visible_pos: usize) -> gpui::Bounds<gpui::Pixels> {
        let y = self
            .presented_rows
            .iter()
            .find(|(row, _)| *row == visible_pos)
            .map_or(visible_pos as f32 * self.row_h, |(_, y)| *y);
        gpui::Bounds {
            origin: gpui::point(
                self.bounds.origin.x,
                self.bounds.origin.y + gpui::px(y - self.scroll_y),
            ),
            size: gpui::size(self.bounds.size.width, gpui::px(self.row_h)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RubberTarget {
    Pane(TileId),
    MillerColumn(TileId, usize),
    Dropzone,
}

// Phase5: pub(crate) so the gpui inspector module (crate::ui::inspector) can reuse
// the same debounce as the legacy egui sync path.
pub const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(90);
/// How often the foreground loop drains background results. Keep this out of the
/// display-refresh path: waking the UI thread at 60 Hz made window moves stutter on
/// Windows even when there was no work to paint.
// How long a palette's exit animation runs before its view is torn down. Must be ≥
// `palette::PALETTE_CLOSE_MS` so the fade-out completes (it reaches opacity 0 first).
const PALETTE_CLOSE_FINALIZE: Duration = Duration::from_millis(150);
// How long a pane's close-out animation runs before it's removed from the dock.
pub(crate) const PANE_CLOSE_FINALIZE: Duration = DockSplitAnimation::CLOSE_DURATION;
pub(crate) const TAB_CLOSE_FINALIZE: Duration = Duration::from_millis(crate::ui::motion::TAB_MS);

/// Map the user's scroll-smoothness setting (0 = instant … 1 = silky) to the gpui
/// scroll handle's (ease, boost). The backend converts the 60 Hz reference ease
/// into an exponential decay constant and integrates actual elapsed time.
pub(crate) fn scroll_smooth_params(smoothness: f32) -> (f32, f32) {
    let s = smoothness.clamp(0.0, 1.0);
    if s <= 0.0 {
        return (0.0, 1.0); // off → instant (ease 0 disables smoothing in gpui)
    }
    // High initial responsiveness: at the default s = 0.6, lambda ≈ 54.4 gives
    // ease ≈ 0.467, covering nearly half the travel on the very first frame (zero
    // perceived input lag) and settling cleanly in ~110-130 ms instead of creeping.
    // Boost scales gently (1.0 -> 1.35) so smooth scrolling feels effortless
    // without requiring excessive wheel turns to navigate large listings.
    let lambda = 82.0 - 46.0 * s;
    let ease = 1.0 - (-lambda / 60.0).exp2();
    let boost = 1.0 + 0.35 * s;
    (ease, boost)
}
// Window after a fresh listing appears during which item rows stagger-animate in.
// Must comfortably exceed the longest staggered row animation; past it, scrolling
// never re-triggers the appear.
#[allow(dead_code)] // reason: only read by the parked `pane_item_appear_seq`
pub(crate) const ITEM_APPEAR_WINDOW: Duration = Duration::from_millis(150);

impl App {
    /// True while any dock split, close, tab, or sidebar animation is actively playing.
    pub(crate) fn is_pane_or_dock_animating(&self) -> bool {
        !self.dock_split_animations.is_empty()
            || !self.pane_animations.is_empty()
            || !self.pane_closing.is_empty()
            || !self.tab_closing.is_empty()
            || self.sidebar_width_animating
            || self.inspector_animating
    }

    /// Record a pane whose column calculation changed during an animation, deferring
    /// its `notify_pane` until the motion settles to eliminate prepaint frame spikes.
    pub(crate) fn defer_column_reflow(&mut self, pane_id: TileId) {
        self.pending_column_reflow.insert(pane_id);
    }

    /// Flush any deferred column reflows once animations finish.
    pub(crate) fn flush_pending_column_reflows(&mut self, cx: &mut gpui::Context<Self>) {
        if self.pending_column_reflow.is_empty() {
            return;
        }
        let pending: Vec<TileId> = self.pending_column_reflow.drain().collect();
        crate::perf::event(|| format!("column_reflow.flush panes={}", pending.len()));
        for pane_id in pending {
            self.notify_pane(pane_id, cx);
        }
    }

    /// Notify only the signal entity for `pane_id`. The `PaneContentView` for that
    /// pane observes this signal, so only it re-renders — not every other pane.
    pub(crate) fn notify_pane(&self, pane_id: TileId, cx: &mut gpui::Context<Self>) {
        let _perf = crate::perf::span_detail("notify_pane", || format!("pane={}", pane_id.0));
        crate::perf::event(|| format!("notify_pane pane={}", pane_id.0));
        if let Some(signal) = self.pane_signals.get(&pane_id) {
            signal.update(cx, |_, cx| cx.notify());
        }
    }

    /// Notify all pane signals at once — used for global changes (settings, theme,
    /// icon cache updates) that affect every pane simultaneously.
    /// Selection fill/text colors for `pane`. Unfocused panes use a neutral,
    /// unemphasized selection (macOS-style) so the active pane is obvious.
    pub(crate) fn selection_colors(
        &self,
        pane: TileId,
        accent: gpui::Hsla,
        accent_fg: gpui::Hsla,
        fg: gpui::Hsla,
        muted: gpui::Hsla,
    ) -> (gpui::Hsla, gpui::Hsla) {
        if pane == self.state.focused_tile {
            (accent, accent_fg)
        } else {
            (muted.opacity(0.30), fg)
        }
    }

    /// Repaint the previously and newly focused panes after a focus change.
    pub(crate) fn sync_focus_paint(&mut self, cx: &mut gpui::Context<Self>) {
        let focused = self.state.focused_tile;
        if self.focus_painted == Some(focused) {
            return;
        }
        if let Some(previous) = self.focus_painted.replace(focused) {
            self.notify_pane(previous, cx);
        }
        self.notify_pane(focused, cx);
    }

    pub(crate) fn notify_all_panes(&self, cx: &mut gpui::Context<Self>) {
        let _perf = crate::perf::span_detail("notify_all_panes", || {
            format!("signals={}", self.pane_signals.len())
        });
        crate::perf::event(|| format!("notify_all_panes signals={}", self.pane_signals.len()));
        for signal in self.pane_signals.values() {
            signal.update(cx, |_, cx| cx.notify());
        }
    }

    /// Repaint panes navigated via `navigate_to` (which has no `cx`). `navigate_to`
    /// updates `pane.location` (chrome/breadcrumb show the new route) and marks the
    /// tile in `dirty_panes`; this flushes those into `notify_pane` so the file view
    /// re-renders in lockstep with the route. Also notifies App (chrome + sidebar
    /// via its `observe`). Call from any path holding a `Context` after navigating.
    pub(crate) fn flush_dirty_panes(&mut self, cx: &mut gpui::Context<Self>) {
        let _perf = crate::perf::span_detail("flush_dirty_panes", || {
            format!(
                "dirty={} dirty_all={}",
                self.dirty_panes.len(),
                self.dirty_all_panes
            )
        });
        if self.dirty_all_panes {
            self.dirty_all_panes = false;
            self.notify_all_panes(cx);
            cx.notify();
            return;
        }
        if self.dirty_panes.is_empty() {
            cx.notify();
            return;
        }
        let dirty: Vec<TileId> = self.dirty_panes.drain().collect();
        for id in dirty {
            self.notify_pane(id, cx);
        }
        cx.notify();
    }

    /// Return (creating lazily) the signal entity for a pane. `PaneContentView`
    /// calls this once at construction to get the entity to observe.
    pub(crate) fn pane_signal_for(
        &mut self,
        pane_id: TileId,
        cx: &mut gpui::Context<Self>,
    ) -> gpui::Entity<PaneSignal> {
        self.pane_signals
            .entry(pane_id)
            .or_insert_with(|| cx.new(|_| PaneSignal))
            .clone()
    }

    /// Remove the signal entity when a pane is closed (no-op if already gone).
    pub(crate) fn drop_pane_signal(&mut self, pane_id: TileId) {
        self.pane_signals.remove(&pane_id);
        self.pane_view_transitions.remove(&pane_id);
    }

    /// If `pane_id` is within its item-appear window, return the `appear_seq` to key
    /// the row animation on (changes per fresh listing, so it plays once). `None`
    /// once the window elapses, so scrolling never re-triggers the cascade.
    // reason: stagger-appear parked — views stopped calling it during the perf
    // work; the arming machinery stays live so re-enabling is one call site.
    #[allow(dead_code)]
    pub(crate) fn pane_item_appear_seq(&self, pane_id: TileId) -> Option<u64> {
        if self.state.settings.disable_animations {
            return None;
        }
        let started = self.pane_items_appear.get(&pane_id)?;
        (started.elapsed() < ITEM_APPEAR_WINDOW)
            .then(|| self.state.tab_states.get(&pane_id).map(|ts| ts.appear_seq))
            .flatten()
    }

    pub(crate) fn pane_animation_for(&self, pane_id: TileId) -> Option<PaneAnimation> {
        self.pane_animations
            .get(&pane_id)
            .copied()
            .filter(|animation| animation.is_alive())
    }

    pub(crate) fn suppress_window_resize_animations(&self) -> bool {
        self.window_resize_suppress_until
            .is_some_and(|until| Instant::now() < until)
    }

    pub(crate) fn prune_pane_animations(&mut self) {
        self.pane_animations
            .retain(|_, animation| animation.is_alive());
    }

    pub(crate) fn prune_pane_layout_animations(&mut self) {
        self.pane_layout_animations
            .retain(|_, animation| animation.is_alive());
    }

    /// (Re)launch the goto bar's content search for the current query + directory.
    /// Cancels any in-flight search, bumps the generation so its stale results are
    /// ignored, and spawns a fresh grep that streams matches back via `tx`.
    pub(crate) fn relaunch_content_search(&mut self, cx: &mut gpui::Context<Self>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        if let Some(c) = self.content_cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
        self.content_gen = self.content_gen.wrapping_add(1);
        self.content_results.clear();
        let query = self.state.overlay.goto_state.query.clone();
        if query.trim().is_empty() {
            return;
        }
        let root = self
            .state
            .active_tab_data()
            .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default());
        let cancel = Arc::new(AtomicBool::new(false));
        self.content_cancel = Some(cancel.clone());
        let gen = self.content_gen;
        let tx = self.tx.clone();
        cx.spawn(async move |this, cx| {
            smol::Timer::after(Duration::from_millis(150)).await;
            let _ = this.update(cx, |app, _| {
                if app.content_gen != gen {
                    return;
                }
                atlas_core::search::content::search_content(root, query, cancel, move |path| {
                    let _ = tx.send(BgResult::ContentResults {
                        gen,
                        paths: vec![path],
                    });
                });
            });
        })
        .detach();
    }

    /// Start an OS (OLE) drag of `paths` / archive items, with stale-source
    /// validation: files that no longer exist are excluded and reported as
    /// toasts (via the drag-and-drop event queue — this runs under a read borrow).
    #[cfg(windows)]
    pub(crate) fn trigger_os_drag(
        &self,
        paths: Vec<PathBuf>,
        archive_items: Option<Vec<(PathBuf, PathBuf)>>,
    ) {
        let Some(hwnd) = self.hwnd else { return };
        let queue = self.dnd.as_ref().map(|d| d.action_queue.clone());
        let archive_len = archive_items.as_ref().map_or(0, Vec::len);
        let (paths, stale): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.exists());
        tracing::debug!(
            paths = paths.len(),
            stale = stale.len(),
            archive_len,
            "dnd trigger_os_drag"
        );
        if !stale.is_empty() {
            if let Some(q) = queue
                .as_ref()
                .and_then(|q| q.lock().or_else(atlas_model::sync::recover).ok())
            {
                let mut q = q;
                for p in &stale {
                    q.push(crate::dnd::windows::DndEvent::Error(
                        rust_i18n::t!(
                            "app_path_not_exists",
                            path = p.file_name().unwrap_or_default().to_string_lossy()
                        )
                        .to_string(),
                    ));
                }
            }
        }
        if paths.is_empty() && archive_items.as_ref().is_none_or(std::vec::Vec::is_empty) {
            return;
        }
        crate::dnd::windows::trigger_drag(hwnd, paths, archive_items, queue);
    }

    #[cfg(windows)]
    pub(crate) fn trigger_os_drag_copy_only(&self, paths: Vec<PathBuf>) {
        let Some(hwnd) = self.hwnd else { return };
        let queue = self.dnd.as_ref().map(|d| d.action_queue.clone());
        let (paths, stale): (Vec<_>, Vec<_>) = paths.into_iter().partition(|p| p.exists());
        if !stale.is_empty() {
            if let Some(q) = queue
                .as_ref()
                .and_then(|q| q.lock().or_else(atlas_model::sync::recover).ok())
            {
                let mut q = q;
                for p in &stale {
                    q.push(crate::dnd::windows::DndEvent::Error(
                        rust_i18n::t!(
                            "app_path_not_exists",
                            path = p.file_name().unwrap_or_default().to_string_lossy()
                        )
                        .to_string(),
                    ));
                }
            }
        }
        if !paths.is_empty() {
            crate::dnd::windows::trigger_drag_copy_only(hwnd, paths);
        }
    }

    /// Record a pane's window-space bounds so the OLE drop target can route external
    /// drops to the correct pane directory. Called from each pane's `on_prepaint`
    /// closure via the `zone_recorder` helper in `root.rs`.
    ///
    /// No-op on non-Windows (OLE drag-and-drop not used; field always empty).
    pub(crate) fn push_pane_zone(&self, pane_id: TileId, bounds: gpui::Bounds<gpui::Pixels>) {
        #[cfg(windows)]
        if let Some(dest) = self
            .tab_data_for(pane_id)
            .and_then(|t| t.location.fs_dir().map(|p| p.to_path_buf()))
        {
            crate::perf::count("dnd.pending_zone_push");
            self.pending_pane_zones
                .borrow_mut()
                .push(crate::dnd::DropZone {
                    x: f32::from(bounds.origin.x),
                    y: f32::from(bounds.origin.y),
                    w: f32::from(bounds.size.width),
                    h: f32::from(bounds.size.height),
                    dest,
                    pane_id: pane_id.0,
                });
        }
    }

    pub(crate) fn tab_data_for(&self, tab_id: TileId) -> Option<&TabData> {
        match self.state.pane_tree.tiles.get(tab_id) {
            Some(Tile::Pane(p)) => Some(p),
            _ => None,
        }
    }

    /// Re-list every tab touched by a finished background operation.
    pub(crate) fn refresh_tabs(&mut self, tabs: Vec<TileId>) {
        for tab_id in tabs {
            if let Some(tab_data) = self.tab_data_for(tab_id) {
                let loc = tab_data.location.clone();
                let sort_col = tab_data.sort_col.clone();
                let sort_dir = tab_data.sort_dir.clone();
                self.dispatch_list(tab_id, loc, sort_col, sort_dir);
            }
            // Miller cascades hold their own listings; without this they'd show
            // pre-operation contents until the next manual re-init.
            self.refresh_miller_columns(tab_id);
        }
    }

    /// Paths currently selected in the specified tab (for drag-and-drop / operations).
    pub(crate) fn selected_paths_for_pane(&self, pane: TileId) -> Vec<PathBuf> {
        if let Some(paths) = self.miller_selection_paths_for_pane(pane) {
            return paths;
        }
        // Tree selection indexes visible tree rows, not the flat listing; mapping
        // it through `entries` would act on the wrong files once a folder is open.
        let mode = self
            .tab_data_for(pane)
            .map(|tab| tab.view_mode.clone())
            .unwrap_or_default();
        self.state
            .tab_states
            .get(&pane)
            .map(|tab| {
                tab.selected_entries(&mode)
                    .into_iter()
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Paths currently selected in the focused tab (for clipboard / delete shortcuts).
    pub(crate) fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected_paths_for_pane(self.state.focused_tile)
    }

    pub(crate) fn selected_paths_with_target_names(
        &self,
    ) -> (Vec<PathBuf>, Option<Vec<std::ffi::OsString>>) {
        // Miller columns never list recycle-bin payloads, so no target names.
        if let Some(paths) = self.miller_focused_selection_paths() {
            return (paths, None);
        }
        let Some(tab) = self.state.active_tab_state() else {
            return (Vec::new(), None);
        };
        let mode = self
            .tab_data_for(self.state.focused_tile)
            .map(|tab| tab.view_mode.clone())
            .unwrap_or_default();
        let entries = tab.selected_entries(&mode);
        let paths = entries.iter().map(|entry| entry.path()).collect();
        let names: Option<Vec<_>> = entries
            .iter()
            .map(|entry| {
                entry
                    .recycle_info()
                    .as_ref()
                    .and_then(|info| info.original_path.file_name())
                    .map(std::ffi::OsStr::to_os_string)
            })
            .collect();
        (paths, names)
    }

    /// Get-or-create the virtualized-list scroll handle for a pane.
    pub(crate) fn scroll_handle_for(&mut self, tile: TileId) -> UniformListScrollHandle {
        let handle = self.scroll_handles.entry(tile).or_default().clone();
        self.configure_file_scroll(&handle);
        handle
    }

    pub(crate) fn configure_file_scroll(&self, handle: &UniformListScrollHandle) {
        // Apply the user's wheel-smoothness setting to the underlying scroll handle so
        // every file view scrolls with the same feel. Cheap; runs per render.
        let (ease, boost) = scroll_smooth_params(self.state.settings.scroll_smoothness);
        handle.0.borrow().base_handle.set_smooth(ease, boost);
        handle
            .0
            .borrow()
            .base_handle
            .set_bottom_scroll_margin(self.state.settings.scroll_overscroll_fraction());
    }

    /// Move the keyboard cursor within the focused tab's visible list. `target`
    /// maps `(current_pos, len)` to the new visible position. `extend` grows the
    /// selection from the anchor (Shift); `cursor_only` moves the cursor without
    /// changing `selected` (Ctrl+arrows); otherwise it becomes a single selection.
    fn nav_apply(
        &mut self,
        target: impl Fn(usize, usize) -> usize,
        extend: bool,
        cursor_only: bool,
    ) {
        // Miller and Tree share the selection model in their own visible index space.
        if self.miller_nav_apply(&target, extend, cursor_only) {
            return;
        }
        if self.tree_nav_apply(&target, extend, cursor_only) {
            return;
        }
        let tile = self.state.focused_tile;
        let visible = self.flat_visible_indices(tile);
        let column_rows = self.list_column_step(tile).map(|(rows, _)| rows);
        {
            let Some(ts) = self.state.tab_states.get_mut(&tile) else {
                return;
            };
            if visible.is_empty() {
                return;
            }
            let len = visible.len();
            let cur = ts
                .selection
                .keyboard_cursor
                .and_then(|i| visible.iter().position(|v| *v == i))
                .unwrap_or(0);
            let np = target(cur, len).min(len - 1);
            let next_idx = visible[np];
            ts.selection
                .navigate(next_idx, &visible, extend, cursor_only, column_rows);
        };
    }

    /// Cells a vertical arrow moves in a grouped Grid (one wrapped row).
    fn grouped_grid_cols(&self, tile: TileId) -> Option<usize> {
        let tab = self.tab_data_for(tile)?;
        (tab.view_mode == atlas_model::settings::ViewMode::Grid
            && tab.group_by != atlas_model::tab::GroupBy::None)
            .then(|| {
                self.grid_hit
                    .get(&tile)
                    .map_or(1, |model| model.cols.max(1))
            })
    }

    /// Cells a horizontal arrow moves in a grouped List (one column).
    fn grouped_list_rows(&self, tile: TileId) -> Option<usize> {
        let tab = self.tab_data_for(tile)?;
        (tab.view_mode == atlas_model::settings::ViewMode::List
            && tab.group_by != atlas_model::tab::GroupBy::None)
            .then(|| {
                self.list_hit
                    .get(&tile)
                    .map_or(1, |model| model.major_rows.max(1))
            })
    }

    /// Move the cursor by `delta` cells of the grouped layout, hopping over headers
    /// and row/column padding.
    fn nav_by_cells(&mut self, delta: isize, extend: bool, cursor_only: bool) {
        let tile = self.state.focused_tile;
        let cells = self.view_cells(tile);
        let visible = self.flat_visible_indices(tile);
        let Some(ts) = self.state.tab_states.get_mut(&tile) else {
            return;
        };
        let current = ts
            .selection
            .keyboard_cursor
            .and_then(|index| cells.iter().position(|cell| *cell == index));
        let target = match current {
            Some(current) => atlas_model::grouping::grouped_step(&cells, current, delta),
            None => cells
                .iter()
                .position(|cell| !atlas_model::grouping::is_virtual_cell(*cell)),
        };
        if let Some(target) = target {
            ts.selection
                .navigate(cells[target], &visible, extend, cursor_only, None);
        }
    }

    pub(crate) fn nav_up(&mut self, extend: bool, cursor_only: bool) {
        let tile = self.state.focused_tile;
        if let Some(cols) = self.grouped_grid_cols(tile) {
            return self.nav_by_cells(-(cols as isize), extend, cursor_only);
        }
        let cols = self.rubber_scroll_dims(tile).0;
        self.nav_apply(move |c, _| c.saturating_sub(cols), extend, cursor_only);
    }

    pub(crate) fn nav_down(&mut self, extend: bool, cursor_only: bool) {
        let tile = self.state.focused_tile;
        if let Some(cols) = self.grouped_grid_cols(tile) {
            return self.nav_by_cells(cols as isize, extend, cursor_only);
        }
        let cols = self.rubber_scroll_dims(tile).0;
        self.nav_apply(move |c, _| c + cols, extend, cursor_only);
    }

    pub(crate) fn nav_left(&mut self, extend: bool, cursor_only: bool) {
        if self.miller_nav_left(extend, cursor_only) {
            return;
        }
        if self.tree_nav_left(extend, cursor_only) {
            return;
        }
        let tile = self.state.focused_tile;
        let is_grid_or_list = self
            .tab_data_for(tile)
            .map(|td| {
                matches!(
                    td.view_mode,
                    atlas_model::settings::ViewMode::Grid | atlas_model::settings::ViewMode::List
                )
            })
            .unwrap_or(false);

        if let Some(rows) = self.grouped_list_rows(tile) {
            self.nav_by_cells(-(rows as isize), extend, cursor_only);
        } else if let Some((rows, _)) = self.list_column_step(tile) {
            // Explorer List: Left moves to the same row of the previous column and
            // stops at the first column instead of wrapping.
            self.nav_apply(move |c, _| list_column_left(c, rows), extend, cursor_only);
        } else if is_grid_or_list {
            self.nav_apply(|c, _| c.saturating_sub(1), extend, cursor_only);
        } else if !extend && !cursor_only {
            // In Details view, Left navigates up to the parent directory.
            self.process_actions(vec![crate::input::action::Action::NavigateUp(tile)]);
        }
    }

    pub(crate) fn nav_right(&mut self, extend: bool, cursor_only: bool) {
        if self.miller_nav_right(extend, cursor_only) {
            return;
        }
        if self.tree_nav_right(extend, cursor_only) {
            return;
        }
        let tile = self.state.focused_tile;
        let is_grid_or_list = self
            .tab_data_for(tile)
            .map(|td| {
                matches!(
                    td.view_mode,
                    atlas_model::settings::ViewMode::Grid | atlas_model::settings::ViewMode::List
                )
            })
            .unwrap_or(false);

        if let Some(rows) = self.grouped_list_rows(tile) {
            self.nav_by_cells(rows as isize, extend, cursor_only);
        } else if let Some((rows, _)) = self.list_column_step(tile) {
            // Explorer List: Right moves to the same row of the next column; when
            // that column is shorter, land on the last item.
            self.nav_apply(
                move |c, len| list_column_right(c, len, rows),
                extend,
                cursor_only,
            );
        } else if is_grid_or_list {
            self.nav_apply(|c, _| c + 1, extend, cursor_only);
        } else if !extend && !cursor_only {
            // In Details view, Right enters the directory under cursor.
            let dir_path = self.state.tab_states.get(&tile).and_then(|ts| {
                let cur = ts.selection.keyboard_cursor?;
                let entry = ts.entries.get(cur)?;
                (entry.kind() == atlas_model::file_entry::FileKind::Dir).then(|| entry.path())
            });
            if let Some(path) = dir_path {
                self.navigate(tile, path);
            }
        }
    }

    pub(crate) fn nav_home(&mut self, extend: bool, cursor_only: bool) {
        self.nav_apply(|_, _| 0, extend, cursor_only);
    }

    pub(crate) fn nav_end(&mut self, extend: bool, cursor_only: bool) {
        self.nav_apply(|_, len| len - 1, extend, cursor_only);
    }

    pub(crate) fn nav_page(&mut self, down: bool, extend: bool, cursor_only: bool) {
        let tile = self.state.focused_tile;
        let cols = self.rubber_scroll_dims(tile).0;
        // A List page is the set of whole columns in the viewport.
        let step = self
            .list_column_step(tile)
            .map_or((cols * 5).max(20), |(rows, page_cols)| rows * page_cols);
        self.nav_apply(
            move |c, _| {
                if down {
                    c + step
                } else {
                    c.saturating_sub(step)
                }
            },
            extend,
            cursor_only,
        );
    }

    /// Toggle the item currently under the keyboard cursor in/out of the selected set.
    pub(crate) fn toggle_selection_cursor(&mut self) {
        let tile = self.state.focused_tile;
        let is_miller = self
            .tab_data_for(tile)
            .is_some_and(|tab| tab.view_mode == atlas_model::settings::ViewMode::Miller);
        if let Some(ts) = self.state.tab_states.get_mut(&tile) {
            if is_miller {
                if let Some(col) = ts.miller.columns.get_mut(ts.miller.focused) {
                    if let Some(cursor) = col.selection.keyboard_cursor {
                        if col.selection.selected.contains(&cursor) {
                            col.selection.selected.remove(&cursor);
                        } else {
                            col.selection.selected.insert(cursor);
                        }
                    }
                }
                return;
            }
            let cursor = match ts.selection.keyboard_cursor {
                Some(c) => c,
                None => match ts.visible_indices().first().copied() {
                    Some(first) => {
                        ts.selection.keyboard_cursor = Some(first);
                        first
                    }
                    None => return,
                },
            };
            if ts.selection.selected.contains(&cursor) {
                ts.selection.selected.remove(&cursor);
            } else {
                ts.selection.selected.insert(cursor);
                ts.selection.selection_anchor = Some(cursor);
            }
            ts.selection.last_clicked_index = Some(cursor);
        }
    }

    /// Open properties modal for focused or selected files.
    pub(crate) fn open_properties_focused(&mut self) {
        let paths = self.selected_paths();
        let target_paths = if paths.is_empty() {
            let tile = self.state.focused_tile;
            self.tab_data_for(tile)
                .and_then(|t| t.location.fs_dir().map(std::path::Path::to_path_buf))
                .into_iter()
                .collect()
        } else {
            paths
        };

        if target_paths.is_empty() {
            return;
        }

        #[cfg(windows)]
        {
            if let Some(first) = target_paths.first() {
                crate::ui::shell_context_menu::show_properties(first);
            }
        }
        #[cfg(not(windows))]
        {
            self.open_properties(target_paths);
        }
    }

    /// Open the shell or background context menu anchored to the focused pane.
    pub(crate) fn open_context_menu_focused(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let tile = self.state.focused_tile;
        let paths = self.selected_paths();
        let anchor = self
            .pane_view_bounds
            .iter()
            .find_map(|(id, b)| (*id == tile).then_some(b.center()))
            .unwrap_or_else(|| gpui::point(gpui::px(200.0), gpui::px(200.0)));

        if paths.is_empty() {
            let active_dir = self
                .tab_data_for(tile)
                .and_then(|t| t.location.fs_dir().map(std::path::Path::to_path_buf));
            self.open_background_ctx_menu(active_dir, anchor, window, cx);
        } else {
            self.open_ctx_menu(paths, anchor, window, cx);
        }
    }

    pub(crate) fn select_first_visible(&mut self, tile: TileId) {
        let is_miller = self
            .tab_data_for(tile)
            .is_some_and(|tab| tab.view_mode == atlas_model::settings::ViewMode::Miller);
        let visible = self.flat_visible_indices(tile);
        let Some(ts) = self.state.tab_states.get_mut(&tile) else {
            return;
        };
        if is_miller {
            let visible = ts.miller_visible_indices(ts.miller.focused);
            if let Some(column) = ts.miller.columns.get_mut(ts.miller.focused) {
                column.selection.clear();
                if let Some(index) = visible.first() {
                    column.selection.select(*index, &visible, false, false);
                }
            }
            ts.selection.scroll_to_selected = true;
            return;
        }
        ts.selection.clear();
        let Some(&idx) = visible.first() else {
            return;
        };
        ts.selection.selected.insert(idx);
        ts.selection.keyboard_cursor = Some(idx);
        ts.selection.selection_anchor = Some(idx);
        ts.selection.last_clicked_index = Some(idx);
        ts.selection.scroll_to_selected = true;
    }

    /// Open (or navigate into) the item under the keyboard cursor. With more than
    /// one item selected, open every selected item instead of just the cursor —
    /// matches Explorer's Enter-on-multi-select behavior (each launches with its
    /// default app / folders open in a new window) rather than silently dropping
    /// the rest of the selection.
    pub(crate) fn open_cursor(&mut self) {
        if self.miller_open_cursor() {
            return;
        }
        if self.tree_open_cursor() {
            return;
        }
        let tile = self.state.focused_tile;
        let Some(ts) = self.state.tab_states.get(&tile) else {
            return;
        };
        if ts.selection.selected.len() > 1 {
            let paths = self.selected_paths();
            if !paths.is_empty() {
                self.spawn_open_files(paths);
            }
            return;
        }
        let idx = ts
            .selection
            .keyboard_cursor
            .or_else(|| ts.selection.selected.iter().next().copied());
        if let Some(idx) = idx {
            self.process_actions(vec![crate::input::action::Action::Activate { tile, idx }]);
        }
    }

    pub(crate) fn focus_next_tab(&mut self) {
        self.cycle_tab(1);
    }
    pub(crate) fn focus_prev_tab(&mut self) {
        self.cycle_tab(-1);
    }
    fn cycle_tab(&mut self, dir: isize) {
        let cur = self.state.focused_tile;
        let Some(container) = self.state.find_tabs_container(cur) else {
            return;
        };
        let children = match self.state.pane_tree.tiles.get(container) {
            Some(Tile::Container(Container::Tabs(tabs))) => tabs.children.clone(),
            _ => return,
        };
        if children.is_empty() {
            return;
        }
        let pos = children.iter().position(|&c| c == cur).unwrap_or(0) as isize;
        let n = children.len() as isize;
        let np = (((pos + dir) % n + n) % n) as usize;
        self.activate_tab(children[np]);
    }

    /// Persist all UI state to disk.
    pub fn save(&self) {
        save_settings(&self.state.settings);
        save_session(&self.state.pane_tree);
        save_profiles(&self.state.profiles);
        atlas_core::util::config::save_ext_enabled(&self.state.ext.enabled);
        #[cfg(feature = "provider-host")]
        atlas_core::util::config::save_cloud_remotes(&self.state.cloud.remotes);
    }

    /// Periodic crash-safety checkpoint. Quit-time autosave alone loses the
    /// whole session when the process dies. Skipped while panes animate closed
    /// (they would persist as ghosts) and while a vault is open (quit closes it
    /// before saving, so its paths never reach the session file).
    pub fn autosave_checkpoint(&mut self) {
        if !self.state.settings.autosave_session
            || self.close_after_tab_transfer
            || self.vault.is_some()
            || !self.pane_closing.is_empty()
            || !self.tab_closing.is_empty()
        {
            return;
        }
        self.save();
    }

    /// Called when the window is about to close. Persists everything only if
    /// autosave is on; a disabled toggle means the user wants the last manual/
    /// theme-triggered save to stick instead of silently overwriting it on exit.
    pub fn save_on_quit(&mut self) {
        self.close_native_vault(true);
        self.vault = None;
        // The receiving process owns the transferred session. The acknowledged
        // source must not overwrite it while its last window is being closed.
        if self.close_after_tab_transfer {
            return;
        }
        if self.state.settings.autosave_session {
            // Flush any panes/tabs that are still in their close-animation so they
            // don't reappear as ghost panels on the next launch.
            if !self.pane_closing.is_empty() {
                let pending: Vec<TileId> = self.pane_closing.keys().copied().collect();
                self.pane_closing.clear();
                self.remove_panes_from_tree(&pending);
            }
            if !self.tab_closing.is_empty() {
                let pending: Vec<TileId> = self.tab_closing.keys().copied().collect();
                self.tab_closing.clear();
                self.remove_panes_from_tree(&pending);
            }
            self.save();
        }
    }

    #[cfg(feature = "provider-host")]
    pub fn on_exit(&mut self) {
        self.cleanup_cloud_on_exit();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BatchRenameField {
    Pattern,
    Replacement,
}

/// Build the one-time migration payload for a fresh tag database: every tag that
/// previously lived only in profile config (globals + each profile's own tags).
fn collect_tag_migration(
    profiles: &atlas_model::profiles::ProfileManager,
) -> Vec<atlas_model::tag_cache::TagDef> {
    use atlas_model::tag_cache::{TagDef, TagId};
    let mut out = Vec::new();
    for tag in &profiles.global_tags {
        out.push(TagDef {
            id: TagId(0),
            name: tag.name.clone(),
            color: tag.color,
            profile: None,
        });
    }
    for profile in &profiles.profiles {
        for tag in &profile.tags {
            out.push(TagDef {
                id: TagId(0),
                name: tag.name.clone(),
                color: tag.color,
                profile: Some(profile.name.clone()),
            });
        }
    }
    out
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ActiveGridAnimation {
    pub(crate) start_time: std::time::Instant,
    pub(crate) start_offset: gpui::Point<f32>,
}

#[derive(Clone, Default)]
pub(crate) struct GridFlipState {
    pub(crate) last_unscrolled_origins: std::collections::HashMap<usize, gpui::Point<f32>>,
    pub(crate) animations: std::collections::HashMap<usize, ActiveGridAnimation>,
}

/// Column-major Left: previous column, same row; no wrap past the first column.
pub(crate) fn list_column_left(pos: usize, rows: usize) -> usize {
    let rows = rows.max(1);
    if pos >= rows {
        pos - rows
    } else {
        pos
    }
}

/// Column-major Right: next column, same row; a shorter last column lands on the
/// final item, and the last column itself does not move.
pub(crate) fn list_column_right(pos: usize, len: usize, rows: usize) -> usize {
    let rows = rows.max(1);
    if len == 0 {
        return pos;
    }
    if pos + rows < len {
        pos + rows
    } else if pos / rows < (len - 1) / rows {
        len - 1
    } else {
        pos
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GridHitModel {
    /// Current painted rectangles. Empty means the regular arithmetic layout.
    pub(crate) presented: Vec<(usize, gpui::Bounds<gpui::Pixels>)>,
    pub(crate) bounds: gpui::Bounds<gpui::Pixels>,
    pub(crate) pad_x: f32,
    pub(crate) pad_y: f32,
    pub(crate) scroll_x: f32,
    pub(crate) scroll_y: f32,
    pub(crate) cell_w: f32,
    pub(crate) cell_h: f32,
    pub(crate) cols: usize,
    /// Column-major flow (Explorer List): items fill `major_rows` rows top to
    /// bottom, then continue in the next column. Zero means row-major.
    pub(crate) major_rows: usize,
    pub(crate) pitch_y: f32,
    pub(crate) pitch_x: f32,
    pub(crate) visible: std::sync::Arc<Vec<usize>>,
    pub(crate) start_idx: usize,
    pub(crate) end_idx: usize,
    pub(crate) total_count: usize,
    /// Group header bands between rows; empty for ungrouped layouts.
    pub(crate) bands: atlas_model::grouping::RowBands,
}

impl GridHitModel {
    /// `(row, col)` of a visible position.
    pub(crate) fn slot(&self, visible_pos: usize) -> (usize, usize) {
        if self.major_rows > 0 {
            (visible_pos % self.major_rows, visible_pos / self.major_rows)
        } else {
            let cols = self.cols.max(1);
            (visible_pos / cols, visible_pos % cols)
        }
    }

    /// Visible position at a content-space point (scroll already applied).
    fn pos_at_content(&self, x: f32, y: f32) -> Option<usize> {
        let (x, y) = (x - self.pad_x, y - self.pad_y);
        if y < 0.0 || x < 0.0 || self.pitch_x <= 0.0 || self.pitch_y <= 0.0 {
            return None;
        }
        let row = if self.major_rows > 0 {
            (y / self.pitch_y).floor() as usize
        } else {
            self.bands.row_at(y, self.pitch_y)
        };
        let col = (x / self.pitch_x).floor() as usize;
        // The pitch includes the inter-cell gap.  A click/drag in that gap is
        // background, never the neighbouring cell; this keeps marquee/context
        // behaviour identical to the painted surface.
        let local_x = x - col as f32 * self.pitch_x;
        let local_y = y - self.bands.row_top(row, self.pitch_y);
        if local_x >= self.cell_w || local_y >= self.cell_h {
            return None;
        }
        let visible_pos = if self.major_rows > 0 {
            if row >= self.major_rows {
                return None;
            }
            col * self.major_rows + row
        } else {
            if col >= self.cols {
                return None;
            }
            row * self.cols + col
        };
        (visible_pos >= self.start_idx
            && visible_pos < self.end_idx
            && visible_pos < self.total_count)
            .then_some(visible_pos)
    }

    pub(crate) fn hit_visible_pos(&self, pos: gpui::Point<gpui::Pixels>) -> Option<usize> {
        if !self.bounds.contains(&pos) {
            return None;
        }
        if !self.presented.is_empty() {
            return self
                .presented
                .iter()
                .rev()
                .find(|(_, bounds)| bounds.contains(&pos))
                .map(|(i, _)| *i);
        }
        self.pos_at_content(
            f32::from(pos.x) - f32::from(self.bounds.left()) + self.scroll_x,
            f32::from(pos.y) - f32::from(self.bounds.top()) + self.scroll_y,
        )
    }

    pub(crate) fn hit_entry(&self, pos: gpui::Point<gpui::Pixels>) -> Option<usize> {
        self.hit_visible_pos(pos).and_then(|visible_pos| {
            self.visible
                .get(visible_pos)
                .copied()
                .filter(|cell| !atlas_model::grouping::is_virtual_cell(*cell))
        })
    }

    pub(crate) fn hit_entry_at_content_offset(
        &self,
        offset: gpui::Point<gpui::Pixels>,
    ) -> Option<usize> {
        if !self.presented.is_empty() {
            return self.hit_entry(gpui::point(
                self.bounds.left() + offset.x - gpui::px(self.scroll_x),
                self.bounds.top() + offset.y - gpui::px(self.scroll_y),
            ));
        }
        self.pos_at_content(f32::from(offset.x), f32::from(offset.y))
            .and_then(|visible_pos| {
                self.visible
                    .get(visible_pos)
                    .copied()
                    .filter(|cell| !atlas_model::grouping::is_virtual_cell(*cell))
            })
    }

    pub(crate) fn cell_bounds(&self, visible_pos: usize) -> gpui::Bounds<gpui::Pixels> {
        if let Some((_, bounds)) = self.presented.iter().find(|(i, _)| *i == visible_pos) {
            return *bounds;
        }
        let (row, col) = self.slot(visible_pos);
        let y = self.bands.row_top(row, self.pitch_y) - self.scroll_y + self.pad_y;
        let x = (col as f32) * self.pitch_x - self.scroll_x + self.pad_x;
        gpui::Bounds {
            origin: gpui::point(
                self.bounds.left() + gpui::px(x),
                self.bounds.top() + gpui::px(y),
            ),
            size: gpui::size(gpui::px(self.cell_w), gpui::px(self.cell_h)),
        }
    }
}
