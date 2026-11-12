//! Grid (icons) view: `render_grid` + grid cell + thumbnail predicate helpers.
#![allow(clippy::too_many_lines)] // reason: cohesive view renderer kept whole

use crate::app::App;
use crate::ui::drag::FileDragSource as _;
use crate::ui::drag_hover::DragHoverId;
use crate::ui::file_icons::FileIcon;
use crate::ui::glass::backdrop_tint;
use crate::ui::layout_region::{
    layout_region, timed_layout_region, PANE_CRUMBS_TIMINGS, PANE_VIEWPORT_TIMINGS,
};
use crate::ui::root::{ctrl_scroll, pane_bounds_recorder, rect_recorder};
use crate::ui::selection_checkbox::SelectionCheckbox;
use crate::ui::views::grid_surface_element_deprecated::{GridSurfaceColors, GridSurfaceElement};
use crate::ui::views::virtual_grid_deprecated::AnimatedVirtualGrid;
use atlas_model::file_entry::{FileEntry, FileKind};
use atlas_model::pane::TabId;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, img, px, AbsoluteLength, AnyElement, Context, Entity, Hsla, MouseButton, SharedString,
    Window,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex, ActiveTheme, ElementExt};
use std::ops::Range;

pub(crate) const GRID_GAP: f32 = 16.0;
pub(crate) const GRID_PAD: f32 = 12.0;
pub(crate) const GRID_ICON_AREA: f32 = 0.72;
pub(crate) const GRID_TEXTURE_ICON_MAX: f32 = 0.66;
pub(crate) const GRID_BUILTIN_ICON: f32 = 0.54;
pub(crate) const GRID_LABEL_H: f32 = 32.0;
pub(crate) const USE_GRID_SURFACE_ELEMENT: bool = true;

