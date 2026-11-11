//! Folder comparison between the two visible panes: what exists on one side
//! only, what changed, and one-click copies of the missing or newer files in
//! either direction (through the normal transfer pipeline, so conflicts still
//! ask before anything is overwritten).
use crate::app::App as AtlasApp;
use atlas_model::compare::{CompareReport, CompareRow, CompareSide, CompareStatus};
use gpui::{prelude::*, *};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex, v_flex, ActiveTheme, Disableable, Sizable,
};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    Differences,
    LeftOnly,
    RightOnly,
    Changed,
    All,
}

impl Filter {
    fn accepts(self, row: &CompareRow) -> bool {
        match self {
            Self::Differences => row.status != CompareStatus::Same,
            Self::LeftOnly => row.status == CompareStatus::LeftOnly,
            Self::RightOnly => row.status == CompareStatus::RightOnly,
            Self::Changed => row.status == CompareStatus::Changed,
            Self::All => true,
        }
    }
}

pub(crate) struct ComparePanel {
    pub(crate) focus: FocusHandle,
    parent: WeakEntity<AtlasApp>,
    left: PathBuf,
    right: PathBuf,
    recursive: bool,
    filter: Filter,
    report: Arc<CompareReport>,
    visible: Arc<Vec<usize>>,
    running: Option<Arc<AtomicBool>>,
    generation: u64,
    scroll: UniformListScrollHandle,
}

