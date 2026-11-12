//! Visual vocabulary shared by the panel and the single painted map. All
//! surfaces, file icons and color roles come from the active Atlas theme.
use super::{tr, CleanupPanel, Tab};
use crate::ui::{
    file_icons::FileIcon,
    icons::{is_windows_theme, theme_icon, AtlasIcon},
};
use atlas_model::file_entry::FileKind;
use gpui::{prelude::*, *};
use gpui_component::{
    button::{Button, ButtonVariants},
    h_flex, v_flex, ActiveTheme, Disableable, Sizable,
};
use std::path::Path;

impl CleanupPanel {
    pub(super) fn windows_icons(&self, cx: &gpui::App) -> bool {
        self.parent
            .upgrade()
            .is_some_and(|p| is_windows_theme(&p.read(cx).state.profiles.active().theme))
    }

    pub(super) fn file_glyph(&self, path: &Path, directory: bool, cx: &gpui::App) -> FileIcon {
        let reference = crate::app::App::file_icon_ref_for_path(path.to_owned(), directory);
        self.parent
            .upgrade()
            .map(|p| p.read(cx).file_icon(&reference))
            .unwrap_or(FileIcon::Builtin {
                kind: reference.kind,
                extension: reference.extension,
            })
    }

    pub(super) fn file_art(
        &self,
        path: &Path,
        directory: bool,
        size: f32,
        cx: &gpui::App,
    ) -> AnyElement {
        // Preserve icon-pack paper artwork while giving it a contrast surface.
        div()
            .size(crate::ui::rem_px(size + 8.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .bg(if !directory && !cx.theme().is_dark() {
                cx.theme().foreground.opacity(0.55)
            } else {
                transparent_black()
            })
            .child(
                self.file_glyph(path, directory, cx)
                    .element(size, cx.theme().foreground),
            )
            .into_any_element()
    }

    pub(super) fn heading(&self, compact: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let fg = theme.foreground;
        let accent = theme.accent;
        let selection = theme.sidebar_accent;
        let muted = super::muted(cx);
        let windows = self.windows_icons(cx);
        let busy = self.execution.is_some() || self.preparing;
        let format = atlas_core::util::display::format_size;
        let title = if self.review.is_some() {
            tr("cleanup_review_title")
        } else if self.tab == Tab::History {
            tr("cleanup_report")
        } else {
            self.volume.display().to_string()
        };
        let subtitle = match (self.tab, self.snapshot.capacity.filter(|c| c.0 > 0)) {
            (Tab::History, _) => tr("cleanup_history_subtitle"),
            (Tab::Cleanup, _) if !self.cats.is_empty() => format!(
                "{} {}",
                format(
                    self.cats
                        .iter()
                        .filter(|c| c.enabled)
                        .map(|c| c.bytes())
                        .sum()
                ),
                tr("cleanup_selected_short")
            ),
            (Tab::Cleanup, _) => format!(
                "{} {}",
                format(
                    self.snapshot
                        .candidates
                        .iter()
                        .filter(|c| self.checked.contains(&c.path))
                        .map(|c| c.identity.bytes)
                        .sum()
                ),
                tr("cleanup_selected_short")
            ),
            (_, Some((total, free))) => format!(
                "{} {} · {} {}",
                format(free),
                tr("cleanup_free"),
                tr("cleanup_capacity"),
                format(total)
            ),
            _ => format!(
                "{} {}",
                format(self.snapshot.bytes),
                tr("cleanup_analyzed_short")
            ),
        };
        let segment = |id: &'static str, key: &str, tab: Tab, cx: &mut Context<Self>| {
            let active = self.tab == tab;
            Button::new(id)
                .ghost()
                .small()
                .disabled(busy)
                .bg(if active {
                    selection
                } else {
                    transparent_black()
                })
                .child(
                    div()
                        .text_sm()
                        .font_weight(if active {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(if active { fg } else { muted })
                        .child(tr(key)),
                )
                .on_click(cx.listener(move |p, _, _, cx| p.switch(tab, cx)))
        };
        let switcher = h_flex()
            .gap_1()
            .p_1()
            .rounded_lg()
            .bg(fg.opacity(0.05))
            .child(segment("cleanup-tab-map", "cleanup_map", Tab::Map, cx))
            .child(segment(
                "cleanup-tab-files",
                "cleanup_tab_files",
                Tab::Files,
                cx,
            ))
            .child(segment(
                "cleanup-tab-types",
                "cleanup_tab_types",
                Tab::Types,
                cx,
            ))
            .child(segment(
                "cleanup-tab-dups",
                "cleanup_tab_duplicates",
                Tab::Duplicates,
                cx,
            ))
            // Temporary-file categories exist on local system volumes only.
            .when(!self.network, |s| {
                s.child(segment(
                    "cleanup-tab-temp",
                    "cleanup_temp",
                    Tab::Cleanup,
                    cx,
                ))
            });
        let capacity = self
            .snapshot
            .capacity
            .filter(|c| c.0 > 0)
            .map(|(total, free)| {
                (total.saturating_sub(free) as f64 / total as f64).clamp(0., 1.) as f32
            });
        v_flex()
            .w_full()
            .gap_3()
            .px_5()
            .pt_5()
            .pb_3()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(theme_icon(
                        if self.network {
                            AtlasIcon::NetworkDrive
                        } else {
                            AtlasIcon::HardDrive
                        },
                        28.,
                        accent,
                        windows,
                    ))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(crate::ui::rem_px(if compact { 20. } else { 24. }))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(div().text_sm().text_color(muted).child(subtitle)),
                    )
                    .when(self.tab != Tab::History, |row| row.child(switcher))
                    .child(
                        Button::new("cleanup-export")
                            .ghost()
                            .small()
                            .tooltip(tr("cleanup_export_csv"))
                            .disabled(busy || !self.can_export())
                            .child(theme_icon(AtlasIcon::TrayArrowDown, 18., muted, windows))
                            .on_click(cx.listener(|p, _, _, cx| p.export_csv(cx))),
                    )
                    .child(
                        Button::new("cleanup-refresh")
                            .ghost()
                            .small()
                            .tooltip(tr("cleanup_refresh"))
                            .disabled(busy)
                            .child(theme_icon(
                                if windows {
                                    AtlasIcon::ArrowCounterClockwise
                                } else {
                                    AtlasIcon::ArrowClockwise
                                },
                                18.,
                                muted,
                                windows,
                            ))
                            .on_click(cx.listener(|p, _, _, cx| {
                                if p.tab == Tab::History {
                                    p.tab = Tab::Map;
                                }
                                p.fast_requested = cfg!(windows) && !p.network;
                                p.scan(cx);
                            })),
                    )
                    .child(
                        Button::new("cleanup-close")
                            .ghost()
                            .small()
                            .tooltip(tr("cleanup_close"))
                            .disabled(self.preparing)
                            .child(theme_icon(AtlasIcon::X, 18., fg, windows))
                            .on_click(cx.listener(|p, _, w, cx| p.close(w, cx))),
                    ),
            )
            .when_some(capacity.filter(|_| self.tab == Tab::Map), |v, fraction| {
                v.child(
                    div()
                        .w_full()
                        .h(crate::ui::rem_px(4.))
                        .rounded_full()
                        .bg(fg.opacity(0.09))
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .w(relative(fraction))
                                .rounded_full()
                                .bg(accent),
                        ),
                )
            })
            .into_any_element()
    }
}

pub(super) fn palette(cx: &gpui::App) -> [Hsla; 5] {
    let theme = cx.theme();
    [
        theme.accent,
        theme.yellow,
        theme.magenta,
        theme.cyan,
        theme.blue,
    ]
}

pub(super) fn file_color(path: Option<&Path>, directory: bool, colors: &[Hsla; 5]) -> Hsla {
    use crate::ui::icons::{file_type_icon_kind, FileTypeIcon};
    let extension = path
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let kind = if directory {
        FileKind::Dir
    } else {
        FileKind::File
    };
    match file_type_icon_kind(&kind, extension.as_deref()) {
        FileTypeIcon::Folder => colors[0],
        FileTypeIcon::Archive => colors[1],
        FileTypeIcon::Image | FileTypeIcon::Video => colors[2],
        FileTypeIcon::Audio => colors[3],
        _ => colors[4],
    }
}
