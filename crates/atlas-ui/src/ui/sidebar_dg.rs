//! Digital Grove Immediate Sidebar Element for Atlas (`SidebarDgElement`).
//!
//! Architectural principles (Digital Grove / Pexer / Raddebugger):
//! 1. Stateless procedural generation: implements `gpui::Element`, `IntoElement`, `Styled`.
//! 2. Single-cycle O(1) arithmetic layout with `RectCut` (`crate::ui::dg::cut::RectCut`)
//!    eliminating multi-pass Taffy flexbox equation solving for headers, sections, rows, and dropzone.
//! 3. Direct primitive painting: hardware-clipped rendering with `paint_fill`, `paint_rounded`,
//!    `paint_theme_icon`, and shaped text.
//! 4. UTF-8 binary search text truncation (`shape_text_truncated`) ensuring multi-byte code points
//!    are never split and text never overflows row boundaries.
//! 5. 100% Drop targets & Drag rehydration:
//!    - Registers `DragHoverTarget` with `DragHoverId::Sidebar(id)` or `DragHoverId::Folder`
//!      for places, bookmarks, tags, drives, cloud remotes, and the sidebar dropzone.
//!    - Purges stale sidebar targets on prepaint.
//!    - Visual hover, blinking (`app.drag_hover.blink_id`), and highlight borders.
//! 6. Native GPUI Hitbox registration (`insert_hitbox`) for precise Z-order occlusion and interactions.
//! 7. Full interactivity: single-click navigation (navigates focused tab/pane), right-click
//!    context menu on items and headers, collapse/expand toggle on sections and sidebar.

use crate::app::cloud::CloudAction;
use crate::app::App as AtlasApp;
use crate::input::action::Action;
use crate::ui::dg::cut::RectCut;
use crate::ui::drag_hover::{
    DragHoverCommand, DragHoverDropCommand, DragHoverId, DragHoverKind, DragHoverTarget,
};
use crate::ui::icons::AtlasIcon;
use crate::ui::sidebar::{place_sidebar_drop_zone, sidebar_label, SidebarStyle};
use crate::ui::surface::{
    line_width, paint_rounded, paint_theme_icon, shape_text_truncated, text_origin_y,
    wrap_text_lines, Slot,
};
use atlas_model::file_entry::FileKind;
use atlas_model::location::Location;
use atlas_model::profiles::SectionKind;
use atlas_model::tag_cache::TagId;
use gpui::ColorExt as _;
use gpui::{
    point, px, size, App as GpuiApp, Bounds, Corners, CursorStyle, DispatchPhase, Edges, Element,
    ElementId, Font, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    IntoElement, LayoutId, MouseButton, MouseDownEvent, Pixels, Refineable, SharedString, Style,
    StyleRefinement, Styled, TextAlign, WeakEntity, Window,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

// Kept only as a test fixture. This prototype is never mounted in production.

// Layout Constants
pub const SIDEBAR_PAD_X: f32 = 8.0;
pub const HEADER_H_WIN: f32 = 38.0;
pub const HEADER_H_DEF: f32 = 34.0;
pub const SECTION_HEADER_H: f32 = 28.0;
pub const ROW_H_WIN: f32 = 30.0;
pub const ROW_H_DEF: f32 = 24.0;
pub const DRIVE_ROW_H_WIN: f32 = 48.0;
pub const DRIVE_ROW_H_DEF: f32 = 44.0;
pub const REMOVABLE_ROW_H_WIN: f32 = 40.0;
pub const REMOVABLE_ROW_H_DEF: f32 = 36.0;
pub const DROPZONE_H_EMPTY: f32 = 88.0;
pub const DROPZONE_H_ITEMS: f32 = 144.0;
pub const COLLAPSED_ROW_H: f32 = 32.0;

/// Represents interactive actions from clicking sidebar items.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SidebarDgHitAction {
    AtlasLauncher,
    CollapseToggle,
    SectionHeader(usize),
    AddBookmark,
    AddTag,
    NavRow(PathBuf),
    VirtualNavRow(Location),
    BookmarkRow {
        index: usize,
        path: PathBuf,
    },
    BookmarkRemove(usize),
    TagRow(TagId),
    TagRename {
        tag: TagId,
        name: String,
        color: [u8; 3],
        global: bool,
    },
    TagDelete(TagId),
    DriveRow(PathBuf),
    RemovableRow(String),
    RemovableEject {
        dev: String,
        is_mounted: bool,
    },
    CloudRemoteRow(String),
    DropzonePlace,
    DropzoneClear,
    DropzoneItem(PathBuf),
    DropzoneItemRemove(PathBuf),
}

/// Represents right-click targets in the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SidebarDgRightClickAction {
    Path(PathBuf),
    Drive(PathBuf),
    SectionHeader(usize),
    Bookmark(usize),
}

/// Single item geometry for layout and painting.
#[derive(Clone)]
pub(crate) struct SidebarRowGeom {
    pub row_slot: Slot,
    pub icon_slot: Slot,
    pub label_slot: Slot,
    pub badge_slot: Option<Slot>,
    pub meter_slot: Option<(Slot, f32, Hsla)>,
    pub action_slots: Vec<(Slot, SidebarDgHitAction)>,
    pub hit_action: SidebarDgHitAction,
    pub right_click: Option<SidebarDgRightClickAction>,
    pub drop_target: Option<DragHoverTarget>,
    pub is_active: bool,
    pub is_blinking: bool,
    pub is_drag_over: bool,
    pub icon: AtlasIcon,
    pub icon_color: Hsla,
    pub shell_tex: Option<Arc<gpui::RenderImage>>,
    pub label: String,
    pub badge_text: Option<String>,
    pub badge_color: Option<Hsla>,
    pub tag_color: Option<[u8; 3]>,
}

/// Section geometry containing header and rows.
#[derive(Clone)]
pub(crate) struct SidebarSectionGeom {
    pub idx: usize,
    pub kind: SectionKind,
    pub collapsed: bool,
    pub header_slot: Slot,
    pub chevron_slot: Slot,
    pub title_slot: Slot,
    pub header_actions: Vec<(Slot, SidebarDgHitAction)>,
    pub header_drop_target: Option<DragHoverTarget>,
    pub rows: Vec<SidebarRowGeom>,
}

/// Drop zone item geometry.
#[derive(Clone)]
pub(crate) struct SidebarDropZoneItemGeom {
    pub slot: Slot,
    pub preview_slot: Slot,
    pub name_slot: Slot,
    pub remove_btn_slot: Slot,
    pub path: PathBuf,
    pub name: String,
    pub kind: FileKind,
    pub ext: Option<String>,
    pub thumb_tex: Option<Arc<gpui::RenderImage>>,
    pub icon_tex: Option<Arc<gpui::RenderImage>>,
    pub selected: bool,
}

/// Complete single-cycle arithmetic layout for the sidebar.
#[derive(Clone)]
pub(crate) struct SidebarDgLayout {
    pub total_w: f32,
    pub total_h: f32,
    pub launcher_slot: Slot,
    pub collapse_btn_slot: Option<Slot>,
    pub sections: Vec<SidebarSectionGeom>,
    pub dropzone_slot: Option<Slot>,
    pub dropzone_header_slot: Option<Slot>,
    pub dropzone_place_btn_slot: Option<Slot>,
    pub dropzone_clear_btn_slot: Option<Slot>,
    pub dropzone_empty_hint_slot: Option<Slot>,
    pub dropzone_items: Vec<SidebarDropZoneItemGeom>,
    pub dropzone_target: Option<DragHoverTarget>,
}

/// Single item data to be arranged in a section.
#[derive(Clone)]
pub(crate) enum SidebarDgItemData {
    Nav {
        id: String,
        label: String,
        icon: AtlasIcon,
        path: PathBuf,
        shell_tex: Option<Arc<gpui::RenderImage>>,
        active: bool,
    },
    VirtualNav {
        id: String,
        label: String,
        icon: AtlasIcon,
        location: Location,
        shell_tex: Option<Arc<gpui::RenderImage>>,
        active: bool,
    },
    Bookmark {
        index: usize,
        name: String,
        path: PathBuf,
        shell_tex: Option<Arc<gpui::RenderImage>>,
        active: bool,
    },
    Tag {
        id: TagId,
        name: String,
        color: [u8; 3],
        global: bool,
    },
    Drive {
        mount_point: PathBuf,
        label: String,
        free_label: String,
        fraction: f32,
        icon: AtlasIcon,
        shell_tex: Option<Arc<gpui::RenderImage>>,
        active: bool,
    },
    Removable {
        dev: String,
        label: String,
        subtitle: String,
        is_mounted: bool,
        active: bool,
    },
    Recent {
        label: String,
        path: PathBuf,
        shell_tex: Option<Arc<gpui::RenderImage>>,
        active: bool,
    },
    Cloud {
        id: String,
        name: String,
        status_label: Option<String>,
        status_color: Option<Hsla>,
        is_mounted: bool,
        is_busy: bool,
        drop_dest: Option<PathBuf>,
    },
    Empty {
        message: String,
    },
}

/// Section data gathered from App state.
#[derive(Clone)]
pub(crate) struct SidebarDgSectionData {
    pub idx: usize,
    pub kind: SectionKind,
    pub title: String,
    pub collapsed: bool,
    pub hidden: bool,
    pub items: Vec<SidebarDgItemData>,
}

/// Drop zone staged item data.
#[derive(Clone)]
pub(crate) struct SidebarDgDropZoneData {
    pub staged: Vec<(
        PathBuf,
        String,
        FileKind,
        Option<String>,
        Option<Arc<gpui::RenderImage>>,
        Option<Arc<gpui::RenderImage>>,
    )>,
    pub selected_set: HashSet<PathBuf>,
    pub can_place: bool,
    pub dest: Option<PathBuf>,
    pub is_drag_over: bool,
}

