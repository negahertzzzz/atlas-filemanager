//! Unified local/cloud transfer center.
//!
//! Renders `state.cloud.transfers` while `state.cloud.show_transfers` is set.
//! Backend (queueing, progress, pause/resume/cancel/retry) already lives in
//! `app/cloud.rs::CloudAction` / the rclone worker; this is display-only.

#[cfg(feature = "provider-host")]
use crate::app::cloud::CloudAction;
use crate::app::App;
use crate::ui::icons::{icon, AtlasIcon};
use atlas_core::util::display::format_size;
#[cfg(feature = "provider-host")]
use atlas_model::cloud_model::{Transfer, TransferKind, TransferState};
use atlas_model::ops::OpState;
use gpui::prelude::*;
use gpui::{deferred, div, px, AnyElement, Context, MouseButton, SharedString, Window};
use gpui_component::scroll::ScrollableElement;
use gpui_component::{h_flex, v_flex, ActiveTheme};

const BAR_W: f32 = 360.;
const ROW_MAX_BYTES_LABEL: f32 = 90.;

impl App {
    pub(crate) fn render_transfer_center(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (popover, fg, muted, border, danger, _success) = {
            let t = cx.theme();
            (
                t.popover,
                t.popover_foreground,
                t.muted_foreground,
                t.border,
                t.danger,
                t.success,
            )
        };
        #[cfg(feature = "provider-host")]
        let view = cx.entity();

        let mut rows = v_flex().gap_2();
        let mut local: Vec<_> = self.state.ops.active_ops.iter().collect();
        local.sort_unstable_by_key(|(id, _)| **id);
        for (id, op) in local {
            rows = rows.child(local_transfer_row(*id, op, fg, muted, border, danger));
        }
        #[cfg(feature = "provider-host")]
        if self.state.cloud.show_transfers {
            for transfer in &self.state.cloud.transfers {
                let account = |id: &str| {
                    self.state
                        .cloud
                        .remotes
                        .iter()
                        .find(|remote| remote.id == id)
                        .map_or_else(
                            || id.to_string(),
                            |remote| format!("{} ({id})", remote.name),
                        )
                };
                let route = transfer.source.as_ref().map_or_else(
                    || account(&transfer.remote_id),
                    |source| format!("{} → {}", account(&source.id), account(&transfer.remote_id)),
                );
                rows = rows.child(cloud_transfer_row(
                    transfer, route, fg, muted, border, danger, _success, &view,
                ));
            }
        }

        let header = h_flex().items_center().justify_between().child(
            div()
                .text_sm()
                .text_color(fg)
                .child(rust_i18n::t!("cloud_transfers_title").to_string()),
        );
        #[cfg(feature = "provider-host")]
        let header = header.child({
            let close_view = view.clone();
            div()
                .id("cloud-transfers-close")
                .size(px(20.))
                .rounded(px(crate::ui::RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .cursor_pointer()
                .hover(move |h| h.bg(border).text_color(fg))
                .child(icon(AtlasIcon::X, 11., muted))
                .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
                    close_view.update(cx, |this, cx| {
                        this.handle_cloud_action(CloudAction::ShowTransfers(false));
                        cx.notify();
                    });
                })
        });

        deferred(
            div().absolute().bottom(px(24.)).right(px(24.)).child(
                v_flex()
                    .id("cloud-transfers-bar")
                    .gap_2()
                    .w(px(BAR_W))
                    .bg(popover)
                    .text_color(fg)
                    .border_1()
                    .border_color(border)
                    .rounded_lg()
                    .shadow_lg()
                    .occlude()
                    .p_3()
                    .child(header)
                    .child(div().max_h(px(360.)).overflow_y_scrollbar().child(rows)),
            ),
        )
        .into_any_element()
    }
}

