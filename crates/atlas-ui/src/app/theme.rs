//! Theme application glue. The active profile stores a theme *name*; here we map
//! it onto gpui-component's global `Theme` via the `ThemeRegistry`. Applying needs
//! `&mut gpui::App` + a `Window`, which `process_actions` doesn't have, so callers
//! (palette, settings, profile switch) just set the name and flip `theme_dirty`;
//! the next render calls [`App::apply_active_theme`].

use crate::app::App;
use atlas_model::profiles::{ThemeAppearanceFeature, DEFAULT_THEME};
use gpui::{App as GpuiApp, SharedString, Window, WindowAppearance, WindowBackgroundAppearance};
use gpui_component::scroll::ScrollbarShow;
use gpui_component::{Theme, ThemeColor, ThemeMode, ThemeRegistry};

/// Retired presets stay out of every picker, including copies in custom theme files.
pub(crate) fn theme_is_available(name: &str) -> bool {
    !matches!(
        name,
        "Default Light"
            | "Default Dark"
            | "Atlas Frost"
            | "Atlas Frost Glass"
            | "Atlas Figma Glass"
            | "Atlas Glass"
    )
}

fn migrated_theme_name(name: &str) -> &str {
    match name {
        "Default Dark" => "Atlas Graphite Glass",
        name if !theme_is_available(name) => DEFAULT_THEME,
        name => name,
    }
}

fn persisted_theme_name<'a>(stored: &str, wanted: &'a str, resolved: &'a str, custom_themes_loaded: bool) -> Option<&'a str> {
    // Migrations are authoritative immediately. Unknown custom themes are only
    // normalized after the asynchronous user-theme directory has been loaded.
    let permanent = custom_themes_loaded || wanted == resolved || wanted != stored;
    (permanent && resolved != stored).then_some(resolved)
}

/// Assign alpha once, after loading a pristine palette. Multiplying the JSON's
/// existing alpha makes materials vary by preset and compounds on each update.
fn material_color(mut color: gpui::Hsla, alpha: f32) -> gpui::Hsla {
    color.alpha = alpha;
    color
}

/// Dark Glass canvas alpha = BASE + opacity * SCALE. The floor keeps the
/// lowest density close to the solid theme: below it a saturated wallpaper
/// floods the sides and the dark panes read as slabs cut out of it.
const DARK_CANVAS_BASE: f32 = 0.40;
const DARK_CANVAS_SCALE: f32 = 0.25;
/// Light Glass canvas alpha = BASE + opacity * SCALE (sides of the window).
/// Light ink reads well through thin white, so the sides stay as airy as the
/// dark presets instead of a near-opaque white sheet.
const LIGHT_CANVAS_BASE: f32 = 0.22;
const LIGHT_CANVAS_SCALE: f32 = 0.50;
/// Pane alpha on top of the canvas = BASE + SPAN * opacity^PANE_CURVE.
/// The curve keeps the low end almost flat and lets density rise gently.
const DARK_PANE_BASE: f32 = 0.14;
const DARK_PANE_SPAN: f32 = 0.24;
const LIGHT_PANE_BASE: f32 = 0.18;
const LIGHT_PANE_SPAN: f32 = 0.40;
const PANE_CURVE: f32 = 1.5;

/// The lowest and highest canvas alpha the Glass opacity setting can produce,
/// so floating controls can follow the same density within their own range.
pub(crate) fn glass_canvas_range(dark: bool) -> (f32, f32) {
    if dark {
        (DARK_CANVAS_BASE + 0.2 * DARK_CANVAS_SCALE, DARK_CANVAS_BASE + DARK_CANVAS_SCALE)
    } else {
        (LIGHT_CANVAS_BASE + 0.2 * LIGHT_CANVAS_SCALE, LIGHT_CANVAS_BASE + LIGHT_CANVAS_SCALE)
    }
}