impl App {
    /// Icons (Grid) view: a virtualized grid of fixed-size cells. Column count is
    /// derived from the viewport width and the per-tab thumbnail size, so it stays
    /// virtualized (only visible rows built) for huge directories.
    pub(crate) fn render_grid(
        &mut self,
        pane_id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        crate::perf::count("render.view.grid");
        let view = cx.entity();
        let active_theme = self.state.profiles.active().theme.as_str();
        let is_glass = active_theme.ends_with(" Glass");
        let (fg, accent, accent_fg, hover, muted, border, _header_bg) = {
            let t = cx.theme();
            (
                t.foreground,
                t.accent,
                t.accent_foreground,
                t.list_active,
                t.muted_foreground,
                t.border,
                if is_glass {
                    t.list_head
                } else {
                    backdrop_tint(t.secondary, 0.18)
                },
            )
        };
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let row_font = window.text_style().font();
        let crumb_strip =
            self.render_crumb_strip(pane_id, &view, fg, muted, border, _header_bg, cx);
        // `thumb` stays in rem units (same meaning as before the zoom refactor) —
        // it flows into `render_cell`/`FileIcon::element`, which resolve it via
        // `rem_px()`. The virtualization math below runs against real measured
        // pixels (pane_w, scroll offsets), so it needs its own scaled copy.
        let thumb = self
            .tab_data_for(pane_id)
            .map_or(128.0, |t| t.thumbnail_size);
        let zoom_scale = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let thumb_px = thumb * zoom_scale;
        let count = self
            .state
            .tab_states
            .get_mut(&pane_id)
            .map_or(0, |ts| ts.visible_indices().len());
        let grid_gap = self.state.settings.grid_spacing.clamp(4.0, 48.0);
        let grid_pad = (grid_gap * 0.75).round().max(4.0);
        let grid_gap_px = grid_gap * zoom_scale;
        let grid_pad_px = grid_pad * zoom_scale;
        // Use the width captured by on_prepaint from the previous frame. This
        // reflects the real pane size and adapts to preview panels and splitter
        // drags. Fall back to a viewport estimate on the very first frame before
        // any paint has run.
        let pane_w = self
            .state
            .tab_states
            .get(&pane_id)
            .map(|ts| ts.last_pane_width)
            .filter(|&w| w > 1.0)
            .unwrap_or_else(|| (f32::from(window.viewport_size().width) - 217.0).max(0.0));
        let cols = grid_column_count(pane_w, thumb_px, grid_gap_px, grid_pad_px);
        let _perf = crate::perf::span_detail("grid.render", || {
            format!(
                "pane={} count={count} cols={cols} thumb={thumb:.1} pane_w={pane_w:.1}",
                pane_id.0
            )
        });

        // Hoist scroll handle so the outer relative wrapper can host the scrollbar
        // (vertical_scrollbar must NOT be inside overflow_y_scroll — it scrolls away).
        let scroll = self.scroll_handle_for(pane_id);
        let base_scroll = scroll.0.borrow().base_handle.clone();

        let animate_grid_reflow = !self.state.settings.disable_animations
            && !self
                .rename_edit
                .as_ref()
                .is_some_and(|edit| edit.pane == pane_id)
            && !self.suppress_window_resize_animations()
            && self.dock_split_animations.is_empty()
            && self.pane_layout_animations.is_empty()
            && self.file_panel_cache.len() <= if cfg!(debug_assertions) { 4 } else { 6 };

        let inner = if count == 0 {
            self.grid_hit.remove(&pane_id);
            if self
                .grid_hover
                .is_some_and(|(hover_pane, _)| hover_pane == pane_id)
            {
                self.grid_hover = None;
            }
            let msg = self.empty_pane_message(pane_id, rust_i18n::t!("empty_folder").into_owned());
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .text_color(muted)
                .on_mouse_down(
                    MouseButton::Left,
                    crate::ui::views::on_pane_rubber_mouse_down(&view, pane_id),
                )
                .children(msg)
                .into_any_element()
        } else {
            let mut scroll_y = (-f32::from(base_scroll.offset().y)).max(0.0);
            let viewport_h = {
                let tracked = f32::from(base_scroll.bounds().size.height);
                if tracked > 1.0 {
                    tracked
                } else {
                    f32::from(window.viewport_size().height)
                }
            };
            let cell_h_px = thumb_px * GRID_ICON_AREA + GRID_LABEL_H * zoom_scale;
            let pitch = cell_h_px + grid_gap_px;
            base_scroll.set_line_scroll_height(px(pitch));
            let rows = count.div_ceil(cols);
            let content_h = if rows == 0 {
                0.0
            } else {
                grid_pad_px * 2.0 + rows as f32 * pitch - grid_gap_px
            };
            let max_scroll = self.file_scroll_max(content_h, viewport_h);
            if scroll_y > max_scroll {
                let offset = base_scroll.offset();
                base_scroll.set_offset(gpui::point(offset.x, gpui::px(-max_scroll)));
                scroll_y = max_scroll;
            }
            let start_row = (scroll_y / pitch).floor().max(0.0) as usize;
            let end_row = ((scroll_y + viewport_h) / pitch).ceil() as usize + 1;
            let start = (start_row * cols).min(count);
            let end = ((end_row * cols).min(count)).max(start);
            self.prepare_grid_visible(pane_id, start..end);

            let list_view = view.clone();
            let inner_scroll_view = view.clone();
            let visible = self
                .state
                .tab_states
                .get(&pane_id)
                .map(|ts| ts.visible.clone())
                .unwrap_or_default();
            let surface_colors = GridSurfaceColors {
                fg,
                accent,
                accent_fg,
                hover,
                muted,
            };
            let surface = if USE_GRID_SURFACE_ELEMENT {
                GridSurfaceElement::new(
                    pane_id,
                    view.downgrade(),
                    visible,
                    start,
                    end,
                    cols,
                    count,
                    thumb_px,
                    cell_h_px,
                    thumb_px + grid_gap_px,
                    pitch,
                    thumb_px,
                    scroll_y,
                    viewport_h,
                    surface_colors,
                    row_font.clone(),
                    font_size,
                )
                .padding(grid_pad_px, grid_pad_px)
                .animated(animate_grid_reflow)
                .absolute()
                .inset_0()
                .into_any_element()
            } else {
                AnimatedVirtualGrid::new(
                    SharedString::from(format!("anim-grid-{}", pane_id.0)),
                    count,
                    cols,
                    gpui::Size {
                        width: gpui::px(thumb_px),
                        height: gpui::px(cell_h_px),
                    },
                    gpui::Size {
                        width: gpui::px(grid_gap_px),
                        height: gpui::px(grid_gap_px),
                    },
                    scroll_y,
                    viewport_h,
                    animate_grid_reflow,
                    move |idx, mut is_animating, window, cx| {
                        let app = list_view.read(cx);
                        if !app.dock_split_animations.is_empty()
                            || !app.pane_layout_animations.is_empty()
                        {
                            is_animating = true;
                        }
                        let Some(ts) = app.state.tab_states.get(&pane_id) else {
                            return div().into_any_element();
                        };
                        let Some(&entry_idx) = ts.visible.get(idx) else {
                            return div().into_any_element();
                        };
                        let Some(entry) = ts.entries.get(entry_idx) else {
                            return div().into_any_element();
                        };
                        let file_icon = app.file_icon_for_entry(
                            &entry.path(),
                            &entry.kind(),
                            entry.extension().as_deref(),
                        );
                        let flashing = matches!(entry.kind(), FileKind::Dir)
                            && app.drag_hover.blink_id.as_ref()
                                == Some(&DragHoverId::Folder(pane_id, entry.path()));
                        let rename_input =
                            crate::ui::views::rename::active_input(app, pane_id, entry_idx);
                        let selected = ts.selection.selected.contains(&entry_idx);
                        render_cell(
                            GridCellProps {
                                entry,
                                entry_idx,
                                selected,
                                show_checkbox: ts.selection.selected.len() > 1,
                                pane_id,
                                view: &list_view,
                                thumb,
                                cell_w: thumb,
                                file_icon,
                                accent,
                                accent_fg,
                                hover,
                                border,
                                flashing,
                                record_rects: true,
                                rename_input,
                                cut: app.state.system_cut_paths.contains(&entry.path()),
                                is_animating,
                                dimmed: crate::ui::views::rename::dim_other_entry(
                                    app, pane_id, entry_idx,
                                ),
                                window,
                            },
                            cx,
                        )
                    },
                )
                .into_any_element()
            };
            let mut rename_overlay = None;
            if USE_GRID_SURFACE_ELEMENT {
                if let Some(ts) = self.state.tab_states.get(&pane_id) {
                    for visible_pos in start..end {
                        let Some(&entry_idx) = ts.visible.get(visible_pos) else {
                            continue;
                        };
                        let Some(rename_input) =
                            crate::ui::views::rename::active_input(self, pane_id, entry_idx)
                        else {
                            continue;
                        };
                        let col = visible_pos % cols;
                        let row = visible_pos / cols;
                        let label =
                            crate::ui::views::grid_surface_element_deprecated::grid_label_bounds(
                                gpui::Bounds {
                                    origin: gpui::point(
                                        px(grid_pad_px + col as f32 * (thumb_px + grid_gap_px)),
                                        px(grid_pad_px + row as f32 * pitch),
                                    ),
                                    size: gpui::size(px(thumb_px), px(cell_h_px)),
                                },
                                thumb_px * GRID_ICON_AREA,
                                zoom_scale,
                            );
                        rename_overlay = Some(
                            div()
                                .absolute()
                                .left(label.origin.x)
                                .top(label.origin.y)
                                .w(label.size.width)
                                .h(label.size.height)
                                .text_color(accent_fg)
                                .child(crate::ui::views::rename::centered_input_element(
                                    &rename_input,
                                    &view,
                                    label.size.height,
                                    font_size,
                                    label.size.width,
                                    window,
                                    cx,
                                ))
                                .into_any_element(),
                        );
                        break;
                    }
                }
            }
            let drag_layer = USE_GRID_SURFACE_ELEMENT.then(|| {
                div()
                    .id(("grid-drag-layer", pane_id.0))
                    .absolute()
                    .top_0()
                    .left_0()
                    .w_full()
                    .h(px(content_h))
                    .opacity(0.0)
                    .on_file_drag_grid(view.clone(), pane_id)
                    .into_any_element()
            });
            let tooltip_data = self
                .grid_hover
                .filter(|(hover_pane, _)| *hover_pane == pane_id)
                .and_then(|(_, entry_idx)| {
                    self.state
                        .tab_states
                        .get(&pane_id)
                        .and_then(|ts| ts.entries.get(entry_idx))
                        .map(|entry| {
                            (
                                entry.name.to_string(),
                                entry.path(),
                                matches!(entry.kind(), FileKind::Dir),
                                entry.size(),
                                entry.modified,
                            )
                        })
                });
            div()
                .id(("anim-grid-scroll", pane_id.0))
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&base_scroll)
                .on_mouse_down(
                    MouseButton::Left,
                    crate::ui::views::on_pane_rubber_mouse_down(&view, pane_id),
                )
                .on_scroll_wheel(move |ev, window, cx| {
                    // Keep wheel travel independent of thumbnail size.
                    if ev.modifiers.control || ev.modifiers.platform {
                        ctrl_scroll(ev, pane_id, &inner_scroll_view, window, cx);
                    }
                })
                .when_some(tooltip_data, |el, (name, path, is_dir, size, modified)| {
                    el.tooltip(move |window, cx| {
                        crate::ui::tooltips::entry_hover_tooltip(
                            name.clone(),
                            path.clone(),
                            is_dir,
                            size,
                            modified,
                            window,
                            cx,
                        )
                    })
                })
                .child(
                    div()
                        .w_full()
                        .h(px(content_h))
                        .relative()
                        .child(surface)
                        .when_some(rename_overlay, |el, overlay| el.child(overlay))
                        .when_some(drag_layer, |el, layer| el.child(layer)),
                )
                .into_any_element()
        };

