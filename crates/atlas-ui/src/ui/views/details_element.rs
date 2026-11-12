//! Custom GPUI `Element` for the Details body (rows only).
//!
//! Replaces the dual-canvas + event layer stack with one leaf element that:
//! - syncs `App.details_row_rings` (shape on miss only)
//! - paints clipped rows
//! - registers mouse handlers for hit-test interactions
//!
//! Scroll chrome stays outside (track_scroll + scrollbar) for parity with rubber/ctrl-zoom.

use crate::app::{App, DetailsColumn, DetailsHitModel};
use crate::ui::drag_hover::{
    DragHoverCommand, DragHoverDropCommand, DragHoverId, DragHoverKind, DragHoverTarget,
};
use crate::ui::icons::AtlasIcon;
use crate::ui::selection_checkbox;
use crate::ui::views::details::{
    DetailsColors, DetailsContentLod, DetailsRowLayout, CELL_PAD, DETAILS_ROW_H, ROW_ICON_SIZE,
    ROW_PAD_X, ROW_TEXT_Y_NUDGE,
};
use crate::ui::views::details_ring::{self, DetailsRingMeta, DetailsRowSlot, DetailsRowText};
use atlas_model::file_entry::FileKind;
use atlas_model::pane::TabId;
use gpui::ColorExt as _;
use gpui::{
    point, px, size, App as GpuiApp, Bounds, Corners, DispatchPhase, Element, ElementId, Font,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Refineable, ShapedLine, SharedString,
    Style, StyleRefinement, Styled, TextRun, WeakEntity, Window,
};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Instant;

/// Toggle lives in `details::USE_DETAILS_BODY_ELEMENT`.
pub(crate) struct DetailsBodyElement {
    id: ElementId,
    pane_id: TabId,
    app: WeakEntity<App>,
    visible: Arc<Vec<usize>>,
    row_h: f32,
    scroll: gpui::ScrollHandle,
    layout: DetailsRowLayout,
    content_lod: DetailsContentLod,
    colors: DetailsColors,
    font: Font,
    font_size: Pixels,
    dragging: Option<DetailsColumn>,
    row_motion: gpui::Entity<crate::ui::dg::motion::RowMotion<std::path::PathBuf>>,
    style: StyleRefinement,
}

/// Data produced in prepaint for paint (ring slots + geometry).
pub(crate) struct DetailsBodyPrepaint {
    folder_size: u128,
    rows: Vec<(DetailsRowSlot, f32, f32)>,
    /// One shaped trailer per pane/style; every row reuses its glyph geometry.
    ellipsis: ShapedLine,
    layout: DetailsRowLayout,
    show_checkboxes: bool,
    content_lod: DetailsContentLod,
    /// Lets window-level listeners participate in GPUI's occlusion hit-testing.
    hitbox: Hitbox,
}

impl DetailsBodyElement {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        pane_id: TabId,
        app: WeakEntity<App>,
        visible: Arc<Vec<usize>>,
        row_h: f32,
        scroll: gpui::ScrollHandle,
        layout: DetailsRowLayout,
        content_lod: DetailsContentLod,
        colors: DetailsColors,
        font: Font,
        font_size: Pixels,
        dragging: Option<DetailsColumn>,
        row_motion: gpui::Entity<crate::ui::dg::motion::RowMotion<std::path::PathBuf>>,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from(format!("details-body-{}", pane_id.0))),
            pane_id,
            app,
            visible,
            row_h,
            scroll,
            layout,
            content_lod,
            colors,
            font,
            font_size,
            dragging,
            row_motion,
            style: StyleRefinement::default(),
        }
    }
}

impl IntoElement for DetailsBodyElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for DetailsBodyElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

/// Filters define a new presentation epoch, including clearing them. Sort order
/// stays out of it so deliberate sorting can keep its short spring.
pub(crate) fn row_content_epoch(
    tab: &atlas_model::tab::TabState,
    types: Option<&std::collections::HashSet<String>>,
) -> u64 {
    let mut epoch = std::collections::hash_map::DefaultHasher::new();
    tab.appear_seq.hash(&mut epoch);
    tab.filter.hash(&mut epoch);
    tab.search.query.hash(&mut epoch);
    tab.search.generation.hash(&mut epoch);
    tab.show_files.hash(&mut epoch);
    tab.show_folders.hash(&mut epoch);
    let mut types: Vec<_> = types.into_iter().flatten().collect();
    types.sort();
    types.hash(&mut epoch);
    epoch.finish()
}