fn apply_frosted_material(colors: &mut ThemeColor, mode: ThemeMode, opacity: f32) {
    let opacity = if opacity.is_finite() {
        opacity.clamp(0.20, 1.0)
    } else {
        atlas_model::settings::default_glass_opacity()
    };
    // One backing tint; panes are the main material. Internal strips and controls
    // use small overlay weights so nested backgrounds don't wash out the frost.
    // The window canvas carries the sidebar and preview directly, so its tint
    // is the visible frost; the Glass opacity setting scales it.
    // Light ink needs a denser canvas than dark ink to stay readable over a
    // blurred (mid-tone) wallpaper; both scale with the Glass opacity setting.
    let canvas = if mode.is_dark() {
        DARK_CANVAS_BASE + opacity * DARK_CANVAS_SCALE
    } else {
        LIGHT_CANVAS_BASE + opacity * LIGHT_CANVAS_SCALE
    };
    colors.background = material_color(colors.background, canvas);
    // Pane cards sit on that canvas, so their visible density compounds with
    // it. Keep them one step above the sides (containers that still let the
    // wallpaper through), not an opaque slab next to translucent sides.
    let weighted = opacity.powf(PANE_CURVE);
    let content_opacity = if mode.is_dark() {
        DARK_PANE_BASE + DARK_PANE_SPAN * weighted
    } else {
        LIGHT_PANE_BASE + LIGHT_PANE_SPAN * weighted
    };
    colors.list = material_color(colors.list, content_opacity);
    colors.table = material_color(colors.table, content_opacity);
    // Sidebar and preview have no surface of their own: they are the canvas.
    colors.sidebar = material_color(colors.sidebar, canvas);
    colors.title_bar = material_color(colors.title_bar, opacity * 0.18);
    colors.tab_bar = material_color(colors.tab_bar, opacity * 0.12);
    colors.tab_active = material_color(colors.tab_active, opacity * 0.32);
    colors.tab_bar_segmented = material_color(colors.tab_bar_segmented, opacity * 0.08);
    colors.list_head = material_color(colors.list_head, opacity * 0.14);
    colors.table_head = material_color(colors.table_head, opacity * 0.14);
    colors.list_even = material_color(colors.list_even, opacity * 0.035);
    colors.table_even = material_color(colors.table_even, opacity * 0.035);
    colors.secondary = material_color(colors.secondary, opacity * 0.20);
    colors.secondary_hover = material_color(colors.secondary_hover, opacity * 0.28);
    colors.secondary_active = material_color(colors.secondary_active, opacity * 0.36);
    colors.muted = material_color(colors.muted, opacity * 0.18);
    // State overlays have their own contrast, independent of material transparency.
    colors.list_hover = material_color(colors.foreground, 0.07);
    colors.table_hover = colors.list_hover;
    // Menus/dialogs need a dense surface even when the workspace is very airy.
    colors.popover = material_color(colors.popover, 0.90 + opacity * 0.04);
    // Functional separators remain; outer chrome has no decorative rim.
    colors.border = material_color(colors.foreground, 0.10);
    colors.input = material_color(colors.foreground, 0.16);
    colors.sidebar_border = gpui::transparent_black();
    colors.title_bar_border = gpui::transparent_black();
    colors.window_border = gpui::transparent_black();
}

/// Solid themes with a translucent canvas (Atlas Ember): the opacity setting
/// still drives the canvas and the pane cards, weighted so the low end opens
/// the cards up noticeably while 100% keeps the theme's opaque cards. Chrome
/// and control colours stay exactly as the theme defines them.
fn apply_translucent_solid_material(colors: &mut ThemeColor, opacity: f32) {
    let opacity = if opacity.is_finite() {
        opacity.clamp(0.20, 1.0)
    } else {
        atlas_model::settings::default_glass_opacity()
    };
    let canvas = (SOLID_CANVAS_BASE + SOLID_CANVAS_SCALE * opacity).min(colors.background.alpha);
    colors.background = material_color(colors.background, canvas);
    let pane = SOLID_PANE_BASE + SOLID_PANE_SPAN * opacity.powf(PANE_CURVE);
    for c in [
        &mut colors.list,
        &mut colors.table,
        &mut colors.list_even,
        &mut colors.table_even,
        &mut colors.list_head,
        &mut colors.table_head,
        &mut colors.tab_bar,
    ] {
        *c = material_color(*c, c.alpha.min(pane));
    }
}

