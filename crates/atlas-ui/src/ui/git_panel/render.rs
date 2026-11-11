//! Drawing of the Git panel: header, tabs, the list of the current tab on the
//! left, the selection's details and diff on the right, and the status line.
use super::*;
use crate::ui::rem_px;
use atlas_model::git::{Branch, RefLabel};

#[derive(Clone, Copy)]
struct Colors {
    fg: Hsla,
    muted: Hsla,
    border: Hsla,
    primary: Hsla,
    success: Hsla,
    warning: Hsla,
    danger: Hsla,
    info: Hsla,
    bg: Hsla,
    graph: [Hsla; 8],
}

fn colors(cx: &App) -> Colors {
    let t = cx.theme();
    let mut bg = t.popover;
    bg.alpha = 1.;
    Colors {
        fg: t.foreground,
        muted: t.muted_foreground,
        border: t.border,
        primary: t.primary,
        success: t.success,
        warning: t.warning,
        danger: t.danger,
        info: t.info,
        bg,
        graph: [
            t.chart_1, t.chart_2, t.chart_3, t.chart_4, t.chart_5, t.info, t.success, t.warning,
        ],
    }
}

fn kind_color(kind: ChangeKind, c: &Colors) -> Hsla {
    match kind {
        ChangeKind::Added | ChangeKind::Untracked => c.success,
        ChangeKind::Deleted => c.danger,
        ChangeKind::Conflicted => c.warning,
        ChangeKind::Renamed | ChangeKind::Copied => c.info,
        ChangeKind::Modified | ChangeKind::TypeChanged => c.primary,
    }
}

fn badge(text: &str, color: Hsla) -> Div {
    div()
        .flex_none()
        .w(rem_px(16.))
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(color)
        .child(text.to_owned())
}

fn section_header(text: String, c: &Colors) -> AnyElement {
    div()
        .h(rem_px(ROW_H))
        .px_2()
        .flex()
        .items_end()
        .pb_1()
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c.muted)
        .child(text)
        .into_any_element()
}

fn centered(content: impl IntoElement) -> AnyElement {
    v_flex()
        .flex_1()
        .min_h_0()
        .items_center()
        .justify_center()
        .gap_3()
        .p_6()
        .child(content)
        .into_any_element()
}

/// `name/dir` split so the file name reads first.
fn split_path(path: &str) -> (String, String) {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((dir, name)) => (
            format!("{name}{}", if path.ends_with('/') { "/" } else { "" }),
            dir.to_owned(),
        ),
        None => (path.to_owned(), String::new()),
    }
}

fn ref_chips(refs: &[RefLabel], c: &Colors) -> Vec<AnyElement> {
    let mut chips = Vec::new();
    let mut head_next = false;
    for label in refs {
        if label.kind == RefKind::Head {
            head_next = true;
            if !refs.iter().any(|r| r.kind == RefKind::Branch) {
                chips.push(chip("HEAD", c.primary, true));
            }
            continue;
        }
        let (color, current) = match label.kind {
            RefKind::Branch => (if head_next { c.primary } else { c.success }, head_next),
            RefKind::Remote => (c.info, false),
            RefKind::Tag => (c.warning, false),
            RefKind::Head => unreachable!(),
        };
        if label.kind == RefKind::Branch {
            head_next = false;
        }
        chips.push(chip(&label.name, color, current));
    }
    chips
}

fn chip(text: &str, color: Hsla, strong: bool) -> AnyElement {
    div()
        .flex_none()
        .max_w(rem_px(160.))
        .truncate()
        .px_1()
        .rounded_sm()
        .text_xs()
        .bg(color.opacity(if strong { 0.28 } else { 0.16 }))
        .text_color(color)
        .when(strong, |d| d.font_weight(FontWeight::SEMIBOLD))
        .child(text.to_owned())
        .into_any_element()
}

/// One row of the commit graph, drawn with paths.
fn graph_cell(row: Option<&GraphRow>, lanes: u16, palette: [Hsla; 8]) -> AnyElement {
    let row = row.cloned().unwrap_or_default();
    let lanes = lanes.clamp(1, 24);
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let lane_w = f32::from(bounds.size.width) / f32::from(lanes);
            let x = |lane: u16| bounds.origin.x + px(lane_w * (f32::from(lane) + 0.5));
            let top = bounds.origin.y;
            let bottom = top + bounds.size.height;
            let mid = top + bounds.size.height / 2.;
            let stroke = px((lane_w * 0.15).clamp(1.2, 2.2));
            let color_of = |index: u16| palette[usize::from(index) % palette.len()];
            for line in &row.lines {
                let (from, to) = match line.span {
                    GraphSpan::Full => (point(x(line.from), top), point(x(line.from), bottom)),
                    GraphSpan::Top => (point(x(line.from), top), point(x(line.to), mid)),
                    GraphSpan::Bottom => (point(x(line.from), mid), point(x(line.to), bottom)),
                };
                let mut path = PathBuilder::stroke(stroke);
                path.move_to(from);
                if from.x == to.x {
                    path.line_to(to);
                } else {
                    let half = (from.y + to.y) / 2.;
                    path.cubic_bezier_to(to, point(from.x, half), point(to.x, half));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color_of(line.color));
                }
            }
            let radius = px((lane_w * 0.3).clamp(3., 5.));
            let center = point(x(row.lane), mid);
            window.paint_quad(
                fill(
                    Bounds::new(
                        point(center.x - radius, center.y - radius),
                        size(radius * 2., radius * 2.),
                    ),
                    color_of(row.lane),
                )
                .corner_radii(radius),
            );
        },
    )
    .flex_none()
    .w(rem_px(LANE_W * f32::from(lanes) + 4.))
    .h_full()
    .into_any_element()
}

impl GitPanel {
    fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// A small action button that sends `action` through [`GitPanel::request`].
    fn action_button(
        &self,
        id: impl Into<ElementId>,
        label: String,
        action: GitAction,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .small()
            .label(label)
            .disabled(self.busy())
            .on_click(cx.listener(move |p, _, _, cx| p.request(action.clone(), cx)))
    }

