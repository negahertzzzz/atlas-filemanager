//! Analyzer views built on the finished scan: treemap data, space by file
//! type, duplicate files and CSV export of what the current tab shows.
use super::{tr, CleanupPanel, DerivedKey, Tab};
use crate::ui::icons::{theme_icon, AtlasIcon};
use atlas_core::cleanup::DuplicateJob;
use atlas_model::cleanup::{DuplicateGroup, DuplicateReport, ScanStatus, TypeStat};
use gpui::{prelude::*, *};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex, v_flex, ActiveTheme, Disableable, Sizable,
};
use std::{
    collections::HashSet,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Rectangles the treemap may draw for one folder.
const TREEMAP_BUDGET: usize = 1_200;
const MIN_SIZES: [u64; 3] = [1 << 20, 10 << 20, 100 << 20];

/// One line of the duplicate list: a group header or one of its copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DupRow {
    Group {
        index: usize,
    },
    Copy {
        group: usize,
        path: PathBuf,
        oldest: bool,
    },
}

pub(super) struct Duplicates {
    job: Option<DuplicateJob>,
    report: Option<Arc<DuplicateReport>>,
    rows: Arc<Vec<DupRow>>,
    /// Folder the last search ran on.
    root: PathBuf,
    min_bytes: u64,
    checked: HashSet<PathBuf>,
    generation: u64,
    trashing: bool,
    scroll: UniformListScrollHandle,
}

impl Default for Duplicates {
    fn default() -> Self {
        Self {
            job: None,
            report: None,
            rows: Arc::default(),
            root: PathBuf::new(),
            min_bytes: atlas_core::cleanup::DUPLICATE_MIN_BYTES,
            checked: HashSet::new(),
            generation: 0,
            trashing: false,
            scroll: UniformListScrollHandle::new(),
        }
    }
}

impl Duplicates {
    fn set_report(&mut self, report: DuplicateReport) {
        self.rows = Arc::new(flatten(&report.groups));
        let present: HashSet<&PathBuf> = report.groups.iter().flat_map(|g| &g.paths).collect();
        self.checked.retain(|p| present.contains(p));
        self.report = Some(Arc::new(report));
    }
    fn running(&self) -> bool {
        self.job.is_some()
    }
    /// Bytes and number of the checked copies.
    fn selection(&self) -> (u64, usize) {
        let Some(report) = &self.report else {
            return (0, 0);
        };
        report
            .groups
            .iter()
            .flat_map(|g| g.paths.iter().map(move |p| (g.bytes, p)))
            .filter(|(_, p)| self.checked.contains(*p))
            .fold((0, 0), |(bytes, count), (size, _)| {
                (bytes.saturating_add(size), count + 1)
            })
    }
}

pub(super) fn flatten(groups: &[DuplicateGroup]) -> Vec<DupRow> {
    let mut rows = Vec::with_capacity(groups.iter().map(|g| g.paths.len() + 1).sum());
    for (index, group) in groups.iter().enumerate() {
        rows.push(DupRow::Group { index });
        rows.extend(
            group
                .paths
                .iter()
                .enumerate()
                .map(|(i, path)| DupRow::Copy {
                    group: index,
                    path: path.clone(),
                    oldest: i == 0,
                }),
        );
    }
    rows
}

/// Paths to check so that exactly one copy of every group, the oldest, stays.
pub(super) fn extra_copies(groups: &[DuplicateGroup]) -> HashSet<PathBuf> {
    groups
        .iter()
        .flat_map(|g| g.paths.iter().skip(1).cloned())
        .collect()
}

/// Drop `removed` from the groups; groups left with one copy are no longer duplicates.
pub(super) fn without(
    groups: &[DuplicateGroup],
    removed: &HashSet<PathBuf>,
) -> Vec<DuplicateGroup> {
    groups
        .iter()
        .filter_map(|g| {
            let paths: Vec<_> = g
                .paths
                .iter()
                .filter(|p| !removed.contains(*p))
                .cloned()
                .collect();
            (paths.len() > 1).then_some(DuplicateGroup {
                bytes: g.bytes,
                paths,
            })
        })
        .collect()
}