const SOLID_CANVAS_BASE: f32 = 0.45;
const SOLID_CANVAS_SCALE: f32 = 0.45;
const SOLID_PANE_BASE: f32 = 0.22;
const SOLID_PANE_SPAN: f32 = 0.78;

/// Whether the OS asks for a dark appearance (Windows: "Choose your app mode").
pub(crate) fn system_prefers_dark(window: &Window) -> bool {
    matches!(
        window.appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// Light/dark pairing helpers: the profile remembers one light and one dark
/// theme, can follow the system appearance, and can swap between the two.
impl App {
    pub(crate) fn theme_follows_system(&self) -> bool {
        self.state
            .profiles
            .active()
            .scoped::<ThemeAppearanceFeature>()
            .follow_system
    }

    /// The profile's theme for a light (`dark == false`) or dark appearance.
    pub(crate) fn appearance_theme(&self, dark: bool) -> String {
        self.state
            .profiles
            .active()
            .scoped::<ThemeAppearanceFeature>()
            .theme_for(dark)
    }

    pub(crate) fn set_theme_follow_system(&mut self, follow: bool) {
        let mut pairing = self.state.profiles.active().scoped::<ThemeAppearanceFeature>();
        if pairing.follow_system == follow {
            return;
        }
        pairing.follow_system = follow;
        self.state
            .profiles
            .active_mut()
            .set_scoped::<ThemeAppearanceFeature>(&pairing);
        if follow {
            // Switch to the theme for the current system appearance now.
            self.theme_follow_pending = true;
            self.theme_dirty = true;
        }
        self.save();
    }

    /// Swap between the profile's light and dark theme. With "follow system"
    /// on this is a manual override until the system appearance next changes.
    pub(crate) fn toggle_theme_appearance(&mut self) {
        // `light_surfaces` mirrors the theme on screen: go to the other one.
        let target = self.appearance_theme(self.light_surfaces);
        self.set_theme(target);
    }

    /// Settings › Light theme / Dark theme. Stores the pairing and, when that
    /// appearance is the one in use, shows the new theme right away.
    pub(crate) fn pick_appearance_theme(&mut self, name: &str, dark: bool) {
        let committed = self
            .theme_preview_before
            .clone()
            .unwrap_or_else(|| self.state.profiles.active().theme.clone());
        let mut pairing = self.state.profiles.active().scoped::<ThemeAppearanceFeature>();
        // After every apply the theme on screen is its appearance's choice, so
        // this tells whether `dark` is the appearance currently shown.
        let in_use = pairing.theme_for(dark) == committed;
        self.settings_theme_previewing = false;
        if pairing.remember(name, dark) {
            self.state
                .profiles
                .active_mut()
                .set_scoped::<ThemeAppearanceFeature>(&pairing);
        }
        if in_use {
            self.set_theme(name.to_string());
        } else {
            self.restore_theme_preview();
            self.save();
        }
    }

    /// Adopt the system appearance's theme when the profile follows it.
    fn follow_system_appearance(&mut self, window: &Window) {
        if self.theme_preview_before.is_some() {
            // A Settings preview owns the screen; sync once it ends.
            self.theme_follow_pending = true;
            return;
        }
        let pairing = self.state.profiles.active().scoped::<ThemeAppearanceFeature>();
        if !pairing.follow_system {
            return;
        }
        let target = pairing.theme_for(system_prefers_dark(window));
        if self.state.profiles.active().theme != target {
            self.state.profiles.active_mut().theme = target;
            self.save();
        }
    }

    /// Remember the theme just applied as the choice for its appearance.
    fn remember_applied_theme(&mut self, name: &str, dark: bool) {
        let mut pairing = self.state.profiles.active().scoped::<ThemeAppearanceFeature>();
        if pairing.remember(name, dark) {
            self.state
                .profiles
                .active_mut()
                .set_scoped::<ThemeAppearanceFeature>(&pairing);
            self.save();
        }
    }
}

impl App {
    /// Suffix that marks a theme as the translucent "Glass" variant of a base
    /// theme. Paired by name: `"Atlas Midnight"` ↔ `"Atlas Midnight Glass"`.
    const GLASS_SUFFIX: &'static str = " Glass";

    /// Record `name` as the active profile's theme, persist, and request that the
    /// next render apply it. Cheap and cx-free so it can run inside `process_actions`.
    pub(crate) fn set_theme(&mut self, name: String) {
        self.theme_preview_before = None;
        self.state.profiles.active_mut().theme = name;
        self.theme_dirty = true;
        self.save();
    }

    pub(crate) fn preview_theme(&mut self, name: String) {
        if self.theme_preview_before.is_none() {
            self.theme_preview_before = Some(self.state.profiles.active().theme.clone());
        }
        if self.state.profiles.active().theme != name {
            self.state.profiles.active_mut().theme = name;
            self.theme_dirty = true;
        }
    }

    pub(crate) fn restore_theme_preview(&mut self) {
        let Some(name) = self.theme_preview_before.take() else {
            return;
        };
        if self.state.profiles.active().theme != name {
            self.state.profiles.active_mut().theme = name;
            self.theme_dirty = true;
        }
    }

    /// Apply the active profile's theme to the global gpui-component `Theme`.
    /// Falls back to [`DEFAULT_THEME`] when the stored name isn't registered (e.g.
    /// legacy enum values, or a custom theme file that was removed), rewriting the
    /// profile so the UI reflects what's actually shown.
    pub(crate) fn apply_active_theme(&mut self, window: &mut Window, cx: &mut GpuiApp) {
        if std::mem::take(&mut self.theme_follow_pending) {
            self.follow_system_appearance(window);
        }
        let wanted = migrated_theme_name(&self.state.profiles.active().theme).to_string();
        let registry = ThemeRegistry::global(cx);
        let custom_themes_loaded = registry.custom_themes_loaded();

        // Try to look up the (possibly redirected) name, falling back to DEFAULT_THEME
        // when the stored name isn't registered (legacy enum values, removed theme, …).
        let (resolved_name, config) =
            if let Some(cfg) = registry.themes().get(&SharedString::from(wanted.as_str())) {
                (wanted.clone(), cfg.clone())
            } else if let Some(cfg) = registry.themes().get(&SharedString::from(DEFAULT_THEME)) {
                (DEFAULT_THEME.to_string(), cfg.clone())
            } else {
                // No themes registered at all — nothing to apply.
                return;
            };

        let wants_backdrop = resolved_name.ends_with(Self::GLASS_SUFFIX);

        // Enable the OS backdrop for explicit glass themes. This is Atlas'
        // equivalent of Zed's `background.appearance=blurred`.
        window.set_background_appearance(if wants_backdrop {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        });

        // Persist the normalized name so the migration is one-time and sticky.
        if let Some(name) = persisted_theme_name(&self.state.profiles.active().theme, &wanted, &resolved_name, custom_themes_loaded) {
            self.state.profiles.active_mut().theme = name.to_string();
            self.save();
        }

        // Only a committed theme that was really found (not a preview, not the
        // fallback shown while custom themes still load) updates the pairing.
        if self.theme_preview_before.is_none() && resolved_name == wanted {
            self.remember_applied_theme(&resolved_name, config.mode.is_dark());
        }

        Theme::global_mut(cx).apply_config(&config);
        let light = !config.mode.is_dark();
        if self.light_surfaces != light {
            self.light_surfaces = light;
            // Cached rows/tabs painted the other icon variant.
            self.relist_panes(|_| true);
        }
        // Solid themes may still use a translucent canvas (e.g. Atlas Ember's
        // sidebar/preview): blur the desktop behind it, keep the cards opaque.
        if !wants_backdrop && Theme::global(cx).colors.background.alpha < 0.999 {
            window.set_background_appearance(WindowBackgroundAppearance::Blurred);
            apply_translucent_solid_material(
                &mut Theme::global_mut(cx).colors,
                self.state.settings.glass_opacity,
            );
        }
        if wants_backdrop {
            apply_frosted_material(
                &mut Theme::global_mut(cx).colors,
                config.mode,
                self.state.settings.glass_opacity,
            );
        }
        // Re-apply zoom so Root's per-frame set_rem_size(theme.font_size) picks up
        // the user's scale instead of the theme default (px(16.)).
        Theme::global_mut(cx).font_size = gpui::px(16.0 * self.state.settings.global_zoom);
        // Always show scrollbar on hover (not only while scrolling) and boost thumb
        // opacity so it's actually visible against glass backgrounds.
        {
            let t = Theme::global_mut(cx);
            t.scrollbar_show = ScrollbarShow::Hover;
            // The text caret follows the foreground: it must read on every theme
            // (the default falls back to the accent, which can vanish on a tinted row).
            t.caret = t.foreground;
            t.scrollbar = gpui::transparent_black();
            let base = t.scrollbar_thumb;
            t.scrollbar_thumb = gpui::hsla(
                (base.hue.into_positive_degrees() / 360.0),
                base.saturation,
                base.lightness,
                0.45,
            );
            let hover_base = t.scrollbar_thumb_hover;
            t.scrollbar_thumb_hover = gpui::hsla(
                (hover_base.hue.into_positive_degrees() / 360.0),
                hover_base.saturation,
                hover_base.lightness,
                0.75,
            );
        }
        window.refresh();
    }
}

#[cfg(test)]
mod gpui_ce_tests {
    use super::{apply_frosted_material, migrated_theme_name, theme_is_available};
    use gpui_component::{Colorize, ThemeSet};

    #[test]
    fn delayed_custom_theme_loading_does_not_overwrite_the_saved_selection() {
        use super::persisted_theme_name;
        let custom = "Atlas Graphite Copy Glass";
        let fallback = atlas_model::profiles::DEFAULT_THEME;
        assert_eq!(persisted_theme_name(custom, custom, fallback, false), None);
        assert_eq!(persisted_theme_name(custom, custom, custom, true), None);
        assert_eq!(persisted_theme_name(custom, custom, fallback, true), Some(fallback));
        assert_eq!(persisted_theme_name("Default Dark", "Atlas Graphite Glass", "Atlas Graphite Glass", false), Some("Atlas Graphite Glass"));
    }

    #[test]
    fn retired_presets_are_hidden_and_saved_names_migrate() {
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        assert!(themes
            .themes
            .iter()
            .all(|t| theme_is_available(t.name.as_ref())));
        assert_eq!(themes.themes.iter().filter(|t| t.is_default).count(), 1);
        for old_name in [
            "Default Light",
            "Default Dark",
            "Atlas Frost",
            "Atlas Frost Glass",
            "Atlas Figma Glass",
            "Atlas Glass",
        ] {
            assert!(!theme_is_available(old_name));
            let replacement = migrated_theme_name(old_name);
            assert!(themes.themes.iter().any(|t| t.name.as_ref() == replacement));
        }
        assert_eq!(migrated_theme_name("Default Dark"), "Atlas Graphite Glass");
        assert_eq!(migrated_theme_name("Custom Glass"), "Custom Glass");
    }

    #[test]
    fn every_glass_palette_updates_without_compounding_or_tinting_text() {
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        for config in themes
            .themes
            .into_iter()
            .filter(|t| t.name.ends_with(" Glass"))
        {
            let config = std::rc::Rc::new(config);
            let mut theme = gpui_component::Theme::default();
            theme.apply_config(&config);
            let palette = theme.colors;
            apply_frosted_material(&mut theme.colors, config.mode, 0.8);
            let initial = serde_json::to_value(theme.colors).unwrap();
            apply_frosted_material(&mut theme.colors, config.mode, 0.4);
            // Sides are the canvas; a pane composited on it is one step denser,
            // never an opaque slab next to translucent sides.
            assert_eq!(theme.colors.sidebar.alpha, theme.colors.background.alpha, "{}", config.name);
            let canvas = theme.colors.background.alpha;
            let pane = 1.0 - (1.0 - canvas) * (1.0 - theme.colors.list.alpha);
            if config.mode.is_dark() {
                assert!(pane > canvas + 0.1, "{}", config.name);
                assert!(pane < 0.85, "{}", config.name);
            } else {
                // Light keeps a readability floor; the pane is still a step above.
                assert!(pane > canvas + 0.05, "{}", config.name);
            }
            assert!(theme.colors.tab_bar.alpha < theme.colors.sidebar.alpha);
            assert!(theme.colors.popover.alpha > 0.9);
            assert!(
                crate::ui::glass::layer_color(theme.colors.secondary, 0.04, 0.92).alpha
                    < theme.colors.list.alpha
            );
            assert_eq!(theme.colors.foreground, palette.foreground);
            assert_eq!(theme.colors.accent, palette.accent);
            assert_eq!(theme.colors.selection, palette.selection);
            assert_eq!(theme.colors.list.hue, palette.list.hue);
            assert_eq!(theme.colors.list.lightness, palette.list.lightness);
            assert_eq!(theme.colors.window_border.alpha, 0.0);
            assert_eq!(theme.colors.sidebar_border.alpha, 0.0);
            apply_frosted_material(&mut theme.colors, config.mode, 0.8);
            assert_eq!(serde_json::to_value(theme.colors).unwrap(), initial);
            // Reapplying a stored preset (close/reopen settings or profile switch)
            // must keep the chosen material; switching to solid restores its palette.
            theme.apply_config(&config);
            apply_frosted_material(&mut theme.colors, config.mode, 0.8);
            assert_eq!(serde_json::to_value(theme.colors).unwrap(), initial);
            theme.apply_config(&config);
            assert_eq!(
                serde_json::to_value(theme.colors).unwrap(),
                serde_json::to_value(palette).unwrap()
            );
        }
    }

    #[test]
    fn translucent_solid_theme_panes_follow_the_opacity_setting() {
        use super::apply_translucent_solid_material;
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        let config = std::rc::Rc::new(
            themes.themes.into_iter().find(|t| t.name.as_ref() == "Atlas Ember").unwrap(),
        );
        let at = |opacity: f32| {
            let mut theme = gpui_component::Theme::default();
            theme.apply_config(&config);
            apply_translucent_solid_material(&mut theme.colors, opacity);
            theme.colors
        };
        let (low, mid, full) = (at(0.2), at(0.6), at(1.0));
        assert!(low.list.alpha < mid.list.alpha && mid.list.alpha < full.list.alpha);
        assert!(low.background.alpha < full.background.alpha);
        // Weighted: the low half of the slider opens the cards up the most.
        assert!(low.list.alpha < 0.35);
        assert!((full.list.alpha - 1.0).abs() < 1e-4);
        assert_eq!(low.list.lightness, full.list.lightness);
        assert_eq!(low.foreground, full.foreground);
    }

    #[test]
    fn bundled_themes_keep_hex_colors_with_gpui_ce() {
        for content in [
            include_str!("../../assets/themes/atlas-themes.json"),
            include_str!(
                "../../../../vendor/gpui-component-fork/crates/ui/src/theme/default-theme.json"
            ),
        ] {
            let themes: ThemeSet =
                serde_json::from_str(content).expect("existing themes must load");
            assert!(!themes.themes.is_empty());
            let serialized = serde_json::to_string(&themes).expect("theme serialization");
            let restored: ThemeSet = serde_json::from_str(&serialized).expect("theme round trip");
            assert_eq!(restored.themes.len(), themes.themes.len());
            for (before, after) in themes.themes.iter().zip(restored.themes.iter()) {
                assert_eq!(before.name, after.name);
            }
        }
    }

    #[test]
    fn floating_controls_stay_readable_at_every_glass_density() {
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        for config in themes
            .themes
            .into_iter()
            .filter(|t| t.name.ends_with(" Glass"))
        {
            let config = std::rc::Rc::new(config);
            for opacity in [0.2, 0.4, 0.8, 1.0, f32::NAN] {
                let mut theme = gpui_component::Theme::default();
                theme.apply_config(&config);
                let original = theme.colors;
                apply_frosted_material(&mut theme.colors, config.mode, opacity);
                let controls = crate::ui::glass::control_colors(&theme, true);
                let (min_alpha, max_alpha) = if config.mode.is_dark() {
                    (0.54, 0.72)
                } else {
                    (0.62, 0.78)
                };
                assert!(
                    (min_alpha..=max_alpha).contains(&controls.surface.alpha),
                    "{}",
                    config.name
                );
                assert_eq!(
                    crate::ui::glass::control_colors(&theme, false)
                        .surface
                        .alpha,
                    1.0
                );
                assert_eq!(controls.foreground, original.foreground);
                assert_eq!(controls.muted, original.muted_foreground);
                assert!(controls.hover.alpha > theme.colors.border.alpha * 0.42);
                assert!(controls.pressed.alpha > controls.hover.alpha);
                if !config.mode.is_dark() {
                    // What the eye sees: sides = canvas; panes = list over canvas.
                    let canvas = theme.colors.background.alpha;
                    let pane = 1.0 - (1.0 - canvas) * (1.0 - theme.colors.list.alpha);
                    // Readability itself is proven by the contrast test; here the
                    // light material must stay translucent yet layered.
                    assert!(canvas < 0.85 && pane > canvas, "{}", config.name);
                    assert_eq!(theme.colors.sidebar.alpha, canvas);
                }
            }
        }
    }

    #[test]
    fn panes_and_capsules_follow_the_glass_slider_gently() {
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        for config in themes.themes.into_iter().filter(|t| t.name.ends_with(" Glass")) {
            let config = std::rc::Rc::new(config);
            let mut previous: Option<(f32, f32)> = None;
            for opacity in [0.2, 0.4, 0.6, 0.8, 1.0] {
                let mut theme = gpui_component::Theme::default();
                theme.apply_config(&config);
                apply_frosted_material(&mut theme.colors, config.mode, opacity);
                let canvas = theme.colors.background.alpha;
                let pane = 1.0 - (1.0 - canvas) * (1.0 - theme.colors.list.alpha);
                let capsule = crate::ui::glass::control_colors(&theme, true).surface.alpha;
                assert!(capsule >= 0.54, "{}: capsule keeps a backing", config.name);
                if let Some((prev_pane, prev_capsule)) = previous {
                    assert!(pane > prev_pane, "{} @{opacity}: pane must densify", config.name);
                    assert!(pane - prev_pane < 0.12, "{} @{opacity}: pane step too abrupt", config.name);
                    assert!(capsule > prev_capsule, "{} @{opacity}: capsule must adapt", config.name);
                    assert!(capsule - prev_capsule < 0.06, "{} @{opacity}: capsule step too abrupt", config.name);
                }
                previous = Some((pane, capsule));
            }
        }
    }

    #[test]
    fn dark_glass_text_stays_readable_over_a_bright_wallpaper() {
        fn composite(color: gpui::Rgba, base: gpui::Rgba) -> gpui::Rgba {
            gpui::Rgba::new(
                color.red * color.alpha + base.red * (1.0 - color.alpha),
                color.green * color.alpha + base.green * (1.0 - color.alpha),
                color.blue * color.alpha + base.blue * (1.0 - color.alpha),
                1.0,
            )
        }
        fn luminance(color: gpui::Rgba) -> f32 {
            let linear = |v: f32| {
                if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * linear(color.red) + 0.7152 * linear(color.green) + 0.0722 * linear(color.blue)
        }
        fn contrast(a: gpui::Rgba, b: gpui::Rgba) -> f32 {
            let (la, lb) = (luminance(a) + 0.05, luminance(b) + 0.05);
            la.max(lb) / la.min(lb)
        }
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        for config in themes
            .themes
            .into_iter()
            .filter(|t| t.mode.is_dark() && t.name.ends_with(" Glass"))
        {
            let config = std::rc::Rc::new(config);
            let mut theme = gpui_component::Theme::default();
            theme.apply_config(&config);
            // Some palettes ship low-contrast muted ink by design (Dracula's
            // comment color); glass must preserve most of it, not exceed it.
            let solid = theme.colors.background.to_rgb();
            let solid_muted = contrast(composite(theme.muted_foreground.to_rgb(), solid), solid);
            let muted_floor = (solid_muted * 0.85).min(3.0);
            // Over a blurred, fairly bright wallpaper: primary text must read even
            // at the lowest density; secondary text at the default density.
            let wallpaper = gpui::Rgba::new(0.45, 0.45, 0.45, 1.0);
            for (opacity, check_muted) in [(0.2, false), (0.8, true)] {
                let mut theme = gpui_component::Theme::default();
                theme.apply_config(&config);
                apply_frosted_material(&mut theme.colors, config.mode, opacity);
                let canvas = composite(theme.colors.background.to_rgb(), wallpaper);
                let pane = composite(theme.colors.list.to_rgb(), canvas);
                for surface in [canvas, pane] {
                    let fg = composite(theme.foreground.to_rgb(), surface);
                    let muted = composite(theme.muted_foreground.to_rgb(), surface);
                    assert!(contrast(fg, surface) >= 4.5, "{} @{opacity}: text {}", config.name, contrast(fg, surface));
                    if check_muted {
                        assert!(contrast(muted, surface) >= muted_floor, "{} @{opacity}: muted {} < {muted_floor}", config.name, contrast(muted, surface));
                    }
                }
            }
        }
    }

    #[test]
    fn light_text_stays_readable_on_content_sidebar_and_hover() {
        fn composite(color: gpui::Rgba, base: gpui::Rgba) -> gpui::Rgba {
            gpui::Rgba::new(
                color.red * color.alpha + base.red * (1.0 - color.alpha),
                color.green * color.alpha + base.green * (1.0 - color.alpha),
                color.blue * color.alpha + base.blue * (1.0 - color.alpha),
                1.0,
            )
        }
        fn luminance(color: gpui::Rgba) -> f32 {
            let linear = |v: f32| {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(color.red) + 0.7152 * linear(color.green) + 0.0722 * linear(color.blue)
        }
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../../assets/themes/atlas-themes.json")).unwrap();
        for config in themes.themes.into_iter().filter(|t| !t.mode.is_dark()) {
            let glass = config.name.ends_with(" Glass");
            let config = std::rc::Rc::new(config);
            let mut theme = gpui_component::Theme::default();
            theme.apply_config(&config);
            if glass {
                apply_frosted_material(&mut theme.colors, config.mode, 0.2);
            }
            let base = if glass {
                // The OS blur averages a wallpaper toward a mid tone; light ink
                // must read on that, not on the pathological pure-black case.
                gpui::Rgba::new(0.5, 0.5, 0.5, 1.0)
            } else {
                theme.background.to_rgb()
            };
            let controls = crate::ui::glass::control_colors(&theme, glass);
            // Glass panes are composited on the canvas, which is what the eye sees.
            let pane_base = if glass {
                composite(theme.colors.background.to_rgb(), base)
            } else {
                base
            };
            for (surface, under) in [(theme.colors.list, pane_base), (theme.sidebar, base)] {
                let surface = composite(surface.to_rgb(), under);
                let hovered = composite(
                    crate::ui::glass::control_hover(theme.foreground).to_rgb(),
                    surface,
                );
                let control_surface = composite(controls.surface.to_rgb(), surface);
                let control_hovered = composite(controls.hover.to_rgb(), control_surface);
                for background in [surface, hovered, control_surface, control_hovered] {
                    for text in [theme.foreground, theme.muted_foreground] {
                        let text = composite(text.to_rgb(), background);
                        let ratio = (luminance(background) + 0.05) / (luminance(text) + 0.05);
                        assert!(ratio >= 4.5, "{}: text contrast {ratio}", config.name);
                    }
                }
            }
        }
    }

    #[test]
    fn palette_colors_preserve_hex_channels_and_alpha() {
        for hex in [
            "#F8FAFC",
            "#FEF2F2",
            "#0413FCAA",
            "#21C55E",
            "#FF0000",
            "#00000000",
        ] {
            let color = gpui::Hsla::parse_hex(hex).unwrap();
            assert_eq!(color.to_hex(), hex);
        }
        let config: gpui_component::highlighter::HighlightThemeStyle =
            serde_json::from_str(r##"{"syntax":{"comment":{"color":"#0413FCAA"}}}"##)
                .expect("hex syntax color");
        let value = serde_json::to_value(config).unwrap();
        assert_eq!(value["syntax"]["comment"]["color"], "#0413FCAA");
    }
}
