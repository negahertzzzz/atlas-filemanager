use super::{
    bubble_map_element::BubbleMapElement, tr, treemap_element::TreemapElement, CleanupPanel, Row,
    Tab,
};
use crate::ui::icons::{theme_icon, AtlasIcon};
use atlas_model::cleanup::ScanStatus;
use gpui::{prelude::*, *};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex, v_flex, ActiveTheme, Disableable, Sizable,
};
use std::sync::{atomic::Ordering, Arc};

fn button(
    id: &'static str,
    key: &str,
    disabled: bool,
    cx: &mut Context<CleanupPanel>,
    action: impl Fn(&mut CleanupPanel, &mut Window, &mut Context<CleanupPanel>) + 'static,
) -> AnyElement {
    Button::new(id)
        .label(tr(key))
        .small()
        .when(
            key == "cleanup_review" || key == "cleanup_review_all",
            |b| b.primary(),
        )
        .disabled(disabled)
        .on_click(cx.listener(move |panel, _, window, cx| {
            action(panel, window, cx);
            cx.notify();
        }))
        .into_any_element()
}

impl Render for CleanupPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_derived(cx);
        let fg = cx.theme().foreground;
        let muted = super::muted(cx);
        let border = cx.theme().border;
        let mut bg = cx.theme().popover;
        bg.alpha = 1.;
        let dark = cx.theme().is_dark();
        let windows = self.windows_icons(cx);
        let busy = self.execution.is_some() || self.preparing;
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let viewport = window.viewport_size();
        let width = (f32::from(viewport.width) - 32.)
            .min(1240. * zoom)
            .max(240.);
        let height = (f32::from(viewport.height) - 38.)
            .min(820. * zoom)
            .max(180.);
        let narrow = width < 1020. * zoom;
        let scanning = !self.snapshot.status.terminal();
        let paused = self.snapshot.status == ScanStatus::Paused;

        // Opening: fade in while rising a few pixels. Labs capture single frames.
        let opening = if cfg!(feature = "ui-lab") {
            1.0
        } else {
            (self.opened.elapsed().as_secs_f32() / 0.26).min(1.0)
        };
        if opening < 1.0 {
            window.request_animation_frame();
        }
        let opening = 1.0 - (1.0 - opening).powi(3);
        let header = self.heading(narrow, cx);
        let mut content = v_flex().flex_1().min_h_0().min_w_0().gap_3().px_5().pb_3();
        if self.tab == Tab::History {
            content = content.child(self.history(cx));
        } else if let Some(plan) = &self.review {
            let bytes: u64 = std::iter::once(plan)
                .chain(self.plans.iter())
                .flat_map(|p| p.candidates.iter())
                .map(|c| c.identity.bytes)
                .sum();
            let count: usize = std::iter::once(plan)
                .chain(self.plans.iter())
                .map(|p| p.candidates.len())
                .sum();
            content = content
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_color(fg)
                        .p_3()
                        .rounded_lg()
                        .bg(cx.theme().danger.opacity(0.08))
                        .child(tr("cleanup_permanent_warning")),
                )
                .when(!cfg!(feature = "windows-cleanup"), |body| {
                    body.child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(tr("cleanup_execution_pending")),
                    )
                })
                .child(div().text_sm().child(format!(
                    "{} · {}",
                    count,
                    atlas_core::util::display::format_size(bytes)
                )))
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(plan.temp_root.display().to_string()),
                )
                .child(div().flex_1())
                .child(
                    h_flex()
                        .gap_2()
                        .child(button(
                            "cleanup-review-back",
                            "cleanup_back",
                            busy,
                            cx,
                            |p, _, _| {
                                p.review = None;
                                p.plans.clear();
                            },
                        ))
                        .child(
                            Button::new("cleanup-execute")
                                .label(tr("cleanup_delete_reviewed"))
                                .danger()
                                .small()
                                .disabled(busy || !cfg!(feature = "windows-cleanup"))
                                .on_click(cx.listener(|p, _, _, cx| p.execute(cx))),
                        ),
                );
        } else if self.tab == Tab::Cleanup && !self.cats.is_empty() {
            content = content.child(self.categories_view(cx));
        } else if self.tab == Tab::Types {
            content = content.child(self.types_view(cx));
        } else if self.tab == Tab::Duplicates {
            content = content.child(self.duplicates_view(cx));
        } else {
            if self.tab == Tab::Map {
                content = content.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new("cleanup-up")
                                .ghost()
                                .small()
                                .tooltip(tr("cleanup_up"))
                                .disabled(self.current == self.volume || busy)
                                .child(theme_icon(AtlasIcon::ArrowUp, 16., muted, windows))
                                .on_click(cx.listener(|p, _, _, cx| p.up(cx))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(muted)
                                .child(self.current.display().to_string()),
                        )
                        .child(
                            Button::new("cleanup-treemap")
                                .ghost()
                                .small()
                                .tooltip(tr(if self.treemap {
                                    "cleanup_show_bubbles"
                                } else {
                                    "cleanup_show_treemap"
                                }))
                                .disabled(self.list_only)
                                .child(theme_icon(
                                    if self.treemap {
                                        AtlasIcon::Grid
                                    } else {
                                        AtlasIcon::Columns
                                    },
                                    17.,
                                    muted,
                                    windows,
                                ))
                                .on_click(cx.listener(|p, _, _, cx| {
                                    p.treemap = !p.treemap;
                                    p.hovered = None;
                                    p.treemap_hovered = None;
                                    p.treemap_hover_path = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("cleanup-view")
                                .ghost()
                                .small()
                                .tooltip(tr("cleanup_toggle_view"))
                                .child(theme_icon(
                                    if self.list_only {
                                        AtlasIcon::Grid
                                    } else {
                                        AtlasIcon::List
                                    },
                                    17.,
                                    muted,
                                    windows,
                                ))
                                .on_click(cx.listener(|p, _, _, cx| {
                                    p.list_only = !p.list_only;
                                    cx.notify();
                                })),
                        ),
                );
            }
            if self.tab == Tab::Cleanup && !scanning {
                let total: u64 = self.rows.iter().map(|r| r.bytes).sum();
                content = content.child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_4()
                        .p_4()
                        .rounded_xl()
                        .bg(cx.theme().accent.opacity(0.08))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .text_size(crate::ui::rem_px(30.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(atlas_core::util::display::format_size(total)),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(muted)
                                        .child(tr("cleanup_temp_hint")),
                                ),
                        )
                        .child(
                            Button::new("cleanup-hero-review")
                                .primary()
                                .flex_none()
                                .disabled(busy || self.rows.is_empty())
                                .label(tr("cleanup_review_all"))
                                .on_click(cx.listener(|p, _, _, cx| p.review_selection(cx))),
                        ),
                );
            }
            if self.tab == Tab::Cleanup {
                content = content.child(div().h_0()).child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(self.snapshot.root.display().to_string()),
                );
            }
            if self.tab == Tab::Files {
                content = content.child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(tr("cleanup_files_hint")),
                );
            }
            if let Some(message) = &self.snapshot.message {
                content = content.child(div().text_sm().text_color(muted).child(tr(message)));
            }
            if self.tab == Tab::Map && scanning {
                content = content.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .child(theme_icon(AtlasIcon::HardDrive, 40., muted, windows))
                        .child(div().text_lg().child(tr("cleanup_scanning")))
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted)
                                .child(tr("cleanup_stable_map_hint")),
                        )
                        .when_some(self.snapshot.progress, |v, progress| {
                            v.child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(format!("{:.0}%", progress * 100.)),
                            )
                        }),
                );
            } else if self.tab == Tab::Map && !self.list_only && !self.map_nodes.is_empty() {
                let map = div()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .rounded_lg()
                    .overflow_hidden()
                    .bg(cx.theme().background);
                let treemap = self.treemap_root.clone().filter(|_| self.treemap);
                let map = if let Some(root) = treemap {
                    map.child(TreemapElement {
                        panel: cx.entity().downgrade(),
                        root,
                        revision: self.treemap_revision,
                        selected: self.selected.clone(),
                        hovered: self.treemap_hovered,
                        cache: self.treemap_cache.clone(),
                        fg,
                        background: cx.theme().background,
                        dark,
                        colors: super::appearance::palette(cx),
                        other_label: tr("cleanup_other"),
                    })
                } else if self.treemap && self.treemap_pending {
                    map.flex().items_center().justify_center().child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child(tr("cleanup_treemap_computing")),
                    )
                } else {
                    map.child(BubbleMapElement {
                        panel: cx.entity().downgrade(),
                        nodes: self.map_nodes.clone(),
                        revision: self.revision,
                        enter: self.enter,
                        selected: self.selected.clone(),
                        hovered: self.hovered,
                        cache: self.map_cache.clone(),
                        fg,
                        muted,
                        dark,
                        windows,
                        colors: super::appearance::palette(cx),
                        icons: Arc::new(
                            self.map_nodes
                                .iter()
                                .map(|node| {
                                    node.path
                                        .as_ref()
                                        .map(|path| self.file_glyph(path, node.directory, cx))
                                })
                                .collect(),
                        ),
                    })
                };
                if narrow {
                    content = content.child(map);
                } else {
                    content = content.child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .items_stretch()
                            .gap_3()
                            .child(map)
                            .child(
                                v_flex()
                                    .w(px(258. * zoom))
                                    .flex_none()
                                    .min_h_0()
                                    .gap_3()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .justify_between()
                                            .px_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(tr("cleanup_contents")),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(muted)
                                                    .child(self.rows.len().to_string()),
                                            ),
                                    )
                                    .child(self.list(cx)),
                            ),
                    );
                }
                let hint = match &self.treemap_hover_path {
                    Some((path, label, size)) if self.treemap => match path {
                        Some(path) => format!("{label} · {size} · {}", path.display()),
                        None => format!("{label} · {size}"),
                    },
                    _ => tr(if self.treemap {
                        "cleanup_treemap_hint"
                    } else {
                        "cleanup_map_hint"
                    }),
                };
                content = content.child(div().text_xs().text_color(muted).truncate().child(hint));
            } else if self.rows.is_empty() {
                content = content.child(
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .child(theme_icon(
                            if scanning {
                                AtlasIcon::Search
                            } else {
                                AtlasIcon::HardDrive
                            },
                            34.,
                            muted,
                            windows,
                        ))
                        .child(div().text_sm().text_color(muted).child(tr(if scanning {
                            "cleanup_scanning"
                        } else {
                            "cleanup_empty"
                        }))),
                );
            } else {
                content = content.child(self.list(cx));
            }

            if let Some(path) = &self.selected {
                content = content.child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(muted)
                                .truncate()
                                .child(path.display().to_string()),
                        )
                        .child(button(
                            "cleanup-reveal",
                            "cleanup_reveal",
                            false,
                            cx,
                            |p, w, cx| p.reveal(w, cx),
                        ))
                        .when(self.tab == Tab::Map && !scanning, |row| {
                            row.child(button(
                                "cleanup-trash",
                                "cleanup_trash",
                                busy || self.trashing || self.network,
                                cx,
                                |p, _, cx| p.trash_selected(cx),
                            ))
                        })
                        .when(self.tab == Tab::Cleanup, |row| {
                            row.child(button(
                                "cleanup-protect",
                                "cleanup_protect",
                                busy,
                                cx,
                                |p, _, _| {
                                    if let Some(path) = p.selected.take() {
                                        p.checked.remove(&path);
                                        p.excluded.insert(path);
                                    }
                                    p.rebuild();
                                },
                            ))
                        }),
                );
            }
        }
        if let Some(notice) = &self.notice {
            content = content.child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .truncate()
                    .child(notice.clone()),
            );
        }
        if let Some(error) = &self.error {
            content = content.child(div().text_sm().text_color(cx.theme().danger).child(format!(
                "{}: {}",
                tr("cleanup_error"),
                super::user_error(error)
            )));
        }

        let status = if self.cancel_requested {
            tr("cleanup_cancelling")
        } else if self.tab == Tab::History && self.error.is_some() {
            tr("cleanup_error")
        } else if self.tab == Tab::History && self.report.is_some() {
            let report = self.report.as_ref().unwrap();
            tr(if report.cancelled {
                "cleanup_cancelled"
            } else if report.error.is_some() || report.results.iter().any(|r| r.error.is_some()) {
                "cleanup_result_partial"
            } else {
                "cleanup_result_complete"
            })
        } else {
            tr(match self.snapshot.status {
                ScanStatus::Scanning => "cleanup_scanning",
                ScanStatus::Paused => "cleanup_paused",
                ScanStatus::Complete => "cleanup_complete",
                ScanStatus::Partial => "cleanup_partial",
                ScanStatus::Cancelled => "cleanup_cancelled",
                ScanStatus::Failed => "cleanup_failed",
            })
        };
        let detail = if let Some(report) = self.report.as_ref().filter(|_| self.tab == Tab::History)
        {
            format!(
                "{} {} · {} {} · {} {}",
                report.results.len(),
                tr("cleanup_processed"),
                report.results.iter().filter(|r| r.error.is_none()).count(),
                tr("cleanup_deleted"),
                report.results.iter().filter(|r| r.error.is_some()).count(),
                tr("cleanup_rejected")
            )
        } else {
            format!(
                "{} {} · {} {} · {} {}{}",
                self.snapshot.entry_count,
                tr("cleanup_entries"),
                self.snapshot.skipped,
                tr("cleanup_skipped"),
                self.snapshot.errors,
                tr("cleanup_errors"),
                if self.snapshot.limited {
                    format!(" · {}", tr("cleanup_limited"))
                } else {
                    String::new()
                }
            )
        };
        let status_color = if self.execution.is_some() || !self.snapshot.status.terminal() {
            cx.theme().accent
        } else if self.error.is_some() || self.snapshot.status == ScanStatus::Failed {
            cx.theme().danger
        } else if self.snapshot.status == ScanStatus::Partial
            || self
                .report
                .as_ref()
                .filter(|_| self.tab == Tab::History)
                .is_some_and(|r| r.error.is_some() || r.results.iter().any(|r| r.error.is_some()))
        {
            cx.theme().warning
        } else {
            cx.theme().success
        };
        let mut footer = h_flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(border.opacity(0.60))
            .child(
                div()
                    .size(crate::ui::rem_px(6.))
                    .rounded_full()
                    .bg(status_color),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().child(if busy {
                        format!(
                            "{} · {} / {}",
                            tr(if self.cancel_requested {
                                "cleanup_cancelling"
                            } else {
                                "cleanup_working"
                            }),
                            self.progress.0,
                            self.progress.1
                        )
                    } else {
                        status
                    }))
                    .child(div().text_xs().text_color(muted).child(detail)),
            );
        if self.execution.is_some() {
            footer = footer.child(button(
                "cleanup-cancel-execute",
                "cleanup_stop",
                self.cancel_requested,
                cx,
                |p, _, _| {
                    p.cancel_requested = true;
                    if let Some(cancel) = &p.execution {
                        cancel.store(true, Ordering::Release);
                    }
                },
            ));
        } else if scanning && self.tab != Tab::History {
            footer = footer
                .child(button(
                    "cleanup-pause",
                    if paused {
                        "cleanup_resume"
                    } else {
                        "cleanup_pause"
                    },
                    busy || self.snapshot.backend == atlas_model::cleanup::ScanBackend::NtfsMft,
                    cx,
                    move |p, _, _| {
                        if let Some(job) = &p.job {
                            job.set_paused(!paused);
                        }
                    },
                ))
                .child(button(
                    "cleanup-stop",
                    "cleanup_stop",
                    busy || self.cancel_requested,
                    cx,
                    |p, _, _| {
                        p.cancel_requested = true;
                        if let Some(job) = &p.job {
                            job.cancel();
                        }
                    },
                ));
        } else if self.tab == Tab::Cleanup && self.review.is_none() && self.cats.is_empty() {
            footer = footer
                .child(button(
                    "cleanup-select-all",
                    "cleanup_select_all",
                    busy || self.rows.is_empty(),
                    cx,
                    |p, _, _| {
                        if p.checked.len() == p.rows.len() {
                            p.checked.clear();
                        } else {
                            p.checked = p.rows.iter().map(|r| r.path.clone()).collect();
                        }
                    },
                ))
                .child(button(
                    "cleanup-review",
                    if self.checked.is_empty() {
                        "cleanup_review_all"
                    } else {
                        "cleanup_review"
                    },
                    busy || self.rows.is_empty(),
                    cx,
                    |p, _, cx| p.review_selection(cx),
                ));
        }
        v_flex()
            .id("cleanup-panel")
            .track_focus(&self.focus)
            .w(px(width))
            .h(px(height))
            .min_h_0()
            .rounded_xl()
            .bg(bg)
            .shadow_lg()
            .opacity(opening)
            .relative()
            .top(px((1.0 - opening) * 16.0 * zoom))
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
                h_flex().size_full().items_stretch().child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .child(header)
                        .child(content)
                        .child(footer),
                ),
            )
    }
}