fn local_transfer_row(
    id: u64,
    op: &OpState,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
    danger: gpui::Hsla,
) -> impl IntoElement {
    let fraction = if op.bytes_total > 0 {
        op.bytes_done as f32 / op.bytes_total as f32
    } else if op.total > 0 {
        op.done as f32 / op.total as f32
    } else {
        0.0
    }
    .clamp(0.0, 1.0);
    let elapsed = op.started_at.elapsed().as_secs_f64();
    let bytes_per_second = if elapsed >= 0.5 {
        (op.bytes_done as f64 / elapsed) as u64
    } else {
        0
    };
    let eta = if bytes_per_second > 0 && op.bytes_total > op.bytes_done {
        Some((op.bytes_total - op.bytes_done) / bytes_per_second)
    } else {
        None
    };
    let file = op
        .current_file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| op.label.clone());
    let detail = if op.bytes_total > 0 {
        format!(
            "{} / {}",
            format_size(op.bytes_done),
            format_size(op.bytes_total)
        )
    } else {
        format!("{} / {}", op.done.min(op.total), op.total)
    };
    let telemetry = if bytes_per_second > 0 {
        let speed = format!("{}/s", format_size(bytes_per_second));
        eta.map_or(speed.clone(), |seconds| {
            format!("{speed} · {}", format_eta(seconds))
        })
    } else {
        detail
    };
    let cancel = op.cancel.clone();

    h_flex()
        .id(SharedString::from(format!("local-xfer-row-{id}")))
        .gap_2()
        .items_center()
        .child(icon(AtlasIcon::Copy, 14., muted))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(div().text_xs().text_color(fg).truncate().child(file))
                .child(
                    div().h(px(3.)).w_full().rounded_full().bg(border).child(
                        div()
                            .h(px(3.))
                            .rounded_full()
                            .bg(danger)
                            .w(gpui::relative(fraction)),
                    ),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(px(ROW_MAX_BYTES_LABEL))
                .text_xs()
                .text_color(muted)
                .child(telemetry),
        )
        .child(
            div()
                .id(SharedString::from(format!("local-xfer-cancel-{id}")))
                .size(px(18.))
                .rounded(px(crate::ui::RADIUS))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(move |hover| hover.bg(border).text_color(fg))
                .child(icon(AtlasIcon::X, 10., muted))
                .on_mouse_down(MouseButton::Left, move |_event, _window, _cx| {
                    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }),
        )
}

fn format_eta(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds.div_ceil(60))
    } else {
        format!("{}h {}m", seconds / 3_600, (seconds % 3_600).div_ceil(60))
    }
}

