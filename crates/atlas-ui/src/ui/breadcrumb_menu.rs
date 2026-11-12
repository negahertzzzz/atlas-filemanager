//! Breadcrumb-chevron folder menu.

use crate::app::App;
use crate::input::action::Action;
use crate::ui::drag_hover::{
    drag_hover_recorder_with_drop, DragHoverCommand, DragHoverDropCommand, DragHoverId,
    DragHoverKind,
};
use crate::ui::file_icons::FileIconRef;
use crate::ui::icons::{icon as atlas_icon, AtlasIcon};
use atlas_model::file_entry::FileKind;
use atlas_model::location::Location;
use atlas_model::pane::TabId;
use gpui::prelude::*;
use gpui::AnimationExt as _;
use gpui::ColorExt as _;
use gpui::{
    div, px, AnyElement, Context, Focusable, InteractiveElement, KeyDownEvent, MouseButton,
    ParentElement, Pixels, Point, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Window,
};
use gpui_component::scroll::ScrollableElement;
use gpui_component::StyledExt as _;
use gpui_component::{h_flex, v_flex, ActiveTheme, ElementExt};
use std::path::PathBuf;

const W: f32 = 280.0;
const ROW_H: f32 = 28.0;
const MAX_H: f32 = 520.0;
const SCROLLBAR_GUTTER: f32 = 16.0;

pub(crate) struct BreadcrumbMenuState {
    pub pane: TabId,
    pub anchor: Point<Pixels>,
    pub items: Vec<BreadcrumbMenuItem>,
    pub selected: usize,
    pub scroll: ScrollHandle,
    pub scroll_to_selected: bool,
}

#[derive(Clone)]
pub(crate) struct BreadcrumbMenuItem {
    pub label: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub icon: FileIconRef,
}

impl App {
    pub(crate) fn open_breadcrumb_menu(
        &mut self,
        pane: TabId,
        dir: PathBuf,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_ctx_menu(cx);
        self.close_type_filter_menu(cx);
        let mut items = breadcrumb_items(dir);
        let icon_refs = items
            .iter()
            .map(|item| item.icon.clone())
            .collect::<Vec<_>>();
        for icon in &icon_refs {
            self.maybe_request_file_icon(icon);
        }
        let input = crate::ui::text_input::single_line(
            window,
            cx,
            rust_i18n::t!("breadcrumb_filter_folders").into_owned(),
            "",
        );
        window.focus(&input.read(cx).focus_handle(cx), cx);
        self.breadcrumb_menu_sub = Some(cx.subscribe(
            &input,
            |this, _input, ev: &gpui_component::input::InputEvent, cx| {
                if matches!(ev, gpui_component::input::InputEvent::Change) {
                    if let Some(menu) = this.breadcrumb_menu.as_mut() {
                        menu.selected = 0;
                        menu.scroll_to_selected = true;
                    }
                    cx.notify();
                }
            },
        ));
        self.breadcrumb_menu_input = Some(input);
        self.breadcrumb_menu = Some(BreadcrumbMenuState {
            pane,
            anchor,
            items: {
                items.shrink_to_fit();
                items
            },
            selected: 0,
            scroll: ScrollHandle::new(),
            scroll_to_selected: true,
        });
        cx.notify();
    }

    pub(crate) fn close_breadcrumb_menu(&mut self, cx: &mut Context<Self>) {
        self.pending_drag_hover_targets
            .retain(|target| !matches!(target.id, DragHoverId::BreadcrumbMenu(_, _)));
        self.breadcrumb_menu = None;
        self.breadcrumb_menu_input = None;
        self.breadcrumb_menu_sub = None;
        cx.notify();
    }