impl Element for DetailsBodyElement {
    type RequestLayoutState = Style;
    type PrepaintState = DetailsBodyPrepaint;

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
        crate::perf::count("details.element.layout");
        let mut style = Style::default();
        style.refine(&self.style);
        // Fill the scroll-content parent (explicit height set by outer div).
        style.size.width =
            gpui::Length::Definite(gpui::DefiniteLength::Relative(gpui::relative(1.0)));
        style.size.height =
            gpui::Length::Definite(gpui::DefiniteLength::Relative(gpui::relative(1.0)));
        let layout_id = window.request_layout(style.clone(), [], cx);
        (layout_id, style)
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
        let _perf = crate::perf::span("details.element.prepaint");
        crate::perf::count("details.element.prepaint");
        // Per-pane wall time also rolls into the frame sum via Span::drop → time_us.

        let pane_id = self.pane_id;
        let visible = self.visible.clone();
        let row_h = self.row_h;
        let tracked = self.scroll.bounds();
        let viewport_bounds = if tracked.size.width > px(1.0) && tracked.size.height > px(1.0) {
            tracked
        } else {
            bounds
        };
        let scroll_y = (-f32::from(self.scroll.offset().y)).max(0.0);
        let viewport_h = f32::from(viewport_bounds.size.height);
        // Smooth scrolling advances after render; shape and hit-test the range
        // that is actually on screen in this prepaint pass.
        let (start_idx, end_idx) = crate::ui::dg::scroll::live_span(
            scroll_y,
            scroll_y + viewport_h,
            row_h,
            2,
            visible.len(),
        );
        let layout = self.layout.clone();
        let content_lod = self.content_lod;
        let font = self.font.clone();
        let font_size = self.font_size;
        let colors = self.colors;
        let visible_rows = end_idx.saturating_sub(start_idx);
        // Aggregate per-frame timings rather than emitting one span per row. This
        // keeps the profiler useful when many panes are visible simultaneously.
        let mut row_build_us = 0_u64;
        // Keep sub-microsecond row costs until the pane aggregate is published.
        let mut icon_lookup_time = std::time::Duration::ZERO;
        let mut icon_texture_lookups = 0_u64;
        let mut icon_builtin_lookups = 0_u64;
        let mut text_shape_time = std::time::Duration::ZERO;
        let mut text_shape_name_calls = 0_u64;
        let mut text_shape_meta_calls = 0_u64;
        let mut ring_collect_us = 0_u64;
        // This element registers window-level listeners in paint. Give them a
        // hitbox so popovers/modals using `.occlude()` suppress underlying rows.
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        // Slit/sub-pixel culling: if pane is collapsing/narrower than 12px, bypass text shaping & processing
        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return DetailsBodyPrepaint {
                folder_size: 0,
                rows: Vec::new(),
                ellipsis: window.text_system().shape_line(
                    SharedString::new_static(""),
                    font_size,
                    &[],
                    None,
                ),
                layout,
                show_checkboxes: false,
                content_lod,
                hitbox,
            };
        }

        let ts = window.text_system();
        let ellipsis_text = SharedString::new_static("…");
        let ellipsis_run = TextRun {
            len: ellipsis_text.len(),
            font: font.clone(),
            // `paint_mono_truncated` supplies the live color. This color only
            // identifies the otherwise reusable shaped geometry.
            color: colors.fg,
            background_color: None,
            underline: None,
            strikethrough: None,
            letter_spacing: None,
        };
        let ellipsis_shape_t0 = Instant::now();
        let ellipsis = ts.shape_line(ellipsis_text, font_size, &[ellipsis_run], None);
        crate::perf::time_us(
            "details.text.ellipsis_shape",
            ellipsis_shape_t0.elapsed().as_micros() as u64,
        );

        let Some(app_entity) = self.app.upgrade() else {
            return DetailsBodyPrepaint {
                folder_size: 0,
                rows: Vec::new(),
                ellipsis,
                layout,
                show_checkboxes: false,
                content_lod,
                hitbox,
            };
        };

        let (targets, epoch, instant) = {
            let _perf = crate::perf::span("details.prepaint.motion_targets");
            let app = app_entity.read(cx);
            let tab = app.state.tab_states.get(&pane_id);
            let targets = (start_idx..end_idx)
                .filter_map(|row| {
                    let index = *visible.get(row)?;
                    let entry = tab?.entries.get(index)?;
                    Some((entry.path(), row, row as f32 * row_h))
                })
                .collect::<Vec<_>>();
            let renaming = (start_idx..end_idx).any(|row| {
                visible.get(row).is_some_and(|&idx| {
                    crate::ui::views::rename::active_input(app, pane_id, idx).is_some()
                })
            });
            (
                targets,
                tab.map_or(0, |ts| {
                    row_content_epoch(ts, app.type_filters.get(&pane_id))
                }),
                cx.reduce_motion()
                    || tab
                        .is_some_and(|ts| ts.search.is_filtering() || !ts.filter.trim().is_empty())
                    || app.rubber.is_some()
                    || renaming
                    || app.is_drag_active(cx.has_active_drag()),
            )
        };
        let motion_perf = crate::perf::span("details.prepaint.row_motion");
        let viewport = [scroll_y, row_h, viewport_h];
        let (samples, active) = self.row_motion.update(cx, |motion, _| {
            motion.sample(&targets, epoch, viewport, instant, Instant::now())
        });
        let mut presented_rows = Vec::new();
        let mut row_opacities = Vec::new();
        if active {
            presented_rows = samples.iter().map(|(row, y, _)| (*row, *y)).collect();
            row_opacities = samples
                .iter()
                .map(|(row, _, opacity)| (*row, *opacity))
                .collect();
            window.request_animation_frame();
        } else {
            presented_rows.clear();
        }
        drop(motion_perf);

        // Hit testing and folder drop targets follow the same presented geometry.
        let hit_perf = crate::perf::span("details.prepaint.hit_model");
        let hit_model = DetailsHitModel {
            bounds: viewport_bounds,
            scroll_y,
            row_h,
            visible: visible.clone(),
            start_idx,
            end_idx,
            total_count: visible.len(),
            presented_rows: presented_rows.clone(),
        };
        drop(hit_perf);

        let ts = window.text_system();
        // Inclusive parent of drop targets, row_build, ring.sync and ring.collect.
        let app_update_perf = crate::perf::span("details.prepaint.app_update");
        let has_gpui_drag = cx.has_active_drag();
        let (mut rows, show_checkboxes) = app_entity.update(cx, |app, cx| {
            let drop_perf = crate::perf::span("details.prepaint.drop_targets");
            app.details_hit.insert(pane_id, hit_model.clone());
            app.pane_view_bounds.insert(pane_id, viewport_bounds);
            app.refresh_rubber_for_pane(pane_id, cx);
            super::details::prime_details_file_icons_range(
                app, pane_id, &visible, start_idx, end_idx,
            );
            if app.is_drag_active(has_gpui_drag) {
                app.pending_drag_hover_targets.retain(
                    |target| !matches!(target.id, DragHoverId::Folder(tab, _) if tab == pane_id),
                );
                let targets = app
                    .state
                    .tab_states
                    .get(&pane_id)
                    .map(|tab_state| {
                        (start_idx..end_idx)
                            .filter_map(|visible_pos| {
                                let entry_idx = *visible.get(visible_pos)?;
                                let entry = tab_state.entries.get(entry_idx)?;
                                matches!(entry.kind(), FileKind::Dir).then(|| {
                                    let id = DragHoverId::Folder(pane_id, entry.path());
                                    DragHoverTarget {
                                        id,
                                        bounds: hit_model
                                            .row_bounds(visible_pos)
                                            .intersect(&hit_model.bounds)
                                            .intersect(&window.content_mask().bounds),
                                        kind: DragHoverKind::Folder,
                                        command: DragHoverCommand::NavigatePathIn(
                                            pane_id,
                                            entry.path(),
                                        ),
                                        drop_command: Some(DragHoverDropCommand::TransferTo(
                                            entry.path(),
                                        )),
                                    }
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for target in targets {
                    app.register_drag_hover_target(target);
                }
            } else if !app.pending_drag_hover_targets.is_empty() {
                app.pending_drag_hover_targets.retain(
                    |target| !matches!(target.id, DragHoverId::Folder(tab, _) if tab == pane_id),
                );
            }
            drop(drop_perf);

            struct VisibleRowInput {
                row: usize,
                entry_idx: usize,
                selected: bool,
                cursor: bool,
                hovered: bool,
                flashing: bool,
                odd: bool,
                dimmed: bool,
                shape_key: u64,
                icon_tex: Option<std::sync::Arc<gpui::RenderImage>>,
                fallback_icon_path: Option<&'static str>,
            }

            let (builds, entries, show_checkboxes) = {
                let row_build_t0 = Instant::now();
                let Some(tab_state) = app.state.tab_states.get(&pane_id) else {
                    return (Vec::new(), false);
                };
                // One pointer clone keeps the immutable directory snapshot alive
                // after the TabState borrow ends. Split panes already share this Arc.
                let entries = tab_state.entries.clone();
                let mut builds = Vec::with_capacity(visible_rows);
                for i in start_idx..end_idx {
                    let Some(&entry_idx) = visible.get(i) else {
                        continue;
                    };
                    let Some(entry) = tab_state.entries.get(entry_idx) else {
                        // Group headers occupy a row but never paint file content.
                        if atlas_model::grouping::is_virtual_cell(entry_idx) {
                            app.details_row_rings
                                .entry(pane_id)
                                .or_default()
                                .remove_row(i);
                        }
                        continue;
                    };
                    if crate::ui::views::rename::active_input(app, pane_id, entry_idx).is_some() {
                        app.details_row_rings
                            .entry(pane_id)
                            .or_default()
                            .remove_row(i);
                        continue;
                    }

                    let selected = tab_state.selection.selected.contains(&entry_idx);
                    let cursor = tab_state.selection.keyboard_cursor == Some(entry_idx);
                    let dimmed = crate::ui::views::rename::dim_other_entry(app, pane_id, entry_idx);
                    let flashing = matches!(entry.kind(), FileKind::Dir)
                        && app.drag_hover.blink_id.as_ref()
                            == Some(&DragHoverId::Folder(pane_id, entry.path()));
                    let hovered = app.details_hover == Some((pane_id, entry_idx))
                        && app.rename_edit.is_none();
                    let odd = i % 2 == 1;

                    let shape_key = details_ring::details_row_shape_key(
                        entry_idx,
                        entry,
                        entry.extension().as_deref(),
                        &font,
                        font_size,
                    );

                    let icon_t0 = Instant::now();
                    let icon_tex = app
                        .row_icon_for_entry(
                            &entry.path(),
                            &entry.kind(),
                            entry.extension().as_deref(),
                        )
                        .texture();
                    icon_lookup_time += icon_t0.elapsed();
                    let fallback_icon_path = if icon_tex.is_none() {
                        icon_builtin_lookups += 1;
                        Some(match entry.kind() {
                            FileKind::Dir => AtlasIcon::Folder.path(),
                            _ => AtlasIcon::File.path(),
                        })
                    } else {
                        icon_texture_lookups += 1;
                        None
                    };

                    builds.push(VisibleRowInput {
                        row: i,
                        entry_idx,
                        selected,
                        cursor,
                        hovered,
                        flashing,
                        odd,
                        dimmed,
                        shape_key,
                        icon_tex,
                        fallback_icon_path,
                    });
                }
                row_build_us += row_build_t0.elapsed().as_micros() as u64;
                let show_checkboxes = tab_state.selection.selected.len() > 1;
                (builds, entries, show_checkboxes)
            };

            let (rings, text_cache) = (&mut app.details_row_rings, &mut app.details_row_texts);
            let ring = rings.entry(pane_id).or_default();
            ring.ensure_capacity((visible_rows + 8).max(16));

            let ring_sync_t0 = Instant::now();
            for b in &builds {
                let shape_key = b.shape_key;
                let entry_idx = b.entry_idx;
                let row_i = b.row;
                let Some(entry) = entries.get(entry_idx) else {
                    continue;
                };
                let selected_at = (b.selected
                    && !ring.get(row_i).is_some_and(|previous| previous.selected))
                .then(Instant::now);

                ring.sync_row(row_i, shape_key, || {
                    let text = text_cache.get_or_insert_with(shape_key, || {
                        let name: SharedString = entry.name.to_string().into();
                        let name_run = TextRun {
                            len: name.len(),
                            font: font.clone(),
                            color: colors.fg,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                            letter_spacing: None,
                        };
                        let shape_t0 = Instant::now();
                        let name_s = ts.shape_line(name, font_size, &[name_run], None);
                        text_shape_time += shape_t0.elapsed();
                        text_shape_name_calls += 1;

                        // Cache every metadata column independently of this pane's
                        // current width. Resizing then only changes clips/geometry.
                        let mut meta_cols = Vec::with_capacity(3);
                        for column in DetailsColumn::DEFAULT_ORDER {
                            let text: SharedString = match column {
                                DetailsColumn::Size => {
                                    crate::ui::views::details::entry_display_size(&entry)
                                        .clone()
                                        .into()
                                }
                                DetailsColumn::Modified => {
                                    crate::ui::views::details::entry_display_date(&entry)
                                        .clone()
                                        .into()
                                }
                                DetailsColumn::Type => {
                                    crate::ui::views::details::entry_type_label(&entry)
                                        .clone()
                                        .into()
                                }
                            };
                            let run = TextRun {
                                len: text.len(),
                                font: font.clone(),
                                color: colors.muted,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                                letter_spacing: None,
                            };
                            let shape_t0 = Instant::now();
                            meta_cols.push(DetailsRingMeta {
                                column,
                                line: ts.shape_line(text, font_size, &[run], None),
                            });
                            text_shape_time += shape_t0.elapsed();
                            text_shape_meta_calls += 1;
                        }

                        DetailsRowText { name_s, meta_cols }
                    });

                    DetailsRowSlot {
                        size: entry.size(),
                        interactive: true,
                        path: entry.path(),
                        row: row_i,
                        shape_key,
                        selected: b.selected,
                        cursor: b.cursor,
                        selected_at,
                        hovered: b.hovered,
                        flashing: b.flashing,
                        odd: b.odd,
                        dimmed: b.dimmed,
                        text,
                        icon_tex: b.icon_tex.clone(),
                        fallback_icon_path: b.fallback_icon_path,
                    }
                });
                ring.touch_flags(
                    row_i, shape_key, b.selected, b.cursor, b.hovered, b.flashing, b.odd, b.dimmed,
                );
            }
            crate::perf::time_us(
                "details.ring.sync",
                ring_sync_t0.elapsed().as_micros() as u64,
            );

            let ring_collect_t0 = Instant::now();
            let rows = ring.collect_rows(start_idx, end_idx);
            ring_collect_us += ring_collect_t0.elapsed().as_micros() as u64;
            (rows, show_checkboxes)
        });
        drop(app_update_perf);

        let selection_perf = crate::perf::span("details.prepaint.selection_feedback");
        let feedback = window.use_keyed_state(
            gpui::SharedString::from(format!("details-selection-feedback-{}", pane_id.0)),
            cx,
            |_, _| selection_checkbox::SelectionFeedback::<std::path::PathBuf>::default(),
        );
        let selection: Vec<_> = {
            let app = app_entity.read(cx);
            rows.iter()
                .map(|row| {
                    let path = self
                        .visible
                        .get(row.row)
                        .and_then(|entry_idx| {
                            app.state.tab_states.get(&pane_id)?.entries.get(*entry_idx)
                        })
                        .map(|entry| entry.path())
                        .unwrap_or_default();
                    (path, row.selected && show_checkboxes)
                })
                .collect()
        };
        let timestamps = feedback.update(cx, |state, _| state.sample(&selection, Instant::now()));
        for (row, at) in rows.iter_mut().zip(timestamps) {
            row.selected_at = at;
        }
        drop(selection_perf);

        let presence_prepare_perf = crate::perf::span("details.prepaint.presence_prepare");
        let presence = window.use_keyed_state(
            gpui::SharedString::from(format!("details-row-presence-{}", pane_id.0)),
            cx,
            |_, _| {
                crate::ui::dg::motion::PresenceMotion::<
                    std::path::PathBuf,
                    (DetailsRowSlot, f32, f32),
                >::new()
            },
        );
        let values = rows
            .into_iter()
            .zip(&selection)
            .map(|(row, (path, _))| {
                let y = presented_rows
                    .iter()
                    .find(|(i, _)| *i == row.row)
                    .map_or(row.row as f32 * self.row_h, |(_, y)| *y);
                let opacity = row_opacities
                    .iter()
                    .find(|(i, _)| *i == row.row)
                    .map_or(1.0, |(_, opacity)| *opacity);
                (path.clone(), (row, y, opacity))
            })
            .collect();
        let presence_epoch = crate::ui::dg::motion::presentation_epoch(
            epoch,
            &[
                scroll_y,
                viewport_h,
                self.row_h,
                f32::from(bounds.size.width),
            ],
        );
        drop(presence_prepare_perf);
        let (samples, active) = {
            let _perf = crate::perf::span("details.prepaint.presence_sample");
            presence.update(cx, |s, _| {
                s.sample(values, presence_epoch, instant, Instant::now())
            })
        };
        let presence_output_perf = crate::perf::span("details.prepaint.presence_output");
        let rows: Vec<_> = samples
            .into_iter()
            .map(|sample| {
                let (mut row, y, opacity) = sample.value;
                row.interactive = sample.interactive;
                if !sample.interactive {
                    row.hovered = false;
                    row.cursor = false;
                    row.flashing = false;
                    row.odd = false;
                }
                (row, y, opacity * sample.opacity)
            })
            .collect();
        if active {
            window.request_animation_frame();
        }
        drop(presence_output_perf);

        crate::perf::count_by("details.element.rows", rows.len() as u64);
        crate::perf::time_us("details.row_build", row_build_us);
        crate::perf::time_us("details.icons.lookup", icon_lookup_time.as_micros() as u64);
        crate::perf::count_by("details.icons.texture", icon_texture_lookups);
        crate::perf::count_by("details.icons.builtin", icon_builtin_lookups);
        crate::perf::time_us("details.text.shape", text_shape_time.as_micros() as u64);
        crate::perf::count_by("details.text.shape_name", text_shape_name_calls);
        crate::perf::count_by("details.text.shape_meta", text_shape_meta_calls);
        crate::perf::time_us("details.ring.collect", ring_collect_us);
        crate::perf::count(content_lod.perf_counter());

        DetailsBodyPrepaint {
            folder_size: app_entity.update(cx, |app, _| {
                app.state
                    .tab_states
                    .get_mut(&pane_id)
                    .map_or(0, |tab| tab.counts().bytes_shown)
            }),
            rows,
            ellipsis,
            layout,
            show_checkboxes,
            content_lod,
            hitbox,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        style: &mut Style,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut GpuiApp,
    ) {
        let _perf = crate::perf::span("details.element.paint");
        crate::perf::count("details.element.paint");

        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) || prepaint.rows.is_empty()
        {
            return;
        }

        let pane_id = self.pane_id;
        let app = self.app.clone();
        let row_h = self.row_h;
        let accent = self.colors.accent;
        let accent_fg = self.colors.accent_fg;
        let fg = self.colors.fg;
        let hover = self.colors.hover;
        let muted = self.colors.muted;
        let stripe_bg = self.colors.stripe_bg;
        let danger = gpui_component::ActiveTheme::theme(cx).danger;
        let floating_bg = gpui_component::ActiveTheme::theme(cx)
            .background
            .opacity(1.0);

        style.paint(bounds, window, cx, |window, cx| {
            let origin_x = px(f32::from(bounds.origin.x).round());
            let origin_y = px(f32::from(bounds.origin.y).round());
            let zoom_scale = f32::from(window.rem_size()) / crate::ui::BASE_PX;
            let row_icon_size = ROW_ICON_SIZE * zoom_scale;
            let row_pad_x = ROW_PAD_X * zoom_scale;
            let cell_pad = CELL_PAD * zoom_scale;
            let name_col_w = prepaint
                .layout
                .name_col_w
                .min((f32::from(bounds.size.width) - prepaint.layout.name_x).max(0.0));

            crate::perf::count_by("details.paint.rows", prepaint.rows.len() as u64);

            let all_t0 = Instant::now();
            let mut truncated_names = 0_u64;
            let mut truncated_meta = 0_u64;
            let mut painted_meta_cells = 0_u64;

            let name_width = prepaint
                .content_lod
                .name_paint_width(name_col_w.max(0.0), zoom_scale);
            let paint_names = prepaint.content_lod.paints_name() && name_width > 15.0 * zoom_scale;

            let stripes_perf = crate::perf::span("details.paint.stripes");
            for (row, _, _) in &prepaint.rows {
                // The table's zebra rhythm belongs to slots, never to flying files.
                if row.odd {
                    window.paint_quad(gpui::fill(
                        Bounds {
                            origin: point(origin_x, origin_y + px(row.row as f32 * row_h)),
                            size: size(bounds.size.width, px(row_h)),
                        },
                        stripe_bg,
                    ));
                }
            }
            drop(stripes_perf);
            let content_perf = crate::perf::span("details.paint.content");
            for (row, presented_y, row_opacity) in &prepaint.rows {
                let y = origin_y + px(*presented_y);
                let row_bounds = Bounds {
                    origin: point(origin_x, y),
                    size: size(bounds.size.width, px(row_h)),
                };

                let opacity = row_opacity
                    * crate::ui::views::rename::cut_path_opacity(&app, &row.path, cx)
                    * if row.dimmed {
                        crate::ui::views::rename::DIM_OPACITY
                    } else {
                        1.0
                    };
                window.with_element_opacity(Some(opacity), |window| {
                    window.paint_layer(row_bounds, |window| {
                        // A row in a live batch rename shows only its text caret: no
                        // selection fill, no hover — the edit itself is the highlight.
                        let batch_edit = row
                            .selected
                            .then(|| super::batch_row::row_edit(&app, &row.path, cx))
                            .flatten();
                        let sel = row.selected && batch_edit.is_none();
                        // 1. Background
                        let bg = if sel {
                            Some(accent)
                        } else if batch_edit.is_some() {
                            None
                        } else if row.flashing {
                            Some(accent.opacity(0.38))
                        } else if row.hovered {
                            Some(hover)
                        } else {
                            None
                        };
                        if let Some(color) = bg {
                            window.paint_quad(gpui::fill(row_bounds, color));
                        }
                        if row.interactive {
                            crate::ui::operation_feedback::paint_file_feedback(
                                &app, &row.path, row_bounds, accent, 0.0, window, cx,
                            );
                        }
                        if row.cursor && !row.selected {
                            let outline_color = accent.opacity(0.35);
                            window.paint_quad(gpui::outline(
                                row_bounds,
                                outline_color,
                                Default::default(),
                            ));
                        }

                        // 2. Icons
                        if sel && prepaint.show_checkboxes {
                            let icon_center_x =
                                f32::from(origin_x) + row_pad_x + row_icon_size * 0.5;
                            let icon_center_y = f32::from(y) + row_h * 0.5;
                            selection_checkbox::paint_animated(
                                window,
                                cx,
                                point(px(icon_center_x), px(icon_center_y)),
                                15.0 * zoom_scale,
                                row.selected_at,
                            );
                        } else if let Some(tex) = &row.icon_tex {
                            let _ = window.paint_image(
                                Bounds {
                                    origin: point(
                                        origin_x + px(row_pad_x),
                                        y + px((row_h - row_icon_size) * 0.5),
                                    ),
                                    size: size(px(row_icon_size), px(row_icon_size)),
                                },
                                Bounds {
                                    origin: point(
                                        origin_x + px(row_pad_x),
                                        y + px((row_h - row_icon_size) * 0.5),
                                    ),
                                    size: size(px(row_icon_size), px(row_icon_size)),
                                },
                                Corners::default(),
                                tex.clone(),
                                0,
                                false,
                            );
                        } else if let Some(path) = row.fallback_icon_path {
                            let _ = window.paint_svg(
                                Bounds {
                                    origin: point(
                                        origin_x + px(row_pad_x),
                                        y + px((row_h - row_icon_size) * 0.5),
                                    ),
                                    size: size(px(row_icon_size), px(row_icon_size)),
                                },
                                SharedString::from(path),
                                None,
                                Default::default(),
                                muted,
                                cx,
                            );
                        }

                        // 3. Name text
                        if paint_names {
                            let text_y = y + px(ROW_TEXT_Y_NUDGE * zoom_scale);
                            let name_x = origin_x + px(prepaint.layout.name_x);
                            let name_color = if sel { accent_fg } else { fg };
                            if let Some(edit) = batch_edit {
                                super::batch_row::paint_row_edit(
                                    &edit,
                                    window,
                                    cx,
                                    &self.font,
                                    self.font_size,
                                    Bounds {
                                        origin: point(name_x, y),
                                        size: size(px(name_width), px(row_h)),
                                    },
                                    text_y,
                                    row_h,
                                    zoom_scale,
                                    fg,
                                    danger,
                                );
                            } else {
                                if row.text.name_s.width() > px(name_width) {
                                    truncated_names += 1;
                                }
                                let _ = row.text.name_s.paint_mono_truncated(
                                    point(name_x, text_y),
                                    px(row_h),
                                    px(name_width),
                                    &prepaint.ellipsis,
                                    name_color,
                                    window,
                                    cx,
                                );
                            }
                        }

                        // 4. Meta text
                        if prepaint.content_lod.paints_metadata() {
                            let text_y = y + px(ROW_TEXT_Y_NUDGE * zoom_scale);
                            let base_meta_color = if sel { accent_fg } else { muted };

                            let metas = row
                                .text
                                .meta_cols
                                .iter()
                                .filter(|m| self.dragging != Some(m.column))
                                .chain(
                                    row.text
                                        .meta_cols
                                        .iter()
                                        .filter(|m| self.dragging == Some(m.column)),
                                );
                            for meta in metas {
                                let Some(layout_col) = prepaint
                                    .layout
                                    .columns
                                    .iter()
                                    .find(|col| col.column == meta.column)
                                else {
                                    continue;
                                };
                                if layout_col.x >= f32::from(bounds.size.width) {
                                    continue;
                                }
                                let col_x = origin_x + px(layout_col.x);
                                let mut col_w = layout_col.w;
                                let remaining_w =
                                    (f32::from(bounds.size.width) - layout_col.x).max(0.0);
                                col_w = col_w.min(remaining_w);
                                let padding = cell_pad.min(col_w);
                                let available_width = (col_w - padding).max(0.0);
                                let text_x = col_x + px(padding);
                                let is_dragging = self.dragging == Some(meta.column);
                                if is_dragging {
                                    let cell = Bounds {
                                        origin: point(col_x, y),
                                        size: size(px(col_w), px(row_h)),
                                    };
                                    if self.colors.glass {
                                        if !row.selected {
                                            window.paint_quad(gpui::fill(cell, fg.opacity(0.05)));
                                        }
                                    } else {
                                        window.paint_quad(gpui::fill(
                                            cell,
                                            if row.selected { accent } else { floating_bg },
                                        ));
                                        if !row.selected {
                                            window
                                                .paint_quad(gpui::fill(cell, accent.opacity(0.08)));
                                        }
                                    }
                                }
                                if meta.column == DetailsColumn::Size && prepaint.folder_size > 0 {
                                    if let Some(bytes) = row.size.filter(|size| *size > 0) {
                                        let fraction = (bytes as f64 / prepaint.folder_size as f64)
                                            .clamp(0.0, 1.0)
                                            as f32;
                                        let bar_width =
                                            (available_width * fraction).min(available_width);
                                        window.paint_quad(gpui::fill(
                                            Bounds {
                                                origin: point(
                                                    text_x,
                                                    y + px(row_h - 4.0 * zoom_scale),
                                                ),
                                                size: size(px(bar_width), px(2.0 * zoom_scale)),
                                            },
                                            if row.selected {
                                                accent_fg.opacity(0.4)
                                            } else {
                                                accent.opacity(0.32)
                                            },
                                        ));
                                    }
                                }
                                if meta.line.width() > px(available_width) {
                                    truncated_meta += 1;
                                }
                                if available_width > 15.0 * zoom_scale {
                                    painted_meta_cells += 1;
                                    let meta_color = base_meta_color;
                                    let _ = meta.line.paint_mono_truncated(
                                        point(text_x, text_y),
                                        px(row_h),
                                        px(available_width),
                                        &prepaint.ellipsis,
                                        meta_color,
                                        window,
                                        cx,
                                    );
                                }
                            }
                        }
                    });
                });
            }
            drop(content_perf);
            crate::perf::count_by("details.paint.meta_cells", painted_meta_cells);
            crate::perf::time_us(
                "details.paint.all_rows",
                all_t0.elapsed().as_micros() as u64,
            );
            crate::perf::count_by("details.paint.text_truncated_name", truncated_names);
            crate::perf::count_by("details.paint.text_truncated_meta", truncated_meta);
            crate::perf::count_by(
                "details.paint.text_truncated",
                truncated_names + truncated_meta,
            );

            // Input: same handlers as the old event canvas.
            let handlers_t0 = Instant::now();
            if let Some(app_entity) = app.upgrade() {
                let move_view = app_entity.clone();
                let move_hitbox = prepaint.hitbox.clone();
                window.on_mouse_event::<MouseMoveEvent>(move |ev, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || ev.dragging() {
                        return;
                    }
                    let pos = ev.position;
                    move_view.update(cx, |this, cx| {
                        let hover_entry = move_hitbox
                            .is_hovered(window)
                            .then(|| this.details_hover_at(pane_id, pos))
                            .flatten();
                        if this.details_hover != hover_entry {
                            gpui_component::tooltip::Tooltip::dismiss(window, cx);
                            this.details_hover = hover_entry;
                            this.notify_pane(pane_id, cx);
                        }
                    });
                });

                let down_view = app_entity;
                let down_hitbox = prepaint.hitbox.clone();
                // The selection checkbox replaces the icon on selected rows.
                let show_checks = prepaint.show_checkboxes;
                let check_x0 = f32::from(origin_x) + row_pad_x - 2.0;
                let check_x1 = check_x0 + row_icon_size + 4.0;
                window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let pos = ev.position;
                    if !down_hitbox.is_hovered(window)
                        || !down_view.read(cx).details_contains_point(pane_id, pos)
                    {
                        return;
                    }
                    down_view.update(cx, |this, cx| {
                        let hit = this.details_entry_at(pane_id, pos);
                        // Clicking a visible checkbox toggles that row (Ctrl semantics).
                        let x = f32::from(pos.x);
                        let on_check = show_checks
                            && x >= check_x0
                            && x <= check_x1
                            && hit.is_some_and(|idx| this.details_row_selected(pane_id, idx));
                        match ev.button {
                            MouseButton::Left => this.details_left_mouse_down(
                                pane_id,
                                hit,
                                pos,
                                ev.modifiers.shift && !on_check,
                                ev.modifiers.control || ev.modifiers.platform || on_check,
                                ev.click_count,
                                cx,
                            ),
                            MouseButton::Right => {
                                this.details_right_mouse_down(pane_id, hit, pos, window, cx)
                            }
                            _ => {}
                        }
                    });
                    if matches!(ev.button, MouseButton::Left | MouseButton::Right) {
                        cx.stop_propagation();
                    }
                });
            }
            crate::perf::time_us(
                "details.paint.handlers",
                handlers_t0.elapsed().as_micros() as u64,
            );
        });
    }
}

// Silence unused import if DETAILS_ROW_H only used externally later.
const _: f32 = DETAILS_ROW_H;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filtering_and_clearing_land_immediately_and_fully_readable() {
        let mut tab = atlas_model::tab::TabState::default();
        let now = Instant::now();
        let viewport = [0.0, 24.0, 400.0];
        let full = [("a", 0, 0.0), ("b", 1, 24.0)];
        let epoch = row_content_epoch(&tab, None);
        let mut motion = crate::ui::dg::motion::RowMotion::new(epoch, viewport);
        motion.sample(&full, epoch, viewport, false, now);
        tab.filter = "b".into();
        let filtered_epoch = row_content_epoch(&tab, None);
        let (rows, running) = motion.sample(&[("b", 0, 0.0)], filtered_epoch, viewport, false, now);
        assert!(!running);
        assert_eq!(rows, [(0, 0.0, 1.0)]);
        tab.filter.clear();
        let (rows, running) =
            motion.sample(&full, row_content_epoch(&tab, None), viewport, false, now);
        assert!(!running);
        assert_eq!(rows, [(0, 0.0, 1.0), (1, 24.0, 1.0)]);
        let types = std::collections::HashSet::from(["Images".into()]);
        assert_ne!(row_content_epoch(&tab, Some(&types)), epoch);
        tab.search.query = "new".into();
        assert_ne!(row_content_epoch(&tab, None), epoch);
    }
}
