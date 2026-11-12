//! Inline batch rename: the selected rows show their new name in place with one
//! synchronized caret, painted like the real rename input.

use crate::app::App;
use atlas_model::batch_rename::RenameIssue;
use gpui::ColorExt as _;
use gpui::{
    point, px, size, App as GpuiApp, Bounds, ContentMask, Font, Hsla, Pixels, SharedString,
    TextAlign, TextRun, WeakEntity, Window,
};
use std::path::Path;

/// Everything a row needs to paint its in-place name.
pub(super) struct RowEdit {
    text: String,
    caret_col: usize,
    /// Characters `0..caret_col` are selected (the stem right after rename starts).
    select_to_caret: bool,
    caret_on: bool,
    issue: bool,
}

pub(super) fn row_edit(app: &WeakEntity<App>, path: &Path, cx: &GpuiApp) -> Option<RowEdit> {
    let app = app.upgrade()?;
    let app = app.read(cx);
    let batch = app.state.batch_rename.as_ref()?;
    let i = batch.files.iter().position(|f| f == path)?;
    let text = batch.multi_lines.get(i)?.clone();
    Some(RowEdit {
        caret_col: batch.pos_in(&text),
        select_to_caret: batch.select_to_caret,
        caret_on: app.batch_blink_visible,
        issue: batch
            .row_issues
            .get(i)
            .is_some_and(|v| *v != RenameIssue::Ok),
        text,
    })
}

fn byte_of(text: &str, col: usize) -> usize {
    text.char_indices().nth(col).map_or(text.len(), |(b, _)| b)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn paint_row_edit(
    edit: &RowEdit,
    window: &mut Window,
    cx: &mut GpuiApp,
    font: &Font,
    font_size: Pixels,
    cell: Bounds<Pixels>,
    text_y: Pixels,
    row_h: f32,
    zoom: f32,
    ink: Hsla,
    danger: Hsla,
) {
    let stem_end = edit
        .text
        .rfind('.')
        .filter(|dot| *dot > 0)
        .unwrap_or(edit.text.len());
    let base = if edit.issue { danger } else { ink };

    // Runs: stem in ink, extension dimmed. Chips are painted separately.
    let mut runs = Vec::new();
    if stem_end > 0 {
        runs.push(TextRun {
            len: stem_end,
            font: font.clone(),
            color: base,
            background_color: None,
            underline: None,
            strikethrough: None,
            letter_spacing: None,
        });
    }
    if stem_end < edit.text.len() {
        runs.push(TextRun {
            len: edit.text.len() - stem_end,
            font: font.clone(),
            color: base.opacity(0.62),
            background_color: None,
            underline: None,
            strikethrough: None,
            letter_spacing: None,
        });
    }
    let shared: SharedString = edit.text.clone().into();
    let line = window
        .text_system()
        .shape_line(shared, font_size, &runs, None);

    let x0 = cell.origin.x;
    window.with_content_mask(
        Some(ContentMask {
            bounds: cell,
            fade_out: Default::default(),
        }),
        |window| {
            let caret_x = x0 + line.x_for_index(byte_of(&edit.text, edit.caret_col));
            // Selected stem: same theme selection colour the real input uses.
            if edit.select_to_caret && edit.caret_col > 0 {
                let sel_h = row_h - 8.0 * zoom;
                window.paint_quad(gpui::fill(
                    Bounds {
                        origin: point(x0, cell.origin.y + px((row_h - sel_h) * 0.5)),
                        size: size(caret_x - x0, px(sel_h)),
                    },
                    gpui_component::ActiveTheme::theme(&*cx).selection,
                ));
            }
            let _ = line.paint(
                point(x0, text_y),
                px(row_h),
                TextAlign::Left,
                None,
                window,
                cx,
            );

            if edit.caret_on {
                window.paint_quad(gpui::fill(
                    Bounds {
                        origin: point(caret_x - px(1.0 * zoom), cell.origin.y + px(6.0 * zoom)),
                        size: size(px(2.0 * zoom), px(row_h - 12.0 * zoom)),
                    },
                    ink,
                ));
            }
        },
    );
}