/// Compute single-cycle O(1) arithmetic layout with RectCut for the entire sidebar.
pub(crate) fn compute_sidebar_layout(
    w: f32,
    zoom: f32,
    windows_theme: bool,
    sidebar_collapsed: bool,
    profile_name: &str,
    sections: &[SidebarDgSectionData],
    drop_zone: Option<&SidebarDgDropZoneData>,
    blink_id: Option<&DragHoverId>,
    drag_target: Option<&DragHoverId>,
    accent: Hsla,
    fg: Hsla,
    muted: Hsla,
) -> SidebarDgLayout {
    let _ = profile_name;
    let zoom = if zoom.is_finite() && zoom > 0.0 {
        zoom
    } else {
        1.0
    };
    let pad_x = SIDEBAR_PAD_X * zoom;
    let available_w = (w - pad_x * 2.0).max(10.0);
    let mut y = 4.0 * zoom;

    let header_h = if windows_theme {
        HEADER_H_WIN
    } else {
        HEADER_H_DEF
    } * zoom;
    let mut header_cut = RectCut::new(pad_x, y, available_w, header_h);

    let (launcher_slot, collapse_btn_slot) = if sidebar_collapsed {
        let btn_sz = 30.0 * zoom;
        let btn_x = (w - btn_sz) * 0.5;
        let l_slot = Slot::new(btn_x, y, btn_sz, btn_sz);
        let col_slot = Slot::new(btn_x, y + btn_sz + 4.0 * zoom, btn_sz, btn_sz);
        y += btn_sz + 4.0 * zoom;
        (l_slot, Some(col_slot))
    } else {
        let collapse_slot = header_cut.cut_right(30.0 * zoom);
        let _ = header_cut.cut_right(4.0 * zoom);
        let launcher = header_cut.remaining();
        (launcher, Some(collapse_slot))
    };
    y += header_h + 6.0 * zoom;

    let mut section_geoms = Vec::with_capacity(sections.len());

    if sidebar_collapsed {
        // Collapsed Rail Mode: single-column icon strip
        let row_sz = COLLAPSED_ROW_H * zoom;
        let icon_x = (w - row_sz) * 0.5;

        for sec in sections.iter() {
            if sec.hidden {
                continue;
            }
            if sec.items.is_empty() {
                continue;
            }

            let mut rows = Vec::with_capacity(sec.items.len());
            for item in &sec.items {
                let row_slot = Slot::new(icon_x, y, row_sz, row_sz);
                let icon_sz = if windows_theme { 18.0 } else { 16.0 } * zoom;
                let icon_slot = Slot::new(
                    icon_x + (row_sz - icon_sz) * 0.5,
                    y + (row_sz - icon_sz) * 0.5,
                    icon_sz,
                    icon_sz,
                );

                let (hit_action, drop_target, icon, shell_tex, is_active, label, tag_col) =
                    match item {
                        SidebarDgItemData::Nav {
                            id,
                            label,
                            icon,
                            path,
                            shell_tex,
                            active,
                        } => {
                            let hover_id = DragHoverId::Sidebar(format!("rail-{id}"));
                            let target = DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Folder,
                                command: DragHoverCommand::NavigatePath(path.clone()),
                                drop_command: Some(DragHoverDropCommand::TransferTo(path.clone())),
                            };
                            (
                                SidebarDgHitAction::NavRow(path.clone()),
                                Some(target),
                                *icon,
                                shell_tex.clone(),
                                *active,
                                label.clone(),
                                None,
                            )
                        }
                        SidebarDgItemData::VirtualNav {
                            id: _,
                            label,
                            icon,
                            location,
                            shell_tex,
                            active,
                        } => (
                            SidebarDgHitAction::VirtualNavRow(location.clone()),
                            None,
                            *icon,
                            shell_tex.clone(),
                            *active,
                            label.clone(),
                            None,
                        ),
                        SidebarDgItemData::Bookmark {
                            index,
                            name,
                            path,
                            shell_tex,
                            active,
                        } => {
                            let hover_id =
                                DragHoverId::Sidebar(format!("rail-bm-{}", path.display()));
                            let target = DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Nav,
                                command: DragHoverCommand::NavigatePath(path.clone()),
                                drop_command: Some(DragHoverDropCommand::AddBookmarks),
                            };
                            (
                                SidebarDgHitAction::BookmarkRow {
                                    index: *index,
                                    path: path.clone(),
                                },
                                Some(target),
                                AtlasIcon::Folder,
                                shell_tex.clone(),
                                *active,
                                name.clone(),
                                None,
                            )
                        }
                        SidebarDgItemData::Tag {
                            id, name, color, ..
                        } => {
                            let hover_id = DragHoverId::Sidebar(format!("rail-tag-{}", id.0));
                            let target = DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Nav,
                                command: DragHoverCommand::OpenTag(*id),
                                drop_command: Some(DragHoverDropCommand::AssignTag(*id)),
                            };
                            (
                                SidebarDgHitAction::TagRow(*id),
                                Some(target),
                                AtlasIcon::Tag,
                                None,
                                false,
                                name.clone(),
                                Some(*color),
                            )
                        }
                        SidebarDgItemData::Drive {
                            mount_point,
                            label,
                            icon,
                            shell_tex,
                            active,
                            ..
                        } => {
                            let hover_id = DragHoverId::Sidebar(format!(
                                "rail-drive-{}",
                                mount_point.display()
                            ));
                            let target = DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Folder,
                                command: DragHoverCommand::NavigatePath(mount_point.clone()),
                                drop_command: Some(DragHoverDropCommand::TransferTo(
                                    mount_point.clone(),
                                )),
                            };
                            (
                                SidebarDgHitAction::DriveRow(mount_point.clone()),
                                Some(target),
                                *icon,
                                shell_tex.clone(),
                                *active,
                                label.clone(),
                                None,
                            )
                        }
                        SidebarDgItemData::Recent {
                            label,
                            path,
                            shell_tex,
                            active,
                        } => (
                            SidebarDgHitAction::NavRow(path.clone()),
                            None,
                            AtlasIcon::Folder,
                            shell_tex.clone(),
                            *active,
                            label.clone(),
                            None,
                        ),
                        SidebarDgItemData::Cloud {
                            id,
                            name,
                            drop_dest,
                            is_busy,
                            is_mounted,
                            ..
                        } => {
                            let hover_id = DragHoverId::Sidebar(format!("rail-cloud-{id}"));
                            let action = if *is_busy {
                                None
                            } else if *is_mounted {
                                Some(Action::Cloud(CloudAction::OpenMount(id.clone())))
                            } else {
                                Some(Action::Cloud(CloudAction::OpenAccess(id.clone())))
                            };
                            let target = action.map(|act| DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Nav,
                                command: DragHoverCommand::Process(act),
                                drop_command: drop_dest
                                    .clone()
                                    .map(DragHoverDropCommand::TransferTo),
                            });
                            (
                                SidebarDgHitAction::CloudRemoteRow(id.clone()),
                                target,
                                AtlasIcon::Cloud,
                                None,
                                false,
                                name.clone(),
                                None,
                            )
                        }
                        _ => continue,
                    };

                let hover_id_check = drop_target.as_ref().map(|t| &t.id);
                let is_blink = hover_id_check.is_some_and(|h| blink_id == Some(h));
                let is_drag = hover_id_check.is_some_and(|h| drag_target == Some(h));
                let right_click = match item {
                    SidebarDgItemData::Nav { path, .. } => {
                        Some(SidebarDgRightClickAction::Path(path.clone()))
                    }
                    SidebarDgItemData::Drive { mount_point, .. } => {
                        Some(SidebarDgRightClickAction::Drive(mount_point.clone()))
                    }
                    SidebarDgItemData::Bookmark { index, .. } => {
                        Some(SidebarDgRightClickAction::Bookmark(*index))
                    }
                    _ => None,
                };

                rows.push(SidebarRowGeom {
                    row_slot,
                    icon_slot,
                    label_slot: Slot::default(),
                    badge_slot: None,
                    meter_slot: None,
                    action_slots: Vec::new(),
                    hit_action,
                    right_click,
                    drop_target,
                    is_active,
                    is_blinking: is_blink,
                    is_drag_over: is_drag,
                    icon,
                    icon_color: if is_active { accent } else { fg },
                    shell_tex,
                    label,
                    badge_text: None,
                    badge_color: None,
                    tag_color: tag_col,
                });
                y += row_sz + 4.0 * zoom;
            }

            section_geoms.push(SidebarSectionGeom {
                idx: sec.idx,
                kind: sec.kind.clone(),
                collapsed: true,
                header_slot: Slot::default(),
                chevron_slot: Slot::default(),
                title_slot: Slot::default(),
                header_actions: Vec::new(),
                header_drop_target: None,
                rows,
            });
        }
    } else {
        // Expanded Mode: Collapsible sections with headers, rows, meters, and actions
        let normal_row_h = if windows_theme { ROW_H_WIN } else { ROW_H_DEF } * zoom;
        let drive_row_h = if windows_theme {
            DRIVE_ROW_H_WIN
        } else {
            DRIVE_ROW_H_DEF
        } * zoom;
        let removable_row_h = if windows_theme {
            REMOVABLE_ROW_H_WIN
        } else {
            REMOVABLE_ROW_H_DEF
        } * zoom;

        for sec in sections {
            if sec.hidden {
                continue;
            }

            y += 6.0 * zoom;
            let header_slot = Slot::new(pad_x, y, available_w, SECTION_HEADER_H * zoom);
            let mut sec_cut = RectCut::new(pad_x, y, available_w, SECTION_HEADER_H * zoom);

            let chevron_slot = sec_cut.cut_right(18.0 * zoom);
            let mut header_actions = Vec::new();
            let mut header_drop_target = None;

            if sec.kind == SectionKind::Bookmarks {
                let add_bm_slot = sec_cut.cut_right(20.0 * zoom);
                header_actions.push((add_bm_slot, SidebarDgHitAction::AddBookmark));
                let h_id = DragHoverId::Sidebar("bookmarks-header".to_string());
                header_drop_target = Some(DragHoverTarget {
                    id: h_id,
                    bounds: Bounds::default(),
                    kind: DragHoverKind::Nav,
                    command: DragHoverCommand::Process(Action::CloseOverlays),
                    drop_command: Some(DragHoverDropCommand::AddBookmarks),
                });
            } else if sec.kind == SectionKind::Tags {
                let add_tag_slot = sec_cut.cut_right(20.0 * zoom);
                header_actions.push((add_tag_slot, SidebarDgHitAction::AddTag));
            }

            let title_slot = sec_cut.remaining();
            y += SECTION_HEADER_H * zoom + 3.0 * zoom;

            let mut rows = Vec::new();
            if !sec.collapsed {
                for item in &sec.items {
                    match item {
                        SidebarDgItemData::Nav {
                            id,
                            label,
                            icon,
                            path,
                            shell_tex,
                            active,
                        } => {
                            let mut cut = RectCut::new(pad_x, y, available_w, normal_row_h);
                            let icon_sz = if windows_theme { 18.0 } else { 16.0 } * zoom;
                            let icon_slot = cut.cut_left(icon_sz + 4.0 * zoom);
                            let _ = cut.cut_left(4.0 * zoom);
                            let label_slot = cut.remaining();

                            let hover_id = DragHoverId::Sidebar(id.clone());
                            let target = DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Folder,
                                command: DragHoverCommand::NavigatePath(path.clone()),
                                drop_command: Some(DragHoverDropCommand::TransferTo(path.clone())),
                            };

                            let is_blink = blink_id == Some(&hover_id);
                            let is_drag = drag_target == Some(&hover_id);

                            rows.push(SidebarRowGeom {
                                row_slot: Slot::new(pad_x, y, available_w, normal_row_h),
                                icon_slot,
                                label_slot,
                                badge_slot: None,
                                meter_slot: None,
                                action_slots: Vec::new(),
                                hit_action: SidebarDgHitAction::NavRow(path.clone()),
                                right_click: Some(SidebarDgRightClickAction::Path(path.clone())),
                                drop_target: Some(target),
                                is_active: *active,
                                is_blinking: is_blink,
                                is_drag_over: is_drag,
                                icon: *icon,
                                icon_color: if *active { accent } else { fg.opacity(0.85) },
                                shell_tex: shell_tex.clone(),
                                label: label.clone(),
                                badge_text: None,
                                badge_color: None,
                                tag_color: None,
                            });
                            y += normal_row_h + 2.0 * zoom;
                        }
                        SidebarDgItemData::VirtualNav {
                            id: _,
                            label,
                            icon,
                            location,
                            shell_tex,
                            active,
                        } => {
                            let mut cut = RectCut::new(pad_x, y, available_w, normal_row_h);
                            let icon_sz = if windows_theme { 18.0 } else { 16.0 } * zoom;
                            let icon_slot = cut.cut_left(icon_sz + 4.0 * zoom);
                            let _ = cut.cut_left(4.0 * zoom);
                            let label_slot = cut.remaining();

                            rows.push(SidebarRowGeom {
                                row_slot: Slot::new(pad_x, y, available_w, normal_row_h),
                                icon_slot,
                                label_slot,
                                badge_slot: None,
                                meter_slot: None,
                                action_slots: Vec::new(),
                                hit_action: SidebarDgHitAction::VirtualNavRow(location.clone()),
                                right_click: None,
                                drop_target: None,
                                is_active: *active,
                                is_blinking: false,
                                is_drag_over: false,
                                icon: *icon,
                                icon_color: if *active { accent } else { fg.opacity(0.85) },
                                shell_tex: shell_tex.clone(),
                                label: label.clone(),
                                badge_text: None,
                                badge_color: None,
                                tag_color: None,
                            });
                            y += normal_row_h + 2.0 * zoom;
                        }
                        SidebarDgItemData::Bookmark {
                            index,
                            name,
                            path,
                            shell_tex,
                            active,
                        } => {
                            let mut cut = RectCut::new(pad_x, y, available_w, normal_row_h);
                            let icon_sz = if windows_theme { 18.0 } else { 16.0 } * zoom;
                            let icon_slot = cut.cut_left(icon_sz + 4.0 * zoom);
                            let _ = cut.cut_left(4.0 * zoom);
                            let remove_slot = cut.cut_right(20.0 * zoom);
                            let label_slot = cut.remaining();

                            let hover_id = DragHoverId::Sidebar(format!("bm-{}", path.display()));
                            let target = DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Nav,
                                command: DragHoverCommand::NavigatePath(path.clone()),
                                drop_command: Some(DragHoverDropCommand::AddBookmarks),
                            };

                            let is_blink = blink_id == Some(&hover_id);
                            let is_drag = drag_target == Some(&hover_id);

                            rows.push(SidebarRowGeom {
                                row_slot: Slot::new(pad_x, y, available_w, normal_row_h),
                                icon_slot,
                                label_slot,
                                badge_slot: None,
                                meter_slot: None,
                                action_slots: vec![(
                                    remove_slot,
                                    SidebarDgHitAction::BookmarkRemove(*index),
                                )],
                                hit_action: SidebarDgHitAction::BookmarkRow {
                                    index: *index,
                                    path: path.clone(),
                                },
                                right_click: Some(SidebarDgRightClickAction::Bookmark(*index)),
                                drop_target: Some(target),
                                is_active: *active,
                                is_blinking: is_blink,
                                is_drag_over: is_drag,
                                icon: AtlasIcon::Folder,
                                icon_color: if *active { accent } else { fg.opacity(0.85) },
                                shell_tex: shell_tex.clone(),
                                label: name.clone(),
                                badge_text: None,
                                badge_color: None,
                                tag_color: None,
                            });
                            y += normal_row_h + 2.0 * zoom;
                        }
                        SidebarDgItemData::Tag {
                            id,
                            name,
                            color,
                            global,
                        } => {
                            let mut cut = RectCut::new(pad_x, y, available_w, normal_row_h);
                            let dot_sz = 11.0 * zoom;
                            let icon_slot = cut.cut_left(dot_sz + 8.0 * zoom);
                            let _ = cut.cut_left(4.0 * zoom);

                            let mut actions = Vec::with_capacity(3);
                            let del_slot = cut.cut_right(20.0 * zoom);
                            actions.push((del_slot, SidebarDgHitAction::TagDelete(*id)));

                            let ren_slot = cut.cut_right(20.0 * zoom);
                            actions.push((
                                ren_slot,
                                SidebarDgHitAction::TagRename {
                                    tag: *id,
                                    name: name.clone(),
                                    color: *color,
                                    global: *global,
                                },
                            ));

                            let badge_slot = if *global {
                                Some(cut.cut_right(18.0 * zoom))
                            } else {
                                None
                            };

                            let label_slot = cut.remaining();

                            let hover_id = DragHoverId::Sidebar(format!("tag-{}", id.0));
                            let target = DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Nav,
                                command: DragHoverCommand::OpenTag(*id),
                                drop_command: Some(DragHoverDropCommand::AssignTag(*id)),
                            };

                            let is_blink = blink_id == Some(&hover_id);
                            let is_drag = drag_target == Some(&hover_id);

                            rows.push(SidebarRowGeom {
                                row_slot: Slot::new(pad_x, y, available_w, normal_row_h),
                                icon_slot,
                                label_slot,
                                badge_slot,
                                meter_slot: None,
                                action_slots: actions,
                                hit_action: SidebarDgHitAction::TagRow(*id),
                                right_click: None,
                                drop_target: Some(target),
                                is_active: false,
                                is_blinking: is_blink,
                                is_drag_over: is_drag,
                                icon: AtlasIcon::Tag,
                                icon_color: fg,
                                shell_tex: None,
                                label: name.clone(),
                                badge_text: None,
                                badge_color: None,
                                tag_color: Some(*color),
                            });
                            y += normal_row_h + 2.0 * zoom;
                        }
                        SidebarDgItemData::Drive {
                            mount_point,
                            label,
                            free_label,
                            fraction,
                            icon,
                            shell_tex,
                            active,
                        } => {
                            let mut row_cut = RectCut::new(pad_x, y, available_w, drive_row_h);
                            let top_h = (drive_row_h - 10.0 * zoom).max(18.0 * zoom);
                            let top_strip_slot = row_cut.cut_top(top_h);
                            let mut top_strip = RectCut::new(
                                top_strip_slot.x,
                                top_strip_slot.y,
                                top_strip_slot.w,
                                top_strip_slot.h,
                            );

                            let icon_sz = if windows_theme { 18.0 } else { 16.0 } * zoom;
                            let icon_slot = top_strip.cut_left(icon_sz + 4.0 * zoom);
                            let _ = top_strip.cut_left(4.0 * zoom);

                            let badge_slot = if !free_label.is_empty() {
                                Some(top_strip.cut_right(65.0 * zoom))
                            } else {
                                None
                            };
                            let label_slot = top_strip.remaining();

                            let _ = row_cut.cut_top(3.0 * zoom);
                            let track_slot = row_cut.cut_top(4.0 * zoom);
                            let bar_color = if *fraction > 0.9 {
                                Hsla::new(0.0, 0.70, 0.55, 1.0)
                            } else {
                                accent
                            };

                            let hover_id =
                                DragHoverId::Sidebar(format!("drive-{}", mount_point.display()));
                            let target = DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Folder,
                                command: DragHoverCommand::NavigatePath(mount_point.clone()),
                                drop_command: Some(DragHoverDropCommand::TransferTo(
                                    mount_point.clone(),
                                )),
                            };

                            let is_blink = blink_id == Some(&hover_id);
                            let is_drag = drag_target == Some(&hover_id);

                            rows.push(SidebarRowGeom {
                                row_slot: Slot::new(pad_x, y, available_w, drive_row_h),
                                icon_slot,
                                label_slot,
                                badge_slot,
                                meter_slot: Some((track_slot, *fraction, bar_color)),
                                action_slots: Vec::new(),
                                hit_action: SidebarDgHitAction::DriveRow(mount_point.clone()),
                                right_click: Some(SidebarDgRightClickAction::Drive(
                                    mount_point.clone(),
                                )),
                                drop_target: Some(target),
                                is_active: *active,
                                is_blinking: is_blink,
                                is_drag_over: is_drag,
                                icon: *icon,
                                icon_color: if *active { accent } else { fg.opacity(0.85) },
                                shell_tex: shell_tex.clone(),
                                label: label.clone(),
                                badge_text: if free_label.is_empty() {
                                    None
                                } else {
                                    Some(free_label.clone())
                                },
                                badge_color: Some(muted),
                                tag_color: None,
                            });
                            y += drive_row_h + 3.0 * zoom;
                        }
                        SidebarDgItemData::Removable {
                            dev,
                            label,
                            subtitle,
                            is_mounted,
                            active,
                        } => {
                            let mut row_cut = RectCut::new(pad_x, y, available_w, removable_row_h);
                            let icon_sz = if windows_theme { 18.0 } else { 16.0 } * zoom;
                            let icon_slot = row_cut.cut_left(icon_sz + 4.0 * zoom);
                            let _ = row_cut.cut_left(4.0 * zoom);
                            let eject_slot = row_cut.cut_right(20.0 * zoom);

                            let label_slot = row_cut.remaining();

                            rows.push(SidebarRowGeom {
                                row_slot: Slot::new(pad_x, y, available_w, removable_row_h),
                                icon_slot,
                                label_slot,
                                badge_slot: None,
                                meter_slot: None,
                                action_slots: vec![(
                                    eject_slot,
                                    SidebarDgHitAction::RemovableEject {
                                        dev: dev.clone(),
                                        is_mounted: *is_mounted,
                                    },
                                )],
                                hit_action: SidebarDgHitAction::RemovableRow(dev.clone()),
                                right_click: None,
                                drop_target: None,
                                is_active: *active,
                                is_blinking: false,
                                is_drag_over: false,
                                icon: AtlasIcon::Usb,
                                icon_color: if *active { accent } else { fg.opacity(0.85) },
                                shell_tex: None,
                                label: label.clone(),
                                badge_text: if subtitle.is_empty() {
                                    None
                                } else {
                                    Some(subtitle.clone())
                                },
                                badge_color: Some(muted),
                                tag_color: None,
                            });
                            y += removable_row_h + 2.0 * zoom;
                        }
                        SidebarDgItemData::Recent {
                            label,
                            path,
                            shell_tex,
                            active,
                        } => {
                            let mut cut = RectCut::new(pad_x, y, available_w, normal_row_h);
                            let icon_sz = if windows_theme { 18.0 } else { 16.0 } * zoom;
                            let icon_slot = cut.cut_left(icon_sz + 4.0 * zoom);
                            let _ = cut.cut_left(4.0 * zoom);
                            let label_slot = cut.remaining();

                            rows.push(SidebarRowGeom {
                                row_slot: Slot::new(pad_x, y, available_w, normal_row_h),
                                icon_slot,
                                label_slot,
                                badge_slot: None,
                                meter_slot: None,
                                action_slots: Vec::new(),
                                hit_action: SidebarDgHitAction::NavRow(path.clone()),
                                right_click: Some(SidebarDgRightClickAction::Path(path.clone())),
                                drop_target: None,
                                is_active: *active,
                                is_blinking: false,
                                is_drag_over: false,
                                icon: AtlasIcon::Folder,
                                icon_color: if *active { accent } else { fg.opacity(0.85) },
                                shell_tex: shell_tex.clone(),
                                label: label.clone(),
                                badge_text: None,
                                badge_color: None,
                                tag_color: None,
                            });
                            y += normal_row_h + 2.0 * zoom;
                        }
                        SidebarDgItemData::Cloud {
                            id,
                            name,
                            status_label,
                            status_color,
                            is_mounted,
                            is_busy,
                            drop_dest,
                        } => {
                            let mut cut = RectCut::new(pad_x, y, available_w, normal_row_h);
                            let icon_sz = if windows_theme { 18.0 } else { 16.0 } * zoom;
                            let icon_slot = cut.cut_left(icon_sz + 4.0 * zoom);
                            let _ = cut.cut_left(4.0 * zoom);

                            let badge_slot =
                                status_label.as_ref().map(|_| cut.cut_right(60.0 * zoom));
                            let label_slot = cut.remaining();

                            let hover_id = DragHoverId::Sidebar(format!("cloud-remote-{id}"));
                            let action = if *is_busy {
                                None
                            } else if *is_mounted {
                                Some(Action::Cloud(CloudAction::OpenMount(id.clone())))
                            } else {
                                Some(Action::Cloud(CloudAction::OpenAccess(id.clone())))
                            };

                            let target = action.map(|act| DragHoverTarget {
                                id: hover_id.clone(),
                                bounds: Bounds::default(),
                                kind: DragHoverKind::Nav,
                                command: DragHoverCommand::Process(act),
                                drop_command: drop_dest
                                    .clone()
                                    .map(DragHoverDropCommand::TransferTo),
                            });

                            let is_blink = blink_id == Some(&hover_id);
                            let is_drag = drag_target == Some(&hover_id);

                            rows.push(SidebarRowGeom {
                                row_slot: Slot::new(pad_x, y, available_w, normal_row_h),
                                icon_slot,
                                label_slot,
                                badge_slot,
                                meter_slot: None,
                                action_slots: Vec::new(),
                                hit_action: SidebarDgHitAction::CloudRemoteRow(id.clone()),
                                right_click: None,
                                drop_target: target,
                                is_active: false,
                                is_blinking: is_blink,
                                is_drag_over: is_drag,
                                icon: AtlasIcon::Cloud,
                                icon_color: fg.opacity(0.85),
                                shell_tex: None,
                                label: name.clone(),
                                badge_text: status_label.clone(),
                                badge_color: *status_color,
                                tag_color: None,
                            });
                            y += normal_row_h + 2.0 * zoom;
                        }
                        SidebarDgItemData::Empty { message } => {
                            let slot = Slot::new(
                                pad_x + 8.0 * zoom,
                                y,
                                available_w - 16.0 * zoom,
                                normal_row_h,
                            );
                            rows.push(SidebarRowGeom {
                                row_slot: slot,
                                icon_slot: Slot::default(),
                                label_slot: slot,
                                badge_slot: None,
                                meter_slot: None,
                                action_slots: Vec::new(),
                                hit_action: SidebarDgHitAction::CollapseToggle,
                                right_click: None,
                                drop_target: None,
                                is_active: false,
                                is_blinking: false,
                                is_drag_over: false,
                                icon: AtlasIcon::Folder,
                                icon_color: muted,
                                shell_tex: None,
                                label: message.clone(),
                                badge_text: None,
                                badge_color: None,
                                tag_color: None,
                            });
                            y += normal_row_h + 2.0 * zoom;
                        }
                    }
                }
            }

            section_geoms.push(SidebarSectionGeom {
                idx: sec.idx,
                kind: sec.kind.clone(),
                collapsed: sec.collapsed,
                header_slot,
                chevron_slot,
                title_slot,
                header_actions,
                header_drop_target,
                rows,
            });
        }
    }

    // Drop Zone Section (when expanded)
    let mut dropzone_slot = None;
    let mut dropzone_header_slot = None;
    let mut dropzone_place_btn_slot = None;
    let mut dropzone_clear_btn_slot = None;
    let mut dropzone_empty_hint_slot = None;
    let mut dropzone_items = Vec::new();
    let mut dropzone_target = None;

    if !sidebar_collapsed {
        if let Some(dz) = drop_zone {
            y += 8.0 * zoom;
            let dz_h = if dz.staged.is_empty() {
                DROPZONE_H_EMPTY
            } else {
                DROPZONE_H_ITEMS
            } * zoom;
            let dz_slot = Slot::new(pad_x, y, available_w, dz_h);
            dropzone_slot = Some(dz_slot);

            let mut dz_cut = RectCut::new(
                pad_x + 6.0 * zoom,
                y + 6.0 * zoom,
                available_w - 12.0 * zoom,
                dz_h - 12.0 * zoom,
            );
            let top_strip_slot = dz_cut.cut_top(24.0 * zoom);
            let mut top_strip = RectCut::new(
                top_strip_slot.x,
                top_strip_slot.y,
                top_strip_slot.w,
                top_strip_slot.h,
            );

            let clear_btn = top_strip.cut_right(20.0 * zoom);
            dropzone_clear_btn_slot = Some(clear_btn);
            let place_btn = top_strip.cut_right(20.0 * zoom);
            dropzone_place_btn_slot = Some(place_btn);
            dropzone_header_slot = Some(top_strip.remaining());

            if dz.staged.is_empty() {
                dropzone_empty_hint_slot = Some(dz_cut.remaining());
            } else {
                let mut item_x = pad_x + 8.0 * zoom;
                let item_y = y + 36.0 * zoom;
                let item_w = 84.0 * zoom;
                let item_h = 74.0 * zoom;

                for (path, name, kind, ext, thumb, icon) in &dz.staged {
                    let item_slot = Slot::new(item_x, item_y, item_w, item_h);
                    let preview_slot = Slot::new(
                        item_x + 6.0 * zoom,
                        item_y + 4.0 * zoom,
                        72.0 * zoom,
                        44.0 * zoom,
                    );
                    let name_slot = Slot::new(
                        item_x + 2.0 * zoom,
                        item_y + 52.0 * zoom,
                        item_w - 4.0 * zoom,
                        18.0 * zoom,
                    );
                    let remove_btn_slot = Slot::new(
                        item_x + item_w - 18.0 * zoom,
                        item_y + 2.0 * zoom,
                        16.0 * zoom,
                        16.0 * zoom,
                    );

                    dropzone_items.push(SidebarDropZoneItemGeom {
                        slot: item_slot,
                        preview_slot,
                        name_slot,
                        remove_btn_slot,
                        path: path.clone(),
                        name: name.clone(),
                        kind: kind.clone(),
                        ext: ext.clone(),
                        thumb_tex: thumb.clone(),
                        icon_tex: icon.clone(),
                        selected: dz.selected_set.contains(path),
                    });
                    item_x += item_w + 8.0 * zoom;
                }
            }

            let dz_hover_id = DragHoverId::Sidebar("sidebar-drop-zone".to_string());
            dropzone_target = Some(DragHoverTarget {
                id: dz_hover_id,
                bounds: Bounds::default(),
                kind: DragHoverKind::Folder,
                command: DragHoverCommand::Process(Action::CloseOverlays),
                drop_command: Some(DragHoverDropCommand::StageInSidebar),
            });
            y += dz_h + 8.0 * zoom;
        }
    }

    SidebarDgLayout {
        total_w: w,
        total_h: y,
        launcher_slot,
        collapse_btn_slot,
        sections: section_geoms,
        dropzone_slot,
        dropzone_header_slot,
        dropzone_place_btn_slot,
        dropzone_clear_btn_slot,
        dropzone_empty_hint_slot,
        dropzone_items,
        dropzone_target,
    }
}

