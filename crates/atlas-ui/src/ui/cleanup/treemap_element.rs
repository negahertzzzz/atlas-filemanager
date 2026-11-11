//! Nested, squarified treemap (WizTree / WinDirStat style): every rectangle's
//! area is its size, folders contain their children, and the whole analysed
//! folder is visible at once.
use super::CleanupPanel;
use crate::ui::surface::{chrome_ui_font, shape_text_truncated};
use atlas_model::cleanup::TreemapNode;
use gpui::*;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

/// One painted rectangle, in element-local pixels.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Tile {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub depth: u8,
    /// `None` for the "small files and the rest" area of a folder.
    pub path: Option<PathBuf>,
    pub directory: bool,
    pub label: String,
    pub size: String,
    /// Folder tiles reserve a header strip for their name.
    pub header: bool,
}

/// Squarified layout (Bruls, Huizing, van Wijk): splits `rect` into one
/// rectangle per value (sorted descending) with aspect ratios close to 1.
pub(super) fn squarify(values: &[f64], rect: (f32, f32, f32, f32)) -> Vec<(f32, f32, f32, f32)> {
    let (mut x, mut y, mut w, mut h) = (
        f64::from(rect.0),
        f64::from(rect.1),
        f64::from(rect.2),
        f64::from(rect.3),
    );
    let total: f64 = values.iter().filter(|v| **v > 0.).sum();
    let mut out = vec![(rect.0, rect.1, 0., 0.); values.len()];
    if total <= 0. || w <= 0. || h <= 0. {
        return out;
    }
    let scale = w * h / total;
    let areas: Vec<f64> = values.iter().map(|v| v.max(0.) * scale).collect();
    let worst = |row: &[f64], short: f64| {
        let sum: f64 = row.iter().sum();
        let (max, min) = row
            .iter()
            .fold((0f64, f64::MAX), |(max, min), a| (max.max(*a), min.min(*a)));
        if sum <= 0. || min <= 0. {
            return f64::MAX;
        }
        (short * short * max / (sum * sum)).max(sum * sum / (short * short * min))
    };
    let mut i = 0;
    while i < areas.len() {
        if areas[i] <= 0. {
            i += 1;
            continue;
        }
        let short = w.min(h).max(f64::EPSILON);
        let mut j = i + 1;
        let mut best = worst(&areas[i..j], short);
        while j < areas.len() && areas[j] > 0. {
            let next = worst(&areas[i..=j], short);
            if next > best {
                break;
            }
            best = next;
            j += 1;
        }
        let sum: f64 = areas[i..j].iter().sum();
        if w >= h {
            let column = sum / h;
            let mut cy = y;
            for k in i..j {
                let ch = areas[k] / column;
                out[k] = (x as f32, cy as f32, column as f32, ch as f32);
                cy += ch;
            }
            x += column;
            w -= column;
        } else {
            let row = sum / w;
            let mut cx = x;
            for k in i..j {
                let cw = areas[k] / row;
                out[k] = (cx as f32, y as f32, cw as f32, row as f32);
                cx += cw;
            }
            y += row;
            h -= row;
        }
        i = j;
    }
    out
}

/// Flatten `root` into painted tiles for a `width` × `height` area.
pub(super) fn layout(
    root: &TreemapNode,
    width: f32,
    height: f32,
    zoom: f32,
    other: &str,
) -> Vec<Tile> {
    let mut tiles = Vec::new();
    place(
        root,
        (0., 0., width, height),
        0,
        zoom,
        other,
        &mut tiles,
        true,
    );
    tiles
}

fn place(
    node: &TreemapNode,
    rect: (f32, f32, f32, f32),
    depth: u8,
    zoom: f32,
    other: &str,
    out: &mut Vec<Tile>,
    is_root: bool,
) {
    let (x, y, w, h) = rect;
    if w < 1. || h < 1. {
        return;
    }
    let pad = if is_root { 0. } else { 2. * zoom };
    let header = !is_root && node.directory && h > 34. * zoom && w > 46. * zoom;
    if !is_root {
        out.push(Tile {
            x,
            y,
            w,
            h,
            depth,
            path: Some(node.path.clone()),
            directory: node.directory,
            label: node.name.clone(),
            size: atlas_core::util::display::format_size(node.bytes),
            header,
        });
    }
    if node.children.is_empty() || w < 12. * zoom || h < 12. * zoom {
        return;
    }
    let top = if header { 17. * zoom } else { pad };
    let inner = (
        x + pad,
        y + top,
        (w - pad * 2.).max(0.),
        (h - top - pad).max(0.),
    );
    let mut values: Vec<f64> = node.children.iter().map(|c| c.bytes as f64).collect();
    values.push(node.other as f64);
    let rects = squarify(&values, inner);
    for (child, rect) in node.children.iter().zip(&rects) {
        place(
            child,
            *rect,
            depth.saturating_add(1),
            zoom,
            other,
            out,
            false,
        );
    }
    if let Some(rest) = rects
        .last()
        .filter(|r| r.2 >= 1. && r.3 >= 1. && node.other > 0)
    {
        out.push(Tile {
            x: rest.0,
            y: rest.1,
            w: rest.2,
            h: rest.3,
            depth: depth.saturating_add(1),
            path: None,
            directory: false,
            label: other.to_owned(),
            size: atlas_core::util::display::format_size(node.other),
            header: false,
        });
    }
}