        let prepaint_view = view.clone();
        let bounds_view = view.clone();
        let pane_view = view;
        crate::ui::views::pane_background_interactions(
            v_flex()
                .id(("pane", pane_id.0))
                .size_full()
                .overflow_hidden()
                .min_w(gpui::px(0.))
                .min_h(gpui::px(0.)),
            &pane_view,
            pane_id,
        )
        // Capture the real pane pixel width every frame so the next frame's
        // column calculation uses the actual container size (handles preview
        // panel open/close and splitter drags, not just window resize). A
        // fractional width change does not alter this fixed-pitch surface until
        // it crosses a column boundary, so only invalidate the pane then.
        .on_prepaint_child(move |bounds: gpui::Bounds<gpui::Pixels>, _window, cx| {
            crate::perf::count("prepaint.grid_pane");
            let new_w = f32::from(bounds.size.width);
            let new_cols = grid_column_count(new_w, thumb_px, grid_gap_px, grid_pad_px);
            if new_cols != cols {
                prepaint_view.update(cx, |this, cx| {
                    if let Some(ts) = this.state.tab_states.get_mut(&pane_id) {
                        let old_w = ts.last_pane_width;
                        crate::perf::event(|| {
                            format!(
                                "grid.width pane={} old={old_w:.1} new={new_w:.1} cols={cols}->{new_cols}",
                                pane_id.0,
                            )
                        });
                        ts.last_pane_width = new_w;
                    }
                    crate::perf::count("grid.width.columns_changed");
                    if this.is_pane_or_dock_animating() {
                        this.defer_column_reflow(pane_id);
                    } else {
                        this.notify_pane(pane_id, cx);
                    }
                });
            }
            // Phase F: register this pane's bounds for OLE drop routing.
            prepaint_view.read(cx).push_pane_zone(pane_id, bounds);
        })
        .child(timed_layout_region(
            "pane.crumbs",
            PANE_CRUMBS_TIMINGS,
            crumb_strip,
        ))
        .child(timed_layout_region(
            "pane.viewport",
            PANE_VIEWPORT_TIMINGS,
            div()
                .flex_1()
                .min_h(px(0.))
                .relative()
                .rounded_bl(px(12.0))
                .rounded_br(px(12.0))
                .on_prepaint_child(pane_bounds_recorder(pane_id, &bounds_view))
                .child(layout_region("pane.viewport_body", inner))
                .vertical_scrollbar(&base_scroll),
        ))
        .into_any_element()
    }

    pub(crate) fn prepare_grid_visible(&mut self, pane_id: TabId, range: Range<usize>) {
        let windows_theme = self.native_file_icons();
        let (to_request, to_request_ext, to_request_shell, request_folder_icon, all_visible) = {
            let Some(ts) = self.state.tab_states.get(&pane_id) else {
                return;
            };
            let mut imgs = Vec::new();
            let mut exts: Vec<(
                std::path::PathBuf,
                String,
                std::collections::HashMap<String, std::path::PathBuf>,
            )> = Vec::new();
            // Windows: non-native-image files routed through IShellItemImageFactory.
            let mut shell_paths: Vec<(std::path::PathBuf, bool)> = Vec::new();
            let mut request_folder_icon = false;
            let mut all_visible = Vec::new();
            for vi in range {
                let Some(&entry_idx) = ts.visible.get(vi) else {
                    break;
                };
                let Some(entry) = ts.entries.get(entry_idx) else {
                    continue;
                };
                if entry.kind() == FileKind::Dir {
                    request_folder_icon |= windows_theme;
                    continue;
                }

                if entry.kind() != FileKind::File {
                    continue;
                }

                let thumbable = is_thumbnailable(entry);
                #[cfg(target_os = "windows")]
                let shell_thumbable = !thumbable
                    && entry
                        .extension()
                        .as_deref()
                        .is_some_and(is_windows_shell_thumbnail_ext);
                #[cfg(not(target_os = "windows"))]
                let shell_thumbable = false;

                if thumbable {
                    all_visible.push(entry.path());
                    if !self.thumb_textures.contains_key(&entry.path()) {
                        imgs.push(entry.path());
                    }
                } else if shell_thumbable {
                    all_visible.push(entry.path());
                    if !self.thumb_textures.contains_key(&entry.path()) {
                        shell_paths.push((entry.path(), false));
                    }
                } else if self.thumb_textures.contains_key(&entry.path()) {
                    all_visible.push(entry.path());
                } else {
                    let ext_cmd = entry.extension().as_deref().and_then(|ext| {
                        self.ext_registry
                            .thumbnailer_for(ext)
                            .map(|cmd| (cmd.command.run.clone(), cmd.tools))
                    });
                    if let Some((run, tools)) = ext_cmd {
                        all_visible.push(entry.path());
                        exts.push((entry.path(), run, tools));
                    }
                }
            }
            (imgs, exts, shell_paths, request_folder_icon, all_visible)
        };

        {
            let mut wanted = self
                .thumb_wanted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            wanted.clear();
            wanted.extend(all_visible);
        }
        #[cfg(target_os = "windows")]
        if request_folder_icon {
            self.request_shell_folder_icon();
        }
        for path in to_request {
            self.request_thumbnail(&path);
        }
        for (path, run, tools) in to_request_ext {
            self.request_ext_thumbnail(&path, run, tools);
        }
        #[cfg(target_os = "windows")]
        for (path, allow_icon) in to_request_shell {
            if allow_icon {
                self.request_shell_thumbnail_or_icon(&path);
            } else {
                self.request_visible_shell_thumbnail(&path);
            }
        }
        // On non-Windows to_request_shell is always empty; drop it to avoid unused warning.
        #[cfg(not(target_os = "windows"))]
        drop(to_request_shell);
    }
}