    pub(crate) fn render_breadcrumb_menu_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.pending_drag_hover_targets
            .retain(|target| !matches!(target.id, DragHoverId::BreadcrumbMenu(_, _)));
        let Some(state) = self.breadcrumb_menu.as_ref() else {
            return div().into_any_element();
        };
        let pane = state.pane;
        let anchor = state.anchor;
        let items = state.items.clone();
        let query = self
            .breadcrumb_menu_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string().to_lowercase())
            .unwrap_or_default();
        let order = self
            .fuzzy_scorer
            .rank(&query, items.iter().map(|item| item.label.as_str()));
        let visible = order
            .into_iter()
            .map(|i| items[i].clone())
            .collect::<Vec<_>>();
        let selected = if visible.is_empty() {
            0
        } else {
            let state = self.breadcrumb_menu.as_mut().unwrap();
            if state.selected >= visible.len() {
                state.selected = visible.len() - 1;
                state.scroll_to_selected = true;
            }
            state.selected
        };
        let scroll = self.breadcrumb_menu.as_ref().unwrap().scroll.clone();
        let (smooth_ease, smooth_boost) =
            crate::app::scroll_smooth_params(self.state.settings.scroll_smoothness);
        scroll.set_smooth(smooth_ease, smooth_boost);
        if self
            .breadcrumb_menu
            .as_ref()
            .is_some_and(|state| state.scroll_to_selected)
        {
            scroll.scroll_to_item(selected);
            self.breadcrumb_menu.as_mut().unwrap().scroll_to_selected = false;
        }

        let (fg, muted, border) = {
            let t = cx.theme();
            (t.popover_foreground, t.muted_foreground, t.border)
        };
        let win = window.bounds().size;
        let input_h = 40.0;
        let list_h = ((visible.len().max(1) as f32) * ROW_H + 8.0).min(MAX_H);
        let panel_h = input_h + list_h;
        let ww = f32::from(win.width);
        let wh = f32::from(win.height);
        let left = f32::from(anchor.x).min((ww - W - 6.0).max(6.0)).max(6.0);
        let top = f32::from(anchor.y)
            .min((wh - panel_h - 6.0).max(6.0))
            .max(6.0);
        let input = self.breadcrumb_menu_input.clone();
        let view = cx.entity();

        let rows = if visible.is_empty() {
            vec![div()
                .h(px(ROW_H))
                .px_2()
                .flex()
                .items_center()
                .text_color(muted)
                .child("No matches")
                .into_any_element()]
        } else {
            visible
                .into_iter()
                .enumerate()
                .map(|(index, item)| {
                    let icon = self.file_icon(&item.icon).element(16.0, muted);
                    let path = item.path.clone();
                    let is_dir = item.is_dir;
                    let v = view.clone();
                    let drop_v = view.clone();
                    let drop_v_external = view.clone();
                    let mut row = breadcrumb_row(item.label, icon, fg, border, index == selected)
                        .on_mouse_move({
                            let v = view.clone();
                            move |_, _, cx| {
                                v.update(cx, |this, cx| {
                                    if let Some(state) = this.breadcrumb_menu.as_mut() {
                                        state.selected = index;
                                        cx.notify();
                                    }
                                });
                            }
                        })
                        .on_mouse_down(MouseButton::Left, {
                            let path = path.clone();
                            move |_, _, cx| {
                                let path = path.clone();
                                v.update(cx, |this, cx| {
                                    this.close_breadcrumb_menu(cx);
                                    this.activate_breadcrumb_item(pane, path, is_dir);
                                    cx.notify();
                                });
                            }
                        });
                    if is_dir {
                        let hover_path = path.clone();
                        let drop_target = path.clone();
                        let drop_target_external = path.clone();
                        row = row
                            .on_prepaint_child(drag_hover_recorder_with_drop(
                                DragHoverId::BreadcrumbMenu(pane, hover_path.clone()),
                                DragHoverKind::Folder,
                                DragHoverCommand::Process(Action::NavigateLocation {
                                    tab: pane,
                                    location: Location::Directory(hover_path.clone()),
                                }),
                                DragHoverDropCommand::TransferTo(hover_path),
                                &view,
                            ))
                            .on_drop::<crate::ui::root::DraggedFiles>(move |d, _window, cx| {
                                let paths = d.paths.clone();
                                drop_v.update(cx, |this, cx| {
                                    this.close_breadcrumb_menu(cx);
                                    this.execute_drag_hover_drop_command(
                                        DragHoverDropCommand::TransferTo(drop_target.clone()),
                                        paths,
                                        true,
                                    );
                                    cx.notify();
                                });
                            })
                            .on_drop::<gpui::ExternalPaths>(move |paths, _window, cx| {
                                let paths = paths.paths().to_vec();
                                drop_v_external.update(cx, |this, cx| {
                                    this.close_breadcrumb_menu(cx);
                                    this.execute_drag_hover_drop_command(
                                        DragHoverDropCommand::TransferTo(
                                            drop_target_external.clone(),
                                        ),
                                        paths,
                                        false,
                                    );
                                    cx.notify();
                                });
                            });
                    }
                    row.into_any_element()
                })
                .collect()
        };

        let input_row = input.as_ref().map(|state| {
            h_flex()
                .w_full()
                .px_2()
                .py_1()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(border)
                .child(atlas_icon(AtlasIcon::Search, 14.0, muted))
                .child(div().flex_1().child(crate::ui::text_input::flat(state)))
        });

        let close_view = view.clone();
        gpui::deferred(
            div()
                .id("breadcrumb-menu-scrim")
                .absolute()
                .inset_0()
                .size_full()
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    close_view.update(cx, |this, cx| {
                        this.close_breadcrumb_menu(cx);
                    });
                })
                .child(
                    v_flex()
                        .id("breadcrumb-menu-panel")
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(W))
                        .h(px(panel_h))
                        .popover_style(cx)
                        .text_color(fg)
                        .occlude()
                        .overflow_hidden()
                        .children(input_row)
                        .child(
                            div()
                                .relative()
                                .flex_1()
                                .min_h(px(0.0))
                                .child(
                                    v_flex()
                                        .id("breadcrumb-menu-list")
                                        .size_full()
                                        .overflow_y_scroll()
                                        .track_scroll(&scroll)
                                        .py_1()
                                        .pr(px(SCROLLBAR_GUTTER))
                                        .children(rows),
                                )
                                .vertical_scrollbar(&scroll),
                        ),
                )
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                    if this.handle_breadcrumb_menu_key(ev, cx) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .with_animation(
                    "breadcrumb-menu-appear",
                    gpui_component::animation::dropdown_animation(),
                    gpui_component::animation::dropdown_appear,
                ),
        )
        .into_any_element()
    }

    fn handle_breadcrumb_menu_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let query = self
            .breadcrumb_menu_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string().to_lowercase())
            .unwrap_or_default();
        let Some(state) = self.breadcrumb_menu.as_mut() else {
            return false;
        };
        let order = self
            .fuzzy_scorer
            .rank(&query, state.items.iter().map(|item| item.label.as_str()));
        let visible = order
            .into_iter()
            .map(|i| state.items[i].clone())
            .collect::<Vec<_>>();

        match ev.keystroke.key.as_str() {
            "escape" => self.close_breadcrumb_menu(cx),
            "down" if !visible.is_empty() => {
                state.selected = (state.selected + 1) % visible.len();
                state.scroll_to_selected = true;
            }
            "up" if !visible.is_empty() => {
                state.selected = (state.selected + visible.len() - 1) % visible.len();
                state.scroll_to_selected = true;
            }
            "enter" => {
                let Some(item) = visible.get(state.selected).cloned() else {
                    return true;
                };
                let pane = state.pane;
                self.close_breadcrumb_menu(cx);
                self.activate_breadcrumb_item(pane, item.path, item.is_dir);
            }
            "down" | "up" => {}
            _ => return false,
        }
        true
    }

    fn activate_breadcrumb_item(&mut self, pane: TabId, path: PathBuf, is_dir: bool) {
        self.state.focused_tile = pane;
        self.process_actions(vec![if is_dir {
            Action::NavigateLocation {
                tab: pane,
                location: Location::Directory(path),
            }
        } else {
            Action::Open(vec![path])
        }]);
    }
}