#[derive(Default)]
pub(super) struct Cache {
    key: String,
    pub(super) tiles: Vec<Tile>,
    labels: Vec<Option<(ShapedLine, Option<ShapedLine>)>>,
}

pub(super) struct TreemapElement {
    pub panel: WeakEntity<CleanupPanel>,
    pub root: Arc<TreemapNode>,
    pub revision: u64,
    pub selected: Option<PathBuf>,
    pub hovered: Option<usize>,
    pub cache: Rc<RefCell<Cache>>,
    pub fg: Hsla,
    pub background: Hsla,
    pub dark: bool,
    pub colors: [Hsla; 5],
    pub other_label: String,
}

impl IntoElement for TreemapElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

pub(super) struct TreemapPrepaint {
    hitbox: Hitbox,
}

fn hit(tiles: &[Tile], bounds: Bounds<Pixels>, position: Point<Pixels>) -> Option<usize> {
    let x = f32::from(position.x - bounds.origin.x);
    let y = f32::from(position.y - bounds.origin.y);
    // Children are pushed after their parent: the last hit is the deepest.
    tiles
        .iter()
        .rposition(|t| x >= t.x && x < t.x + t.w && y >= t.y && y < t.y + t.h)
}

impl Element for TreemapElement {
    type RequestLayoutState = ();
    type PrepaintState = TreemapPrepaint;
    fn id(&self) -> Option<ElementId> {
        Some("cleanup-treemap".into())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> TreemapPrepaint {
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let font = chrome_ui_font(window);
        let key = format!(
            "{}:{:?}:{:?}:{:?}:{zoom}",
            self.revision, bounds.size, font, self.fg
        );
        let mut cache = self.cache.borrow_mut();
        if cache.key != key {
            cache.tiles = layout(
                &self.root,
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
                zoom,
                &self.other_label,
            );
            let mut bold = font.clone();
            bold.weight = FontWeight::SEMIBOLD;
            cache.labels = cache
                .tiles
                .iter()
                .map(|tile| {
                    let room = tile.w - 8. * zoom;
                    let fits = room > 28. * zoom && tile.h > 15. * zoom;
                    fits.then(|| {
                        let name = shape_text_truncated(
                            &tile.label,
                            &bold,
                            px(11. * zoom),
                            self.fg,
                            room,
                            window,
                        );
                        let size = (!tile.header && tile.h > 32. * zoom).then(|| {
                            shape_text_truncated(
                                &tile.size,
                                &font,
                                px(10. * zoom),
                                self.fg.opacity(0.75),
                                room,
                                window,
                            )
                        });
                        (name, size)
                    })
                })
                .collect();
            cache.key = key;
        }
        TreemapPrepaint {
            hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut TreemapPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let cache = self.cache.borrow();
        window.with_content_mask(
            Some(ContentMask {
                bounds,
                fade_out: Default::default(),
            }),
            |window| {
                for (i, tile) in cache.tiles.iter().enumerate() {
                    let rect = Bounds {
                        origin: point(bounds.origin.x + px(tile.x), bounds.origin.y + px(tile.y)),
                        size: size(px(tile.w), px(tile.h)),
                    };
                    let color = if tile.path.is_none() {
                        self.fg
                    } else {
                        super::appearance::file_color(
                            tile.path.as_deref(),
                            tile.directory,
                            &self.colors,
                        )
                    };
                    let selected = tile.path.is_some() && tile.path == self.selected;
                    let hovered = self.hovered == Some(i);
                    // Deeper folders read slightly stronger so nesting stays visible.
                    let depth = f32::from(tile.depth.min(6));
                    let fill = if tile.path.is_none() {
                        if self.dark {
                            0.05
                        } else {
                            0.04
                        }
                    } else if tile.directory {
                        (if self.dark { 0.07 } else { 0.05 }) + 0.025 * depth
                    } else {
                        if self.dark {
                            0.30
                        } else {
                            0.24
                        }
                    };
                    window.paint_quad(quad(
                        rect,
                        Corners::all(px(3. * zoom)),
                        color.opacity(
                            fill + if hovered { 0.10 } else { 0. }
                                + if selected { 0.16 } else { 0. },
                        ),
                        Edges::all(px(if selected { 2. * zoom } else { 1. })),
                        if selected {
                            color.opacity(0.95)
                        } else {
                            self.background.opacity(if self.dark { 0.9 } else { 0.85 })
                        },
                        Default::default(),
                    ));
                    if let Some(Some((name, size))) = cache.labels.get(i) {
                        let origin =
                            point(rect.origin.x + px(4. * zoom), rect.origin.y + px(2. * zoom));
                        let _ =
                            name.paint(origin, px(14. * zoom), TextAlign::Left, None, window, cx);
                        if let Some(size) = size {
                            let _ = size.paint(
                                point(origin.x, origin.y + px(13. * zoom)),
                                px(14. * zoom),
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                    }
                }
            },
        );
        let tiles = Arc::new(cache.tiles.clone());
        drop(cache);
        let hitbox = prepaint.hitbox.clone();
        let panel = self.panel.clone();
        let down_tiles = tiles.clone();
        window.on_mouse_event::<MouseDownEvent>(move |event, phase, window, cx| {
            if phase != DispatchPhase::Bubble
                || event.button != MouseButton::Left
                || !hitbox.is_hovered(window)
            {
                return;
            }
            let Some(index) = hit(&down_tiles, bounds, event.position) else {
                return;
            };
            let tile = down_tiles[index].clone();
            let _ = panel.update(cx, |panel, cx| {
                panel.treemap_click(tile.path.clone(), tile.directory, event.click_count, cx);
            });
            cx.stop_propagation();
        });
        let hitbox = prepaint.hitbox.clone();
        let panel = self.panel.clone();
        window.on_mouse_event::<MouseMoveEvent>(move |event, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let hovered = hitbox
                .is_hovered(window)
                .then(|| hit(&tiles, bounds, event.position))
                .flatten();
            let _ = panel.update(cx, |panel, cx| {
                if panel.treemap_hovered != hovered {
                    panel.treemap_hovered = hovered;
                    panel.treemap_hover_path = hovered.and_then(|i| {
                        tiles
                            .get(i)
                            .map(|t| (t.path.clone(), t.label.clone(), t.size.clone()))
                    });
                    cx.notify();
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    // Explicit imports: the parent's `gpui::*` glob would make `#[test]` mean `gpui::test`.
    use super::{hit, layout, squarify};
    use atlas_model::cleanup::TreemapNode;
    use gpui::{point, px, Bounds};
    use std::path::PathBuf;

    fn node(name: &str, bytes: u64, children: Vec<TreemapNode>) -> TreemapNode {
        let shown: u64 = children.iter().map(|c| c.bytes).sum();
        TreemapNode {
            path: PathBuf::from(name),
            name: name.into(),
            bytes,
            directory: !children.is_empty(),
            other: bytes - shown,
            children,
        }
    }

    #[test]
    fn squarified_areas_are_proportional_and_fill_the_rectangle() {
        let values = [60., 30., 10.];
        let rects = squarify(&values, (0., 0., 100., 50.));
        let total: f32 = rects.iter().map(|r| r.2 * r.3).sum();
        assert!((total - 5_000.).abs() < 1.);
        for (value, rect) in values.iter().zip(&rects) {
            let share = rect.2 * rect.3 / 5_000.;
            assert!((share - (*value as f32) / 100.).abs() < 0.01);
            assert!(
                rect.0 >= -0.01
                    && rect.1 >= -0.01
                    && rect.0 + rect.2 <= 100.01
                    && rect.1 + rect.3 <= 50.01
            );
        }
        assert!(squarify(&[0., 0.], (0., 0., 10., 10.))
            .iter()
            .all(|r| r.2 == 0.));
    }

    #[test]
    fn nested_layout_keeps_children_inside_their_folder() {
        let root = node(
            "root",
            1_000,
            vec![
                node(
                    "a",
                    700,
                    vec![node("a1", 400, vec![]), node("a2", 200, vec![])],
                ),
                node("b", 250, vec![]),
            ],
        );
        let tiles = layout(&root, 400., 300., 1., "other");
        let folder = tiles.iter().find(|t| t.label == "a").unwrap();
        for child in tiles
            .iter()
            .filter(|t| t.label.starts_with("a") && t.label.len() == 2)
        {
            assert!(child.x >= folder.x && child.y >= folder.y);
            assert!(child.x + child.w <= folder.x + folder.w + 0.01);
            assert!(child.y + child.h <= folder.y + folder.h + 0.01);
            assert!(child.depth > folder.depth);
        }
        // Folder "a" has 100 bytes outside a1/a2, the root 50: both drawn as "other".
        assert_eq!(tiles.iter().filter(|t| t.path.is_none()).count(), 2);
        assert_eq!(
            hit(&tiles, Bounds::default(), point(px(1.), px(1.))).map(|i| tiles[i].depth),
            Some(
                tiles
                    .iter()
                    .filter(|t| t.x <= 1. && t.y <= 1. && t.x + t.w > 1. && t.y + t.h > 1.)
                    .map(|t| t.depth)
                    .max()
                    .unwrap()
            )
        );
    }
}