#[cfg(feature = "provider-host")]
fn cloud_transfer_row(
    t: &Transfer,
    route: String,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    border: gpui::Hsla,
    danger: gpui::Hsla,
    success: gpui::Hsla,
    view: &gpui::Entity<App>,
) -> impl IntoElement {
    let kind_icon = match t.kind {
        TransferKind::Download => AtlasIcon::Download,
        TransferKind::Upload => AtlasIcon::ArrowUp,
        TransferKind::RemoteCopy => AtlasIcon::Copy,
    };
    let pct = (t.fraction() * 100.0).round() as i32;
    let state_label = match &t.state {
        TransferState::Queued => rust_i18n::t!("cloud_transfer_queued").to_string(),
        TransferState::Running if t.bytes_total > 0 => format!("{pct}%"),
        TransferState::Running => {
            if t.bytes_done > 0 {
                format_size(t.bytes_done)
            } else {
                rust_i18n::t!("cloud_transfer_running").to_string()
            }
        }
        TransferState::Paused => rust_i18n::t!("cloud_transfer_paused").to_string(),
        TransferState::Pausing => rust_i18n::t!("cloud_transfer_pausing").to_string(),
        TransferState::Cancelling => rust_i18n::t!("cloud_transfer_cancelling").to_string(),
        TransferState::Cancelled => rust_i18n::t!("cloud_transfer_cancelled").to_string(),
        TransferState::Interrupted(_) => rust_i18n::t!("cloud_transfer_interrupted").to_string(),
        TransferState::Done => rust_i18n::t!("cloud_transfer_done").to_string(),
        TransferState::Failed(_) => rust_i18n::t!("cloud_transfer_failed").to_string(),
    };
    let state_color = match &t.state {
        TransferState::Failed(_) | TransferState::Interrupted(_) => danger,
        TransferState::Done => success,
        _ => muted,
    };
    let bar_pct = if t.state == TransferState::Done {
        1.0
    } else if t.state == TransferState::Running && t.bytes_total == 0 {
        0.35
    } else {
        t.fraction().clamp(0.0, 1.0)
    };
    let id = t.id;

    let mut actions = h_flex().gap_1().flex_shrink_0();
    match &t.state {
        TransferState::Running => {
            actions = actions.child(transfer_btn(
                AtlasIcon::Square,
                muted,
                fg,
                border,
                view,
                CloudAction::PauseTransfer(id),
                format!("xfer-pause-{id}"),
            ));
            actions = actions.child(transfer_btn(
                AtlasIcon::X,
                muted,
                fg,
                border,
                view,
                CloudAction::CancelTransfer(id),
                format!("xfer-cancel-{id}"),
            ));
        }
        TransferState::Paused | TransferState::Queued => {
            let (button, action, name) = if t.state == TransferState::Queued {
                (
                    AtlasIcon::Square,
                    CloudAction::PauseTransfer(id),
                    format!("xfer-pause-{id}"),
                )
            } else {
                (
                    AtlasIcon::CaretRight,
                    CloudAction::ResumeTransfer(id),
                    format!("xfer-resume-{id}"),
                )
            };
            actions = actions.child(transfer_btn(button, muted, fg, border, view, action, name));
            actions = actions.child(transfer_btn(
                AtlasIcon::X,
                muted,
                fg,
                border,
                view,
                CloudAction::CancelTransfer(id),
                format!("xfer-cancel-{id}"),
            ));
        }
        TransferState::Failed(_) => {
            actions = actions.child(transfer_btn(
                AtlasIcon::ArrowClockwise,
                muted,
                fg,
                border,
                view,
                CloudAction::RetryTransfer(id),
                format!("xfer-retry-{id}"),
            ));
        }
        TransferState::Pausing => {
            actions = actions.child(transfer_btn(
                AtlasIcon::X,
                muted,
                fg,
                border,
                view,
                CloudAction::CancelTransfer(id),
                format!("xfer-cancel-{id}"),
            ));
        }
        TransferState::Done
        | TransferState::Cancelling
        | TransferState::Cancelled
        | TransferState::Interrupted(_) => {}
    }

    // Surface the failure reason: a bare "Failed" label is undebuggable.
    let fail_reason = match &t.state {
        TransferState::Failed(err) | TransferState::Interrupted(err) if !err.is_empty() => {
            Some(err.clone())
        }
        _ => None,
    };
    h_flex()
        .id(SharedString::from(format!("xfer-row-{id}")))
        .gap_2()
        .items_center()
        .when_some(fail_reason, |row, reason| {
            row.tooltip(move |window, cx| {
                gpui_component::tooltip::Tooltip::new(reason.clone()).build(window, cx)
            })
        })
        .child(icon(kind_icon, 14., muted))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .text_xs()
                        .text_color(fg)
                        .truncate()
                        .child(t.label.clone()),
                )
                .child(div().text_xs().text_color(muted).truncate().child(route))
                .child(
                    div().h(px(3.)).w_full().rounded_full().bg(border).child(
                        div()
                            .h(px(3.))
                            .rounded_full()
                            .bg(state_color)
                            .w(gpui::relative(bar_pct)),
                    ),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .w(px(ROW_MAX_BYTES_LABEL))
                .text_xs()
                .text_color(state_color)
                .child(state_label),
        )
        .child(actions)
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "provider-host")]
fn transfer_btn(
    ic: AtlasIcon,
    muted: gpui::Hsla,
    fg: gpui::Hsla,
    border: gpui::Hsla,
    view: &gpui::Entity<App>,
    action: CloudAction,
    id: String,
) -> impl IntoElement {
    let view = view.clone();
    div()
        .id(SharedString::from(id))
        .size(px(18.))
        .rounded(px(crate::ui::RADIUS))
        .flex()
        .items_center()
        .justify_center()
        .text_color(muted)
        .cursor_pointer()
        .hover(move |h| h.bg(border).text_color(fg))
        .child(icon(ic, 10., muted))
        .on_mouse_down(MouseButton::Left, move |_ev, _w, cx| {
            let action = action.clone();
            view.update(cx, |this, cx| {
                this.handle_cloud_action(action);
                cx.notify();
            });
        })
}