    fn header(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let ready = matches!(self.repo, Repo::Ready(_));
        let head = &self.overview.status.head;
        let name = self
            .root()
            .as_deref()
            .unwrap_or(&self.dir)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.dir.display().to_string());
        let mut branch = h_flex().gap_2().items_center().text_sm();
        if ready {
            branch = branch.child(
                div()
                    .px_2()
                    .rounded_md()
                    .bg(c.primary.opacity(0.14))
                    .text_color(c.primary)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(head.label()),
            );
            if let Some(upstream) = &head.upstream {
                branch = branch.child(div().text_color(c.muted).child(format!(
                    "→ {upstream}{}{}",
                    if head.ahead > 0 {
                        format!("  ↑{}", head.ahead)
                    } else {
                        String::new()
                    },
                    if head.behind > 0 {
                        format!("  ↓{}", head.behind)
                    } else {
                        String::new()
                    },
                )));
            }
        }
        let busy = self.busy();
        let has_remote = !self.overview.remotes.is_empty();
        let push = self.push_action(false);
        let force_push = self.push_action(true);
        let weak = cx.entity().downgrade();
        let more = Button::new("git-more")
            .ghost()
            .small()
            .icon(icon(AtlasIcon::CaretDown))
            .tooltip(tr("git_more_actions"))
            .disabled(busy || !ready)
            .dropdown_menu(move |menu, _, _| {
                let item = |label: String, action: Option<GitAction>| {
                    let weak = weak.clone();
                    let enabled = action.is_some();
                    PopupMenuItem::new(label)
                        .disabled(!enabled)
                        .on_click(move |_, _, cx| {
                            if let Some(action) = action.clone() {
                                let _ = weak.update(cx, |p, cx| p.request(action, cx));
                            }
                        })
                };
                menu.item(item(
                    tr("git_pull_rebase"),
                    has_remote.then_some(GitAction::Pull(PullMode::Rebase)),
                ))
                .item(item(
                    tr("git_pull_ff_only"),
                    has_remote.then_some(GitAction::Pull(PullMode::FastForwardOnly)),
                ))
                .item(item(
                    tr("git_fetch_prune"),
                    has_remote.then_some(GitAction::Fetch {
                        remote: None,
                        prune: true,
                    }),
                ))
                .separator()
                .item(item(tr("git_force_push"), force_push.clone()))
                .item(item(
                    tr("git_push_tags"),
                    has_remote.then_some(GitAction::PushTags),
                ))
            });
        h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_5()
            .pt_4()
            .pb_3()
            .child(
                Icon::empty()
                    .path(AtlasIcon::GitBranch.path())
                    .size(rem_px(22.))
                    .text_color(c.primary),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(rem_px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(name),
            )
            .child(branch)
            .child(div().flex_1())
            .child(
                Button::new("git-fetch")
                    .small()
                    .icon(icon(AtlasIcon::TrayArrowDown))
                    .label(tr("git_fetch"))
                    .disabled(busy || !ready || !has_remote)
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.request(
                            GitAction::Fetch {
                                remote: None,
                                prune: false,
                            },
                            cx,
                        );
                    })),
            )
            .child(
                Button::new("git-pull")
                    .small()
                    .icon(icon(AtlasIcon::ArrowDown))
                    .label(tr("git_pull"))
                    .disabled(busy || !ready || head.upstream.is_none())
                    .on_click(cx.listener(|p, _, _, cx| {
                        p.request(GitAction::Pull(PullMode::Default), cx);
                    })),
            )
            .child(
                Button::new("git-push")
                    .small()
                    .icon(icon(AtlasIcon::ArrowUp))
                    .label(if head.upstream.is_none() && push.is_some() {
                        tr("git_publish")
                    } else {
                        tr("git_push")
                    })
                    .disabled(busy || push.is_none())
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if let Some(action) = push.clone() {
                            p.request(action, cx);
                        }
                    })),
            )
            .child(more)
            .child(
                Button::new("git-refresh")
                    .ghost()
                    .small()
                    .icon(icon(AtlasIcon::ArrowClockwise))
                    .tooltip(tr("git_refresh"))
                    .on_click(cx.listener(|p, _, _, cx| p.reload(cx))),
            )
            .child(
                Button::new("git-close")
                    .ghost()
                    .small()
                    .label(tr("cleanup_close"))
                    .on_click(cx.listener(|p, _, w, cx| p.close(w, cx))),
            )
            .into_any_element()
    }

    fn tabs(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let o = &self.overview;
        let count = |tab: Tab| match tab {
            Tab::Changes => Some(o.status.changes.len()),
            Tab::History => None,
            Tab::Branches => Some(o.branches.iter().filter(|b| !b.remote).count()),
            Tab::Stashes => Some(o.stashes.len()),
            Tab::Tags => Some(o.tags.len()),
            Tab::Remotes => Some(o.remotes.len()),
        };
        let mut row = h_flex()
            .px_4()
            .pb_2()
            .gap_1()
            .border_b_1()
            .border_color(c.border.opacity(0.6));
        for tab in Tab::ALL {
            let active = self.tab == tab;
            let label = match count(tab) {
                Some(n) => format!("{} · {n}", tr(tab.key())),
                None => tr(tab.key()),
            };
            row = row.child(
                Button::new(SharedString::from(format!("git-tab-{}", tab.key())))
                    .ghost()
                    .small()
                    .bg(if active {
                        c.primary.opacity(0.16)
                    } else {
                        transparent_black()
                    })
                    .label(label)
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if p.tab != tab {
                            p.tab = tab;
                            p.list_scroll = UniformListScrollHandle::new();
                            p.select(Selection::None, cx);
                            cx.notify();
                        }
                    })),
            );
        }
        row.into_any_element()
    }

    /// A merge, rebase… waiting for the user: continue, skip or abort.
    fn operation_banner(&self, c: &Colors, cx: &mut Context<Self>) -> Option<AnyElement> {
        let op = self.overview.status.head.operation?;
        let key = match op {
            RepoOperation::Merge => "git_op_merge",
            RepoOperation::Rebase => "git_op_rebase",
            RepoOperation::CherryPick => "git_op_cherry_pick",
            RepoOperation::Revert => "git_op_revert",
            RepoOperation::Bisect => "git_op_bisect",
        };
        let conflicts = self.overview.status.head.conflicts;
        let mut text = tr(key);
        if conflicts > 0 {
            text.push_str(" · ");
            text.push_str(&rust_i18n::t!("git_conflicts_left", count = conflicts));
        }
        let mut bar = h_flex()
            .mx_4()
            .mt_2()
            .px_3()
            .py_2()
            .gap_2()
            .items_center()
            .rounded_md()
            .bg(c.warning.opacity(0.14))
            .child(div().flex_1().text_sm().text_color(c.warning).child(text))
            .child(
                self.action_button(
                    "git-op-continue",
                    tr("git_continue"),
                    GitAction::Continue(op),
                    cx,
                )
                .primary()
                .disabled(self.busy() || conflicts > 0),
            );
        if matches!(
            op,
            RepoOperation::Rebase | RepoOperation::CherryPick | RepoOperation::Bisect
        ) {
            bar = bar.child(self.action_button(
                "git-op-skip",
                tr("git_skip"),
                GitAction::Skip(op),
                cx,
            ));
        }
        bar = bar.child(
            self.action_button("git-op-abort", tr("git_abort"), GitAction::Abort(op), cx)
                .danger(),
        );
        Some(bar.into_any_element())
    }

    // ── Left column ────────────────────────────────────────────────────────

    fn toolbar(c: &Colors) -> Div {
        h_flex()
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .items_center()
            .border_b_1()
            .border_color(c.border.opacity(0.5))
    }

    fn changes_list(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let changes = &self.overview.status.changes;
        let staged = changes
            .iter()
            .filter(|ch| ch.staged.is_some() && !ch.conflicted())
            .count();
        let unstaged = changes
            .iter()
            .filter(|ch| ch.unstaged.is_some() && !ch.conflicted())
            .count();
        let busy = self.busy();
        let commit_box = v_flex()
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(c.border.opacity(0.5))
            .child(crate::ui::text_input::bordered(&self.commit_input))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Checkbox::new("git-amend")
                            .label(tr("git_amend"))
                            .checked(self.amend)
                            .small()
                            .on_click(cx.listener(|p, checked: &bool, window, cx| {
                                p.set_amend(*checked, window, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        self.action_button(
                            "git-stage-all",
                            tr("git_stage_all"),
                            GitAction::StageAll,
                            cx,
                        )
                        .ghost()
                        .disabled(busy || unstaged == 0),
                    )
                    .child(
                        self.action_button(
                            "git-unstage-all",
                            tr("git_unstage_all"),
                            GitAction::UnstageAll,
                            cx,
                        )
                        .ghost()
                        .disabled(busy || staged == 0),
                    )
                    .child(
                        Button::new("git-commit")
                            .primary()
                            .small()
                            .icon(icon(AtlasIcon::Check))
                            .label(if self.amend {
                                tr("git_amend_commit")
                            } else {
                                rust_i18n::t!("git_commit_count", count = staged).into_owned()
                            })
                            .tooltip(tr("git_commit_hint"))
                            .disabled(busy || (staged == 0 && !self.amend))
                            .on_click(cx.listener(|p, _, w, cx| p.commit(w, cx))),
                    ),
            );
        let rows = Arc::new(self.change_rows());
        let entity = cx.entity();
        let list = uniform_list("git-changes", rows.len(), move |range, _, cx| {
            range
                .map(|i| Self::change_row(&entity, rows[i], i, cx))
                .collect()
        })
        .track_scroll(&self.list_scroll)
        .flex_1()
        .min_h_0()
        .px_1();
        v_flex()
            .size_full()
            .child(commit_box)
            .child(if changes.is_empty() {
                centered(div().text_sm().text_color(c.muted).child(tr("git_clean")))
            } else {
                list.into_any_element()
            })
            .into_any_element()
    }

    fn change_row(panel: &Entity<Self>, row: ChangeRow, ix: usize, cx: &mut App) -> AnyElement {
        let c = colors(cx);
        let this = panel.read(cx);
        let (staged, index) = match row {
            ChangeRow::Header(key, count) => {
                return section_header(format!("{} · {count}", tr(key)), &c);
            }
            ChangeRow::File { staged, index } => (staged, index),
        };
        let Some(change) = this.overview.status.changes.get(index) else {
            return div().into_any_element();
        };
        let kind = if staged {
            change.staged
        } else {
            change.unstaged
        }
        .unwrap_or(ChangeKind::Modified);
        let path = change.path.clone();
        let selection = Selection::Change {
            staged,
            path: path.clone(),
        };
        let selected = this.selection == selection;
        let (name, dir) = split_path(&path);
        let busy = this.running.is_some();
        let conflicted = change.conflicted();
        let untracked = change.untracked();
        let weak = panel.downgrade();
        let send = move |action: GitAction| {
            let weak = weak.clone();
            move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let _ = weak.update(cx, |p, cx| p.request(action.clone(), cx));
            }
        };
        let mut buttons = h_flex().flex_none().gap_0p5();
        if conflicted {
            buttons = buttons
                .child(
                    Button::new(("git-ours", ix))
                        .ghost()
                        .xsmall()
                        .label(tr("git_use_ours"))
                        .disabled(busy)
                        .on_click(send(GitAction::Resolve {
                            paths: vec![path.clone()],
                            ours: true,
                        })),
                )
                .child(
                    Button::new(("git-theirs", ix))
                        .ghost()
                        .xsmall()
                        .label(tr("git_use_theirs"))
                        .disabled(busy)
                        .on_click(send(GitAction::Resolve {
                            paths: vec![path.clone()],
                            ours: false,
                        })),
                )
                .child(
                    Button::new(("git-resolved", ix))
                        .ghost()
                        .xsmall()
                        .icon(icon(AtlasIcon::Check))
                        .tooltip(tr("git_mark_resolved"))
                        .disabled(busy)
                        .on_click(send(GitAction::Stage(vec![path.clone()]))),
                );
        } else if staged {
            buttons = buttons.child(
                Button::new(("git-unstage", ix))
                    .ghost()
                    .xsmall()
                    .icon(icon(AtlasIcon::Minus))
                    .tooltip(tr("git_unstage"))
                    .disabled(busy)
                    .on_click(send(GitAction::Unstage(vec![path.clone()]))),
            );
        } else {
            let discard = if untracked {
                GitAction::Discard {
                    tracked: Vec::new(),
                    untracked: vec![path.clone()],
                }
            } else {
                GitAction::Discard {
                    tracked: vec![path.clone()],
                    untracked: Vec::new(),
                }
            };
            buttons = buttons
                .child(
                    Button::new(("git-discard", ix))
                        .ghost()
                        .xsmall()
                        .icon(icon(AtlasIcon::ArrowCounterClockwise))
                        .tooltip(tr(if untracked {
                            "git_discard_untracked"
                        } else {
                            "git_discard"
                        }))
                        .disabled(busy)
                        .on_click(send(discard)),
                )
                .child(
                    Button::new(("git-stage", ix))
                        .ghost()
                        .xsmall()
                        .icon(icon(AtlasIcon::Plus))
                        .tooltip(tr("git_stage"))
                        .disabled(busy)
                        .on_click(send(GitAction::Stage(vec![path.clone()]))),
                );
        }
        let weak = panel.downgrade();
        h_flex()
            .id(("git-change", ix))
            .w_full()
            .h(rem_px(ROW_H))
            .px_2()
            .gap_2()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(c.fg.opacity(0.08)))
            .hover(|s| s.bg(c.fg.opacity(0.05)))
            .on_click(move |_, _, cx| {
                let _ = weak.update(cx, |p, cx| p.select(selection.clone(), cx));
            })
            .child(badge(kind.letter(), kind_color(kind, &c)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_none()
                            .max_w(relative(0.7))
                            .truncate()
                            .text_sm()
                            .child(name),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(c.muted)
                            .child(dir),
                    ),
            )
            .child(buttons)
            .into_any_element()
    }

    fn history_list(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let history = self.history.clone();
        let toolbar = Self::toolbar(c)
            .child(
                Checkbox::new("git-all-branches")
                    .label(tr("git_all_branches"))
                    .checked(self.history_all)
                    .small()
                    .on_click(cx.listener(|p, checked: &bool, _, cx| {
                        p.history_all = *checked;
                        p.load_history(cx);
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(c.muted)
                    .child(if self.history_loading {
                        tr("git_loading")
                    } else {
                        rust_i18n::t!("git_commit_total", count = history.commits.len())
                            .into_owned()
                    }),
            )
            .when(history.truncated, |bar| {
                bar.child(
                    Button::new("git-more-history")
                        .ghost()
                        .small()
                        .label(tr("git_load_more"))
                        .disabled(self.history_loading)
                        .on_click(cx.listener(|p, _, _, cx| {
                            p.history_limit += HISTORY_PAGE;
                            p.load_history(cx);
                            cx.notify();
                        })),
                )
            });
        let entity = cx.entity();
        let count = history.commits.len();
        let body = if count == 0 {
            centered(
                div()
                    .text_sm()
                    .text_color(c.muted)
                    .child(if self.history_loading {
                        tr("git_loading")
                    } else {
                        tr("git_no_commits")
                    }),
            )
        } else {
            uniform_list("git-history", count, move |range, _, cx| {
                range.map(|i| Self::history_row(&entity, i, cx)).collect()
            })
            .track_scroll(&self.list_scroll)
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        v_flex()
            .size_full()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }

    fn history_row(panel: &Entity<Self>, ix: usize, cx: &mut App) -> AnyElement {
        let c = colors(cx);
        let this = panel.read(cx);
        let Some(commit) = this.history.commits.get(ix) else {
            return div().into_any_element();
        };
        let selected = matches!(&this.selection, Selection::Commit { id, .. } if *id == commit.id);
        let id = commit.id.clone();
        let weak = panel.downgrade();
        h_flex()
            .id(("git-commit", ix))
            .w_full()
            .h(rem_px(ROW_H))
            .pr_2()
            .gap_2()
            .items_center()
            .cursor_pointer()
            .when(selected, |d| d.bg(c.fg.opacity(0.08)))
            .hover(|s| s.bg(c.fg.opacity(0.05)))
            .on_click(move |_, _, cx| {
                let _ = weak.update(cx, |p, cx| {
                    p.select(
                        Selection::Commit {
                            id: id.clone(),
                            file: None,
                        },
                        cx,
                    );
                });
            })
            .child(graph_cell(
                this.history.graph.get(ix),
                this.graph_width,
                c.graph,
            ))
            .children(ref_chips(&commit.refs, &c))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .child(commit.subject.clone()),
            )
            .child(
                div()
                    .w(rem_px(96.))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .text_color(c.muted)
                    .child(commit.author.clone()),
            )
            .child(
                div()
                    .w(rem_px(104.))
                    .flex_none()
                    .truncate()
                    .text_xs()
                    .text_color(c.muted)
                    .child(date(commit.time)),
            )
            .into_any_element()
    }

    fn branches_list(&self, c: &Colors, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let toolbar = Self::toolbar(c)
            .child(
                Button::new("git-new-branch")
                    .small()
                    .icon(icon(AtlasIcon::Plus))
                    .label(tr("git_new_branch"))
                    .disabled(self.busy())
                    .on_click(cx.listener(|p, _, w, cx| {
                        p.open_prompt(Prompt::NewBranch { start: None }, w, cx);
                    })),
            )
            .child(div().flex_1());
        let branches = &self.overview.branches;
        let local: Vec<usize> = (0..branches.len())
            .filter(|&i| !branches[i].remote)
            .collect();
        let remote: Vec<usize> = (0..branches.len())
            .filter(|&i| branches[i].remote)
            .collect();
        let mut rows: Vec<Option<usize>> = Vec::new();
        let mut headers: Vec<String> = Vec::new();
        headers.push(format!("{} · {}", tr("git_local_branches"), local.len()));
        rows.push(None);
        rows.extend(local.into_iter().map(Some));
        if !remote.is_empty() {
            headers.push(format!("{} · {}", tr("git_remote_branches"), remote.len()));
            rows.push(None);
            rows.extend(remote.into_iter().map(Some));
        }
        let rows = Arc::new(rows);
        let headers = Arc::new(headers);
        let entity = cx.entity();
        let list = uniform_list("git-branches", rows.len(), move |range, _, cx| {
            range
                .map(|i| match rows[i] {
                    Some(index) => Self::branch_row(&entity, index, i, cx),
                    None => {
                        let header = rows[..i].iter().filter(|r| r.is_none()).count();
                        section_header(headers[header].clone(), &colors(cx))
                    }
                })
                .collect()
        })
        .track_scroll(&self.list_scroll)
        .flex_1()
        .min_h_0()
        .px_1();
        v_flex()
            .size_full()
            .child(toolbar)
            .child(list)
            .into_any_element()
    }

    fn branch_row(panel: &Entity<Self>, index: usize, ix: usize, cx: &mut App) -> AnyElement {
        let c = colors(cx);
        let this = panel.read(cx);
        let Some(branch) = this.overview.branches.get(index) else {
            return div().into_any_element();
        };
        let selected = this.selection == Selection::Branch(branch.name.clone());
        let name = branch.name.clone();
        let weak = panel.downgrade();
        let mut track = String::new();
        if branch.ahead > 0 {
            track.push_str(&format!("↑{} ", branch.ahead));
        }
        if branch.behind > 0 {
            track.push_str(&format!("↓{}", branch.behind));
        }
        if branch.gone {
            track = tr("git_upstream_gone");
        }
        h_flex()
            .id(("git-branch", ix))
            .w_full()
            .h(rem_px(ROW_H))
            .px_2()
            .gap_2()
            .items_center()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |d| d.bg(c.fg.opacity(0.08)))
            .hover(|s| s.bg(c.fg.opacity(0.05)))
            .on_click(move |_, _, cx| {
                let _ = weak.update(cx, |p, cx| p.select(Selection::Branch(name.clone()), cx));
            })
            .child(
                Icon::empty()
                    .path(AtlasIcon::GitBranch.path())
                    .size(rem_px(14.))
                    .text_color(if branch.current {
                        c.primary
                    } else if branch.remote {
                        c.info
                    } else {
                        c.muted
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(relative(0.45))
                    .truncate()
                    .text_sm()
                    .when(branch.current, |d| d.font_weight(FontWeight::SEMIBOLD))
                    .child(branch.name.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(c.warning)
                    .child(track),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(c.muted)
                    .child(branch.subject.clone()),
            )
            .into_any_element()
    }

    fn simple_list<T: 'static>(
        &self,
        id: &'static str,
        items: Vec<T>,
        row: impl Fn(&T, &Colors) -> (Selection, Vec<AnyElement>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let items = Arc::new(items);
        let entity = cx.entity();
        let row = Arc::new(row);
        uniform_list(id, items.len(), move |range, _, cx| {
            let c = colors(cx);
            let selection = entity.read(cx).selection.clone();
            range
                .map(|i| {
                    let (select, children) = row(&items[i], &c);
                    let selected = select == selection;
                    let weak = entity.downgrade();
                    h_flex()
                        .id((id, i))
                        .w_full()
                        .h(rem_px(ROW_H))
                        .px_2()
                        .gap_2()
                        .items_center()
                        .rounded_md()
                        .cursor_pointer()
                        .when(selected, |d| d.bg(c.fg.opacity(0.08)))
                        .hover(|s| s.bg(c.fg.opacity(0.05)))
                        .on_click(move |_, _, cx| {
                            let _ = weak.update(cx, |p, cx| p.select(select.clone(), cx));
                        })
                        .children(children)
                        .into_any_element()
                })
                .collect()
        })
        .track_scroll(&self.list_scroll)
        .flex_1()
        .min_h_0()
        .px_1()
        .into_any_element()
    }

    fn stashes_list(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let has_changes = !self.overview.status.changes.is_empty();
        let toolbar = Self::toolbar(c)
            .child(
                Button::new("git-stash-save")
                    .small()
                    .icon(icon(AtlasIcon::TrayArrowDown))
                    .label(tr("git_stash_save"))
                    .disabled(self.busy() || !has_changes)
                    .on_click(cx.listener(|p, _, w, cx| p.open_prompt(Prompt::Stash, w, cx))),
            )
            .child(div().flex_1());
        let body = if self.overview.stashes.is_empty() {
            centered(
                div()
                    .text_sm()
                    .text_color(c.muted)
                    .child(tr("git_no_stashes")),
            )
        } else {
            self.simple_list(
                "git-stash",
                self.overview.stashes.clone(),
                |stash, c| {
                    (
                        Selection::Stash(stash.index),
                        vec![
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(c.muted)
                                .child(format!("stash@{{{}}}", stash.index))
                                .into_any_element(),
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_sm()
                                .child(stash.message.clone())
                                .into_any_element(),
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(c.muted)
                                .child(date(stash.time))
                                .into_any_element(),
                        ],
                    )
                },
                cx,
            )
        };
        v_flex()
            .size_full()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }

    fn tags_list(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let has_remote = !self.overview.remotes.is_empty();
        let toolbar = Self::toolbar(c)
            .child(
                Button::new("git-new-tag")
                    .small()
                    .icon(icon(AtlasIcon::Plus))
                    .label(tr("git_new_tag"))
                    .disabled(self.busy() || self.overview.status.head.head.is_empty())
                    .on_click(cx.listener(|p, _, w, cx| {
                        p.open_prompt(Prompt::NewTag { target: None }, w, cx);
                    })),
            )
            .child(
                self.action_button(
                    "git-push-tags",
                    tr("git_push_tags"),
                    GitAction::PushTags,
                    cx,
                )
                .ghost()
                .disabled(self.busy() || !has_remote),
            )
            .child(div().flex_1());
        let body = if self.overview.tags.is_empty() {
            centered(div().text_sm().text_color(c.muted).child(tr("git_no_tags")))
        } else {
            self.simple_list(
                "git-tag",
                self.overview.tags.clone(),
                |tag, c| {
                    (
                        Selection::Tag(tag.name.clone()),
                        vec![
                            Icon::empty()
                                .path(AtlasIcon::Tag.path())
                                .size(rem_px(14.))
                                .text_color(c.warning)
                                .into_any_element(),
                            div()
                                .flex_none()
                                .max_w(relative(0.4))
                                .truncate()
                                .text_sm()
                                .child(tag.name.clone())
                                .into_any_element(),
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(c.muted)
                                .child(tag.message.clone())
                                .into_any_element(),
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(c.muted)
                                .child(date(tag.time))
                                .into_any_element(),
                        ],
                    )
                },
                cx,
            )
        };
        v_flex()
            .size_full()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }

    fn remotes_list(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let toolbar = Self::toolbar(c)
            .child(
                Button::new("git-add-remote")
                    .small()
                    .icon(icon(AtlasIcon::Plus))
                    .label(tr("git_add_remote"))
                    .disabled(self.busy())
                    .on_click(cx.listener(|p, _, w, cx| p.open_prompt(Prompt::AddRemote, w, cx))),
            )
            .child(div().flex_1());
        let body = if self.overview.remotes.is_empty() {
            centered(
                div()
                    .text_sm()
                    .text_color(c.muted)
                    .child(tr("git_no_remotes")),
            )
        } else {
            self.simple_list(
                "git-remote",
                self.overview.remotes.clone(),
                |remote, c| {
                    (
                        Selection::Remote(remote.name.clone()),
                        vec![
                            Icon::empty()
                                .path(AtlasIcon::GlobeSimple.path())
                                .size(rem_px(14.))
                                .text_color(c.info)
                                .into_any_element(),
                            div()
                                .flex_none()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(remote.name.clone())
                                .into_any_element(),
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(c.muted)
                                .child(remote.url.clone())
                                .into_any_element(),
                        ],
                    )
                },
                cx,
            )
        };
        v_flex()
            .size_full()
            .child(toolbar)
            .child(body)
            .into_any_element()
    }

    // ── Right column ───────────────────────────────────────────────────────

    fn diff_view(&self, c: &Colors) -> AnyElement {
        if let Some(error) = &self.detail.error {
            return centered(div().text_sm().text_color(c.danger).child(error.clone()));
        }
        if self.detail.lines.is_empty() {
            return centered(
                div()
                    .text_sm()
                    .text_color(c.muted)
                    .child(if self.detail.loading {
                        tr("git_loading")
                    } else {
                        tr("git_no_diff")
                    }),
            );
        }
        let lines = self.detail.lines.clone();
        let c = *c;
        uniform_list("git-diff", lines.len(), move |range, _, _| {
            range
                .map(|i| {
                    let (kind, text) = &lines[i];
                    let (bg, color) = match kind {
                        LineKind::Add => (c.success.opacity(0.14), c.fg),
                        LineKind::Remove => (c.danger.opacity(0.14), c.fg),
                        LineKind::Hunk => (c.info.opacity(0.10), c.info),
                        LineKind::Header => (transparent_black(), c.primary),
                        LineKind::Meta => (transparent_black(), c.muted),
                        LineKind::Context => (transparent_black(), c.fg.opacity(0.85)),
                    };
                    div()
                        .w_full()
                        .h(rem_px(18.))
                        .px_2()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .bg(bg)
                        .text_color(color)
                        .child(text.clone())
                        .into_any_element()
                })
                .collect()
        })
        .track_scroll(&self.diff_scroll)
        .flex_1()
        .min_h_0()
        .font_family("Geist Mono")
        .text_xs()
        .into_any_element()
    }

    fn detail_header(title: String, subtitle: String, c: &Colors) -> Div {
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_size(rem_px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .when(!subtitle.is_empty(), |d| {
                d.child(div().text_xs().text_color(c.muted).child(subtitle))
            })
    }

    fn detail(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.busy();
        let top = v_flex()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(c.border.opacity(0.5));
        match self.selection.clone() {
            Selection::None => centered(
                div()
                    .text_sm()
                    .text_color(c.muted)
                    .child(tr("git_select_hint")),
            ),
            Selection::Change { staged, path } => {
                let Some(change) = self
                    .overview
                    .status
                    .changes
                    .iter()
                    .find(|ch| ch.path == path)
                else {
                    return centered(div());
                };
                let mut actions = h_flex().gap_2().flex_wrap();
                if change.conflicted() {
                    actions = actions
                        .child(self.action_button(
                            "git-d-ours",
                            tr("git_use_ours"),
                            GitAction::Resolve {
                                paths: vec![path.clone()],
                                ours: true,
                            },
                            cx,
                        ))
                        .child(self.action_button(
                            "git-d-theirs",
                            tr("git_use_theirs"),
                            GitAction::Resolve {
                                paths: vec![path.clone()],
                                ours: false,
                            },
                            cx,
                        ))
                        .child(self.action_button(
                            "git-d-resolved",
                            tr("git_mark_resolved"),
                            GitAction::Stage(vec![path.clone()]),
                            cx,
                        ));
                } else if staged {
                    actions = actions.child(self.action_button(
                        "git-d-unstage",
                        tr("git_unstage"),
                        GitAction::Unstage(vec![path.clone()]),
                        cx,
                    ));
                } else {
                    actions = actions.child(
                        self.action_button(
                            "git-d-stage",
                            tr("git_stage"),
                            GitAction::Stage(vec![path.clone()]),
                            cx,
                        )
                        .primary(),
                    );
                    let discard = if change.untracked() {
                        GitAction::Discard {
                            tracked: Vec::new(),
                            untracked: vec![path.clone()],
                        }
                    } else {
                        GitAction::Discard {
                            tracked: vec![path.clone()],
                            untracked: Vec::new(),
                        }
                    };
                    actions = actions.child(
                        self.action_button(
                            "git-d-discard",
                            tr(if change.untracked() {
                                "git_discard_untracked"
                            } else {
                                "git_discard"
                            }),
                            discard,
                            cx,
                        )
                        .danger(),
                    );
                }
                let reveal = path.clone();
                actions = actions.child(
                    Button::new("git-d-reveal")
                        .ghost()
                        .small()
                        .icon(icon(AtlasIcon::FolderOpen))
                        .label(tr("git_show_in_folder"))
                        .on_click(cx.listener(move |p, _, _, cx| p.reveal(&reveal, cx))),
                );
                let subtitle = match &change.original {
                    Some(original) => {
                        rust_i18n::t!("git_renamed_from", path = original).into_owned()
                    }
                    None => tr(if staged { "git_staged" } else { "git_unstaged" }),
                };
                v_flex()
                    .size_full()
                    .child(
                        top.child(Self::detail_header(path.clone(), subtitle, c))
                            .child(actions),
                    )
                    .child(self.diff_view(c))
                    .into_any_element()
            }
            Selection::Commit { id, file } => self.commit_detail(&id, file.as_deref(), c, cx),
            Selection::Branch(name) => {
                let Some(branch) = self
                    .overview
                    .branches
                    .iter()
                    .find(|b| b.name == name)
                    .cloned()
                else {
                    return centered(div());
                };
                v_flex()
                    .size_full()
                    .child(top.child(self.branch_detail(&branch, c, cx)))
                    .into_any_element()
            }
            Selection::Stash(index) => {
                let message = self
                    .overview
                    .stashes
                    .iter()
                    .find(|s| s.index == index)
                    .map(|s| s.message.clone())
                    .unwrap_or_default();
                let actions = h_flex()
                    .gap_2()
                    .child(
                        self.action_button(
                            "git-s-pop",
                            tr("git_stash_pop"),
                            GitAction::StashPop(index),
                            cx,
                        )
                        .primary(),
                    )
                    .child(self.action_button(
                        "git-s-apply",
                        tr("git_stash_apply"),
                        GitAction::StashApply(index),
                        cx,
                    ))
                    .child(
                        self.action_button(
                            "git-s-drop",
                            tr("git_stash_drop"),
                            GitAction::StashDrop(index),
                            cx,
                        )
                        .danger(),
                    );
                v_flex()
                    .size_full()
                    .child(
                        top.child(Self::detail_header(
                            message,
                            format!("stash@{{{index}}}"),
                            c,
                        ))
                        .child(actions),
                    )
                    .child(self.diff_view(c))
                    .into_any_element()
            }
            Selection::Tag(name) => {
                let Some(tag) = self.overview.tags.iter().find(|t| t.name == name).cloned() else {
                    return centered(div());
                };
                let remote = self.default_remote();
                let mut actions = h_flex().gap_2().flex_wrap().child(self.action_button(
                    "git-t-checkout",
                    tr("git_checkout"),
                    GitAction::Detach(tag.name.clone()),
                    cx,
                ));
                if let Some(remote) = remote {
                    actions = actions.child(self.action_button(
                        "git-t-push",
                        rust_i18n::t!("git_push_to", remote = remote.as_str()).into_owned(),
                        GitAction::PushTag {
                            remote,
                            name: tag.name.clone(),
                        },
                        cx,
                    ));
                }
                actions = actions.child(
                    self.action_button(
                        "git-t-delete",
                        tr("git_delete"),
                        GitAction::DeleteTag(tag.name.clone()),
                        cx,
                    )
                    .danger(),
                );
                v_flex()
                    .size_full()
                    .child(
                        top.child(Self::detail_header(
                            tag.name.clone(),
                            format!("{} · {}", tag.target, date(tag.time)),
                            c,
                        ))
                        .when(!tag.message.is_empty(), |d| {
                            d.child(div().text_sm().child(tag.message.clone()))
                        })
                        .child(actions),
                    )
                    .into_any_element()
            }
            Selection::Remote(name) => {
                let url = self
                    .overview
                    .remotes
                    .iter()
                    .find(|r| r.name == name)
                    .map(|r| r.url.clone())
                    .unwrap_or_default();
                let actions = h_flex()
                    .gap_2()
                    .child(
                        self.action_button(
                            "git-r-fetch",
                            tr("git_fetch"),
                            GitAction::Fetch {
                                remote: Some(name.clone()),
                                prune: false,
                            },
                            cx,
                        )
                        .primary(),
                    )
                    .child(
                        self.action_button(
                            "git-r-remove",
                            tr("git_remove_remote"),
                            GitAction::RemoveRemote(name.clone()),
                            cx,
                        )
                        .danger(),
                    );
                let _ = busy;
                v_flex()
                    .size_full()
                    .child(
                        top.child(Self::detail_header(name.clone(), url, c))
                            .child(actions),
                    )
                    .into_any_element()
            }
        }
    }

    fn branch_detail(&self, branch: &Branch, c: &Colors, cx: &mut Context<Self>) -> Div {
        let head = &self.overview.status.head;
        let current_name = head.branch.clone();
        let mut subtitle = match &branch.upstream {
            Some(upstream) => format!("→ {upstream}"),
            None if branch.remote => tr("git_remote_branch"),
            None => tr("git_no_upstream"),
        };
        if branch.ahead > 0 || branch.behind > 0 {
            subtitle.push_str(&format!("  ↑{} ↓{}", branch.ahead, branch.behind));
        }
        let mut actions = h_flex().gap_2().flex_wrap();
        let name = branch.name.clone();
        if branch.remote {
            let local = name
                .split_once('/')
                .map(|(_, b)| b.to_owned())
                .unwrap_or_default();
            let has_local = self
                .overview
                .branches
                .iter()
                .any(|b| !b.remote && b.name == local);
            if !has_local {
                actions = actions.child(
                    self.action_button(
                        "git-b-track",
                        tr("git_checkout_track"),
                        GitAction::Track(name.clone()),
                        cx,
                    )
                    .primary(),
                );
            }
            if current_name.is_some() {
                actions = actions
                    .child(self.action_button(
                        "git-b-merge",
                        tr("git_merge_into_current"),
                        GitAction::Merge {
                            name: name.clone(),
                            no_ff: false,
                        },
                        cx,
                    ))
                    .child(self.action_button(
                        "git-b-rebase",
                        tr("git_rebase_current_onto"),
                        GitAction::Rebase(name.clone()),
                        cx,
                    ));
            }
            if let Some((remote, rest)) = name.split_once('/') {
                actions = actions.child(
                    self.action_button(
                        "git-b-delete-remote",
                        tr("git_delete_on_remote"),
                        GitAction::DeleteRemoteBranch {
                            remote: remote.to_owned(),
                            branch: rest.to_owned(),
                        },
                        cx,
                    )
                    .danger(),
                );
            }
        } else if branch.current {
            if head.upstream.is_some() {
                actions = actions
                    .child(
                        self.action_button(
                            "git-b-pull",
                            tr("git_pull"),
                            GitAction::Pull(PullMode::Default),
                            cx,
                        )
                        .primary(),
                    )
                    .child(self.action_button(
                        "git-b-pull-rebase",
                        tr("git_pull_rebase"),
                        GitAction::Pull(PullMode::Rebase),
                        cx,
                    ));
            }
            if let Some(push) = self.push_action(false) {
                actions = actions.child(self.action_button(
                    "git-b-push",
                    if head.upstream.is_some() {
                        tr("git_push")
                    } else {
                        tr("git_publish")
                    },
                    push,
                    cx,
                ));
            }
            actions = actions.child(
                Button::new("git-b-rename")
                    .small()
                    .icon(icon(AtlasIcon::PencilSimple))
                    .label(tr("git_rename"))
                    .disabled(self.busy())
                    .on_click(cx.listener({
                        let name = name.clone();
                        move |p, _, w, cx| p.open_prompt(Prompt::RenameBranch(name.clone()), w, cx)
                    })),
            );
        } else {
            actions = actions.child(
                self.action_button(
                    "git-b-switch",
                    tr("git_switch"),
                    GitAction::Switch(name.clone()),
                    cx,
                )
                .primary(),
            );
            if current_name.is_some() {
                actions = actions
                    .child(self.action_button(
                        "git-b-merge",
                        tr("git_merge_into_current"),
                        GitAction::Merge {
                            name: name.clone(),
                            no_ff: false,
                        },
                        cx,
                    ))
                    .child(self.action_button(
                        "git-b-merge-noff",
                        tr("git_merge_no_ff"),
                        GitAction::Merge {
                            name: name.clone(),
                            no_ff: true,
                        },
                        cx,
                    ))
                    .child(self.action_button(
                        "git-b-rebase",
                        tr("git_rebase_current_onto"),
                        GitAction::Rebase(name.clone()),
                        cx,
                    ));
            }
            actions = actions
                .child(
                    Button::new("git-b-rename")
                        .small()
                        .icon(icon(AtlasIcon::PencilSimple))
                        .label(tr("git_rename"))
                        .disabled(self.busy())
                        .on_click(cx.listener({
                            let name = name.clone();
                            move |p, _, w, cx| {
                                p.open_prompt(Prompt::RenameBranch(name.clone()), w, cx);
                            }
                        })),
                )
                .child(
                    self.action_button(
                        "git-b-delete",
                        tr("git_delete"),
                        GitAction::DeleteBranch {
                            name: name.clone(),
                            force: false,
                        },
                        cx,
                    )
                    .danger(),
                )
                .child(
                    self.action_button(
                        "git-b-delete-force",
                        tr("git_delete_force"),
                        GitAction::DeleteBranch {
                            name: name.clone(),
                            force: true,
                        },
                        cx,
                    )
                    .danger()
                    .ghost(),
                );
        }
        actions = actions.child(
            Button::new("git-b-new-here")
                .ghost()
                .small()
                .icon(icon(AtlasIcon::Plus))
                .label(tr("git_branch_from_here"))
                .disabled(self.busy())
                .on_click(cx.listener({
                    let name = name.clone();
                    move |p, _, w, cx| {
                        p.open_prompt(
                            Prompt::NewBranch {
                                start: Some(name.clone()),
                            },
                            w,
                            cx,
                        );
                    }
                })),
        );
        v_flex()
            .gap_3()
            .child(Self::detail_header(branch.name.clone(), subtitle, c))
            .child(div().text_sm().text_color(c.muted).child(format!(
                "{} · {} · {}",
                branch.target,
                branch.subject,
                date(branch.time)
            )))
            .child(actions)
    }

    fn commit_detail(
        &self,
        id: &str,
        file: Option<&str>,
        c: &Colors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(commit) = self
            .history
            .commits
            .iter()
            .find(|commit| commit.id == id)
            .cloned()
        else {
            return centered(div());
        };
        let message = if self.detail.message.is_empty() {
            commit.subject.clone()
        } else {
            self.detail.message.clone()
        };
        let head_id = self.overview.status.head.head.clone();
        let is_head = !head_id.is_empty() && commit.id.starts_with(&head_id);
        let weak = cx.entity().downgrade();
        let reset_target = commit.id.clone();
        let reset = Button::new("git-c-reset")
            .small()
            .label(tr("git_reset_here"))
            .icon(icon(AtlasIcon::CaretDown))
            .disabled(self.busy() || is_head)
            .dropdown_menu(move |menu, _, _| {
                let item = |key: &str, mode: ResetMode| {
                    let weak = weak.clone();
                    let target = reset_target.clone();
                    PopupMenuItem::new(tr(key)).on_click(move |_, _, cx| {
                        let action = GitAction::Reset {
                            target: target.clone(),
                            mode,
                        };
                        let _ = weak.update(cx, |p, cx| p.request(action, cx));
                    })
                };
                menu.item(item("git_reset_soft", ResetMode::Soft))
                    .item(item("git_reset_mixed", ResetMode::Mixed))
                    .item(item("git_reset_hard", ResetMode::Hard))
            });
        let copy_id = commit.id.clone();
        let actions = h_flex()
            .gap_2()
            .flex_wrap()
            .child(self.action_button(
                "git-c-checkout",
                tr("git_checkout"),
                GitAction::Detach(commit.id.clone()),
                cx,
            ))
            .child(
                Button::new("git-c-branch")
                    .small()
                    .icon(icon(AtlasIcon::GitBranch))
                    .label(tr("git_branch_here"))
                    .disabled(self.busy())
                    .on_click(cx.listener({
                        let id = commit.id.clone();
                        move |p, _, w, cx| {
                            p.open_prompt(
                                Prompt::NewBranch {
                                    start: Some(id.clone()),
                                },
                                w,
                                cx,
                            );
                        }
                    })),
            )
            .child(
                Button::new("git-c-tag")
                    .small()
                    .icon(icon(AtlasIcon::Tag))
                    .label(tr("git_tag_here"))
                    .disabled(self.busy())
                    .on_click(cx.listener({
                        let id = commit.id.clone();
                        move |p, _, w, cx| {
                            p.open_prompt(
                                Prompt::NewTag {
                                    target: Some(id.clone()),
                                },
                                w,
                                cx,
                            );
                        }
                    })),
            )
            .child(self.action_button(
                "git-c-cherry",
                tr("git_cherry_pick"),
                GitAction::CherryPick(commit.id.clone()),
                cx,
            ))
            .child(self.action_button(
                "git-c-revert",
                tr("git_revert"),
                GitAction::Revert(commit.id.clone()),
                cx,
            ))
            .child(reset)
            .child(
                Button::new("git-c-copy")
                    .ghost()
                    .small()
                    .icon(icon(AtlasIcon::Copy))
                    .tooltip(tr("git_copy_id"))
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy_id.clone()));
                    }),
            );
        let subtitle = format!(
            "{} · {} <{}> · {}",
            commit.short_id(),
            commit.author,
            commit.email,
            date(commit.time)
        );
        let (title, body) = match message.split_once('\n') {
            Some((title, body)) => (title.to_owned(), body.trim().to_owned()),
            None => (message.clone(), String::new()),
        };
        let mut files = v_flex()
            .id("git-c-files")
            .max_h(rem_px(150.))
            .overflow_y_scroll()
            .gap_0p5();
        let selected_all = file.is_none();
        let commit_id = commit.id.clone();
        files = files.child(
            div()
                .id("git-c-all")
                .px_2()
                .rounded_sm()
                .cursor_pointer()
                .text_xs()
                .when(selected_all, |d| d.bg(c.fg.opacity(0.08)))
                .hover(|s| s.bg(c.fg.opacity(0.05)))
                .child(rust_i18n::t!("git_all_files", count = self.detail.files.len()).into_owned())
                .on_click(cx.listener({
                    let id = commit_id.clone();
                    move |p, _, _, cx| {
                        p.select(
                            Selection::Commit {
                                id: id.clone(),
                                file: None,
                            },
                            cx,
                        )
                    }
                })),
        );
        for (i, entry) in self.detail.files.iter().enumerate() {
            let selected = file == Some(entry.path.as_str());
            let path = entry.path.clone();
            let id = commit_id.clone();
            files = files.child(
                h_flex()
                    .id(("git-c-file", i))
                    .px_2()
                    .gap_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(c.fg.opacity(0.08)))
                    .hover(|s| s.bg(c.fg.opacity(0.05)))
                    .child(badge(entry.kind.letter(), kind_color(entry.kind, c)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .child(match &entry.original {
                                Some(original) => format!("{original} → {}", entry.path),
                                None => entry.path.clone(),
                            }),
                    )
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.select(
                            Selection::Commit {
                                id: id.clone(),
                                file: Some(path.clone()),
                            },
                            cx,
                        );
                    })),
            );
        }
        let top = v_flex()
            .gap_2()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(c.border.opacity(0.5))
            .child(Self::detail_header(title, subtitle, c))
            .when(!commit.refs.is_empty(), |d| {
                d.child(
                    h_flex()
                        .gap_1()
                        .flex_wrap()
                        .children(ref_chips(&commit.refs, c)),
                )
            })
            .when(!body.is_empty(), |d| {
                d.child(
                    div()
                        .id("git-c-body")
                        .max_h(rem_px(90.))
                        .overflow_y_scroll()
                        .text_sm()
                        .text_color(c.fg.opacity(0.85))
                        .child(body),
                )
            })
            .child(actions)
            .child(files);
        v_flex()
            .size_full()
            .child(top)
            .child(self.diff_view(c))
            .into_any_element()
    }

    // ── Footer ─────────────────────────────────────────────────────────────

    fn footer(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        let bar = h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(c.border.opacity(0.6));
        if let Some((text, _)) = &self.confirm {
            return bar
                .bg(c.danger.opacity(0.08))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_color(c.danger)
                        .child(text.clone()),
                )
                .child(
                    Button::new("git-confirm-cancel")
                        .ghost()
                        .small()
                        .label(tr("git_cancel"))
                        .on_click(cx.listener(|p, _, _, cx| {
                            p.confirm = None;
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("git-confirm-ok")
                        .danger()
                        .small()
                        .label(tr("git_confirm"))
                        .on_click(cx.listener(|p, _, _, cx| {
                            if let Some((_, action)) = p.confirm.take() {
                                p.execute(action, cx);
                            }
                        })),
                )
                .into_any_element();
        }
        if let Some(prompt) = &self.prompt {
            let extra = matches!(prompt, Prompt::NewTag { .. } | Prompt::AddRemote);
            let stash = matches!(prompt, Prompt::Stash);
            return bar
                .child(div().flex_none().text_sm().child(prompt.label()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(crate::ui::text_input::bordered(&self.prompt_input).small()),
                )
                .when(extra, |bar| {
                    bar.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(crate::ui::text_input::bordered(&self.prompt_extra).small()),
                    )
                })
                .when(stash, |bar| {
                    bar.child(
                        Checkbox::new("git-stash-untracked")
                            .label(tr("git_include_untracked"))
                            .checked(self.stash_untracked)
                            .small()
                            .on_click(cx.listener(|p, checked: &bool, _, cx| {
                                p.stash_untracked = *checked;
                                cx.notify();
                            })),
                    )
                })
                .child(
                    Button::new("git-prompt-cancel")
                        .ghost()
                        .small()
                        .label(tr("git_cancel"))
                        .on_click(cx.listener(|p, _, w, cx| {
                            p.prompt = None;
                            w.focus(&p.focus, cx);
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("git-prompt-ok")
                        .primary()
                        .small()
                        .label(tr("git_ok"))
                        .on_click(cx.listener(|p, _, w, cx| p.submit_prompt(w, cx))),
                )
                .into_any_element();
        }
        if let Some((label, _, network)) = &self.running {
            return bar
                .child(div().size(rem_px(6.)).rounded_full().bg(c.primary))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(rust_i18n::t!("git_running", command = label.as_str()).into_owned()),
                )
                .when(*network, |bar| {
                    bar.child(
                        Button::new("git-cancel-run")
                            .ghost()
                            .small()
                            .label(tr("git_cancel"))
                            .on_click(cx.listener(|p, _, _, cx| {
                                p.cancel();
                                cx.notify();
                            })),
                    )
                })
                .into_any_element();
        }
        let (dot, text) = match &self.message {
            Some((true, text)) => (c.danger, text.clone()),
            Some((false, text)) => (c.success, text.clone()),
            None => (c.muted, tr("git_footer_hint")),
        };
        let failed = self.message.as_ref().is_some_and(|(failed, _)| *failed);
        bar.child(div().flex_none().size(rem_px(6.)).rounded_full().bg(dot))
            .child(
                div()
                    .id("git-message")
                    .flex_1()
                    .min_w_0()
                    .max_h(rem_px(64.))
                    .overflow_y_scroll()
                    .text_sm()
                    .when(failed, |d| d.text_color(c.danger))
                    .child(text),
            )
            .into_any_element()
    }

    fn empty_state(&self, c: &Colors, cx: &mut Context<Self>) -> AnyElement {
        match &self.repo {
            Repo::Loading => centered(div().text_sm().text_color(c.muted).child(tr("git_loading"))),
            Repo::Missing => centered(
                v_flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_size(rem_px(16.)).child(tr("git_missing")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(c.muted)
                            .child(tr("git_missing_hint")),
                    ),
            ),
            Repo::NotRepo => centered(
                v_flex()
                    .items_center()
                    .gap_3()
                    .child(div().text_size(rem_px(16.)).child(tr("git_not_repo")))
                    .child(
                        div()
                            .text_sm()
                            .text_color(c.muted)
                            .child(self.dir.display().to_string()),
                    )
                    .child(
                        self.action_button("git-init", tr("git_init"), GitAction::Init, cx)
                            .primary(),
                    ),
            ),
            Repo::Failed(error) => {
                let ownership =
                    error.contains("dubious ownership") || error.contains("safe.directory");
                centered(
                    v_flex()
                        .items_center()
                        .gap_2()
                        .max_w(rem_px(640.))
                        .child(div().text_size(rem_px(16.)).child(tr("git_failed")))
                        .child(div().text_sm().text_color(c.danger).child(error.clone()))
                        .when(ownership, |d| {
                            d.child(
                                div()
                                    .text_sm()
                                    .text_color(c.muted)
                                    .child(tr("git_ownership_hint")),
                            )
                        }),
                )
            }
            Repo::Ready(_) => div().into_any_element(),
        }
    }
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.clear_commit {
            self.clear_commit = false;
            self.commit_input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        let c = colors(cx);
        let viewport = window.viewport_size();
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let width = (f32::from(viewport.width) - 32.)
            .min(1320. * zoom)
            .max(320.);
        let height = (f32::from(viewport.height) - 38.)
            .min(860. * zoom)
            .max(240.);
        let ready = matches!(self.repo, Repo::Ready(_));
        let body = if ready {
            let left = match self.tab {
                Tab::Changes => self.changes_list(&c, cx),
                Tab::History => self.history_list(&c, cx),
                Tab::Branches => self.branches_list(&c, window, cx),
                Tab::Stashes => self.stashes_list(&c, cx),
                Tab::Tags => self.tags_list(&c, cx),
                Tab::Remotes => self.remotes_list(&c, cx),
            };
            let left_share = if self.tab == Tab::History { 0.56 } else { 0.42 };
            h_flex()
                .flex_1()
                .min_h_0()
                .child(
                    v_flex()
                        .w(relative(left_share))
                        .h_full()
                        .flex_none()
                        .border_r_1()
                        .border_color(c.border.opacity(0.5))
                        .child(left),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(self.detail(&c, cx)),
                )
                .into_any_element()
        } else {
            self.empty_state(&c, cx)
        };
        v_flex()
            .id("git-panel")
            .track_focus(&self.focus)
            .w(px(width))
            .h(px(height))
            .rounded_xl()
            .bg(c.bg)
            .shadow_lg()
            .overflow_hidden()
            .text_color(c.fg)
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(self.header(&c, cx))
            .when(ready, |d| d.child(self.tabs(&c, cx)))
            .children(self.operation_banner(&c, cx))
            .child(body)
            .child(self.footer(&c, cx))
    }
}

impl AtlasApp {
    /// Open the Git panel for `dir` (the focused pane's folder when `None`).
    pub(crate) fn open_git_panel(&mut self, dir: Option<PathBuf>) {
        if !self.state.settings.git_integration {
            self.toast_info(tr("git_disabled_hint"));
            return;
        }
        let dir = dir.or_else(|| {
            self.tab_data_for(self.state.focused_tile)
                .and_then(|td| td.location.fs_dir())
                .map(std::path::Path::to_path_buf)
        });
        match dir {
            Some(dir) => self.pending_git = Some(dir),
            None => self.toast_info(tr("git_need_folder")),
        }
    }

    /// Mount the panel requested by `open_git_panel` (needs a window).
    pub(crate) fn mount_pending_git(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(dir) = self.pending_git.take() {
            let parent = cx.entity().downgrade();
            let panel = cx.new(|cx| GitPanel::new(dir, parent, window, cx));
            let focus = panel.read(cx).focus.clone();
            window.focus(&focus, cx);
            self.git_panel = Some(panel);
            cx.notify();
        }
    }

    pub(crate) fn render_git_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(panel) = self.git_panel.clone() else {
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
