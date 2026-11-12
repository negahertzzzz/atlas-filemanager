//! Searchable tag-picker overlay for the inspector preview.
//!
//! Opened via `Action::OpenTagPicker` when the user clicks the add-tag button
//! below an image in the inspector. Renders as an absolutely-positioned panel anchored
//! to the button, with a search input that filters existing tags.
//!
//! - Existing tags: shown as rows with a color swatch. Clicking assigns the tag.
//! - A create-tag row appears when the query has no exact match; clicking it opens
//!   the create-tag modal and preserves the files that should receive the new tag.
//!
//! Follows the exact structural pattern of `type_filter_menu.rs`:
//!   - State lives on `App` (`tag_picker_menu`, `tag_picker_input`, `tag_picker_sub`).
//!   - `open_tag_picker` / `close_tag_picker` manage lifecycle.
//!   - `render_tag_picker_layer` paints the overlay layer (called from `overlays/mod.rs`).
//!   - Focus is claimed by `overlay_focus.rs` → `overlay_focus_target`.

use crate::app::App;
use crate::input::action::Action;
use crate::ui::icons::{icon, AtlasIcon};
use atlas_model::clamp::ClampLenient;
use gpui::prelude::*;
use gpui::AnimationExt as _;
use gpui::ColorExt as _;
use gpui::{
    div, px, AnyElement, Context, KeyDownEvent, MouseButton, Pixels, Point, SharedString, Window,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::StyledExt as _;
use gpui_component::{v_flex, ActiveTheme, Sizable as _};

/// State for the open tag-picker overlay.
pub(crate) struct TagPickerState {
    /// Window-relative position used to anchor the panel.
    pub anchor: Point<Pixels>,
    /// Paths that will receive the chosen tag (usually just the focused image).
    pub paths: Vec<std::path::PathBuf>,
    pub selected: usize,
    pub scroll_to_selected: bool,
}

#[derive(Clone)]
enum TagPickerItem {
    Existing {
        id: atlas_model::tag_cache::TagId,
        name: String,
        color: [u8; 3],
        assigned: bool,
    },
    Create {
        name: String,
    },
}

impl App {
    /// Open the tag-picker overlay anchored at `anchor`, targeting `paths`.
    pub(crate) fn open_tag_picker(
        &mut self,
        anchor: Point<Pixels>,
        paths: Vec<std::path::PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = crate::ui::text_input::single_line(
            window,
            cx,
            rust_i18n::t!("search_tag").into_owned(),
            "",
        );
        self.tag_picker_sub = Some(cx.subscribe(
            &input,
            |_this, _input, ev: &gpui_component::input::InputEvent, cx| {
                if matches!(ev, gpui_component::input::InputEvent::Change) {
                    cx.notify();
                }
            },
        ));
        self.tag_picker_input = Some(input);
        self.tag_picker_menu = Some(TagPickerState {
            anchor,
            paths,
            selected: 0,
            scroll_to_selected: true,
        });
        cx.notify();
    }

    /// Close the tag-picker overlay.
    pub(crate) fn close_tag_picker(&mut self, cx: &mut Context<Self>) {
        self.tag_picker_menu = None;
        self.tag_picker_input = None;
        self.tag_picker_sub = None;
        cx.notify();
    }

    /// Render the tag-picker overlay layer (called from `render_overlays`).
    pub(crate) fn render_tag_picker_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(state) = self.tag_picker_menu.as_ref() else {
            return div().into_any_element();
        };
        let anchor = state.anchor;
        let paths = state.paths.clone();

        let (fg, muted, border, accent) = {
            let t = cx.theme();
            (t.popover_foreground, t.muted_foreground, t.border, t.accent)
        };

        let query = self
            .tag_picker_input
            .as_ref()
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default();
        let (smooth_ease, smooth_boost) =
            crate::app::scroll_smooth_params(self.state.settings.scroll_smoothness);
        self.tag_picker_scroll.set_smooth(smooth_ease, smooth_boost);

        let profile_name = self.state.profiles.active().name.clone();

        let items = tag_picker_items(&self.state.tag_cache, &profile_name, &paths, &query);
        let selected = if items.is_empty() {
            0
        } else {
            let state = self.tag_picker_menu.as_mut().unwrap();
            if state.selected >= items.len() {
                state.selected = items.len() - 1;
                state.scroll_to_selected = true;
            }
            state.selected
        };
        if self
            .tag_picker_menu
            .as_ref()
            .is_some_and(|state| state.scroll_to_selected)
        {
            self.tag_picker_scroll.scroll_to_item(selected);
            self.tag_picker_menu.as_mut().unwrap().scroll_to_selected = false;
        }

        // Panel geometry.
        let win = window.bounds().size;
        const W: f32 = 284.;
        let row_h = 34_f32;
        // Search header; the list adds vertical padding and
        // a 2px gap between rows.
        let chrome_h = 44.0;
        let list_pad = 12.0 + 2.0;
        let est_h = (items.len().max(1) as f32) * (row_h + 2.0) + list_pad + chrome_h;
        let ax = f32::from(anchor.x);
        let ay = f32::from(anchor.y);
        let ww = f32::from(win.width);
        let wh = f32::from(win.height);
        let max_h = 480.0_f32.min((wh - 72.0 * 2.0).max(140.0));
        let panel_h = est_h.clamp_lenient(row_h + list_pad + chrome_h, max_h);
        let list_h = (panel_h - chrome_h).max(row_h + list_pad);
        let left = ax.min((ww - W - 6.0).max(6.0)).max(6.0);
        let top = ay.min((wh - panel_h - 6.0).max(6.0)).max(6.0);

        let input = self.tag_picker_input.clone();
        let view = cx.entity();

        // Build rows.
        let mut rows: Vec<AnyElement> = Vec::new();

        for (index, item) in items.iter().cloned().enumerate() {
            let is_selected = index == selected;
            let v = view.clone();
            rows.push(
                tag_picker_row(index, &item, fg, muted, accent, is_selected, row_h)
                    .on_mouse_move({
                        let v = view.clone();
                        move |_, _, cx| {
                            v.update(cx, |this, cx| {
                                if let Some(state) = this.tag_picker_menu.as_mut() {
                                    state.selected = index;
                                    cx.notify();
                                }
                            });
                        }
                    })
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        let item = item.clone();
                        v.update(cx, |this, cx| {
                            this.activate_tag_picker_item(item, cx);
                        });
                    })
                    .into_any_element(),
            );
        }

        let close_view = view.clone();

        // Full-window backdrop (click-to-close).
        div()
            .absolute()
            .inset_0()
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                close_view.update(cx, |this, cx| {
                    this.close_tag_picker(cx);
                });
            })
            // Panel itself.
            .child(
                v_flex()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(W))
                    .h(px(panel_h))
                    .popover_style(cx)
                    .rounded(px(12.))
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    // Search input.
                    .when_some(input, |el, input| {
                        el.child(
                            gpui_component::h_flex()
                                .h(px(44.))
                                .flex_shrink_0()
                                .px_3()
                                .gap_2()
                                .items_center()
                                .border_b_1()
                                .border_color(border.opacity(0.6))
                                .child(icon(AtlasIcon::Tag, 15., muted))
                                .child(div().flex_1().child(crate::ui::text_input::flat(&input))),
                        )
                    })
                    // Scrollable tag list.
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .max_h(px(list_h))
                            .py_1p5()
                            .child(
                                v_flex()
                                    .id("tag-picker-list")
                                    .size_full()
                                    .px_1p5()
                                    .gap_0p5()
                                    .overflow_y_scroll()
                                    .track_scroll(&self.tag_picker_scroll)
                                    .when(rows.is_empty(), |list| {
                                        list.child(
                                            div().px_2().py_2().text_sm().text_color(muted).child(
                                                rust_i18n::t!("tag_picker_empty").to_string(),
                                            ),
                                        )
                                    })
                                    .children(rows),
                            )
                            .vertical_scrollbar(&self.tag_picker_scroll),
                    )
                    .with_animation(
                        "tag-picker-appear",
                        gpui_component::animation::dropdown_animation(),
                        gpui_component::animation::dropdown_appear,
                    ),
            )
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                if this.handle_tag_picker_key(ev, cx) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    fn handle_tag_picker_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let items = self.visible_tag_picker_items(cx);
        let Some(state) = self.tag_picker_menu.as_mut() else {
            return false;
        };

        match ev.keystroke.key.as_str() {
            "escape" => self.close_tag_picker(cx),
            "down" if !items.is_empty() => {
                state.selected = (state.selected + 1) % items.len();
                state.scroll_to_selected = true;
            }
            "up" if !items.is_empty() => {
                state.selected = (state.selected + items.len() - 1) % items.len();
                state.scroll_to_selected = true;
            }
            "enter" => {
                let Some(item) = items.get(state.selected).cloned() else {
                    return true;
                };
                self.activate_tag_picker_item(item, cx);
            }
            "down" | "up" => {}
            _ => return false,
        }
        true
    }

    fn visible_tag_picker_items(&self, cx: &mut Context<Self>) -> Vec<TagPickerItem> {
        let Some(state) = self.tag_picker_menu.as_ref() else {
            return Vec::new();
        };
        let query = self
            .tag_picker_input
            .as_ref()
            .map(|i| i.read(cx).value().to_string())
            .unwrap_or_default();
        let profile_name = self.state.profiles.active().name.clone();
        tag_picker_items(&self.state.tag_cache, &profile_name, &state.paths, &query)
    }

    fn activate_tag_picker_item(&mut self, item: TagPickerItem, cx: &mut Context<Self>) {
        match item {
            TagPickerItem::Existing { id, assigned, .. } => {
                let paths = self
                    .tag_picker_menu
                    .as_ref()
                    .map(|state| state.paths.clone())
                    .unwrap_or_default();
                if assigned {
                    self.state.tag_cache.unassign_local(id, &paths);
                    let _ = self
                        .tag_db_tx
                        .send(atlas_core::fs::tag_db::TagDbCmd::UnassignTag { tag: id, paths });
                } else {
                    self.state.tag_cache.assign_local(id, &paths);
                    let _ = self
                        .tag_db_tx
                        .send(atlas_core::fs::tag_db::TagDbCmd::AssignTag { tag: id, paths });
                }
                // Stay open so several tags can be toggled in one visit.
                cx.notify();
            }
            TagPickerItem::Create { name } => {
                let paths = self
                    .tag_picker_menu
                    .as_ref()
                    .map(|state| state.paths.clone())
                    .unwrap_or_default();
                self.close_tag_picker(cx);
                self.process_actions(vec![Action::OpenCreateTagOverlay { global: false }]);
                self.state.profile_ui.new_tag_name = name;
                self.state.profile_ui.new_tag_paths = paths;
            }
        }
    }
}

