//! Custom theme copies are persisted independently of bundled presets.
use crate::app::App;
use crate::input::action::Action;
use crate::ui::rem_px;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, px, AnyElement, Context, Entity, FontWeight, Hsla, SharedString, Subscription, Window,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use gpui_component::{
    h_flex, v_flex, ActiveTheme, Colorize, Disableable, Sizable, ThemeConfig, ThemeRegistry,
    ThemeSet,
};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy)]
enum Role {
    Accent,
    Background,
    Foreground,
}

impl Role {
    fn label(self) -> String {
        rust_i18n::t!(match self {
            Self::Accent => "theme_color_accent",
            Self::Background => "theme_color_background",
            Self::Foreground => "theme_color_foreground",
        })
        .to_string()
    }
    fn get(self, config: &ThemeConfig) -> Option<&SharedString> {
        match self {
            Self::Accent => config.colors.accent.as_ref(),
            Self::Background => config.colors.background.as_ref(),
            Self::Foreground => config.colors.foreground.as_ref(),
        }
    }
    fn set(self, config: &mut ThemeConfig, color: Hsla) {
        let value = Some(color.to_hex().into());
        match self {
            Self::Accent => config.colors.accent = value,
            Self::Background => config.colors.background = value,
            Self::Foreground => config.colors.foreground = value,
        }
    }
}

pub(crate) struct ThemeEditor {
    config: ThemeConfig,
    /// Last state on disk; Discard restores it (edits preview live).
    saved: ThemeConfig,
    file_name: String,
    controls: Vec<(Role, Entity<ColorPickerState>)>,
    dirty: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

fn fork_config(base: &ThemeConfig, existing: impl Fn(&str) -> bool) -> ThemeConfig {
    let (stem, suffix) = base
        .name
        .strip_suffix(" Glass")
        .map_or((base.name.as_ref(), ""), |stem| (stem, " Glass"));
    let mut number = 1u64;
    let name = loop {
        let candidate = if number == 1 {
            format!("{stem} Copy{suffix}")
        } else {
            format!("{stem} Copy {number}{suffix}")
        };
        if !existing(&candidate) {
            break candidate;
        }
        number += 1;
    };
    let mut config = base.clone();
    config.name = name.into();
    config.is_default = false;
    config
}

fn theme_set(config: &ThemeConfig) -> ThemeSet {
    ThemeSet {
        name: config.name.clone(),
        themes: vec![config.clone()],
        ..ThemeSet::default()
    }
}

impl App {
    pub(crate) fn fork_active_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let registry = ThemeRegistry::global(cx);
        let Some(base) = registry
            .themes()
            .get(self.state.profiles.active().theme.as_str())
        else {
            self.theme_fork_error = Some(rust_i18n::t!("theme_fork_unavailable").to_string());
            cx.notify();
            return;
        };
        let config = fork_config(base, |name| registry.themes().contains_key(name));
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let file_name = format!(
            "custom-{timestamp:x}-{}-{}.json",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        if let Err(error) =
            atlas_core::util::config::save_custom_theme(&file_name, &theme_set(&config))
        {
            self.theme_fork_error =
                Some(rust_i18n::t!("theme_save_error", error = error.to_string()).to_string());
            cx.notify();
            return;
        }
        let mut editor = ThemeEditor {
            config: config.clone(),
            saved: config.clone(),
            file_name,
            controls: Vec::new(),
            dirty: false,
            error: None,
            _subscriptions: Vec::new(),
        };
        for role in [Role::Accent, Role::Background, Role::Foreground] {
            let fallback = match role {
                Role::Accent => cx.theme().accent,
                Role::Background => cx.theme().background,
                Role::Foreground => cx.theme().foreground,
            };
            let color = role
                .get(&config)
                .and_then(|value| Hsla::parse_hex(value).ok())
                .unwrap_or(fallback);
            let animate = !self.state.settings.disable_animations;
            let control = cx.new(|cx| {
                ColorPickerState::new(window, cx)
                    .default_value(color)
                    .default_angular()
                    .angular_animation(animate)
            });
            editor._subscriptions.push(cx.subscribe(
                &control,
                move |this, _, event: &ColorPickerEvent, cx| {
                    if let ColorPickerEvent::Change(Some(color)) = event {
                        if let Some(editor) = &mut this.theme_editor {
                            role.set(&mut editor.config, *color);
                            editor.dirty = true;
                            editor.error = None;
                            // Live preview: the fork is the active theme.
                            ThemeRegistry::global_mut(cx)
                                .register_custom_theme(editor.config.clone());
                            this.theme_dirty = true;
                        }
                        this.notify_all_panes(cx);
                        cx.notify();
                    }
                },
            ));
            editor.controls.push((role, control));
        }
        ThemeRegistry::global_mut(cx).register_custom_theme(config.clone());
        self.set_theme(config.name.to_string());
        self.theme_editor = Some(editor);
        self.theme_fork_error = None;
        cx.notify();
    }