impl CleanupPanel {
    fn list(&self, cx: &mut Context<Self>) -> AnyElement {
        let rows = if let Some(plan) = &self.review {
            Arc::new(
                plan.candidates
                    .iter()
                    .map(|c| Row {
                        path: c.path.clone(),
                        name: c
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        size: atlas_core::util::display::format_size(c.identity.bytes),
                        directory: false,
                        bytes: c.identity.bytes,
                    })
                    .collect(),
            )
        } else {
            self.rows.clone()
        };
        let panel = cx.entity().downgrade();
        let cleanup = self.tab == Tab::Cleanup;
        let review = self.review.is_some();
        uniform_list("cleanup-results", rows.len(), move |range, _, cx| {
            let Some(entity) = panel.upgrade() else {
                return Vec::new();
            };
            entity.update(cx, |panel, cx| {
                range
                    .map(|index| {
                        let row = &rows[index];
                        let path = row.path.clone();
                        let selected = panel.selected.as_ref() == Some(&row.path);
                        let checked = panel.checked.contains(&path);
                        let fg = cx.theme().foreground;
                        let muted = super::muted(cx);
                        let accent = cx.theme().accent;
                        let view = cx.entity().downgrade();
                        let directory = row.directory;
                        let ratio = (row.bytes as f64 / panel.snapshot.bytes.max(1) as f64)
                            .clamp(0., 1.) as f32;
                        let tint = super::appearance::file_color(
                            Some(&path),
                            directory,
                            &super::appearance::palette(cx),
                        );
                        let glyph = panel.file_art(&path, directory, 28., cx);
                        h_flex()
                            .id(index)
                            .h(crate::ui::rem_px(66.))
                            .w_full()
                            .min_w_0()
                            .items_center()
                            .px_2()
                            .gap_3()
                            .rounded_md()
                            .bg(if selected {
                                accent.opacity(0.12)
                            } else {
                                gpui::transparent_black()
                            })
                            .hover(move |s| s.bg(fg.opacity(0.05)))
                            .cursor_pointer()
                            .when(cleanup && !review, |row| {
                                row.child(
                                    Checkbox::new(("cleanup-check", index))
                                        .checked(checked)
                                        .small()
                                        .tab_stop(false)
                                        .disabled(panel.execution.is_some() || panel.preparing),
                                )
                            })
                            .child(glyph)
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .flex_1()
                                    .gap_1()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(row.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child(row.size.clone()),
                                            ),
                                    )
                                    .when(cleanup || panel.tab == Tab::Files, |v| {
                                        v.child(
                                            div()
                                                .text_xs()
                                                .truncate()
                                                .text_color(muted)
                                                .child(row.path.display().to_string()),
                                        )
                                    })
                                    .when(!cleanup && panel.tab != Tab::Files, |v| {
                                        v.child(
                                            h_flex()
                                                .w_full()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .h(crate::ui::rem_px(2.))
                                                        .rounded_full()
                                                        .bg(fg.opacity(0.06))
                                                        .child(
                                                            div()
                                                                .h_full()
                                                                .w(relative(ratio))
                                                                .rounded_full()
                                                                .bg(tint.opacity(0.72)),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(muted)
                                                        .child(format!("{:.1}%", ratio * 100.)),
                                                ),
                                        )
                                    }),
                            )
                            .on_mouse_down(MouseButton::Left, move |event, w, cx| {
                                let _ = view.update(cx, |panel, cx| {
                                    w.focus(&panel.focus, cx);
                                    panel.cursor = index;
                                    panel.selected = Some(path.clone());
                                    if cleanup && !review {
                                        panel.toggle(path.clone());
                                    } else if event.click_count >= 2 && directory {
                                        panel.navigate(path.clone(), cx);
                                    }
                                    cx.notify();
                                });
                                cx.stop_propagation();
                            })
                            .into_any_element()
                    })
                    .collect()
            })
        })
        .track_scroll(&self.scroll)
        .flex_1()
        .min_h_0()
        .into_any_element()
    }
    /// Keyboard for the open panel. Returns true when the key was consumed; the
    /// window root calls this first so the file list behind never sees it.
    pub(crate) fn handle_key(
        &mut self,
        event: &KeyDownEvent,
        w: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;

        if key == "escape" {
            if self.review.is_some() {
                self.review = None;
                self.plans.clear();
                cx.notify();
            } else {
                self.close(w, cx);
            }
            return true;
        }
        if mods.alt && matches!(key, "left" | "right") {
            self.go_history(key == "left", cx);
            return true;
        }
        if mods.control || mods.platform || mods.alt {
            return false;
        }
        let navigation = matches!(
            key,
            "left"
                | "right"
                | "up"
                | "down"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
                | "enter"
                | "space"
                | "backspace"
                | "tab"
                | "delete"
        );
        if self.execution.is_some() || self.preparing || self.review.is_some() {
            return navigation;
        }
        if self.tab == Tab::Map
            && !self.list_only
            && !self.treemap
            && !self.map_nodes.is_empty()
            && matches!(key, "left" | "right" | "up" | "down")
        {
            let (dx, dy) = match key {
                "left" => (-1., 0.),
                "right" => (1., 0.),
                "up" => (0., -1.),
                _ => (0., 1.),
            };
            self.arrow(dx, dy);
            self.scroll
                .scroll_to_item(self.cursor, ScrollStrategy::Nearest);
            cx.notify();
            return true;
        }
        let rows = self.rows.len().saturating_sub(1);
        let mut handled = true;
        match key {
            "up" | "left" => self.cursor = self.cursor.saturating_sub(1),
            "down" | "right" => self.cursor = (self.cursor + 1).min(rows),
            "pageup" => self.cursor = self.cursor.saturating_sub(8),
            "pagedown" => self.cursor = (self.cursor + 8).min(rows),
            "home" => self.cursor = 0,
            "end" => self.cursor = rows,
            "backspace" => self.up(cx),
            "enter" => {
                if let Some(row) = self.rows.get(self.cursor).cloned() {
                    if row.directory && self.tab != Tab::Cleanup {
                        self.navigate(row.path, cx);
                    } else {
                        self.selected = Some(row.path);
                    }
                }
            }
            "delete" if self.tab == Tab::Map && !self.network => self.trash_selected(cx),
            "space" if self.tab == Tab::Cleanup && self.cats.is_empty() => {
                if let Some(row) = self.rows.get(self.cursor) {
                    self.toggle(row.path.clone());
                }
            }
            _ => handled = navigation,
        }
        if handled && !matches!(key, "backspace" | "delete" | "tab" | "space") {
            if let Some(row) = self.rows.get(self.cursor) {
                self.selected = Some(row.path.clone());
            }
            self.scroll
                .scroll_to_item(self.cursor, ScrollStrategy::Nearest);
        }
        cx.notify();
        handled
    }
    fn categories_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = super::muted(cx);
        let fg = cx.theme().foreground;
        let accent = cx.theme().accent;
        let busy = self.execution.is_some() || self.preparing;
        let scanning = self.cat_pending > 0;
        let format = atlas_core::util::display::format_size;
        let total: u64 = self
            .cats
            .iter()
            .filter(|c| c.enabled)
            .map(|c| c.bytes())
            .sum();
        let mut list = v_flex().w_full().gap_1();
        for (i, cat) in self.cats.iter().enumerate() {
            let detail = if cat.ready() {
                format!("{} · {}", format(cat.bytes()), cat.files())
            } else {
                tr("cleanup_cat_scanning")
            };
            list = list.child(
                h_flex()
                    .id(("cleanup-cat-row", i))
                    .w_full()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_3()
                    .rounded_lg()
                    .cursor_pointer()
                    .when(cat.enabled, |row| row.bg(accent.opacity(0.07)))
                    .hover(|row| row.bg(fg.opacity(0.05)))
                    .child(
                        Checkbox::new(("cleanup-cat-check", i))
                            .checked(cat.enabled)
                            .small()
                            .tab_stop(false)
                            .disabled(busy),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(crate::ui::rem_px(15.))
                            .child(tr(cat.key)),
                    )
                    .child(div().text_sm().text_color(muted).child(detail))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if !(p.execution.is_some() || p.preparing) {
                            p.cats[i].enabled = !p.cats[i].enabled;
                            cx.notify();
                        }
                    })),
            );
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_4()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_4()
                    .p_4()
                    .rounded_xl()
                    .bg(accent.opacity(0.08))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(crate::ui::rem_px(34.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if scanning {
                                        tr("cleanup_cat_scanning")
                                    } else {
                                        format(total)
                                    }),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted)
                                    .child(tr("cleanup_cat_hint")),
                            ),
                    )
                    .child(
                        Button::new("cleanup-clean")
                            .primary()
                            .flex_none()
                            .disabled(busy || scanning || total == 0)
                            .label(tr("cleanup_clean_action"))
                            .on_click(cx.listener(|p, _, _, cx| p.clean(cx))),
                    ),
            )
            .child(list)
            .into_any_element()
    }
    pub(super) fn reveal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = &self.selected else {
            return;
        };
        let path = if self.rows.iter().any(|r| r.path == *selected && r.directory) {
            selected.clone()
        } else {
            selected.parent().unwrap_or(selected).to_owned()
        };
        let _ = self.parent.update(cx, |app, cx| {
            let tile = app.state.focused_tile;
            app.navigate(tile, path);
            cx.notify();
        });
        self.close(window, cx);
    }
    fn history(&self, cx: &mut Context<Self>) -> AnyElement {
        let muted = super::muted(cx);
        if let Some(report) = &self.report {
            let deleted = report.results.iter().filter(|r| r.error.is_none()).count();
            let bytes = report.results.iter().map(|r| r.deleted_bytes).sum::<u64>();
            let mut body = v_flex()
                .flex_1()
                .min_h_0()
                .gap_3()
                .child(
                    h_flex()
                        .items_baseline()
                        .gap_3()
                        .child(
                            div()
                                .text_size(crate::ui::rem_px(26.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(atlas_core::util::display::format_size(bytes)),
                        )
                        .child(div().text_sm().text_color(muted).child(format!(
                            "{} · {deleted} / {}",
                            tr("cleanup_deleted"),
                            report.results.len()
                        ))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(tr("cleanup_report_hint")),
                );
            if let Some(error) = &report.error {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                );
            }
            if let (Some(before), Some(after)) = (report.free_before, report.free_after) {
                body = body.child(div().text_sm().child(format!(
                    "{}: {} → {}",
                    tr("cleanup_free_change"),
                    atlas_core::util::display::format_size(before),
                    atlas_core::util::display::format_size(after)
                )));
            }
            let results = Arc::new(report.results.clone());
            let parent = self.parent.clone();
            let success = cx.theme().success;
            let warning = cx.theme().warning;
            let border = cx.theme().border;
            body.child(
                uniform_list(
                    "cleanup-report-results",
                    results.len(),
                    move |range, _, cx| {
                        range
                            .map(|i| {
                                let r = &results[i];
                                let reference =
                                    crate::app::App::file_icon_ref_for_path(r.path.clone(), false);
                                let glyph =
                                    parent.upgrade().map(|p| p.read(cx).file_icon(&reference));
                                h_flex()
                                    .h(crate::ui::rem_px(78.))
                                    .gap_3()
                                    .w_full()
                                    .min_w_0()
                                    .border_b_1()
                                    .border_color(border.opacity(0.5))
                                    .when_some(glyph, |v, icon| {
                                        v.child(
                                            div()
                                                .size(crate::ui::rem_px(36.))
                                                .flex_none()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded_md()
                                                .bg(if !cx.theme().is_dark() {
                                                    cx.theme().foreground.opacity(0.55)
                                                } else {
                                                    transparent_black()
                                                })
                                                .child(icon.element(28., muted)),
                                        )
                                    })
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .truncate()
                                                    .child(
                                                        r.path
                                                            .file_name()
                                                            .unwrap_or_default()
                                                            .to_string_lossy()
                                                            .into_owned(),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .truncate()
                                                    .child(r.path.display().to_string()),
                                            )
                                            .child(
                                                div().text_xs().text_color(muted).truncate().child(
                                                    r.error
                                                        .as_deref()
                                                        .map(super::user_error)
                                                        .unwrap_or_else(|| tr("cleanup_deleted")),
                                                ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .size(crate::ui::rem_px(6.))
                                            .rounded_full()
                                            .bg(if r.error.is_some() { warning } else { success }),
                                    )
                                    .into_any_element()
                            })
                            .collect()
                    },
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(theme_icon(
                    AtlasIcon::ClockCounterClockwise,
                    40.,
                    muted,
                    self.windows_icons(cx),
                ))
                .child(
                    div()
                        .text_sm()
                        .text_color(muted)
                        .child(tr("cleanup_no_history")),
                )
                .into_any_element()
        }
    }
}