fn grid_column_count(pane_w: f32, thumb: f32, gap: f32, pad: f32) -> usize {
    let inner_avail = (pane_w - 2.0 * pad).max(thumb);
    // n*(thumb+gap) - gap = inner_avail
    (((inner_avail + gap) / (thumb + gap)).floor() as usize).max(1)
}

#[cfg(test)]
mod tests {
    use super::grid_column_count;

    #[test]
    fn grid_columns_change_only_at_exact_pitch_boundary() {
        let thumb = 128.0;
        let gap = 8.0;

        assert_eq!(grid_column_count(263.99, thumb, gap, 0.0), 1);
        assert_eq!(grid_column_count(264.0, thumb, gap, 0.0), 2);
        assert_eq!(grid_column_count(399.99, thumb, gap, 0.0), 2);
        assert_eq!(grid_column_count(400.0, thumb, gap, 0.0), 3);
    }

    #[test]
    fn grid_columns_never_drop_below_one() {
        assert_eq!(grid_column_count(0.0, 128.0, 8.0, 0.0), 1);
    }
}

/// Whether this entry should get a downscaled thumbnail (an image or audio file).
fn is_thumbnailable(entry: &FileEntry) -> bool {
    matches!(entry.kind(), FileKind::File)
        && entry.extension().as_deref().is_some_and(|e| {
            atlas_model::preview_cache::is_image_ext(e)
                || atlas_model::preview_cache::is_audio_ext(e)
        })
}