fn tr(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

impl ComparePanel {
    pub(crate) fn new(
        left: PathBuf,
        right: PathBuf,
        parent: WeakEntity<AtlasApp>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut panel = Self {
            focus: cx.focus_handle(),
            parent,
            left,
            right,
            recursive: true,
            filter: Filter::Differences,
            report: Arc::default(),
            visible: Arc::default(),
            running: None,
            generation: 0,
            scroll: UniformListScrollHandle::new(),
        };
        panel.run(cx);
        panel
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.running.take() {
            cancel.store(true, Ordering::Release);
        }
        self.generation += 1;
        let generation = self.generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.running = Some(cancel.clone());
        self.report = Arc::new(CompareReport {
            left: self.left.clone(),
            right: self.right.clone(),
            ..Default::default()
        });
        self.refilter();
        let (left, right, recursive) = (self.left.clone(), self.right.clone(), self.recursive);
        let (sender, receiver) = flume::bounded::<CompareReport>(1);
        std::thread::Builder::new()
            .name("atlas-compare".into())
            .spawn(move || {
                let drain = sender.clone();
                let push = |report: &CompareReport| {
                    let mut value = report.clone();
                    // Keep only the newest progress report in the channel.
                    while let Err(flume::TrySendError::Full(back)) = drain.try_send(value) {
                        value = back;
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                };
                atlas_core::fs::compare::compare(&left, &right, recursive, &cancel, push);
            })
            .ok();
        cx.spawn(async move |this, cx| {
            while let Ok(report) = receiver.recv_async().await {
                let finished = report.finished;
                let alive = this
                    .update(cx, |panel, cx| {
                        if panel.generation != generation {
                            return;
                        }
                        panel.report = Arc::new(report);
                        if finished {
                            panel.running = None;
                        }
                        panel.refilter();
                        cx.notify();
                    })
                    .is_ok();
                if !alive || finished {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn refilter(&mut self) {
        let filter = self.filter;
        self.visible = Arc::new(
            self.report
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| filter.accepts(row))
                .map(|(i, _)| i)
                .collect(),
        );
    }

    fn plan_len(&self, from: CompareSide) -> usize {
        atlas_core::fs::compare::copy_plan(&self.report, from)
            .iter()
            .map(|(_, sources)| sources.len())
            .sum()
    }

    fn copy(&mut self, from: CompareSide, window: &mut Window, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        let plan = atlas_core::fs::compare::copy_plan(&self.report, from);
        if plan.is_empty() {
            return;
        }
        let _ = self.parent.update(cx, |app, cx| {
            for (dest, sources) in plan {
                app.request_transfer(atlas_model::ops::TransferRequest {
                    sources,
                    target_names: None,
                    dest,
                    kind: atlas_model::ops::TransferKind::Copy,
                    // Overwrites of changed files go through the conflict dialog.
                    policy: atlas_model::ops::ConflictPolicy::Ask,
                    origin: atlas_model::ops::TransferOrigin::Menu,
                });
            }
            cx.notify();
        });
        self.close(window, cx);
    }

    pub(crate) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(cancel) = self.running.take() {
            cancel.store(true, Ordering::Release);
        }
        let _ = self.parent.update(cx, |app, cx| {
            app.compare_panel = None;
            window.focus(&app.focus_handle, cx);
            cx.notify();
        });
    }

    /// Keys for the open panel; true when consumed.
    pub(crate) fn handle_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match event.keystroke.key.as_str() {
            "escape" => self.close(window, cx),
            "f5" => self.run(cx),
            "up" | "down" | "pageup" | "pagedown" | "home" | "end" | "enter" | "tab" => {}
            _ => return false,
        }
        true
    }

    fn row(&self, index: usize, cx: &App) -> AnyElement {
        let row = &self.report.rows[index];
        let theme = cx.theme();
        let (label, color) = match row.status {
            CompareStatus::Same => ("compare_same", theme.muted_foreground),
            CompareStatus::Changed => ("compare_changed", theme.warning),
            CompareStatus::LeftOnly => ("compare_left_only", theme.accent),
            CompareStatus::RightOnly => ("compare_right_only", theme.success),
        };
        let side = |entry: &Option<atlas_model::compare::CompareEntry>, newer: bool| {
            let text = entry.as_ref().map_or_else(
                || "—".to_owned(),
                |entry| {
                    let size = entry.bytes.map_or_else(
                        || tr("compare_folder"),
                        atlas_core::util::display::format_size,
                    );
                    let date = entry
                        .modified
                        .map(atlas_core::util::display::format_date)
                        .unwrap_or_default();
                    format!("{size} · {date}")
                },
            );
            div()
                .w(relative(0.24))
                .flex_none()
                .truncate()
                .text_xs()
                .text_color(if newer {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .when(newer, |d| d.font_weight(FontWeight::SEMIBOLD))
                .child(text)
        };
        h_flex()
            .id(("compare-row", index))
            .w_full()
            .h(crate::ui::rem_px(30.))
            .px_2()
            .gap_3()
            .items_center()
            .rounded_md()
            .hover(|s| s.bg(theme.foreground.opacity(0.05)))
            .child(
                div()
                    .size(crate::ui::rem_px(7.))
                    .flex_none()
                    .rounded_full()
                    .bg(color),
            )
            .child(
                div()
                    .w(crate::ui::rem_px(96.))
                    .flex_none()
                    .text_xs()
                    .text_color(color)
                    .child(tr(label)),
            )
            .child(div().flex_1().min_w_0().truncate().text_sm().child(format!(
                "{}{}",
                row.relative.display(),
                if row.directory {
                    std::path::MAIN_SEPARATOR_STR
                } else {
                    ""
                }
            )))
            .child(side(&row.left, row.newer == Some(CompareSide::Left)))
            .child(side(&row.right, row.newer == Some(CompareSide::Right)))
            .into_any_element()
    }
}

impl Render for ComparePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (fg, muted, border, accent, success, mut bg) = {
            let theme = cx.theme();
            (
                theme.foreground,
                theme.muted_foreground,
                theme.border,
                theme.accent,
                theme.success,
                theme.popover,
            )
        };
        bg.alpha = 1.;
        let viewport = window.viewport_size();
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let width = (f32::from(viewport.width) - 32.)
            .min(1100. * zoom)
            .max(260.);
        let height = (f32::from(viewport.height) - 38.)
            .min(760. * zoom)
            .max(200.);
        let running = self.running.is_some();
        let report = self.report.clone();
        let to_right = self.plan_len(CompareSide::Left);
        let to_left = self.plan_len(CompareSide::Right);

        let chip =
            |id: &'static str, key: &str, filter: Filter, count: usize, cx: &mut Context<Self>| {
                let active = self.filter == filter;
                Button::new(id)
                    .ghost()
                    .small()
                    .bg(if active {
                        accent.opacity(0.16)
                    } else {
                        transparent_black()
                    })
                    .label(format!("{} · {count}", tr(key)))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.filter = filter;
                        p.refilter();
                        cx.notify();
                    }))
            };
        let differences = report
            .rows
            .iter()
            .filter(|r| r.status != CompareStatus::Same)
            .count();
        let filters = h_flex()
            .gap_1()
            .flex_wrap()
            .child(chip(
                "compare-f-diff",
                "compare_filter_differences",
                Filter::Differences,
                differences,
                cx,
            ))
            .child(chip(
                "compare-f-left",
                "compare_left_only",
                Filter::LeftOnly,
                report.count(CompareStatus::LeftOnly),
                cx,
            ))
            .child(chip(
                "compare-f-right",
                "compare_right_only",
                Filter::RightOnly,
                report.count(CompareStatus::RightOnly),
                cx,
            ))
            .child(chip(
                "compare-f-changed",
                "compare_changed",
                Filter::Changed,
                report.count(CompareStatus::Changed),
                cx,
            ))
            .child(chip(
                "compare-f-all",
                "compare_filter_all",
                Filter::All,
                report.rows.len(),
                cx,
            ))
            .child(div().flex_1())
            .child(
                Checkbox::new("compare-recursive")
                    .label(tr("compare_recursive"))
                    .checked(self.recursive)
                    .small()
                    .on_click(cx.listener(|p, checked: &bool, _, cx| {
                        p.recursive = *checked;
                        p.run(cx);
                    })),
            )
            .child(
                Button::new("compare-refresh")
                    .ghost()
                    .small()
                    .label(tr("compare_refresh"))
                    .on_click(cx.listener(|p, _, _, cx| p.run(cx))),
            );

        let header_paths = h_flex()
            .w_full()
            .gap_3()
            .text_xs()
            .text_color(muted)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("◀ {}", self.left.display())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("{} ▶", self.right.display())),
            );

        let visible = self.visible.clone();
        let entity = cx.entity().downgrade();
        let list: AnyElement = if visible.is_empty() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted)
                .child(tr(if running {
                    "compare_running"
                } else {
                    "compare_no_differences"
                }))
                .into_any_element()
        } else {
            uniform_list("compare-rows", visible.len(), move |range, _, cx| {
                let Some(panel) = entity.upgrade() else {
                    return Vec::new();
                };
                let panel = panel.read(cx);
                range.map(|i| panel.row(visible[i], cx)).collect()
            })
            .track_scroll(&self.scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };

        let status = if running {
            tr("compare_running")
        } else if report.cancelled {
            tr("compare_cancelled")
        } else {
            let mut text = rust_i18n::t!(
                "compare_summary",
                total = report.rows.len(),
                differences = differences
            )
            .into_owned();
            if report.truncated {
                text.push_str(" · ");
                text.push_str(&tr("compare_truncated"));
            }
            if report.errors > 0 {
                text.push_str(&format!(" · {} {}", report.errors, tr("cleanup_errors")));
            }
            text
        };

        let footer = h_flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(border.opacity(0.6))
            .child(
                div()
                    .size(crate::ui::rem_px(6.))
                    .rounded_full()
                    .bg(if running { accent } else { success }),
            )
            .child(div().flex_1().min_w_0().truncate().text_sm().child(status))
            .child(
                Button::new("compare-copy-left")
                    .small()
                    .label(rust_i18n::t!("compare_copy_to_left", count = to_left).into_owned())
                    .disabled(running || to_left == 0)
                    .on_click(cx.listener(|p, _, w, cx| p.copy(CompareSide::Right, w, cx))),
            )
            .child(
                Button::new("compare-copy-right")
                    .small()
                    .primary()
                    .label(rust_i18n::t!("compare_copy_to_right", count = to_right).into_owned())
                    .disabled(running || to_right == 0)
                    .on_click(cx.listener(|p, _, w, cx| p.copy(CompareSide::Left, w, cx))),
            );

        v_flex()
            .id("compare-panel")
            .track_focus(&self.focus)
            .w(px(width))
            .h(px(height))
            .rounded_xl()
            .bg(bg)
            .shadow_lg()
            .overflow_hidden()
            .text_color(fg)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|p, _, w, cx| {
                    w.focus(&p.focus, cx);
                    cx.stop_propagation();
                }),
            )
            .on_key_down(cx.listener(|p, event: &KeyDownEvent, w, cx| {
                if p.handle_key(event, w, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                v_flex()
                    .gap_3()
                    .px_5()
                    .pt_5()
                    .pb_3()
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(crate::ui::rem_px(22.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(tr("compare_title")),
                            )
                            .child(
                                Button::new("compare-close")
                                    .ghost()
                                    .small()
                                    .label(tr("cleanup_close"))
                                    .on_click(cx.listener(|p, _, w, cx| p.close(w, cx))),
                            ),
                    )
                    .child(header_paths)
                    .child(filters),
            )
            .child(v_flex().flex_1().min_h_0().px_4().pb_2().child(list))
            .child(footer)
    }
}

impl AtlasApp {
    /// Compare the focused pane's folder with the neighbouring visible pane.
    pub(crate) fn open_folder_compare(&mut self) {
        let panes = self.state.pane_tree.visible_panes();
        let dir = |app: &Self, id| {
            app.tab_data_for(id)
                .and_then(|td| td.location.fs_dir())
                .map(std::path::Path::to_path_buf)
        };
        let focused = self.state.focused_tile;
        let Some(at) = panes.iter().position(|id| *id == focused) else {
            return;
        };
        // The neighbour to the right/below, or to the left for the last pane.
        let other = panes
            .get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|i| panes.get(i)));
        let (Some(other), Some(here)) = (other.copied(), dir(self, focused)) else {
            self.toast_info(tr("compare_need_two_panes"));
            return;
        };
        let Some(there) = dir(self, other) else {
            self.toast_info(tr("compare_need_two_panes"));
            return;
        };
        if here == there {
            self.toast_info(tr("compare_same_folder"));
            return;
        }
        // Keep the on-screen order: the earlier pane is "left".
        let (left, right) = if panes.iter().position(|id| *id == other) < Some(at) {
            (there, here)
        } else {
            (here, there)
        };
        self.pending_compare = Some((left, right));
    }

    /// Mount the panel requested by `open_folder_compare` (needs a `Context`).
    pub(crate) fn mount_pending_compare(&mut self, cx: &mut Context<Self>) {
        if let Some((left, right)) = self.pending_compare.take() {
            let parent = cx.entity().downgrade();
            self.compare_panel = Some(cx.new(|cx| ComparePanel::new(left, right, parent, cx)));
            cx.notify();
        }
    }

    pub(crate) fn render_folder_compare(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = self.compare_panel.clone() else {
            return div().into_any_element();
        };
        deferred(
            div()
                .absolute()
                .inset_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_3()
                .bg(cx.theme().overlay)
                .occlude()
                .child(panel),
        )
        .into_any_element()
    }
}