/// Prepaint state holding hitboxes, slots, and interactive targets.
pub(crate) struct SidebarDgPrepaint {
    pub hitbox: Hitbox,
    pub layout: SidebarDgLayout,
    pub all_hits: Vec<(Slot, SidebarDgHitAction)>,
    pub right_clicks: Vec<(Slot, SidebarDgRightClickAction)>,
}

/// Stateless procedural Digital Grove element for the entire Sidebar.
pub(crate) struct SidebarDgElement {
    id: ElementId,
    app: WeakEntity<AtlasApp>,
    style: SidebarStyle,
    active_profile_name: String,
    sidebar_collapsed: bool,
    sections: Vec<SidebarDgSectionData>,
    drop_zone: Option<SidebarDgDropZoneData>,
    font: Font,
    font_size: Pixels,
    zoom: f32,
    windows_theme: bool,
    element_style: StyleRefinement,
}

impl SidebarDgElement {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        app: WeakEntity<AtlasApp>,
        style: SidebarStyle,
        active_profile_name: String,
        sidebar_collapsed: bool,
        sections: Vec<SidebarDgSectionData>,
        drop_zone: Option<SidebarDgDropZoneData>,
        font: Font,
        font_size: Pixels,
        zoom: f32,
        windows_theme: bool,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from("sidebar-dg-root")),
            app,
            style,
            active_profile_name,
            sidebar_collapsed,
            sections,
            drop_zone,
            font,
            font_size,
            zoom,
            windows_theme,
            element_style: StyleRefinement::default(),
        }
    }
}