fn breadcrumb_items(dir: PathBuf) -> Vec<BreadcrumbMenuItem> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut items = read_dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_string_lossy().into_owned();
            let file_type = entry.file_type().ok()?;
            let is_dir = file_type.is_dir();
            if !is_dir {
                return None;
            }
            let kind = if is_dir {
                FileKind::Dir
            } else {
                FileKind::File
            };
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase());
            Some(BreadcrumbMenuItem {
                label: name,
                path: path.clone(),
                is_dir,
                icon: FileIconRef {
                    path,
                    kind,
                    extension: extension.map(gpui::SharedString::from),
                },
            })
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    items
}

fn breadcrumb_row(
    label: String,
    icon: AnyElement,
    fg: gpui::Hsla,
    border: gpui::Hsla,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    h_flex()
        .id(SharedString::from(format!("breadcrumb-menu-row-{label}")))
        .h(px(ROW_H))
        .px_2()
        .gap_2()
        .items_center()
        .cursor_pointer()
        .when(selected, move |s| s.bg(border.opacity(0.5)))
        .hover(move |s| s.bg(border.opacity(0.5)))
        .child(icon)
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_color(fg)
                .child(SharedString::from(label)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chevron_menu_contains_only_immediate_subfolders() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("atlas-breadcrumbs-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(root.join("Beta/nested")).unwrap();
        std::fs::create_dir(root.join("alpha")).unwrap();
        std::fs::write(root.join("file.txt"), "fixture").unwrap();
        let items = breadcrumb_items(root.clone());
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "Beta"]
        );
        assert!(items
            .iter()
            .all(|item| item.is_dir && item.path.parent() == Some(root.as_path())));
        std::fs::remove_file(root.join("file.txt")).unwrap();
        for dir in ["Beta/nested", "Beta", "alpha", ""] {
            std::fs::remove_dir(root.join(dir)).unwrap();
        }
    }
}