/// File types where asking Windows Shell for a real thumbnail is worth the COM call.
/// Generic type icons are cheaper as builtins/theme icons and must not be requested per path.
fn is_windows_shell_thumbnail_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "3fr"
            | "3gp"
            | "arw"
            | "asf"
            | "avi"
            | "avif"
            | "cr2"
            | "cr3"
            | "crw"
            | "dng"
            | "flv"
            | "heic"
            | "heif"
            | "iiq"
            | "m2ts"
            | "m4v"
            | "mkv"
            | "mov"
            | "mp4"
            | "mpeg"
            | "mpg"
            | "mts"
            | "nef"
            | "orf"
            | "pdf"
            | "pef"
            | "psd"
            | "raf"
            | "rw2"
            | "srw"
            | "tga"
            | "ts"
            | "vob"
            | "webm"
            | "wmv"
    )
}

struct GridCellProps<'a> {
    entry: &'a FileEntry,
    entry_idx: usize,
    selected: bool,
    show_checkbox: bool,
    pane_id: TabId,
    view: &'a Entity<App>,
    thumb: f32,
    cell_w: f32,
    file_icon: FileIcon,
    accent: Hsla,
    accent_fg: Hsla,
    hover: Hsla,
    border: Hsla,
    flashing: bool,
    record_rects: bool,
    rename_input: Option<gpui::Entity<gpui_component::input::InputState>>,
    is_animating: bool,
    dimmed: bool,
    cut: bool,
    window: &'a mut Window,
}