fn tag_picker_items(
    cache: &atlas_model::tag_cache::TagCache,
    profile: &str,
    paths: &[std::path::PathBuf],
    query: &str,
) -> Vec<TagPickerItem> {
    let query = query.trim();
    let folded = query.to_lowercase();
    let mut exact_match = false;
    let mut items = Vec::new();
    for definition in cache.visible_defs(profile) {
        let name = definition.name.to_lowercase();
        exact_match |= name == folded;
        if folded.is_empty() || name.contains(&folded) {
            items.push(TagPickerItem::Existing {
                id: definition.id,
                name: definition.name.clone(),
                color: definition.color,
                assigned: !paths.is_empty()
                    && paths
                        .iter()
                        .all(|path| cache.tags_of(path).contains(&definition.id)),
            });
        }
    }
    if !query.is_empty() && !exact_match {
        items.push(TagPickerItem::Create {
            name: query.to_string(),
        });
    }
    items
}

/// A row tints itself with its own tag color when active; text keeps the
/// popover foreground so contrast holds in every theme.
fn tag_picker_row(
    index: usize,
    item: &TagPickerItem,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    accent: gpui::Hsla,
    selected: bool,
    row_h: f32,
) -> gpui::Stateful<gpui::Div> {
    let tint: gpui::Hsla = match item {
        TagPickerItem::Existing { color, .. } => rgb8_to_hsla(*color),
        TagPickerItem::Create { .. } => accent,
    };
    let row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_2p5()
        .pl_2()
        .pr_2()
        .h(px(row_h))
        .flex_shrink_0()
        .rounded(px(8.))
        .text_sm()
        .text_color(fg)
        .cursor_pointer()
        .when(selected, |row| row.bg(fg.opacity(0.07)))
        .hover(move |s| s.bg(fg.opacity(0.07)));
    match item {
        TagPickerItem::Existing { name, assigned, .. } => row
            .id(SharedString::from(format!("tp-{index}-{name}")))
            .child(div().size(px(8.)).flex_none().rounded_full().bg(tint))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .child(SharedString::from(name.clone())),
            )
            .when(*assigned, |row| {
                row.child(
                    gpui_component::Icon::new(gpui_component::IconName::Check)
                        .with_size(gpui_component::Size::XSmall)
                        .text_color(muted),
                )
            }),
        TagPickerItem::Create { name } => row
            .id(SharedString::from("tp-create"))
            .child(
                gpui_component::Icon::new(gpui_component::IconName::Plus)
                    .with_size(gpui_component::Size::XSmall)
                    .text_color(muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .child(SharedString::from(rust_i18n::t!(
                        "create_tag",
                        name = name.clone()
                    ))),
            ),
    }
}