impl IntoElement for SidebarDgElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for SidebarDgElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.element_style
    }
}

impl Element for SidebarDgElement {
    type RequestLayoutState = Style;
    type PrepaintState = SidebarDgPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) -> (LayoutId, Self::RequestLayoutState) {
        crate::perf::count("sidebar_dg.request_layout");

        let mut style = Style::default();
        style.refine(&self.element_style);
        style.size.width =
            gpui::Length::Definite(gpui::DefiniteLength::Relative(gpui::relative(1.0)));

        // Precompute height for scroll container tracking
        let layout = compute_sidebar_layout(
            220.0,
            self.zoom,
            self.windows_theme,
            self.sidebar_collapsed,
            &self.active_profile_name,
            &self.sections,
            self.drop_zone.as_ref(),
            None,
            None,
            self.style.accent,
            self.style.fg,
            self.style.muted,
        );
        style.size.height = gpui::Length::Definite(gpui::DefiniteLength::Absolute(
            gpui::AbsoluteLength::Pixels(px(layout.total_h)),
        ));

        (window.request_layout(style.clone(), [], cx), style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) -> Self::PrepaintState {
        crate::perf::count("sidebar_dg.prepaint");

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        // Slit/sub-pixel culling: if sidebar is collapsing/narrower than 12px, bypass layout & processing
        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return SidebarDgPrepaint {
                hitbox,
                layout: SidebarDgLayout {
                    total_w: 0.0,
                    total_h: 0.0,
                    launcher_slot: Slot::new(0.0, 0.0, 0.0, 0.0),
                    collapse_btn_slot: None,
                    sections: Vec::new(),
                    dropzone_slot: None,
                    dropzone_header_slot: None,
                    dropzone_place_btn_slot: None,
                    dropzone_clear_btn_slot: None,
                    dropzone_empty_hint_slot: None,
                    dropzone_items: Vec::new(),
                    dropzone_target: None,
                },
                all_hits: Vec::new(),
                right_clicks: Vec::new(),
            };
        }

        let origin = bounds.origin;
        let total_w = f32::from(bounds.size.width);

        let blink_id = self
            .app
            .upgrade()
            .and_then(|app| app.read(cx).drag_hover.blink_id.clone());

        let mut layout = compute_sidebar_layout(
            total_w,
            self.zoom,
            self.windows_theme,
            self.sidebar_collapsed,
            &self.active_profile_name,
            &self.sections,
            self.drop_zone.as_ref(),
            blink_id.as_ref(),
            None,
            self.style.accent,
            self.style.fg,
            self.style.muted,
        );

        let mut all_hits = Vec::new();
        let mut right_clicks = Vec::new();
        let mut pending_targets = Vec::new();

        // 1. Header hits
        all_hits.push((layout.launcher_slot, SidebarDgHitAction::AtlasLauncher));
        if let Some(col_slot) = layout.collapse_btn_slot {
            all_hits.push((col_slot, SidebarDgHitAction::CollapseToggle));
        }

        // 2. Sections hits & drop targets
        for sec in &mut layout.sections {
            all_hits.push((sec.header_slot, SidebarDgHitAction::SectionHeader(sec.idx)));
            right_clicks.push((
                sec.header_slot,
                SidebarDgRightClickAction::SectionHeader(sec.idx),
            ));

            for (act_slot, act) in &sec.header_actions {
                all_hits.push((*act_slot, act.clone()));
            }

            if let Some(ref mut dt) = sec.header_drop_target {
                dt.bounds = sec.header_slot.to_bounds(origin);
                pending_targets.push(dt.clone());
            }

            for row in &mut sec.rows {
                all_hits.push((row.row_slot, row.hit_action.clone()));
                if let Some(ref rc) = row.right_click {
                    right_clicks.push((row.row_slot, rc.clone()));
                }

                for (act_slot, act) in &row.action_slots {
                    all_hits.push((*act_slot, act.clone()));
                }

                if let Some(ref mut dt) = row.drop_target {
                    dt.bounds = row.row_slot.to_bounds(origin);
                    pending_targets.push(dt.clone());
                }
            }
        }

        // 3. Dropzone hits & drop target
        if let Some(dz_slot) = layout.dropzone_slot {
            if let Some(place_slot) = layout.dropzone_place_btn_slot {
                all_hits.push((place_slot, SidebarDgHitAction::DropzonePlace));
            }
            if let Some(clear_slot) = layout.dropzone_clear_btn_slot {
                all_hits.push((clear_slot, SidebarDgHitAction::DropzoneClear));
            }
            for item in &layout.dropzone_items {
                all_hits.push((
                    item.slot,
                    SidebarDgHitAction::DropzoneItem(item.path.clone()),
                ));
                all_hits.push((
                    item.remove_btn_slot,
                    SidebarDgHitAction::DropzoneItemRemove(item.path.clone()),
                ));
            }

            if let Some(ref mut dt) = layout.dropzone_target {
                dt.bounds = dz_slot.to_bounds(origin);
                pending_targets.push(dt.clone());
            }
        }

        // Register drop targets & purge stale on App
        let has_gpui_drag = cx.has_active_drag();
        if let Some(app) = self.app.upgrade() {
            let dz_b = layout.dropzone_slot.map(|s| s.to_bounds(origin));
            app.update(cx, |this, _| {
                let is_drag_active = this.is_drag_active(has_gpui_drag);
                if is_drag_active {
                    this.pending_drag_hover_targets
                        .retain(|t| !matches!(t.id, DragHoverId::Sidebar(_)));
                    for t in pending_targets {
                        this.register_drag_hover_target(t);
                    }
                } else if !this.pending_drag_hover_targets.is_empty() {
                    this.pending_drag_hover_targets
                        .retain(|t| !matches!(t.id, DragHoverId::Sidebar(_)));
                }
                this.dropzone_bounds = dz_b;
            });
        }

        SidebarDgPrepaint {
            hitbox,
            layout,
            all_hits,
            right_clicks,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) {
        crate::perf::count("sidebar_dg.paint");

        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return;
        }

        let origin = bounds.origin;
        let zoom = self.zoom;
        let font = self.font.clone();
        let font_size = self.font_size;
        let windows_theme = self.windows_theme;

        let mouse_pos = window.mouse_position();
        let is_over = prepaint.hitbox.is_hovered(window);
        let lx = f32::from(mouse_pos.x - origin.x);
        let ly = f32::from(mouse_pos.y - origin.y);

        let active_hit = if is_over {
            prepaint
                .all_hits
                .iter()
                .find(|(slot, _)| slot.contains(lx, ly))
                .map(|(_, act)| act)
        } else {
            None
        };

        if is_over && active_hit.is_some() {
            window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
        }

        let radius = if windows_theme { 7.0 } else { 6.0 } * zoom;
        let accent = self.style.accent;
        let fg = self.style.fg;
        let muted = self.style.muted;
        let hover_bg = self.style.hover;
        let active_bg = self.style.active_bg;

        // Hardware clipped layer
        window.paint_layer(bounds, |window| {
            // 1. Paint Header: Atlas launcher trigger + collapse button
            let launcher_b = prepaint.layout.launcher_slot.to_bounds(origin);
            if active_hit == Some(&SidebarDgHitAction::AtlasLauncher) {
                paint_rounded(window, launcher_b, radius, hover_bg);
            }

            if self.sidebar_collapsed {
                // Centered Atlas simple logo
                paint_theme_icon(
                    window,
                    cx,
                    origin,
                    prepaint.layout.launcher_slot,
                    AtlasIcon::Folder,
                    windows_theme,
                    16.0 * zoom,
                    muted,
                );
            } else {
                // Logo on left + profile name + caret on right
                let mut cut = RectCut::new(
                    prepaint.layout.launcher_slot.x,
                    prepaint.layout.launcher_slot.y,
                    prepaint.layout.launcher_slot.w,
                    prepaint.layout.launcher_slot.h,
                );
                let logo_slot = cut.cut_left(24.0 * zoom);
                let caret_slot = cut.cut_right(16.0 * zoom);
                let name_slot = cut.remaining();

                paint_theme_icon(
                    window,
                    cx,
                    origin,
                    logo_slot,
                    AtlasIcon::Folder,
                    windows_theme,
                    16.0 * zoom,
                    muted,
                );

                paint_theme_icon(
                    window,
                    cx,
                    origin,
                    caret_slot,
                    AtlasIcon::CaretDown,
                    windows_theme,
                    11.0 * zoom,
                    muted.opacity(0.8),
                );

                let line = shape_text_truncated(
                    &self.active_profile_name,
                    &font,
                    font_size,
                    fg,
                    name_slot.w,
                    window,
                );
                let _ = line.paint(
                    point(
                        origin.x + px(name_slot.x),
                        origin.y + px(text_origin_y(name_slot.y, zoom)),
                    ),
                    px(name_slot.h),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }

            if let Some(col_slot) = prepaint.layout.collapse_btn_slot {
                let col_b = col_slot.to_bounds(origin);
                if active_hit == Some(&SidebarDgHitAction::CollapseToggle) {
                    paint_rounded(window, col_b, radius, hover_bg);
                }
                paint_theme_icon(
                    window,
                    cx,
                    origin,
                    col_slot,
                    AtlasIcon::SidebarSimple,
                    windows_theme,
                    16.0 * zoom,
                    muted,
                );
            }

            // 2. Paint Sections
            for sec in &prepaint.layout.sections {
                if !self.sidebar_collapsed {
                    // Section Header
                    let h_slot = sec.header_slot;
                    let h_bounds = h_slot.to_bounds(origin);
                    if active_hit == Some(&SidebarDgHitAction::SectionHeader(sec.idx)) {
                        paint_rounded(window, h_bounds, radius, hover_bg);
                    }

                    // Section title
                    let title_str = sidebar_label(&sec.kind);
                    let title_line = shape_text_truncated(
                        &title_str,
                        &font,
                        px(11.0 * zoom),
                        muted,
                        sec.title_slot.w,
                        window,
                    );
                    let _ = title_line.paint(
                        point(
                            origin.x + px(sec.title_slot.x),
                            origin.y + px(text_origin_y(sec.title_slot.y, zoom)),
                        ),
                        px(sec.title_slot.h),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );

                    // Chevron
                    let ch_icon = if sec.collapsed {
                        AtlasIcon::CaretRight
                    } else {
                        AtlasIcon::CaretDown
                    };
                    paint_theme_icon(
                        window,
                        cx,
                        origin,
                        sec.chevron_slot,
                        ch_icon,
                        windows_theme,
                        11.0 * zoom,
                        muted,
                    );

                    // Header Action Buttons (+ Tag / + Bookmark)
                    for (act_slot, act) in &sec.header_actions {
                        let act_b = act_slot.to_bounds(origin);
                        if active_hit == Some(act) {
                            paint_rounded(window, act_b, radius, hover_bg);
                        }
                        paint_theme_icon(
                            window,
                            cx,
                            origin,
                            *act_slot,
                            AtlasIcon::Plus,
                            windows_theme,
                            12.0 * zoom,
                            muted,
                        );
                    }
                }

                // Section Rows
                for row in &sec.rows {
                    let r_bounds = row.row_slot.to_bounds(origin);

                    // Background highlights: Selection > Drag Hover > Mouse Hover > Blink
                    if row.is_active {
                        paint_rounded(window, r_bounds, radius, active_bg);
                    } else if active_hit == Some(&row.hit_action) {
                        paint_rounded(window, r_bounds, radius, hover_bg);
                    }

                    if row.is_blinking {
                        paint_rounded(window, r_bounds, radius, accent.opacity(0.40));
                    }

                    if row.is_drag_over {
                        window.paint_quad(gpui::quad(
                            r_bounds,
                            Corners::all(px(radius)),
                            accent.opacity(0.18),
                            Edges::all(px(1.5)),
                            accent,
                            Default::default(),
                        ));
                    }

                    // Row Icon / Shell Texture / Tag color dot
                    if let Some(col) = row.tag_color {
                        let dot_sz = 10.0 * zoom;
                        let dot_b = Bounds {
                            origin: point(
                                origin.x + px(row.icon_slot.x + (row.icon_slot.w - dot_sz) * 0.5),
                                origin.y + px(row.icon_slot.y + (row.icon_slot.h - dot_sz) * 0.5),
                            ),
                            size: size(px(dot_sz), px(dot_sz)),
                        };
                        let dot_hsla = gpui::rgb_to_hsla(gpui::rgb(
                            (u32::from(col[0]) << 16)
                                | (u32::from(col[1]) << 8)
                                | u32::from(col[2]),
                        ));
                        paint_rounded(window, dot_b, dot_sz * 0.5, dot_hsla);
                    } else if let Some(tex) = &row.shell_tex {
                        let sz = if windows_theme { 18.0 } else { 16.0 } * zoom;
                        let img_b = Bounds {
                            origin: point(
                                origin.x + px(row.icon_slot.x + (row.icon_slot.w - sz) * 0.5),
                                origin.y + px(row.icon_slot.y + (row.icon_slot.h - sz) * 0.5),
                            ),
                            size: size(px(sz), px(sz)),
                        };
                        let img_b = crate::ui::thumbs::contain_bounds(tex, img_b);
                        let _ = window.paint_image(
                            img_b,
                            img_b,
                            Corners::all(px(0.0)),
                            tex.clone(),
                            0,
                            false,
                        );
                    } else {
                        paint_theme_icon(
                            window,
                            cx,
                            origin,
                            row.icon_slot,
                            row.icon,
                            windows_theme,
                            if windows_theme { 18.0 } else { 16.0 } * zoom,
                            row.icon_color,
                        );
                    }

                    if !self.sidebar_collapsed {
                        // Row Label
                        let text_color = if row.is_active { accent } else { fg };
                        let line = shape_text_truncated(
                            &row.label,
                            &font,
                            font_size,
                            text_color,
                            row.label_slot.w,
                            window,
                        );
                        let _ = line.paint(
                            point(
                                origin.x + px(row.label_slot.x),
                                origin.y + px(text_origin_y(row.label_slot.y, zoom)),
                            ),
                            px(row.label_slot.h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );

                        // Row Badge (e.g. Free Space or Cloud status)
                        if let (Some(b_slot), Some(b_text)) = (row.badge_slot, &row.badge_text) {
                            let b_color = row.badge_color.unwrap_or(muted);
                            let b_line = shape_text_truncated(
                                b_text,
                                &font,
                                px(11.0 * zoom),
                                b_color,
                                b_slot.w,
                                window,
                            );
                            let _ = b_line.paint(
                                point(
                                    origin.x + px(b_slot.x),
                                    origin.y + px(text_origin_y(b_slot.y, zoom)),
                                ),
                                px(b_slot.h),
                                TextAlign::Right,
                                None,
                                window,
                                cx,
                            );
                        }

                        // Drive Used Space Meter Bar
                        if let Some((track_slot, frac, bar_col)) = row.meter_slot {
                            let track_b = track_slot.to_bounds(origin);
                            paint_rounded(window, track_b, 2.0 * zoom, fg.opacity(0.12));
                            let fill_w = track_slot.w * frac.clamp(0.0, 1.0);
                            if fill_w > 0.0 {
                                let fill_b = Bounds {
                                    origin: track_b.origin,
                                    size: size(px(fill_w), track_b.size.height),
                                };
                                paint_rounded(window, fill_b, 2.0 * zoom, bar_col);
                            }
                        }

                        // Action Buttons (e.g. remove bookmark X, edit/delete tag)
                        for (act_slot, act) in &row.action_slots {
                            let act_b = act_slot.to_bounds(origin);
                            if active_hit == Some(act) {
                                paint_rounded(window, act_b, radius, hover_bg);
                            }
                            let act_icon = match act {
                                SidebarDgHitAction::BookmarkRemove(_)
                                | SidebarDgHitAction::TagDelete(_) => AtlasIcon::X,
                                SidebarDgHitAction::TagRename { .. } => AtlasIcon::PencilSimple,
                                SidebarDgHitAction::RemovableEject { is_mounted, .. } => {
                                    if *is_mounted {
                                        AtlasIcon::Minus
                                    } else {
                                        AtlasIcon::Plus
                                    }
                                }
                                _ => AtlasIcon::X,
                            };
                            paint_theme_icon(
                                window,
                                cx,
                                origin,
                                *act_slot,
                                act_icon,
                                windows_theme,
                                11.0 * zoom,
                                muted,
                            );
                        }
                    }
                }
            }

            // 3. Paint Drop Zone
            if let Some(dz_slot) = prepaint.layout.dropzone_slot {
                let dz_bounds = dz_slot.to_bounds(origin);
                let drop_bg = fg.opacity(0.03);
                paint_rounded(window, dz_bounds, 12.0 * zoom, drop_bg);

                // Header
                if let Some(h_slot) = prepaint.layout.dropzone_header_slot {
                    let title_line = shape_text_truncated(
                        "Dropzone",
                        &font,
                        px(12.0 * zoom),
                        muted,
                        h_slot.w,
                        window,
                    );
                    let _ = title_line.paint(
                        point(
                            origin.x + px(h_slot.x),
                            origin.y + px(text_origin_y(h_slot.y, zoom)),
                        ),
                        px(h_slot.h),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }

                if let Some(place_slot) = prepaint.layout.dropzone_place_btn_slot {
                    let place_b = place_slot.to_bounds(origin);
                    if active_hit == Some(&SidebarDgHitAction::DropzonePlace) {
                        paint_rounded(window, place_b, radius, hover_bg);
                    }
                    paint_theme_icon(
                        window,
                        cx,
                        origin,
                        place_slot,
                        AtlasIcon::ArrowUp,
                        windows_theme,
                        13.0 * zoom,
                        muted,
                    );
                }

                if let Some(clear_slot) = prepaint.layout.dropzone_clear_btn_slot {
                    let clear_b = clear_slot.to_bounds(origin);
                    if active_hit == Some(&SidebarDgHitAction::DropzoneClear) {
                        paint_rounded(window, clear_b, radius, hover_bg);
                    }
                    paint_theme_icon(
                        window,
                        cx,
                        origin,
                        clear_slot,
                        AtlasIcon::X,
                        windows_theme,
                        12.0 * zoom,
                        muted,
                    );
                }

                // Empty hint or items
                if let Some(hint_slot) = prepaint.layout.dropzone_empty_hint_slot {
                    let hint_text = rust_i18n::t!("sidebar_drop_zone_hint").to_string();
                    // Wrap instead of eliding so the hint survives a narrow sidebar;
                    // the block is centered in the slot.
                    let line_h = 15.0 * zoom;
                    let max_lines = ((hint_slot.h / line_h).floor() as usize).max(1);
                    let hint_lines = wrap_text_lines(
                        &hint_text,
                        &font,
                        px(11.0 * zoom),
                        muted.opacity(0.8),
                        hint_slot.w,
                        max_lines,
                        window,
                    );
                    let top = hint_slot.y
                        + (hint_slot.h - line_h * hint_lines.len() as f32).max(0.0) * 0.5;
                    for (i, hint_line) in hint_lines.iter().enumerate() {
                        let h_x =
                            hint_slot.x + (hint_slot.w - line_width(hint_line)).max(0.0) * 0.5;
                        let _ = hint_line.paint(
                            point(
                                origin.x + px(h_x),
                                origin.y + px(text_origin_y(top + line_h * i as f32, zoom)),
                            ),
                            px(line_h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                } else {
                    for item in &prepaint.layout.dropzone_items {
                        let it_b = item.slot.to_bounds(origin);
                        if item.selected {
                            paint_rounded(window, it_b, radius, accent.opacity(0.18));
                        } else if active_hit
                            == Some(&SidebarDgHitAction::DropzoneItem(item.path.clone()))
                        {
                            paint_rounded(window, it_b, radius, hover_bg);
                        }

                        // Preview thumbnail/icon
                        let prev_b = item.preview_slot.to_bounds(origin);
                        if let Some(thumb) = &item.thumb_tex {
                            let _ = window.paint_image(
                                prev_b,
                                prev_b,
                                Corners::all(px(4.0 * zoom)),
                                thumb.clone(),
                                0,
                                false,
                            );
                        } else {
                            paint_theme_icon(
                                window,
                                cx,
                                origin,
                                item.preview_slot,
                                AtlasIcon::File,
                                windows_theme,
                                24.0 * zoom,
                                muted,
                            );
                        }

                        // Name
                        let name_line = shape_text_truncated(
                            &item.name,
                            &font,
                            px(10.0 * zoom),
                            fg,
                            item.name_slot.w,
                            window,
                        );
                        let name_w = line_width(&name_line);
                        let nx = item.name_slot.x + (item.name_slot.w - name_w) * 0.5;
                        let _ = name_line.paint(
                            point(
                                origin.x + px(nx),
                                origin.y + px(text_origin_y(item.name_slot.y, zoom)),
                            ),
                            px(item.name_slot.h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );

                        // Remove button
                        paint_theme_icon(
                            window,
                            cx,
                            origin,
                            item.remove_btn_slot,
                            AtlasIcon::X,
                            windows_theme,
                            10.0 * zoom,
                            muted,
                        );
                    }
                }
            }
        });

        // Interactive mouse handling
        if let Some(app_entity) = self.app.upgrade() {
            let click_view = app_entity.clone();
            let click_hitbox = prepaint.hitbox.clone();
            let all_hits = prepaint.all_hits.clone();
            let right_clicks = prepaint.right_clicks.clone();
            let current_path = self.drop_zone.as_ref().and_then(|dz| dz.dest.clone());

            window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if !click_hitbox.is_hovered(window) {
                    return;
                }

                let lx = f32::from(ev.position.x - origin.x);
                let ly = f32::from(ev.position.y - origin.y);

                if ev.button == MouseButton::Left {
                    for (slot, act) in &all_hits {
                        if slot.contains(lx, ly) {
                            let act = act.clone();
                            let current_path = current_path.clone();
                            click_view.update(cx, |this, cx| {
                                let pane = this.state.focused_tile;
                                match act {
                                    SidebarDgHitAction::AtlasLauncher => {
                                        this.state.profile_ui.atlas_launcher_profiles_open =
                                            !this.state.profile_ui.atlas_launcher_profiles_open;
                                    }
                                    SidebarDgHitAction::CollapseToggle => {
                                        this.process_actions(vec![Action::ToggleSidebarCollapsed]);
                                    }
                                    SidebarDgHitAction::SectionHeader(idx) => {
                                        this.process_actions(vec![Action::ToggleSidebarSection(
                                            idx,
                                        )]);
                                    }
                                    SidebarDgHitAction::AddBookmark => {
                                        if let Some(dest) = &current_path {
                                            let name = dest
                                                .file_name()
                                                .map(|n| n.to_string_lossy().into_owned())
                                                .unwrap_or_else(|| dest.display().to_string());
                                            this.process_actions(vec![Action::AddBookmark {
                                                name,
                                                path: dest.clone(),
                                            }]);
                                        }
                                    }
                                    SidebarDgHitAction::AddTag => {
                                        this.process_actions(vec![Action::OpenCreateTagOverlay {
                                            global: false,
                                        }]);
                                    }
                                    SidebarDgHitAction::NavRow(path) => {
                                        this.navigate(pane, path);
                                    }
                                    SidebarDgHitAction::VirtualNavRow(loc) => {
                                        this.navigate_to(pane, loc, crate::app::nav::NavKind::Push);
                                    }
                                    SidebarDgHitAction::BookmarkRow { path, .. } => {
                                        this.navigate(pane, path);
                                    }
                                    SidebarDgHitAction::BookmarkRemove(idx) => {
                                        this.process_actions(vec![Action::RemoveBookmark(idx)]);
                                    }
                                    SidebarDgHitAction::TagRow(id) => {
                                        this.open_tag_results(pane, id);
                                    }
                                    SidebarDgHitAction::TagRename {
                                        tag,
                                        name,
                                        color,
                                        global,
                                    } => {
                                        this.process_actions(vec![Action::OpenRenameTagOverlay {
                                            tag,
                                            name,
                                            color,
                                            global,
                                        }]);
                                    }
                                    SidebarDgHitAction::TagDelete(id) => {
                                        this.process_actions(vec![Action::DeleteTag(id)]);
                                    }
                                    SidebarDgHitAction::DriveRow(path) => {
                                        this.navigate(pane, path);
                                    }
                                    SidebarDgHitAction::RemovableRow(dev) => {
                                        #[cfg(target_os = "linux")]
                                        this.mount_device(dev);
                                        #[cfg(not(target_os = "linux"))]
                                        let _ = dev;
                                    }
                                    SidebarDgHitAction::RemovableEject { dev, is_mounted } => {
                                        #[cfg(target_os = "linux")]
                                        if is_mounted {
                                            this.unmount_device(dev);
                                        } else {
                                            this.mount_device(dev);
                                        }
                                        #[cfg(not(target_os = "linux"))]
                                        let _ = (dev, is_mounted);
                                    }
                                    SidebarDgHitAction::CloudRemoteRow(id) => {
                                        this.handle_cloud_action(CloudAction::OpenAccess(id));
                                    }
                                    SidebarDgHitAction::DropzonePlace => {
                                        if let Some(dest) = current_path {
                                            place_sidebar_drop_zone(this, dest);
                                        }
                                    }
                                    SidebarDgHitAction::DropzoneClear => {
                                        this.state.sidebar_drop_zone.clear();
                                        this.dropzone_selection.clear();
                                        this.dropzone_rects.clear();
                                        this.dropzone_pending_tags.clear();
                                    }
                                    SidebarDgHitAction::DropzoneItem(path) => {
                                        if this.dropzone_selection.selected.contains(&path) {
                                            this.dropzone_selection.selected.remove(&path);
                                        } else {
                                            this.dropzone_selection.selected.insert(path);
                                        }
                                    }
                                    SidebarDgHitAction::DropzoneItemRemove(path) => {
                                        if let Some(pos) = this
                                            .state
                                            .sidebar_drop_zone
                                            .iter()
                                            .position(|it| it.path == path)
                                        {
                                            this.state.sidebar_drop_zone.remove(pos);
                                            this.dropzone_selection.selected.remove(&path);
                                        }
                                    }
                                }
                                cx.notify();
                            });
                            return;
                        }
                    }
                } else if ev.button == MouseButton::Right {
                    for (slot, rc) in &right_clicks {
                        if slot.contains(lx, ly) {
                            let rc = rc.clone();
                            let pos = ev.position;
                            click_view.update(cx, |this, cx| {
                                match rc {
                                    SidebarDgRightClickAction::Path(path) => {
                                        this.open_ctx_menu(vec![path], pos, window, cx);
                                    }
                                    SidebarDgRightClickAction::Drive(path) => {
                                        this.open_ctx_menu(vec![path], pos, window, cx);
                                    }
                                    SidebarDgRightClickAction::SectionHeader(idx) => {
                                        this.process_actions(vec![Action::ToggleSidebarSection(
                                            idx,
                                        )]);
                                    }
                                    SidebarDgRightClickAction::Bookmark(idx) => {
                                        this.process_actions(vec![Action::RemoveBookmark(idx)]);
                                    }
                                }
                                cx.notify();
                            });
                            return;
                        }
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_header_layout_expanded() {
        let layout = compute_sidebar_layout(
            240.0,
            1.0,
            true,
            false,
            "Default Profile",
            &[],
            None,
            None,
            None,
            Hsla::default(),
            Hsla::default(),
            Hsla::default(),
        );

        assert!(layout.launcher_slot.w > 150.0);
        assert!(layout.collapse_btn_slot.is_some());
        let col = layout.collapse_btn_slot.unwrap();
        assert_eq!(col.w, 30.0);
        assert!(layout.total_h >= 38.0);
    }

    #[test]
    fn test_compute_header_layout_collapsed() {
        let layout = compute_sidebar_layout(
            60.0,
            1.0,
            true,
            true,
            "Default Profile",
            &[],
            None,
            None,
            None,
            Hsla::default(),
            Hsla::default(),
            Hsla::default(),
        );

        assert_eq!(layout.launcher_slot.w, 30.0);
        assert_eq!(layout.launcher_slot.x, (60.0 - 30.0) * 0.5);
        assert!(layout.collapse_btn_slot.is_some());
    }

    #[test]
    fn test_sections_visibility_and_collapsing() {
        let sections = vec![
            SidebarDgSectionData {
                idx: 0,
                kind: SectionKind::Places,
                title: "Places".to_string(),
                collapsed: false,
                hidden: false,
                items: vec![
                    SidebarDgItemData::Nav {
                        id: "place-home".to_string(),
                        label: "Home".to_string(),
                        icon: AtlasIcon::House,
                        path: PathBuf::from("/home/user"),
                        shell_tex: None,
                        active: false,
                    },
                    SidebarDgItemData::Nav {
                        id: "place-desktop".to_string(),
                        label: "Desktop".to_string(),
                        icon: AtlasIcon::Monitor,
                        path: PathBuf::from("/home/user/Desktop"),
                        shell_tex: None,
                        active: true,
                    },
                ],
            },
            SidebarDgSectionData {
                idx: 1,
                kind: SectionKind::Bookmarks,
                title: "Bookmarks".to_string(),
                collapsed: true,
                hidden: false,
                items: vec![SidebarDgItemData::Bookmark {
                    index: 0,
                    name: "Rust".to_string(),
                    path: PathBuf::from("/rust"),
                    shell_tex: None,
                    active: false,
                }],
            },
            SidebarDgSectionData {
                idx: 2,
                kind: SectionKind::Tags,
                title: "Tags".to_string(),
                collapsed: false,
                hidden: true,
                items: vec![],
            },
        ];

        let layout = compute_sidebar_layout(
            220.0,
            1.0,
            true,
            false,
            "Profile",
            &sections,
            None,
            None,
            None,
            Hsla::default(),
            Hsla::default(),
            Hsla::default(),
        );

        // Hidden section (idx 2) is not emitted
        assert_eq!(layout.sections.len(), 2);
        // Section 0 is expanded with 2 rows
        assert_eq!(layout.sections[0].rows.len(), 2);
        assert_eq!(layout.sections[0].rows[0].label, "Home");
        assert_eq!(layout.sections[0].rows[1].label, "Desktop");
        assert!(layout.sections[0].rows[1].is_active);

        // Section 1 is collapsed: 0 rows emitted
        assert_eq!(layout.sections[1].rows.len(), 0);
        assert!(layout.sections[1].collapsed);
    }

    #[test]
    fn test_drive_row_meter_layout() {
        let sections = vec![SidebarDgSectionData {
            idx: 0,
            kind: SectionKind::Drives,
            title: "Drives".to_string(),
            collapsed: false,
            hidden: false,
            items: vec![SidebarDgItemData::Drive {
                mount_point: PathBuf::from("C:\\"),
                label: "Local Disk (C:)".to_string(),
                free_label: "50 GB free".to_string(),
                fraction: 0.85,
                icon: AtlasIcon::HardDrive,
                shell_tex: None,
                active: false,
            }],
        }];

        let layout = compute_sidebar_layout(
            240.0,
            1.0,
            true,
            false,
            "Profile",
            &sections,
            None,
            None,
            None,
            Hsla::default(),
            Hsla::default(),
            Hsla::default(),
        );

        let row = &layout.sections[0].rows[0];
        assert_eq!(row.label, "Local Disk (C:)");
        assert_eq!(row.badge_text.as_deref(), Some("50 GB free"));
        assert!(row.meter_slot.is_some());
        let (meter_slot, frac, _) = row.meter_slot.unwrap();
        assert_eq!(frac, 0.85);
        assert!(meter_slot.w > 0.0);
        assert_eq!(meter_slot.h, 4.0);
    }

    #[test]
    fn test_drop_targets_geometry() {
        let sections = vec![SidebarDgSectionData {
            idx: 0,
            kind: SectionKind::Places,
            title: "Places".to_string(),
            collapsed: false,
            hidden: false,
            items: vec![SidebarDgItemData::Nav {
                id: "place-home".to_string(),
                label: "Home".to_string(),
                icon: AtlasIcon::House,
                path: PathBuf::from("/home/user"),
                shell_tex: None,
                active: false,
            }],
        }];

        let layout = compute_sidebar_layout(
            220.0,
            1.0,
            true,
            false,
            "Profile",
            &sections,
            None,
            None,
            None,
            Hsla::default(),
            Hsla::default(),
            Hsla::default(),
        );

        let target = &layout.sections[0].rows[0].drop_target;
        assert!(target.is_some());
        let t = target.as_ref().unwrap();
        assert_eq!(t.id, DragHoverId::Sidebar("place-home".to_string()));
        assert!(
            matches!(t.command, DragHoverCommand::NavigatePath(ref p) if p == &PathBuf::from("/home/user"))
        );
        assert!(matches!(
            t.drop_command,
            Some(DragHoverDropCommand::TransferTo(ref p)) if p == &PathBuf::from("/home/user")
        ));
    }

    #[test]
    fn test_collapsed_rail_layout() {
        let sections = vec![SidebarDgSectionData {
            idx: 0,
            kind: SectionKind::Places,
            title: "Places".to_string(),
            collapsed: false,
            hidden: false,
            items: vec![SidebarDgItemData::Nav {
                id: "home".to_string(),
                label: "Home".to_string(),
                icon: AtlasIcon::House,
                path: PathBuf::from("/home"),
                shell_tex: None,
                active: false,
            }],
        }];

        let layout = compute_sidebar_layout(
            48.0,
            1.0,
            true,
            true,
            "Profile",
            &sections,
            None,
            None,
            None,
            Hsla::default(),
            Hsla::default(),
            Hsla::default(),
        );

        assert_eq!(layout.sections.len(), 1);
        assert_eq!(layout.sections[0].rows.len(), 1);
        let row = &layout.sections[0].rows[0];
        assert_eq!(row.row_slot.w, COLLAPSED_ROW_H);
        assert_eq!(row.row_slot.x, (48.0 - COLLAPSED_ROW_H) * 0.5);
    }
}