/// One CSV field (RFC 4180). Text that a spreadsheet would run as a formula
/// gets a leading apostrophe, so an exported file name cannot execute.
pub(super) fn csv_field(value: &str) -> String {
    let value = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

fn csv_line(out: &mut String, fields: &[&str]) {
    let line: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
    let _ = writeln!(out, "{}", line.join(","));
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn percent(part: u64, total: u64) -> String {
    format!("{:.2}", part as f64 * 100. / total.max(1) as f64)
}

fn type_label(stat: &TypeStat) -> String {
    if stat.small_files {
        tr("cleanup_types_small_files")
    } else if stat.extension.is_empty() {
        tr("cleanup_types_no_extension")
    } else {
        format!(".{}", stat.extension)
    }
}

impl CleanupPanel {
    /// Start the background work the visible tab needs from the finished scan.
    pub(super) fn ensure_derived(&mut self, cx: &mut Context<Self>) {
        if !self.snapshot.status.terminal() || self.snapshot.status == ScanStatus::Failed {
            return;
        }
        let key: DerivedKey = (self.current.clone(), self.generation, self.snapshot.bytes);
        let wants_treemap = self.tab == Tab::Map && self.treemap && !self.list_only;
        if wants_treemap && self.treemap_key.as_ref() != Some(&key) {
            self.treemap_key = Some(key.clone());
            // Never leave the previous folder's tiles clickable while computing.
            self.treemap_root = None;
            self.treemap_pending = true;
            self.treemap_hovered = None;
            self.treemap_hover_path = None;
            let path = self.current.clone();
            let key = key.clone();
            cx.spawn(async move |this, cx| {
                let root = cx
                    .background_executor()
                    .spawn(
                        async move { atlas_core::cleanup::cached_treemap(&path, TREEMAP_BUDGET) },
                    )
                    .await;
                let _ = this.update(cx, |panel, cx| {
                    if panel.treemap_key.as_ref() == Some(&key) {
                        panel.treemap_root = root.map(Arc::new);
                        panel.treemap_pending = false;
                        panel.treemap_revision += 1;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        if self.tab == Tab::Types && self.types_key.as_ref() != Some(&key) {
            self.types_key = Some(key.clone());
            self.types_pending = true;
            let path = self.current.clone();
            cx.spawn(async move |this, cx| {
                let types = cx
                    .background_executor()
                    .spawn(async move { atlas_core::cleanup::cached_types(&path) })
                    .await;
                let _ = this.update(cx, |panel, cx| {
                    if panel.types_key.as_ref() == Some(&key) {
                        panel.types = types.map(Arc::new);
                        panel.types_pending = false;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    pub(super) fn treemap_click(
        &mut self,
        path: Option<PathBuf>,
        directory: bool,
        clicks: usize,
        cx: &mut Context<Self>,
    ) {
        self.selected = path.clone();
        if let Some(path) = path {
            if let Some(row) = self.rows.iter().position(|r| r.path == path) {
                self.cursor = row;
                self.scroll.scroll_to_item(row, ScrollStrategy::Nearest);
            }
            if clicks >= 2 && directory {
                self.navigate(path, cx);
                return;
            }
        }
        cx.notify();
    }

    pub(super) fn types_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = super::muted(cx);
        let fg = cx.theme().foreground;
        let windows = self.windows_icons(cx);
        let hint = div()
            .text_sm()
            .text_color(muted)
            .child(tr("cleanup_types_hint"));
        let placeholder = |key: &str, icon: AtlasIcon| {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(theme_icon(icon, 34., muted, windows))
                .child(div().text_sm().text_color(muted).child(tr(key)))
                .into_any_element()
        };
        let body = if !self.snapshot.status.terminal() {
            placeholder("cleanup_scanning", AtlasIcon::Search)
        } else if self.types_pending {
            placeholder("cleanup_types_computing", AtlasIcon::Search)
        } else if let Some(types) = self.types.clone().filter(|t| !t.is_empty()) {
            let total = types
                .iter()
                .map(|t| t.bytes)
                .fold(0u64, u64::saturating_add);
            let colors = super::appearance::palette(cx);
            uniform_list("cleanup-types", types.len(), move |range, _, _| {
                range
                    .map(|i| {
                        let stat = &types[i];
                        let ratio = (stat.bytes as f64 / total.max(1) as f64).clamp(0., 1.) as f32;
                        let probe = PathBuf::from(format!("x.{}", stat.extension));
                        let tint = if stat.small_files {
                            fg.opacity(0.45)
                        } else {
                            super::appearance::file_color(Some(&probe), false, &colors)
                        };
                        h_flex()
                            .id(i)
                            .h(crate::ui::rem_px(44.))
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .gap_3()
                            .px_2()
                            .rounded_md()
                            .hover(move |s| s.bg(fg.opacity(0.05)))
                            .child(
                                div()
                                    .size(crate::ui::rem_px(10.))
                                    .flex_none()
                                    .rounded_full()
                                    .bg(tint),
                            )
                            .child(
                                div()
                                    .w(crate::ui::rem_px(170.))
                                    .flex_none()
                                    .truncate()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(type_label(stat)),
                            )
                            .child(
                                div()
                                    .w(crate::ui::rem_px(110.))
                                    .flex_none()
                                    .truncate()
                                    .text_xs()
                                    .text_color(fg.opacity(0.65))
                                    .child(
                                        rust_i18n::t!("cleanup_types_files", count = stat.files)
                                            .into_owned(),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h(crate::ui::rem_px(6.))
                                    .rounded_full()
                                    .bg(fg.opacity(0.06))
                                    .child(
                                        div()
                                            .h_full()
                                            .w(relative(ratio))
                                            .rounded_full()
                                            .bg(tint.opacity(0.8)),
                                    ),
                            )
                            .child(
                                div()
                                    .w(crate::ui::rem_px(86.))
                                    .flex_none()
                                    .text_right()
                                    .text_sm()
                                    .child(atlas_core::util::display::format_size(stat.bytes)),
                            )
                            .child(
                                div()
                                    .w(crate::ui::rem_px(56.))
                                    .flex_none()
                                    .text_right()
                                    .text_xs()
                                    .text_color(fg.opacity(0.65))
                                    .child(format!("{:.1}%", ratio * 100.)),
                            )
                            .into_any_element()
                    })
                    .collect()
            })
            .track_scroll(&self.types_scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        } else {
            placeholder("cleanup_types_unavailable", AtlasIcon::HardDrive)
        };
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(hint)
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .truncate()
                    .child(self.current.display().to_string()),
            )
            .child(body)
            .into_any_element()
    }

    fn start_duplicates(&mut self, cx: &mut Context<Self>) {
        if !self.snapshot.status.terminal() || self.execution.is_some() || self.dup.trashing {
            return;
        }
        self.dup.job = None;
        self.dup.generation += 1;
        let generation = self.dup.generation;
        self.dup.root = self.current.clone();
        self.dup.report = None;
        self.dup.rows = Arc::default();
        self.dup.checked.clear();
        self.notice = None;
        self.error = None;
        let job = DuplicateJob::start(self.current.clone(), generation, self.dup.min_bytes);
        let receiver = job.receiver.clone();
        self.dup.job = Some(job);
        cx.spawn(async move |this, cx| {
            while let Ok(report) = receiver.recv_async().await {
                let finished = report.finished;
                let alive = this.update(cx, |panel, cx| {
                    if panel.dup.generation == generation {
                        panel.dup.set_report(report);
                        if finished {
                            panel.dup.job = None;
                        }
                        cx.notify();
                    }
                });
                if alive.is_err() || finished {
                    return;
                }
            }
            // The worker ended without a final report (it could not start).
            let _ = this.update(cx, |panel, cx| {
                if panel.dup.generation == generation {
                    panel.dup.job = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn stop_duplicates(&mut self, cx: &mut Context<Self>) {
        if let Some(job) = &self.dup.job {
            job.cancel();
        }
        cx.notify();
    }

    /// Check or uncheck one copy. The last unchecked copy of a group cannot be
    /// checked: a duplicate cleanup never removes every copy.
    fn toggle_duplicate(&mut self, group: usize, path: PathBuf) {
        if self.dup.trashing || self.dup.checked.remove(&path) {
            return;
        }
        let Some(group) = self.dup.report.as_ref().and_then(|r| r.groups.get(group)) else {
            return;
        };
        let others_kept = group
            .paths
            .iter()
            .any(|p| *p != path && !self.dup.checked.contains(p));
        if others_kept {
            self.dup.checked.insert(path);
        } else {
            self.notice = Some(tr("cleanup_dup_keep_one"));
        }
    }

    fn trash_duplicates(&mut self, cx: &mut Context<Self>) {
        if self.network || self.dup.trashing || self.execution.is_some() {
            return;
        }
        let Some(report) = self.dup.report.clone() else {
            return;
        };
        // Re-check the keep-one rule on the whole selection before touching anything.
        let paths: Vec<PathBuf> = report
            .groups
            .iter()
            .filter(|g| g.paths.iter().any(|p| !self.dup.checked.contains(p)))
            .flat_map(|g| {
                g.paths
                    .iter()
                    .filter(|p| self.dup.checked.contains(*p))
                    .cloned()
            })
            .collect();
        if paths.is_empty() {
            return;
        }
        self.dup.trashing = true;
        self.notice = None;
        self.error = None;
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|path| {
                            let outcome = atlas_core::cleanup::move_to_trash(&path).err();
                            (path, outcome.map(|e| e.to_string()))
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |panel, cx| {
                panel.dup.trashing = false;
                let removed: HashSet<PathBuf> = results
                    .iter()
                    .filter(|(_, e)| e.is_none())
                    .map(|(p, _)| p.clone())
                    .collect();
                if let Some(report) = &panel.dup.report {
                    let mut updated = (**report).clone();
                    updated.groups = without(&report.groups, &removed);
                    panel.dup.set_report(updated);
                }
                let failed: Vec<_> = results.iter().filter_map(|(_, e)| e.as_ref()).collect();
                panel.notice =
                    Some(rust_i18n::t!("cleanup_dup_trashed", count = removed.len()).into_owned());
                if let Some(first) = failed.first() {
                    panel.error = Some(format!(
                        "{} ({})",
                        super::user_error(first),
                        rust_i18n::t!("cleanup_dup_failed", count = failed.len())
                    ));
                }
                // Sizes in the map and lists come from the updated in-memory tree.
                panel.completed.clear();
                let current = panel.current.clone();
                if let Some(s) = atlas_core::cleanup::cached_snapshot(&current, panel.generation) {
                    panel.snapshot = Arc::new(s);
                }
                panel.rebuild();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn duplicates_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = super::muted(cx);
        let fg = cx.theme().foreground;
        let selection_bg = cx.theme().sidebar_accent;
        let busy = self.execution.is_some() || self.preparing;
        let ready = self.snapshot.status.terminal() && self.snapshot.status != ScanStatus::Failed;
        let running = self.dup.running();
        let format = atlas_core::util::display::format_size;

        let mut toolbar = h_flex().w_full().items_center().gap_1().child(
            div()
                .text_sm()
                .text_color(muted)
                .mr_1()
                .child(tr("cleanup_dup_min_size")),
        );
        for (i, size) in MIN_SIZES.into_iter().enumerate() {
            let active = self.dup.min_bytes == size;
            toolbar = toolbar.child(
                Button::new(("cleanup-dup-min", i))
                    .ghost()
                    .small()
                    .disabled(running || self.dup.trashing)
                    .bg(if active {
                        selection_bg
                    } else {
                        transparent_black()
                    })
                    .child(
                        div()
                            .text_sm()
                            .text_color(if active { fg } else { muted })
                            .child(format!("≥ {}", format(size))),
                    )
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.dup.min_bytes = size;
                        cx.notify();
                    })),
            );
        }
        toolbar = toolbar.child(div().flex_1()).child(if running {
            Button::new("cleanup-dup-stop")
                .small()
                .label(tr("cleanup_stop"))
                .on_click(cx.listener(|p, _, _, cx| p.stop_duplicates(cx)))
        } else {
            Button::new("cleanup-dup-start")
                .primary()
                .small()
                .disabled(!ready || busy || self.dup.trashing)
                .label(tr(if self.dup.report.is_some() {
                    "cleanup_dup_search_again"
                } else {
                    "cleanup_dup_search"
                }))
                .on_click(cx.listener(|p, _, _, cx| p.start_duplicates(cx)))
        });

        let summary = match &self.dup.report {
            _ if !ready => tr("cleanup_dup_wait_scan"),
            None if running => tr("cleanup_dup_collecting"),
            None => tr("cleanup_dup_hint"),
            Some(report) if !report.finished => rust_i18n::t!(
                "cleanup_dup_progress",
                percent = format!(
                    "{:.0}",
                    report.hashed_bytes as f64 * 100. / report.total_bytes.max(1) as f64
                ),
                done = format(report.hashed_bytes),
                total = format(report.total_bytes)
            )
            .into_owned(),
            Some(report) if report.groups.is_empty() => tr(if report.cancelled {
                "cleanup_dup_cancelled"
            } else {
                "cleanup_dup_none"
            }),
            Some(report) => {
                let wasted = report
                    .groups
                    .iter()
                    .map(DuplicateGroup::wasted)
                    .fold(0u64, u64::saturating_add);
                let mut text = rust_i18n::t!(
                    "cleanup_dup_summary",
                    groups = report.groups.len(),
                    size = format(wasted)
                )
                .into_owned();
                if report.cancelled {
                    text = format!("{text} · {}", tr("cleanup_dup_cancelled"));
                }
                text
            }
        };
        let errors = self.dup.report.as_ref().map_or(0, |r| r.errors);
        let mut body = v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(toolbar)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(div().text_sm().child(summary))
                    .when(errors > 0, |row| {
                        row.child(div().text_xs().text_color(cx.theme().warning).child(
                            rust_i18n::t!("cleanup_dup_errors", count = errors).into_owned(),
                        ))
                    }),
            )
            .when(!self.dup.root.as_os_str().is_empty(), |v| {
                v.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .truncate()
                        .child(self.dup.root.display().to_string()),
                )
            });

        if let Some(report) = self.dup.report.clone().filter(|r| !r.groups.is_empty()) {
            let rows = self.dup.rows.clone();
            let panel = cx.entity().downgrade();
            body = body.child(
                uniform_list("cleanup-duplicates", rows.len(), move |range, _, cx| {
                    let Some(entity) = panel.upgrade() else {
                        return Vec::new();
                    };
                    entity.update(cx, |panel, cx| {
                        let fg = cx.theme().foreground;
                        let muted = super::muted(cx);
                        let accent = cx.theme().accent;
                        range
                            .map(|index| match &rows[index] {
                                DupRow::Group { index: group } => {
                                    let group = &report.groups[*group];
                                    h_flex()
                                        .id(index)
                                        .h(crate::ui::rem_px(40.))
                                        .w_full()
                                        .items_center()
                                        .gap_2()
                                        .px_2()
                                        .mt_1()
                                        .rounded_md()
                                        .bg(fg.opacity(0.04))
                                        .child(
                                            div()
                                                .flex_1()
                                                .text_sm()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child(format!(
                                                    "{} × {}",
                                                    group.paths.len(),
                                                    format(group.bytes)
                                                )),
                                        )
                                        .child(
                                            div().text_xs().text_color(muted).child(
                                                rust_i18n::t!(
                                                    "cleanup_dup_recoverable",
                                                    size = format(group.wasted())
                                                )
                                                .into_owned(),
                                            ),
                                        )
                                        .into_any_element()
                                }
                                DupRow::Copy {
                                    group,
                                    path,
                                    oldest,
                                } => {
                                    let checked = panel.dup.checked.contains(path);
                                    let selected = panel.selected.as_ref() == Some(path);
                                    let view = cx.entity().downgrade();
                                    let (group, target) = (*group, path.clone());
                                    let parent = path
                                        .parent()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_default();
                                    h_flex()
                                        .id(index)
                                        .h(crate::ui::rem_px(40.))
                                        .w_full()
                                        .min_w_0()
                                        .items_center()
                                        .gap_3()
                                        .pl_4()
                                        .pr_2()
                                        .rounded_md()
                                        .cursor_pointer()
                                        .bg(if selected {
                                            accent.opacity(0.12)
                                        } else {
                                            transparent_black()
                                        })
                                        .hover(move |s| s.bg(fg.opacity(0.05)))
                                        .child(
                                            Checkbox::new(("cleanup-dup-check", index))
                                                .checked(checked)
                                                .small()
                                                .tab_stop(false)
                                                .disabled(panel.dup.trashing),
                                        )
                                        .child(panel.file_art(path, false, 18., cx))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .child(
                                                    div().text_sm().truncate().child(name_of(path)),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .truncate()
                                                        .child(parent),
                                                ),
                                        )
                                        .when(*oldest, |row| {
                                            row.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(tr("cleanup_dup_oldest")),
                                            )
                                        })
                                        .on_mouse_down(MouseButton::Left, move |_, w, cx| {
                                            let _ = view.update(cx, |panel, cx| {
                                                w.focus(&panel.focus, cx);
                                                panel.selected = Some(target.clone());
                                                panel.notice = None;
                                                panel.toggle_duplicate(group, target.clone());
                                                cx.notify();
                                            });
                                            cx.stop_propagation();
                                        })
                                        .into_any_element()
                                }
                            })
                            .collect()
                    })
                })
                .track_scroll(&self.dup.scroll)
                .flex_1()
                .min_h_0(),
            );
            let (bytes, count) = self.dup.selection();
            body = body.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("cleanup-dup-extra")
                            .small()
                            .disabled(self.dup.trashing)
                            .label(tr("cleanup_dup_select_extra"))
                            .on_click(cx.listener(|p, _, _, cx| {
                                if let Some(report) = &p.dup.report {
                                    p.dup.checked = extra_copies(&report.groups);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("cleanup-dup-clear")
                            .small()
                            .ghost()
                            .disabled(self.dup.trashing || count == 0)
                            .label(tr("cleanup_dup_clear"))
                            .on_click(cx.listener(|p, _, _, cx| {
                                p.dup.checked.clear();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("cleanup-dup-reveal")
                            .small()
                            .ghost()
                            .disabled(self.selected.is_none())
                            .label(tr("cleanup_reveal"))
                            .on_click(cx.listener(|p, _, w, cx| p.reveal(w, cx))),
                    )
                    .child(
                        Button::new("cleanup-dup-trash")
                            .small()
                            .danger()
                            .disabled(self.network || self.dup.trashing || running || count == 0)
                            .label(if count == 0 {
                                tr("cleanup_trash")
                            } else {
                                format!("{} · {count} · {}", tr("cleanup_trash"), format(bytes))
                            })
                            .on_click(cx.listener(|p, _, _, cx| p.trash_duplicates(cx))),
                    ),
            );
        } else if self.dup.report.is_none() && !running {
            body = body.child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(theme_icon(
                        AtlasIcon::Copy,
                        34.,
                        muted,
                        self.windows_icons(cx),
                    ))
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .max_w(crate::ui::rem_px(460.))
                            .text_center()
                            .child(tr("cleanup_dup_explain")),
                    ),
            );
        } else {
            body = body.child(div().flex_1());
        }
        if self.network {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(tr("cleanup_network_no_trash")),
            );
        }
        body.into_any_element()
    }

    /// True when the current tab has rows to export.
    pub(super) fn can_export(&self) -> bool {
        if !self.snapshot.status.terminal() || self.review.is_some() {
            return false;
        }
        match self.tab {
            Tab::Map => !self.snapshot.entries.is_empty(),
            Tab::Files => !self.snapshot.large_files.is_empty(),
            Tab::Types => self.types.as_ref().is_some_and(|t| !t.is_empty()),
            Tab::Duplicates => self
                .dup
                .report
                .as_ref()
                .is_some_and(|r| !r.groups.is_empty()),
            Tab::Cleanup | Tab::History => false,
        }
    }

    fn csv(&self) -> Option<(&'static str, String)> {
        let format = atlas_core::util::display::format_size;
        let total = self.snapshot.bytes;
        // BOM: spreadsheet apps then read accented names as UTF-8.
        let mut out = String::from("\u{feff}");
        let name = match self.tab {
            Tab::Map => {
                csv_line(
                    &mut out,
                    &["path", "name", "type", "bytes", "size", "percent"],
                );
                for e in &self.snapshot.entries {
                    let kind = if e.directory { "folder" } else { "file" };
                    csv_line(
                        &mut out,
                        &[
                            &e.path.display().to_string(),
                            &name_of(&e.path),
                            kind,
                            &e.bytes.to_string(),
                            &format(e.bytes),
                            &percent(e.bytes, total),
                        ],
                    );
                }
                "atlas-folders.csv"
            }
            Tab::Files => {
                csv_line(&mut out, &["path", "name", "bytes", "size"]);
                for e in &self.snapshot.large_files {
                    csv_line(
                        &mut out,
                        &[
                            &e.path.display().to_string(),
                            &name_of(&e.path),
                            &e.bytes.to_string(),
                            &format(e.bytes),
                        ],
                    );
                }
                "atlas-large-files.csv"
            }
            Tab::Types => {
                let types = self.types.as_ref()?;
                let total = types
                    .iter()
                    .map(|t| t.bytes)
                    .fold(0u64, u64::saturating_add);
                csv_line(&mut out, &["type", "files", "bytes", "size", "percent"]);
                for t in types.iter() {
                    csv_line(
                        &mut out,
                        &[
                            &type_label(t),
                            &t.files.to_string(),
                            &t.bytes.to_string(),
                            &format(t.bytes),
                            &percent(t.bytes, total),
                        ],
                    );
                }
                "atlas-file-types.csv"
            }
            Tab::Duplicates => {
                let report = self.dup.report.as_ref()?;
                csv_line(
                    &mut out,
                    &["group", "bytes", "size", "copies", "path", "selected"],
                );
                for (i, g) in report.groups.iter().enumerate() {
                    for path in &g.paths {
                        csv_line(
                            &mut out,
                            &[
                                &(i + 1).to_string(),
                                &g.bytes.to_string(),
                                &format(g.bytes),
                                &g.paths.len().to_string(),
                                &path.display().to_string(),
                                if self.dup.checked.contains(path) {
                                    "yes"
                                } else {
                                    "no"
                                },
                            ],
                        );
                    }
                }
                "atlas-duplicates.csv"
            }
            Tab::Cleanup | Tab::History => return None,
        };
        Some((name, out))
    }

    pub(super) fn export_csv(&mut self, cx: &mut Context<Self>) {
        if !self.can_export() {
            return;
        }
        let Some((name, content)) = self.csv() else {
            return;
        };
        self.error = None;
        self.notice = None;
        cx.spawn(async move |this, cx| {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_file_name(name)
                .add_filter("CSV", &["csv"])
                .save_file()
                .await
            else {
                return;
            };
            let path = file.path().to_owned();
            let target = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { std::fs::write(&target, content) })
                .await;
            let _ = this.update(cx, |panel, cx| {
                match result {
                    Ok(()) => {
                        panel.notice = Some(
                            rust_i18n::t!("cleanup_export_done", path = path.display().to_string())
                                .into_owned(),
                        );
                    }
                    Err(e) => panel.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the parent's `gpui::*` glob would make `#[test]` mean `gpui::test`.
    use super::{csv_field, extra_copies, flatten, without, DupRow};
    use atlas_model::cleanup::DuplicateGroup;
    use std::{collections::HashSet, path::PathBuf};

    fn group(bytes: u64, names: &[&str]) -> DuplicateGroup {
        DuplicateGroup {
            bytes,
            paths: names.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn csv_fields_are_quoted_and_formulas_neutralized() {
        assert_eq!(csv_field("plain.txt"), "plain.txt");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("=HYPERLINK(1)"), "'=HYPERLINK(1)");
        assert_eq!(csv_field("@cmd,x"), "\"'@cmd,x\"");
        assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
    }

    #[test]
    fn duplicate_rows_selection_and_removal_keep_one_copy() {
        let groups = vec![group(10, &["a1", "a2", "a3"]), group(5, &["b1", "b2"])];
        let rows = flatten(&groups);
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[0], DupRow::Group { index: 0 });
        assert!(matches!(&rows[1], DupRow::Copy { oldest: true, .. }));
        assert!(matches!(&rows[4], DupRow::Group { index: 1 }));

        let extra = extra_copies(&groups);
        assert_eq!(extra.len(), 3);
        assert!(!extra.contains(&PathBuf::from("a1")) && !extra.contains(&PathBuf::from("b1")));

        let left = without(&groups, &extra);
        assert!(left.is_empty(), "every group is down to one copy");
        let partial = without(&groups, &HashSet::from([PathBuf::from("a2")]));
        assert_eq!(partial.len(), 2);
        assert_eq!(partial[0].paths.len(), 2);
    }
}