fn rgb8_to_hsla([r, g, b]: [u8; 3]) -> gpui::Hsla {
    let (r, g, b) = (
        f32::from(r) / 255.,
        f32::from(g) / 255.,
        f32::from(b) / 255.,
    );
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.;
    let d = max - min;
    if d == 0. {
        return gpui::hsla(0., 0., l, 1.);
    }
    let s = d / (1. - (2. * l - 1.).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.)
    } else if max == g {
        (b - r) / d + 2.
    } else {
        (r - g) / d + 4.
    };
    gpui::hsla(h / 6., s, l, 1.)
}

#[cfg(test)]
mod tests {
    use super::{tag_picker_items, TagPickerItem};
    use atlas_model::tag_cache::{TagCache, TagDef, TagId};
    use std::path::PathBuf;

    #[test]
    fn partial_selection_assigns_to_every_target_before_allowing_removal() {
        let tag = TagId(1);
        let paths = vec![PathBuf::from("first.png"), PathBuf::from("second.png")];
        let mut cache = TagCache::default();
        cache.defs.push(TagDef {
            id: tag,
            name: "Art".into(),
            color: [120, 80, 240],
            profile: None,
        });
        cache.assign_local(tag, &paths[..1]);

        let items = tag_picker_items(&cache, "Main", &paths, "");
        assert!(matches!(
            items.as_slice(),
            [TagPickerItem::Existing {
                assigned: false,
                ..
            }]
        ));
        cache.assign_local(tag, &paths);
        let items = tag_picker_items(&cache, "Main", &paths, "");
        assert!(matches!(
            items.as_slice(),
            [TagPickerItem::Existing { assigned: true, .. }]
        ));
    }

    #[test]
    fn create_preserves_case_and_remains_available_beside_partial_name_matches() {
        let mut cache = TagCache::default();
        cache.defs.push(TagDef {
            id: TagId(1),
            name: "Artworks".into(),
            color: [120, 80, 240],
            profile: None,
        });
        let items = tag_picker_items(&cache, "Main", &[], " Art ");
        assert!(matches!(items.last(), Some(TagPickerItem::Create { name }) if name == "Art"));
        let items = tag_picker_items(&cache, "Main", &[], "artWORKS");
        assert_eq!(items.len(), 1);
        assert!(matches!(
            items[0],
            TagPickerItem::Existing {
                assigned: false,
                ..
            }
        ));
    }
}
