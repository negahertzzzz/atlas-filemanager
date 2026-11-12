//! Batch rename, inline: the selected rows are edited in place by one
//! synchronized caret (see `views/batch_row.rs`). This layer is only the keyboard
//! owner plus a quiet hint pill; it never covers or dims the list.

use crate::app::App;
use crate::input::action::Action;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{deferred, div, px, AnyElement, Context, KeyDownEvent, SharedString, Window};
use gpui_component::{h_flex, ActiveTheme, StyledExt as _};

impl App {
    pub(crate) fn close_batch_rename(&mut self) {
        self.state.batch_rename = None;
        self.batch_pattern_input = None;
        self.batch_replacement_input = None;
        self.batch_rename_field = crate::app::BatchRenameField::Pattern;
        self.batch_input_sub = None;
    }

    pub(crate) fn render_batch_rename(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (fg, muted, border, danger) = {
            let t = cx.theme();
            (t.popover_foreground, t.muted_foreground, t.border, t.danger)
        };
        // The session lives and dies with the rename input (blur, navigation...).
        if self.rename_edit.is_none() {
            self.close_batch_rename();
            cx.notify();
            return div().into_any_element();
        }
        let Some(batch) = self.state.batch_rename.as_ref() else {
            return div().into_any_element();
        };
        let count = batch.files.len();
        let blocked = batch.has_blocking_issues();
        let changed = batch
            .files
            .iter()
            .zip(&batch.previews)
            .filter(|(path, name)| {
                path.file_name()
                    .is_none_or(|n| n.to_string_lossy() != **name)
            })
            .count();
        let has_numbers = batch
            .multi_lines
            .first()
            .is_some_and(|l| !atlas_model::batch_rename::detect_tokens(l).is_empty());

        let key = move |label: &'static str| {
            div()
                .px_1()
                .min_w(px(16.))
                .h(px(16.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .bg(fg.opacity(0.08))
                .text_color(fg)
                .child(label)
        };
        let hint = move |keys: &[&'static str], text: String| {
            h_flex()
                .gap_1()
                .items_center()
                .children(keys.iter().map(|k| key(k)))
                .child(div().text_color(muted).child(text))
        };

        let status = if blocked {
            div()
                .text_color(danger)
                .child(rust_i18n::t!("batch_inline_conflict").to_string())
        } else {
            div().text_color(muted).child(
                rust_i18n::t!("batch_inline_count", changed = changed, count = count).to_string(),
            )
        };

        let pill = h_flex()
            .id("batch-rename-pill")
            .gap_3()
            .items_center()
            .h(px(34.))
            .px_3()
            .rounded(px(10.))
            .border_1()
            .border_color(border.opacity(0.6))
            .popover_style(cx)
            .text_xs()
            .occlude()
            .child(status)
            .child(hint(&["↵"], rust_i18n::t!("batch_hint_apply").to_string()))
            .child(hint(&["⇥"], rust_i18n::t!("batch_hint_jump").to_string()))
            .when(has_numbers, |p| {
                p.child(hint(
                    &["Alt", "N"],
                    rust_i18n::t!("batch_hint_number").to_string(),
                ))
                .child(hint(
                    &["Alt", "D"],
                    rust_i18n::t!("batch_hint_dates").to_string(),
                ))
            })
            .child(hint(
                &["esc"],
                rust_i18n::t!("batch_hint_cancel").to_string(),
            ));

        deferred(
            div()
                .id(SharedString::from("batch-rename-layer"))
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(22.))
                .flex()
                .justify_center()
                .child(pill),
        )
        .priority(5)
        .into_any_element()
    }

    pub(crate) fn batch_rename_actions(&self) -> Vec<Action> {
        self.state
            .batch_rename
            .as_ref()
            .map(|batch| {
                batch
                    .files
                    .iter()
                    .zip(&batch.previews)
                    .filter_map(|(path, new_name)| {
                        let parent = path.parent()?;
                        let to = parent.join(new_name);
                        (to != *path).then(|| Action::Rename {
                            from: path.clone(),
                            to,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}
