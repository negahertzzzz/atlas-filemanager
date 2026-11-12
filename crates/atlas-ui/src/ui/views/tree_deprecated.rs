//! Dedicated hierarchical tree view (`ViewMode::Tree`).
use crate::app::App;
use crate::input::action::Action;
use crate::ui::glass::backdrop_tint;
use crate::ui::icons::{is_windows_theme, theme_icon, AtlasIcon};
use atlas_model::file_entry::{FileEntry, FileKind};
use atlas_model::pane::TabId;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, px, uniform_list, AnyElement, Context, Entity, Hsla, MouseButton, SharedString, Window,
};
use gpui_component::{h_flex, v_flex, ActiveTheme};
use std::sync::Arc;

impl App {
    /// Renders the pane in full tree view mode.
    pub(crate) fn render_tree_view(
        &mut self,
        pane_id: TabId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        crate::perf::count("render.view.tree");
        let view = cx.entity();
        let active_theme = self.state.profiles.active().theme.as_str();
        let is_glass = active_theme.ends_with(" Glass");
        let windows_theme = is_windows_theme(active_theme);
        let (fg, accent, accent_fg, hover, muted, border, header_bg) = {
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

        let crumb_strip = self.render_crumb_strip(pane_id, &view, fg, muted, border, header_bg, cx);

        let rows: Arc<[(FileEntry, usize, bool, bool)]> = {
            let Some(ts) = self.state.tab_states.get_mut(&pane_id) else {
                return div().into_any_element();
            };
            ts.visible_tree_rows()
                .into_iter()
                .map(|(e, d, exp, has)| (e.clone(), d, exp, has))
                .collect::<Vec<_>>()
                .into()
        };

        // Fetch children asynchronously if an expanded directory has not loaded them yet.
        for (entry, _, is_expanded, _) in rows.iter() {
            if entry.kind() == FileKind::Dir && *is_expanded {
                let has_children = self
                    .state
                    .tab_states
                    .get(&pane_id)
                    .is_some_and(|ts| ts.tree.children.contains_key(&entry.path()));
                if !has_children {
                    self.spawn_list_tree(pane_id, entry.path());
                }
            }
        }

        let body: AnyElement = if rows.is_empty() {
            let empty_msg =
                self.empty_pane_message(pane_id, rust_i18n::t!("empty_folder").into_owned());
            v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .children(empty_msg)
                .into_any_element()
        } else {
            let scroll_handle = self.scroll_handle_for(pane_id);
            let list_rows = rows.clone();
            let list_view = view.clone();
            let tree_list = uniform_list(
                SharedString::from(format!("tree-list-{}", pane_id.0)),
                rows.len(),
                move |range, _window, cx| {
                    list_view.update(cx, |this, _cx| {
                        range
                            .map(|i| {
                                let (entry, depth, is_expanded, has_children) = &list_rows[i];
                                let selected = this
                                    .state
                                    .tab_states
                                    .get(&pane_id)
                                    .is_some_and(|ts| ts.selection.selected.contains(&i));
                                let cursor = this
                                    .state
                                    .tab_states
                                    .get(&pane_id)
                                    .is_some_and(|ts| ts.selection.keyboard_cursor == Some(i));

                                let file_ref = crate::ui::file_icons::FileIconRef {
                                    path: entry.path(),
                                    kind: entry.kind().clone(),
                                    extension: entry
                                        .extension()
                                        .as_ref()
                                        .map(|s| SharedString::from(s.to_string())),
                                };
                                this.maybe_request_file_icon(&file_ref);
                                let file_icon = this.file_icon(&file_ref);
                                let is_editing = crate::ui::views::rename::active_input_for_path(
                                    this,
                                    pane_id,
                                    &entry.path(),
                                )
                                .is_some();

                                render_tree_row(TreeRowProps {
                                    entry,
                                    depth: *depth,
                                    is_expanded: *is_expanded,
                                    has_children: *has_children,
                                    idx: i,
                                    selected,
                                    cursor,
                                    is_editing,
                                    pane_id,
                                    file_icon,
                                    cut: this.state.system_cut_paths.contains(&entry.path()),
                                    accent,
                                    accent_fg,
                                    hover,
                                    muted,
                                    fg,
                                    border,
                                    windows_theme,
                                    view: &list_view,
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                },
            )
            .track_scroll(&scroll_handle)
            .flex_1()
            .h_full()
            .min_h(px(0.));

            v_flex()
                .size_full()
                .flex_1()
                .min_h(px(0.))
                .capture_key_down(
                    cx.listener(move |this, ev: &gpui::KeyDownEvent, _window, cx| {
                        let key = ev.keystroke.key.as_str();
                        let shift = ev.keystroke.modifiers.shift;
                        let ctrl =
                            ev.keystroke.modifiers.control || ev.keystroke.modifiers.platform;
                        let handled = match key {
                            "up" | "k" => {
                                this.nav_up(shift, ctrl);
                                true
                            }
                            "down" | "j" => {
                                this.nav_down(shift, ctrl);
                                true
                            }
                            "left" | "h" => {
                                this.nav_left(false, false);
                                true
                            }
                            "right" | "l" => {
                                this.nav_right(false, false);
                                true
                            }
                            "enter" => {
                                this.open_cursor();
                                true
                            }
                            _ => false,
                        };
                        if handled {
                            cx.stop_propagation();
                            this.notify_pane(pane_id, cx);
                            cx.notify();
                        }
                    }),
                )
                .child(tree_list)
                .into_any_element()
        };

        let pane_view = view;
        crate::ui::views::pane_background_interactions(
            v_flex()
                .id(SharedString::from(format!("pane-{}", pane_id.0)))
                .size_full()
                .overflow_hidden(),
            &pane_view,
            pane_id,
        )
        .child(crumb_strip)
        .child(body)
        .into_any_element()
    }
}

struct TreeRowProps<'a> {
    cut: bool,
    entry: &'a FileEntry,
    depth: usize,
    is_expanded: bool,
    #[allow(dead_code)]
    has_children: bool,
    idx: usize,
    selected: bool,
    cursor: bool,
    is_editing: bool,
    pane_id: TabId,
    file_icon: crate::ui::file_icons::FileIcon,
    accent: Hsla,
    accent_fg: Hsla,
    hover: Hsla,
    muted: Hsla,
    fg: Hsla,
    border: Hsla,
    windows_theme: bool,
    view: &'a Entity<App>,
}

fn render_tree_row(props: TreeRowProps) -> AnyElement {
    let TreeRowProps {
        cut,
        entry,
        depth,
        is_expanded,
        has_children: _,
        idx,
        selected,
        cursor,
        is_editing,
        pane_id,
        file_icon,
        accent,
        accent_fg,
        hover,
        muted,
        fg,
        border,
        windows_theme,
        view,
    } = props;

    let mut row = h_flex()
        .opacity(if cut { 0.5 } else { 1.0 })
        .id(SharedString::from(format!("tree-row-{}-{idx}", pane_id.0)))
        .w_full()
        .h(px(24.0))
        .px_2()
        .items_center()
        .rounded_sm()
        .cursor_pointer()
        .when(selected && !is_editing, |s| {
            s.bg(accent).text_color(accent_fg)
        })
        .when(!selected || is_editing, |s| s.hover(move |h| h.bg(hover)))
        .when(cursor && !is_editing, |s| {
            s.border_1()
                .border_color(accent.opacity(if selected { 0.95 } else { 0.65 }))
        });

    // 1. Indentation guide lines
    let guide_color = if selected && !is_editing {
        accent_fg.opacity(0.35)
    } else {
        border.opacity(0.40)
    };
    for _ in 0..depth {
        row = row.child(
            div().w(px(18.0)).h_full().relative().flex_none().child(
                div()
                    .absolute()
                    .left(px(8.0))
                    .top_0()
                    .bottom_0()
                    .w(px(1.0))
                    .bg(guide_color),
            ),
        );
    }

    // 2. Directory chevron or aligned indentation spacer
    if entry.kind() == FileKind::Dir {
        let chevron_icon = if is_expanded {
            AtlasIcon::CaretDown
        } else {
            AtlasIcon::CaretRight
        };
        let chevron_color = if selected && !is_editing {
            accent_fg
        } else {
            muted
        };
        let chevron_view = view.clone();
        let chevron_path = entry.path();
        row = row.child(
            div()
                .w(px(18.0))
                .h_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .child(theme_icon(chevron_icon, 12.0, chevron_color, windows_theme))
                .on_mouse_down(MouseButton::Left, move |_ev, _window, cx| {
                    cx.stop_propagation();
                    chevron_view.update(cx, |this, cx| {
                        this.activate_tab(pane_id);
                        this.process_actions(vec![Action::ToggleTreeExpand(chevron_path.clone())]);
                        this.notify_pane(pane_id, cx);
                        cx.notify();
                    });
                }),
        );
    } else {
        row = row.child(div().w(px(18.0)).h_full().flex_none());
    }

    // 3. File icon
    row = row.child(
        div()
            .w(px(18.0))
            .h_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .child(file_icon.element(14.0, accent)),
    );

    // 4. Filename
    row = row.child(
        div()
            .flex_1()
            .min_w(px(0.))
            .truncate()
            .text_xs()
            .text_color(if selected && !is_editing {
                accent_fg
            } else {
                fg
            })
            .child(entry.name.to_string()),
    );

    // 5. Aligned metadata (size and date)
    let meta_color = if selected && !is_editing {
        accent_fg.opacity(0.85)
    } else {
        muted
    };
    row = row.child(
        div()
            .w(px(70.0))
            .flex_none()
            .text_right()
            .text_xs()
            .text_color(meta_color)
            .truncate()
            .child(crate::ui::views::details::entry_display_size(&entry).to_string()),
    );
    row = row.child(
        div()
            .w(px(130.0))
            .flex_none()
            .text_right()
            .text_xs()
            .text_color(meta_color)
            .truncate()
            .child(crate::ui::views::details::entry_display_date(&entry).to_string()),
    );

    // 6. Click, double click, right click selection and navigation
    let row_view = view.clone();
    let click_path = entry.path();
    let is_dir = entry.kind() == FileKind::Dir;
    row = row
        .on_mouse_down(MouseButton::Left, |_ev, _window, cx| {
            cx.stop_propagation();
        })
        .on_click(move |ev, _window, cx| {
            let activate = ev.click_count() >= 2;
            let m = ev.modifiers();
            let (shift, ctrl) = (m.shift, m.control || m.platform);
            row_view.update(cx, |this, cx| {
                if this.suppress_navigation_double_click(pane_id, ev.position(), ev.click_count()) {
                    return;
                }
                if this.rename_edit.is_some() {
                    this.commit_rename(cx);
                }
                if this.state.settings.single_click_opens_folder(
                    is_dir,
                    ev.click_count(),
                    shift || ctrl,
                ) {
                    this.navigate_clicked_folder(pane_id, click_path.clone());
                    this.recent_folder_navigation =
                        Some(crate::ui::click_navigation::NavigationClick {
                            pane: pane_id,
                            origin: ev.position(),
                            at: std::time::Instant::now(),
                        });
                } else if activate {
                    if is_dir {
                        this.process_actions(vec![Action::ToggleTreeExpand(click_path.clone())]);
                    } else {
                        this.spawn_open_files(vec![click_path.clone()]);
                    }
                } else {
                    this.activate_tab(pane_id);
                    if let Some(ts) = this.state.tab_states.get_mut(&pane_id) {
                        ts.selection.keyboard_cursor = Some(idx);
                        ts.selection.last_clicked_index = Some(idx);
                        if shift {
                            let anchor = ts.selection.selection_anchor.unwrap_or(idx);
                            let (lo, hi) = (anchor.min(idx), anchor.max(idx));
                            ts.selection.selected.clear();
                            ts.selection.selected.extend(lo..=hi);
                        } else if ctrl {
                            if ts.selection.selected.contains(&idx) {
                                ts.selection.selected.remove(&idx);
                            } else {
                                ts.selection.selected.insert(idx);
                            }
                        } else {
                            ts.selection.selection_anchor = Some(idx);
                            ts.selection.selected.clear();
                            ts.selection.selected.insert(idx);
                        }
                    }
                }
                this.notify_pane(pane_id, cx);
                cx.notify();
            });
        })
        .on_mouse_down(MouseButton::Right, {
            let r_view = view.clone();
            let ctx_path = entry.path();
            move |ev, window, cx| {
                let pos = ev.position;
                r_view.update(cx, |this, cx| {
                    this.activate_tab(pane_id);
                    if let Some(ts) = this.state.tab_states.get_mut(&pane_id) {
                        if !ts.selection.selected.contains(&idx) {
                            ts.selection.keyboard_cursor = Some(idx);
                            ts.selection.selection_anchor = Some(idx);
                            ts.selection.last_clicked_index = Some(idx);
                            ts.selection.selected.clear();
                            ts.selection.selected.insert(idx);
                        }
                    }
                    this.open_ctx_menu(vec![ctx_path.clone()], pos, window, cx);
                    this.notify_pane(pane_id, cx);
                    cx.notify();
                });
                cx.stop_propagation();
            }
        });

    // 7. Hover tooltip
    let t_name = entry.name.to_string();
    let t_path = entry.path();
    let t_size = entry.size();
    let t_mod = entry.modified;
    row = row.tooltip(move |window, cx| {
        crate::ui::tooltips::entry_hover_tooltip(
            t_name.clone(),
            t_path.clone(),
            is_dir,
            t_size,
            t_mod,
            window,
            cx,
        )
    });

    row.into_any_element()
}