/// One grid cell: thumbnail (or a scaled glyph fallback while it loads / for
/// non-images) + truncated name, fixed to the thumbnail size. The texture is a
/// cached, downscaled `RenderImage` (see `ui::thumbs`) — never a full-res decode.
fn render_cell(props: GridCellProps, cx: &gpui::App) -> AnyElement {
    let GridCellProps {
        entry,
        entry_idx,
        selected,
        show_checkbox,
        pane_id,
        view,
        thumb,
        cell_w,
        file_icon,
        accent,
        accent_fg,
        hover,
        border,
        flashing,
        record_rects,
        rename_input,
        is_animating,
        dimmed,
        cut,
        window,
    } = props;
    let name = entry.name.to_string();
    let font_size = window.rem_size() * 0.75;
    v_flex()
        .id(("cell", entry_idx))
        .when(record_rects && !is_animating, |s| {
            s.on_prepaint_child(rect_recorder(pane_id, entry_idx, view))
        })
        .w(crate::ui::rem_px(cell_w))
        .p_1()
        .gap_1()
        .items_center()
        .relative()
        .rounded_md()
        .opacity((if cut { 0.5 } else { 1.0 }) * if dimmed { 0.45 } else { 1.0 })
        .cursor_pointer()
        .when(selected, |s| s.bg(accent).text_color(accent_fg))
        .when(flashing && !selected, |s| s.bg(accent.opacity(0.38)))
        .when(!selected && rename_input.is_none() && !is_animating, |s| {
            s.hover(|h| h.bg(hover))
        })
        .when(selected && show_checkbox, |s| {
            s.child(
                div()
                    .absolute()
                    .top(px(3.))
                    .left(px(3.))
                    .child(SelectionCheckbox::new(
                        SharedString::from(format!("grid-selection-{pane_id:?}-{entry_idx}")),
                        16.0,
                    )),
            )
        })
        .child({
            let area = div()
                .h(crate::ui::rem_px(thumb * GRID_ICON_AREA))
                .flex()
                .items_center()
                .justify_center();
            match file_icon {
                FileIcon::PreviewTexture(tex) => {
                    let natural = tex.size(0);
                    let natural_w = (i32::from(natural.width).max(1)) as f32;
                    let natural_h = (i32::from(natural.height).max(1)) as f32;
                    let (render_w, render_h) = crate::ui::thumbs::thumbnail_size(
                        natural_w,
                        natural_h,
                        thumb,
                        thumb * GRID_ICON_AREA,
                    );
                    let outline = border.opacity(if cx.theme().is_dark() { 0.25 } else { 0.18 });
                    area.child(
                        div()
                            .relative()
                            .w(crate::ui::rem_px(render_w))
                            .h(crate::ui::rem_px(render_h))
                            .rounded(px(3.0))
                            .overflow_hidden()
                            .child(img(tex).size_full())
                            .child(
                                div()
                                    .absolute()
                                    .inset_0()
                                    .border_1()
                                    .border_color(outline)
                                    .rounded(px(3.0)),
                            ),
                    )
                }
                FileIcon::IconTexture(tex) => {
                    let sz = thumb * GRID_TEXTURE_ICON_MAX;
                    area.child(
                        img(tex.for_size(sz))
                            .max_h(crate::ui::rem_px(sz))
                            .max_w(crate::ui::rem_px(sz))
                            .flex_none(),
                    )
                }
                FileIcon::Builtin { kind, extension } => area.child(
                    FileIcon::Builtin { kind, extension }
                        .element(thumb * GRID_BUILTIN_ICON, accent),
                ),
            }
        })
        .child({
            // The plain (non-editing) name is `.text_xs()`-sized, i.e. it uses
            // `font_size` (already `rem_size * 0.75`), not the ambient default.
            // The rename `Input` widget paints its single line pinned to the top
            // of its own box rather than centering it (unlike plain text), so
            // giving it the "right" height alone isn't enough — wrapping both
            // branches in the same fixed-height, real-flex-centered row is what
            // actually keeps the glyphs lined up between editing/not-editing.
            let line_h = window
                .text_style()
                .line_height
                .to_pixels(AbsoluteLength::Pixels(font_size), window.rem_size());
            h_flex()
                .w_full()
                .h(line_h)
                .items_center()
                .justify_center()
                .child(match &rename_input {
                    Some(input) => div()
                        .w(px(crate::ui::views::rename::input_width(
                            &name,
                            cell_w * f32::from(window.rem_size()) / crate::ui::BASE_PX,
                            font_size,
                            window,
                        )))
                        .child(crate::ui::views::rename::input_element(
                            input, view, line_h, font_size, window, cx,
                        ))
                        .into_any_element(),
                    None => div()
                        .w_full()
                        .text_center()
                        .text_xs()
                        .truncate()
                        .child(name)
                        .into_any_element(),
                })
        })
        .when(!is_animating, |s| {
            s.on_click(crate::ui::views::on_entry_click(view, pane_id, entry_idx))
                .on_mouse_down(
                    MouseButton::Right,
                    crate::ui::views::on_entry_ctx_menu(view, pane_id, entry_idx),
                )
                .on_mouse_down(MouseButton::Left, |_ev, _window, cx| {
                    cx.stop_propagation();
                })
        })
        .when(rename_input.is_none() && !is_animating, |s| {
            s.on_file_drag_item(view.clone(), pane_id, entry_idx, entry.path())
        })
        .tooltip({
            let tip_name = entry.name.to_string();
            let tip_path = entry.path();
            let is_dir = matches!(entry.kind(), FileKind::Dir);
            let size = entry.size();
            let modified = entry.modified;
            move |window, cx| {
                crate::ui::tooltips::entry_hover_tooltip(
                    tip_name.clone(),
                    tip_path.clone(),
                    is_dir,
                    size,
                    modified,
                    window,
                    cx,
                )
            }
        })
        .when(
            matches!(entry.kind(), FileKind::Dir) && !is_animating,
            |s| crate::ui::views::folder_drop_target(s, view, pane_id, entry.path()),
        )
        .into_any_element()
}