    fn save_theme_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.theme_editor else {
            return;
        };
        if !editor.dirty {
            return;
        }
        if let Err(error) = atlas_core::util::config::save_custom_theme(
            &editor.file_name,
            &theme_set(&editor.config),
        ) {
            editor.error =
                Some(rust_i18n::t!("theme_save_error", error = error.to_string()).to_string());
        } else {
            ThemeRegistry::global_mut(cx).register_custom_theme(editor.config.clone());
            editor.saved = editor.config.clone();
            editor.dirty = false;
            editor.error = None;
            let name = editor.config.name.to_string();
            self.set_theme(name);
        }
        cx.notify();
    }

    /// Close the editor; unsaved live edits revert to the file on disk.
    pub(crate) fn close_theme_editor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.theme_editor.take() {
            if editor.dirty {
                ThemeRegistry::global_mut(cx).register_custom_theme(editor.saved);
                self.theme_dirty = true;
                self.notify_all_panes(cx);
            }
        }
        cx.notify();
    }

    pub(crate) fn render_theme_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, danger, border, primary) = (
            theme.popover_foreground,
            theme.danger,
            theme.border,
            theme.primary,
        );
        let muted = fg.opacity(0.62);
        let (bg, accent, text) = (theme.background, theme.primary, theme.foreground);
        let editing = self.theme_editor.is_some();
        let name = self.state.profiles.active().theme.clone();

        // A miniature window painted with the live theme colours.
        let line = |w: f32, alpha: f32| {
            div()
                .h(rem_px(4.))
                .w(rem_px(w))
                .rounded_full()
                .bg(text.opacity(alpha))
        };
        let preview = v_flex()
            .flex_shrink_0()
            .w(rem_px(176.))
            .h(rem_px(112.))
            .rounded(rem_px(10.))
            .overflow_hidden()
            .bg(bg)
            .border_1()
            .border_color(border)
            .shadow_md()
            .child(
                h_flex()
                    .h(rem_px(16.))
                    .px(rem_px(6.))
                    .gap(rem_px(4.))
                    .items_center()
                    .border_b_1()
                    .border_color(text.opacity(0.08))
                    .children(
                        [0.5, 0.35, 0.2]
                            .map(|a| div().size(rem_px(5.)).rounded_full().bg(text.opacity(a))),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .child(
                        v_flex()
                            .w(rem_px(44.))
                            .h_full()
                            .p(rem_px(6.))
                            .gap(rem_px(6.))
                            .bg(text.opacity(0.04))
                            .child(line(28., 0.35))
                            .child(line(22., 0.2))
                            .child(line(26., 0.2)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .p(rem_px(8.))
                            .gap(rem_px(7.))
                            .child(
                                h_flex()
                                    .gap(rem_px(5.))
                                    .child(div().size(rem_px(6.)).rounded_sm().bg(accent))
                                    .child(line(60., 0.55)),
                            )
                            .child(
                                h_flex()
                                    .px(rem_px(4.))
                                    .py(rem_px(3.))
                                    .rounded(rem_px(3.))
                                    .bg(accent.opacity(0.22))
                                    .child(line(70., 0.6)),
                            )
                            .child(line(54., 0.3))
                            .child(line(64., 0.3))
                            .child(
                                h_flex().justify_end().child(
                                    div()
                                        .h(rem_px(10.))
                                        .w(rem_px(28.))
                                        .rounded_full()
                                        .bg(accent),
                                ),
                            ),
                    ),
            );

        let hero = h_flex()
            .w_full()
            .gap(rem_px(18.))
            .p(rem_px(16.))
            .items_center()
            .rounded(rem_px(12.))
            .border_1()
            .border_color(fg.opacity(0.08))
            .bg(fg.opacity(0.03))
            .child(preview)
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .gap(rem_px(4.))
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child(rust_i18n::t!("sp_theme_active").to_string()),
                    )
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(name),
                    )
                    .child(
                        div().text_xs().text_color(muted).child(
                            rust_i18n::t!(if editing {
                                "theme_editor_description"
                            } else {
                                "theme_fork_description"
                            })
                            .to_string(),
                        ),
                    )
                    .child(
                        h_flex()
                            .pt(rem_px(8.))
                            .gap(rem_px(8.))
                            .child(
                                Button::new("fork-active-theme")
                                    .small()
                                    .primary()
                                    .label(rust_i18n::t!("theme_fork").to_string())
                                    .disabled(editing)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.fork_active_theme(window, cx)
                                    })),
                            )
                            .child(
                                // Custom theme JSON lives here and hot-reloads; opening it in
                                // Atlas lets people share, import or delete their themes.
                                Button::new("open-themes-folder")
                                    .small()
                                    .ghost()
                                    .label(rust_i18n::t!("theme_open_folder").to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let path = atlas_core::util::config::themes_dir();
                                        let pane = this.state.focused_tile;
                                        this.close_settings_state();
                                        this.process_actions([Action::NewTabWithPath {
                                            pane,
                                            path,
                                        }]);
                                        cx.notify();
                                    })),
                            ),
                    ),
            );

        let mut content = v_flex()
            .w_full()
            .gap(rem_px(14.))
            .py(rem_px(4.))
            .child(hero);
        if let Some(error) = &self.theme_fork_error {
            content = content.child(div().text_sm().text_color(danger).child(error.clone()));
        }
        let Some(editor) = &self.theme_editor else {
            return content.into_any_element();
        };

        content = content.child(
            h_flex()
                .px(rem_px(4.))
                .gap(rem_px(6.))
                .items_center()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(muted)
                .child(rust_i18n::t!("sp_theme_colors").to_string().to_uppercase())
                .when(editor.dirty, |row| {
                    row.child(div().size(rem_px(6.)).rounded_full().bg(primary))
                        .child(
                            div()
                                .font_weight(FontWeight::NORMAL)
                                .child(rust_i18n::t!("sp_theme_unsaved").to_string()),
                        )
                }),
        );
        for (role, control) in &editor.controls {
            let value = control.read(cx).value();
            let hex = value.map(|value| value.to_hex()).unwrap_or_default();
            content = content.child(
                h_flex()
                    .w_full()
                    .h(rem_px(52.))
                    .px(rem_px(10.))
                    .gap(rem_px(12.))
                    .items_center()
                    .rounded(rem_px(8.))
                    .hover(|h| h.bg(fg.opacity(0.045)))
                    .child(
                        div()
                            .size(rem_px(30.))
                            .flex_shrink_0()
                            .rounded(rem_px(8.))
                            .border_1()
                            .border_color(fg.opacity(0.15))
                            .when_some(value, |d, color| d.bg(color)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .child(div().text_sm().text_color(fg).child(role.label()))
                            .child(div().text_xs().text_color(muted).child(hex.clone())),
                    )
                    .child(ColorPicker::new(control).small().label(hex)),
            );
        }
        if let Some(error) = &editor.error {
            content = content.child(div().text_sm().text_color(danger).child(error.clone()));
        }
        content
            .child(
                h_flex()
                    .w_full()
                    .pt(rem_px(6.))
                    .gap(rem_px(8.))
                    .justify_end()
                    .child(
                        Button::new("close-theme-editor")
                            .small()
                            .ghost()
                            .label(
                                rust_i18n::t!(if editor.dirty {
                                    "theme_discard"
                                } else {
                                    "theme_close_editor"
                                })
                                .to_string(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.close_theme_editor(cx))),
                    )
                    .child(
                        Button::new("save-custom-theme")
                            .primary()
                            .small()
                            .label(rust_i18n::t!("theme_save").to_string())
                            .disabled(!editor.dirty)
                            .on_click(cx.listener(|this, _, _, cx| this.save_theme_editor(cx))),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_fork_preserves_material_fonts_and_colors_with_a_unique_name() {
        let set: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        let source = set
            .themes
            .iter()
            .find(|theme| theme.name.ends_with(" Glass"))
            .unwrap();
        let first = fork_config(source, |_| false);
        let second = fork_config(source, |name| name == first.name.as_ref());
        assert_ne!(first.name, second.name);
        assert!(first.name.ends_with(" Glass"));
        assert!(second.name.ends_with(" Glass"));
        assert!(!first.is_default);
        assert_eq!(
            serde_json::to_value(&first.colors).unwrap(),
            serde_json::to_value(&source.colors).unwrap()
        );
        assert_eq!(first.font_family, source.font_family);
        assert_eq!(first.mode, source.mode);
        let serialized = serde_json::to_string(&theme_set(&first)).unwrap();
        let restored: ThemeSet = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored.themes[0].name, first.name);
    }
}
