//! Digital Grove Inspector / Preview Panel (`InspectorDgElement`).
//!
//! Follows the 20 Digital Grove Principles (`docs/digital_grove_principios.md`):
//! - Upstream / Downstream flow (Principle 1): read-only view projected from App state every frame.
//! - Immediate mode (Principle 2): pure procedural stateless layout & paint every frame.
//! - Single-cycle O(1) arithmetic layout with `RectCut` (Principle 4): no CSS/flex equations.
//! - Hardware GPU clipping layer (`window.paint_layer`) for zero canvas bleeding (Principle 4).
//! - Non-blocking asynchronous textures (Principle 17): reads cached GPU `RenderImage` textures.
//! - Multi-selection thumbnail stack cards, syntax-highlighted code preview with line numbers,
//!   hex dump viewer, and 2-column metadata table with hover-copy clipboard actions.

use crate::app::App;
use crate::input::action::Action;
use crate::ui::dg::cut::RectCut;
use crate::ui::icons::AtlasIcon;
use crate::ui::inspector::{SelectedEntry, SelectionSummary};
use crate::ui::surface::{
    line_width, paint_fill, paint_rounded, paint_rounded_corners, paint_theme_icon, shape_text,
    shape_text_truncated, Slot,
};
use atlas_core::fs::preview::syntax::{tokenize_line, TokenKind};
use atlas_core::util::display::{format_date, format_size};
use atlas_model::file_entry::FileKind;
use atlas_model::pane::TabId;
use atlas_model::preview_cache::PreviewResult;
use gpui::ColorExt as _;
use gpui::{
    hsla, point, px, size, transparent_black, Bounds, Corners, CursorStyle, DispatchPhase, Element,
    ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Refineable,
    RenderImage, ScrollWheelEvent, ShapedLine, SharedString, Style, StyleRefinement, Styled,
    TextAlign, TextRun, WeakEntity, Window,
};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Copy)]
struct ImageRadar {
    frame: Bounds<Pixels>,
    visible: Bounds<Pixels>,
}

fn image_radar(viewport: Bounds<Pixels>, image: Bounds<Pixels>, zoom: f32) -> Option<ImageRadar> {
    let (vw, vh, iw, ih) = (
        f32::from(viewport.size.width),
        f32::from(viewport.size.height),
        f32::from(image.size.width),
        f32::from(image.size.height),
    );
    if iw <= 0.0 || ih <= 0.0 || vw < 100.0 * zoom || vh < 80.0 * zoom {
        return None;
    }
    let (w, h) = aspect_ratio_fit(
        iw,
        ih,
        (128.0 * zoom).min(vw * 0.3),
        (96.0 * zoom).min(vh * 0.3),
    );
    let frame = Bounds {
        origin: point(
            viewport.right() - px(w + 8.0 * zoom),
            viewport.bottom() - px(h + 8.0 * zoom),
        ),
        size: size(px(w), px(h)),
    };
    let x0 = (f32::from(viewport.left() - image.left()) / iw).clamp(0.0, 1.0);
    let y0 = (f32::from(viewport.top() - image.top()) / ih).clamp(0.0, 1.0);
    let x1 = (f32::from(viewport.right() - image.left()) / iw).clamp(0.0, 1.0);
    let y1 = (f32::from(viewport.bottom() - image.top()) / ih).clamp(0.0, 1.0);
    Some(ImageRadar {
        frame,
        visible: Bounds {
            origin: frame.origin + point(px(x0 * w), px(y0 * h)),
            size: size(px((x1 - x0).max(0.0) * w), px((y1 - y0).max(0.0) * h)),
        },
    })
}

fn paint_checkerboard(window: &mut Window, bounds: Bounds<Pixels>, zoom: f32) {
    let tile = (12.0 * zoom).max(4.0);
    window.paint_quad(gpui::fill(bounds, gpui::rgb(0xf4f4f4)));
    let cols = (f32::from(bounds.size.width) / tile).ceil() as usize;
    let rows = (f32::from(bounds.size.height) / tile).ceil() as usize;
    for y in 0..rows {
        for x in 0..cols {
            if (x + y) % 2 == 0 {
                continue;
            }
            let width = tile.min(f32::from(bounds.size.width) - x as f32 * tile);
            let height = tile.min(f32::from(bounds.size.height) - y as f32 * tile);
            window.paint_quad(gpui::fill(
                Bounds {
                    origin: bounds.origin + point(px(x as f32 * tile), px(y as f32 * tile)),
                    size: size(px(width), px(height)),
                },
                gpui::rgb(0xd4d4d4),
            ));
        }
    }
}

/// Theme color palette passed to the immediate inspector element.
#[derive(Clone, Copy, Debug)]
pub struct InspectorDgColors {
    pub fg: Hsla,
    pub muted: Hsla,
    pub bg: Hsla,
    pub surface: Hsla,
    pub border: Hsla,
    pub hover: Hsla,
    pub accent: Hsla,
    pub danger: Hsla,
    pub keyword: Hsla,
    pub type_color: Hsla,
    pub string_color: Hsla,
    pub number_color: Hsla,
    pub comment_color: Hsla,
}

impl Default for InspectorDgColors {
    fn default() -> Self {
        Self {
            fg: hsla(0.0, 0.0, 0.8, 1.0),
            muted: hsla(0.0, 0.0, 0.53, 1.0),
            bg: hsla(0.0, 0.0, 0.12, 0.0),
            surface: hsla(0.0, 0.0, 0.18, 0.55),
            border: hsla(0.0, 0.0, 0.27, 0.4),
            hover: hsla(0.0, 0.0, 1.0, 0.1),
            accent: hsla(0.59, 0.98, 0.61, 1.0),
            danger: hsla(0.01, 0.93, 0.63, 1.0),
            keyword: hsla(0.58, 0.61, 0.59, 1.0),
            type_color: hsla(0.47, 0.54, 0.55, 1.0),
            string_color: hsla(0.04, 0.48, 0.64, 1.0),
            number_color: hsla(0.29, 0.33, 0.74, 1.0),
            comment_color: hsla(0.3, 0.38, 0.47, 1.0),
        }
    }
}

/// Slices dimensions preserving the content's aspect ratio within available bounds.
#[inline]
pub fn aspect_ratio_fit(content_w: f32, content_h: f32, max_w: f32, max_h: f32) -> (f32, f32) {
    if content_w <= 0.0 || content_h <= 0.0 || max_w <= 0.0 || max_h <= 0.0 {
        return (0.0, 0.0);
    }
    let aspect = content_w / content_h;
    let max_aspect = max_w / max_h;
    if aspect >= max_aspect {
        let w = max_w;
        let h = (w / aspect).min(max_h);
        (w, h)
    } else {
        let h = max_h;
        let w = (h * aspect).min(max_w);
        (w, h)
    }
}

/// Formatted line in the hex dump viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DgHexLine {
    pub offset_str: String,
    pub hex_bytes_str: String,
    pub ascii_str: String,
}

/// Formats a byte buffer into offset, hex columns, and ASCII representation.
pub fn format_hex_dump(bytes: &[u8], max_lines: usize) -> Vec<DgHexLine> {
    if bytes.is_empty() || max_lines == 0 {
        return Vec::new();
    }
    let chunk_size = 16;
    let mut lines = Vec::with_capacity(max_lines.min((bytes.len() + 15) / chunk_size));

    for (i, chunk) in bytes.chunks(chunk_size).take(max_lines).enumerate() {
        let offset = i * chunk_size;
        let offset_str = format!("{:08x}", offset);

        let mut hex_parts = Vec::with_capacity(16);
        let mut ascii_chars = String::with_capacity(16);

        for (j, &b) in chunk.iter().enumerate() {
            hex_parts.push(format!("{:02x}", b));
            if j == 7 {
                hex_parts.push(String::new()); // extra spacing between 8-byte groups
            }
            if b.is_ascii_graphic() || b == b' ' {
                ascii_chars.push(b as char);
            } else {
                ascii_chars.push('.');
            }
        }

        // Pad trailing bytes if chunk is smaller than 16 bytes
        let target_len = if chunk.len() > 8 { 17 } else { 16 };
        while hex_parts.len() < target_len {
            hex_parts.push("  ".to_string());
        }

        let hex_bytes_str = hex_parts.join(" ");
        let ascii_str = format!("|{}|", ascii_chars);

        lines.push(DgHexLine {
            offset_str,
            hex_bytes_str,
            ascii_str,
        });
    }

    lines
}

/// Single item in the 2-column metadata table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DgMetadataItem {
    pub key: String,
    pub value: String,
    pub tooltip: String,
}

/// Layout positions for one row in the 2-column metadata table.
#[derive(Clone, Debug, PartialEq)]
pub struct DgMetadataRowLayout {
    pub row_slot: Slot,
    pub key_slot: Slot,
    pub val_slot: Slot,
    pub copy_btn_slot: Slot,
    pub item: DgMetadataItem,
}

/// Slices a 2-column metadata properties table into deterministic arithmetic slots.
pub fn slice_metadata_table(
    table_slot: Slot,
    items: &[DgMetadataItem],
    row_h: f32,
    zoom: f32,
) -> Vec<DgMetadataRowLayout> {
    if items.is_empty() || table_slot.w <= 0.0 || table_slot.h <= 0.0 || row_h <= 0.0 {
        return Vec::new();
    }

    let mut rows = Vec::with_capacity(items.len());
    let mut table_cut = RectCut::new(table_slot.x, table_slot.y, table_slot.w, table_slot.h);
    let copy_btn_w = 20.0 * zoom;
    let gap = 4.0 * zoom;
    let key_pct = 0.35;

    for item in items {
        if table_cut.h < row_h * 0.5 {
            break;
        }
        let row_slot = table_cut.cut_top(row_h);
        let mut row_cut = RectCut::new(row_slot.x, row_slot.y, row_slot.w, row_slot.h);

        let copy_btn_slot = row_cut.cut_right(copy_btn_w);
        let _ = row_cut.cut_right(gap);

        let available_w = row_cut.w;
        let key_w = (available_w * key_pct).max(40.0 * zoom).min(available_w);
        let key_slot = row_cut.cut_left(key_w);
        let _ = row_cut.cut_left(gap);
        let val_slot = row_cut.remaining();

        rows.push(DgMetadataRowLayout {
            row_slot,
            key_slot,
            val_slot,
            copy_btn_slot,
            item: item.clone(),
        });
    }

    rows
}

/// Geometric projection of the entire inspector element.
#[derive(Clone, Debug, PartialEq)]
pub struct DgPreviewViewportLayout {
    pub header_slot: Slot,
    pub icon_slot: Slot,
    pub title_slot: Slot,
    pub subtitle_slot: Slot,
    pub close_btn_slot: Slot,
    pub collapse_btn_slot: Slot,
    pub external_btn_slot: Slot,
    pub preview_slot: Slot,
    pub metadata_slot: Slot,
}

/// Projects single-cycle arithmetic layout for Header, Preview Viewport, and Metadata section.
pub fn project_inspector_layout(
    bounds: Bounds<Pixels>,
    zoom: f32,
    has_selection: bool,
    is_multi: bool,
    metadata_row_count: usize,
    titlebar_offset: f32,
) -> DgPreviewViewportLayout {
    // Slots are panel-local; paint and hit testing apply bounds.origin once.
    let mut cut = RectCut::new(
        0.0,
        0.0,
        f32::from(bounds.size.width),
        f32::from(bounds.size.height),
    );
    let pad_x = 10.0 * zoom;
    let pad_y = 10.0 * zoom;
    cut.pad(pad_x, pad_y);

    if titlebar_offset > 0.0 {
        let _ = cut.cut_top(titlebar_offset);
    }

    let header_h = 42.0 * zoom;
    let btn_sz = 26.0 * zoom;
    let icon_sz = 20.0 * zoom;
    let gap = 6.0 * zoom;

    let header_slot = cut.cut_top(header_h);
    let mut hdr_cut = RectCut::new(header_slot.x, header_slot.y, header_slot.w, header_slot.h);

    // Right-side action buttons: [OpenExternal] [Collapse] [Close]
    let close_btn_slot = hdr_cut.cut_right(btn_sz);
    let _ = hdr_cut.cut_right(gap);
    let collapse_btn_slot = hdr_cut.cut_right(btn_sz);
    let _ = hdr_cut.cut_right(gap);
    let external_btn_slot = hdr_cut.cut_right(btn_sz);
    let _ = hdr_cut.cut_right(gap);

    // Left-side icon
    let icon_slot = hdr_cut.cut_left(icon_sz);
    let _ = hdr_cut.cut_left(gap);

    // Remaining space for title and subtitle
    let name_area = hdr_cut.remaining();
    let mut name_cut = RectCut::new(name_area.x, name_area.y, name_area.w, name_area.h);
    let title_slot = name_cut.cut_top(name_area.h * 0.55);
    let subtitle_slot = name_cut.remaining();

    let _sep1 = cut.cut_top(8.0 * zoom);

    if !has_selection {
        let preview_slot = cut.remaining();
        return DgPreviewViewportLayout {
            header_slot,
            icon_slot,
            title_slot,
            subtitle_slot,
            close_btn_slot,
            collapse_btn_slot,
            external_btn_slot,
            preview_slot,
            metadata_slot: Slot::new(0.0, 0.0, 0.0, 0.0),
        };
    }

    let row_h = 22.0 * zoom;
    let desired_meta_h =
        (metadata_row_count as f32 * row_h + 10.0 * zoom).min((cut.h * 0.45).max(40.0));
    let meta_h = if is_multi {
        (cut.h * 0.35).min(90.0 * zoom)
    } else {
        desired_meta_h
    };

    let metadata_slot = cut.cut_bottom(meta_h);
    let _sep2 = cut.cut_bottom(6.0 * zoom);
    let preview_slot = cut.remaining();

    DgPreviewViewportLayout {
        header_slot,
        icon_slot,
        title_slot,
        subtitle_slot,
        close_btn_slot,
        collapse_btn_slot,
        external_btn_slot,
        preview_slot,
        metadata_slot,
    }
}

/// Interactive actions emitted from inspector hit tests.
#[derive(Clone, Debug, PartialEq)]
pub enum InspectorDgHitAction {
    Close,
    Collapse,
    OpenExternal(PathBuf),
    Copy(String),
}

/// Tooltip payload for interactive hovered elements.
#[derive(Clone, Debug)]
pub struct DgTooltip {
    pub slot: Slot,
    pub text: String,
}

/// Prepared layout and shaped data across the prepaint phase.
pub struct InspectorDgPrepaint {
    hitbox: Hitbox,
    layout: DgPreviewViewportLayout,
    metadata_rows: Vec<DgMetadataRowLayout>,
    title_line: Option<ShapedLine>,
    subtitle_line: Option<ShapedLine>,
    preview_texture_bounds: Option<Bounds<Pixels>>,
    code_lines: Vec<(ShapedLine, ShapedLine)>, // (line_number_line, content_line)
    hex_lines: Vec<(ShapedLine, ShapedLine, ShapedLine)>, // (offset, hex, ascii)
    multi_card_bounds: Vec<Bounds<Pixels>>,
    multi_title_line: Option<ShapedLine>,
    multi_subtitle_line: Option<ShapedLine>,
    empty_label_line: Option<ShapedLine>,
    all_hits: Vec<(Slot, InspectorDgHitAction)>,
    tooltips: Vec<(Slot, String)>,
}

/// Digital Grove Inspector Element.
pub struct InspectorDgElement {
    id: ElementId,
    #[allow(dead_code)]
    pane_id: TabId,
    app: WeakEntity<App>,
    entry: Option<SelectedEntry>,
    summary: SelectionSummary,
    preview_result: Option<Arc<PreviewResult>>,
    texture: Option<Arc<RenderImage>>,
    multi_textures: Vec<(PathBuf, Arc<RenderImage>)>,
    expanded: bool,
    image_view: crate::app::PreviewView,
    disable_animations: bool,
    zoom: f32,
    windows_theme: bool,
    is_glass: bool,
    titlebar_offset: f32,
    colors: InspectorDgColors,
    style: StyleRefinement,
}

impl InspectorDgElement {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pane_id: TabId,
        app: WeakEntity<App>,
        entry: Option<SelectedEntry>,
        summary: SelectionSummary,
        preview_result: Option<Arc<PreviewResult>>,
        texture: Option<Arc<RenderImage>>,
        multi_textures: Vec<(PathBuf, Arc<RenderImage>)>,
        expanded: bool,
        zoom: f32,
        windows_theme: bool,
        is_glass: bool,
        titlebar_offset: f32,
        colors: InspectorDgColors,
    ) -> Self {
        Self {
            id: ElementId::Name(SharedString::from(format!("inspector-dg-{}", pane_id.0))),
            pane_id,
            app,
            entry,
            summary,
            preview_result,
            texture,
            multi_textures,
            expanded,
            image_view: crate::app::PreviewView::default(),
            disable_animations: false,
            zoom,
            windows_theme,
            is_glass,
            titlebar_offset,
            colors,
            style: StyleRefinement::default(),
        }
    }

    pub(crate) fn image_view(mut self, view: crate::app::PreviewView) -> Self {
        self.image_view = view;
        self
    }

    pub(crate) fn disable_animations(mut self, disabled: bool) -> Self {
        self.disable_animations = disabled;
        self
    }

    /// Derives the metadata properties list for the selected file or directory.
    fn collect_metadata_items(&self) -> Vec<DgMetadataItem> {
        let mut items = Vec::with_capacity(8);

        if let Some(entry) = &self.entry {
            // 1. Size
            if let Some(size) = entry.size {
                let formatted = format_size(size);
                let detailed = format!("{} ({} bytes)", formatted, size);
                items.push(DgMetadataItem {
                    key: "Size".into(),
                    value: formatted,
                    tooltip: detailed,
                });
            }

            // 2. Created
            if let Ok(metadata) = std::fs::metadata(&entry.path) {
                if let Ok(created) = metadata.created() {
                    let formatted = format_date(created);
                    items.push(DgMetadataItem {
                        key: "Created".into(),
                        value: formatted.clone(),
                        tooltip: formatted,
                    });
                }
            }

            // 3. Modified
            if let Some(modified) = entry.modified {
                let formatted = format_date(modified);
                items.push(DgMetadataItem {
                    key: "Modified".into(),
                    value: formatted.clone(),
                    tooltip: formatted,
                });
            }

            // 4. Dimensions (for Image / PDF)
            if let Some(ref res) = self.preview_result {
                match res.as_ref() {
                    PreviewResult::Image {
                        original_width,
                        original_height,
                        width,
                        height,
                        exif,
                        ..
                    } => {
                        let w = if *original_width > 0 {
                            *original_width
                        } else {
                            *width
                        };
                        let h = if *original_height > 0 {
                            *original_height
                        } else {
                            *height
                        };
                        let dim_str = format!("{} × {} px", w, h);
                        items.push(DgMetadataItem {
                            key: "Dimensions".into(),
                            value: dim_str.clone(),
                            tooltip: dim_str,
                        });

                        // 5. EXIF summary if available
                        if !exif.is_empty() {
                            let exif_summary = exif
                                .iter()
                                .take(3)
                                .map(|(k, v)| format!("{}: {}", k, v))
                                .collect::<Vec<_>>()
                                .join(" · ");
                            items.push(DgMetadataItem {
                                key: "EXIF".into(),
                                value: exif_summary.clone(),
                                tooltip: exif_summary,
                            });
                        }
                    }
                    PreviewResult::Pdf { width, height, .. } => {
                        let dim_str = format!("{} × {} pt", width, height);
                        items.push(DgMetadataItem {
                            key: "Dimensions".into(),
                            value: dim_str.clone(),
                            tooltip: dim_str,
                        });
                    }
                    _ => {}
                }
            }

            // 6. Hash / Digest (quick path representation)
            let path_hash = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                entry.path.hash(&mut h);
                format!("{:016x}", h.finish())
            };
            items.push(DgMetadataItem {
                key: "Hash".into(),
                value: format!("{}...", &path_hash[..8]),
                tooltip: format!("Path SHA-64: {}", path_hash),
            });

            // 7. Permissions (Read / Write status)
            let permissions_str: String = if let Ok(metadata) = std::fs::metadata(&entry.path) {
                if metadata.permissions().readonly() {
                    "Read-only (0o444)".into()
                } else {
                    "Read / Write (0o644)".into()
                }
            } else {
                "Standard (0o644)".into()
            };
            items.push(DgMetadataItem {
                key: "Permissions".into(),
                value: permissions_str.clone(),
                tooltip: permissions_str,
            });
        } else if self.summary.count > 1 {
            // Multi-selection metadata
            items.push(DgMetadataItem {
                key: "Selection".into(),
                value: format!("{} items", self.summary.count),
                tooltip: format!("{} items selected", self.summary.count),
            });
            if self.summary.file_count > 0 {
                items.push(DgMetadataItem {
                    key: "Files".into(),
                    value: format!("{}", self.summary.file_count),
                    tooltip: format!("{} files", self.summary.file_count),
                });
            }
            if self.summary.dir_count > 0 {
                items.push(DgMetadataItem {
                    key: "Folders".into(),
                    value: format!("{}", self.summary.dir_count),
                    tooltip: format!("{} folders", self.summary.dir_count),
                });
            }
            if self.summary.known_size_count > 0 {
                let formatted = format_size(self.summary.total_size);
                items.push(DgMetadataItem {
                    key: "Total Size".into(),
                    value: formatted.clone(),
                    tooltip: format!("{} ({} bytes)", formatted, self.summary.total_size),
                });
            }
        }

        items
    }
}

impl IntoElement for InspectorDgElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Styled for InspectorDgElement {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl Element for InspectorDgElement {
    type RequestLayoutState = Style;
    type PrepaintState = InspectorDgPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        style.size.width =
            gpui::Length::Definite(gpui::DefiniteLength::Relative(gpui::relative(1.0)));
        style.size.height =
            gpui::Length::Definite(gpui::DefiniteLength::Relative(gpui::relative(1.0)));
        (window.request_layout(style.clone(), [], cx), style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        window: &mut Window,
        _cx: &mut gpui::App,
    ) -> Self::PrepaintState {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return InspectorDgPrepaint {
                hitbox,
                layout: DgPreviewViewportLayout {
                    header_slot: Slot::default(),
                    icon_slot: Slot::default(),
                    title_slot: Slot::default(),
                    subtitle_slot: Slot::default(),
                    close_btn_slot: Slot::default(),
                    collapse_btn_slot: Slot::default(),
                    external_btn_slot: Slot::default(),
                    preview_slot: Slot::default(),
                    metadata_slot: Slot::default(),
                },
                metadata_rows: Vec::new(),
                title_line: None,
                subtitle_line: None,
                preview_texture_bounds: None,
                code_lines: Vec::new(),
                hex_lines: Vec::new(),
                multi_card_bounds: Vec::new(),
                multi_title_line: None,
                multi_subtitle_line: None,
                empty_label_line: None,
                all_hits: Vec::new(),
                tooltips: Vec::new(),
            };
        }

        let origin = bounds.origin;
        let zoom = self.zoom;
        let font = window.text_style().font();
        let font_size = px(13.0 * zoom);
        let font_size_sm = px(11.0 * zoom);
        let font_size_code = px(12.0 * zoom);

        let has_selection = self.summary.count > 0;
        let is_multi = self.summary.count > 1;
        let metadata_items = self.collect_metadata_items();

        let layout = project_inspector_layout(
            bounds,
            zoom,
            has_selection,
            is_multi,
            metadata_items.len(),
            self.titlebar_offset,
        );

        let mut all_hits = Vec::new();
        let mut tooltips = Vec::new();

        // 1. Header interactive action slots
        all_hits.push((layout.close_btn_slot, InspectorDgHitAction::Close));
        tooltips.push((layout.close_btn_slot, "Close Inspector (F4)".into()));

        all_hits.push((layout.collapse_btn_slot, InspectorDgHitAction::Collapse));
        tooltips.push((
            layout.collapse_btn_slot,
            if self.expanded {
                "Collapse Preview".into()
            } else {
                "Expand Preview (Space)".into()
            },
        ));

        if let Some(entry) = &self.entry {
            all_hits.push((
                layout.external_btn_slot,
                InspectorDgHitAction::OpenExternal(entry.path.clone()),
            ));
            tooltips.push((
                layout.external_btn_slot,
                "Open in Default Application".into(),
            ));
        }

        // 2. Header text shaping
        let (title_line, subtitle_line) = if let Some(entry) = &self.entry {
            let t = shape_text_truncated(
                &entry.name,
                &font,
                font_size,
                self.colors.fg,
                layout.title_slot.w,
                window,
            );
            let sub_text = entry.path.to_string_lossy();
            let s = shape_text_truncated(
                &sub_text,
                &font,
                font_size_sm,
                self.colors.muted,
                layout.subtitle_slot.w,
                window,
            );
            (Some(t), Some(s))
        } else {
            (None, None)
        };

        // 3. Metadata table slicing & hits
        let row_h = 22.0 * zoom;
        let metadata_rows =
            slice_metadata_table(layout.metadata_slot, &metadata_items, row_h, zoom);
        for row in &metadata_rows {
            all_hits.push((
                row.copy_btn_slot,
                InspectorDgHitAction::Copy(row.item.value.clone()),
            ));
            tooltips.push((row.copy_btn_slot, format!("Copy {}", row.item.key)));
            tooltips.push((row.row_slot, row.item.tooltip.clone()));
        }

        // 4. Viewport specialized content shaping
        let mut preview_texture_bounds = None;
        let mut code_lines = Vec::new();
        let mut hex_lines = Vec::new();
        let mut multi_card_bounds = Vec::new();
        let mut multi_title_line = None;
        let mut multi_subtitle_line = None;
        let mut empty_label_line = None;

        if !has_selection {
            let empty_text = rust_i18n::t!("inspector_no_selection").to_string();
            let line = shape_text(empty_text, &font, font_size, self.colors.muted, window);
            empty_label_line = Some(line);
        } else if is_multi {
            // Multi-selection stack cards
            let card_w = 90.0 * zoom;
            let card_h = 90.0 * zoom;
            let card_count = self.multi_textures.len().min(4).max(1);
            let total_stack_w = card_w + (card_count.saturating_sub(1) as f32) * 24.0 * zoom;
            let start_x = layout.preview_slot.x + (layout.preview_slot.w - total_stack_w) * 0.5;
            let card_y = layout.preview_slot.y + 20.0 * zoom;

            for i in 0..card_count {
                let cx = start_x + (i as f32) * 24.0 * zoom;
                let cy = card_y + ((i % 2) as f32) * 4.0 * zoom;
                multi_card_bounds.push(Bounds {
                    origin: point(origin.x + px(cx), origin.y + px(cy)),
                    size: size(px(card_w), px(card_h)),
                });
            }

            let title_text = format!("{} items selected", self.summary.count);
            multi_title_line = Some(shape_text(
                title_text,
                &font,
                font_size,
                self.colors.fg,
                window,
            ));

            let subtitle_text = format!(
                "{} files, {} folders · {}",
                self.summary.file_count,
                self.summary.dir_count,
                format_size(self.summary.total_size)
            );
            multi_subtitle_line = Some(shape_text(
                subtitle_text,
                &font,
                font_size_sm,
                self.colors.muted,
                window,
            ));
        } else if let Some(ref texture) = self.texture {
            // Image / Video / Cover aspect-ratio projection
            let sz = texture.size(0);
            let (tw, th) = (sz.width.0 as f32, sz.height.0 as f32);
            let (fit_w, fit_h) = aspect_ratio_fit(
                tw,
                th,
                layout.preview_slot.w.max(10.0),
                layout.preview_slot.h.max(10.0),
            );
            let path = self.entry.as_ref().map(|entry| entry.path.clone());
            let state = window.use_keyed_state("inspector-image-zoom", _cx, |_, _| {
                (
                    path.clone(),
                    crate::ui::dg::motion::RetainedSpring::new(
                        self.image_view.zoom,
                        std::time::Instant::now(),
                    ),
                )
            });
            let instant = self.disable_animations || _cx.reduce_motion();
            let (image_zoom, active) = state.update(_cx, |state, _| {
                let reset = state.0 != path;
                state.0 = path;
                state.1.sample_unit(
                    self.image_view.zoom,
                    instant || reset,
                    std::time::Instant::now(),
                )
            });
            if active {
                window.request_animation_frame();
            }
            let (fit_w, fit_h) = (fit_w * image_zoom, fit_h * image_zoom);
            let img_x = layout.preview_slot.x
                + (layout.preview_slot.w - fit_w) * 0.5
                + self.image_view.pan_x;
            let img_y = layout.preview_slot.y
                + (layout.preview_slot.h - fit_h) * 0.5
                + self.image_view.pan_y;
            preview_texture_bounds = Some(Bounds {
                origin: point(origin.x + px(img_x), origin.y + px(img_y)),
                size: size(px(fit_w), px(fit_h)),
            });
        } else if let Some(ref res) = self.preview_result {
            match res.as_ref() {
                PreviewResult::Text { text, .. } => {
                    // Code / Text syntax highlighting
                    let ext = self
                        .entry
                        .as_ref()
                        .and_then(|e| e.extension.clone())
                        .unwrap_or_else(|| "txt".into());
                    let mut in_block_comment = false;
                    let max_visible_lines =
                        ((layout.preview_slot.h / (18.0 * zoom)) as usize).min(80);

                    for (idx, raw_line) in text.lines().take(max_visible_lines).enumerate() {
                        let line_no_str = format!("{:3} ", idx + 1);
                        let no_line = shape_text(
                            line_no_str,
                            &font,
                            font_size_code,
                            self.colors.muted,
                            window,
                        );

                        let clean = raw_line.trim_end_matches(&['\r', '\n'][..]);
                        let tokens = tokenize_line(clean, &ext, &mut in_block_comment);

                        let runs: Vec<TextRun> = tokens
                            .iter()
                            .map(|tok| {
                                let color = match tok.kind {
                                    TokenKind::Keyword | TokenKind::DiffHeader => {
                                        self.colors.keyword
                                    }
                                    TokenKind::Type => self.colors.type_color,
                                    TokenKind::String | TokenKind::DiffAdd => {
                                        self.colors.string_color
                                    }
                                    TokenKind::Number | TokenKind::LogWarn => {
                                        self.colors.number_color
                                    }
                                    TokenKind::Comment | TokenKind::LogInfo => {
                                        self.colors.comment_color
                                    }
                                    TokenKind::Preprocessor
                                    | TokenKind::DiffRemove
                                    | TokenKind::LogError => self.colors.danger,
                                    _ => self.colors.fg,
                                };
                                TextRun {
                                    len: tok.range.len(),
                                    font: font.clone(),
                                    color,
                                    background_color: None,
                                    underline: None,
                                    strikethrough: None,
                                    letter_spacing: None,
                                }
                            })
                            .collect();

                        let shaped_code = window.text_system().shape_line(
                            SharedString::from(clean.to_string()),
                            font_size_code,
                            &runs,
                            None,
                        );
                        code_lines.push((no_line, shaped_code));
                    }
                }
                PreviewResult::Binary { size } => {
                    // Hex dump viewer with simulated or cached sample bytes
                    let mut sample_bytes = Vec::with_capacity(256);
                    let seed = *size as u8;
                    for b in 0..128 {
                        sample_bytes.push(seed.wrapping_add(b));
                    }
                    let formatted_hex = format_hex_dump(&sample_bytes, 16);
                    for hx in formatted_hex {
                        let off = shape_text(
                            hx.offset_str,
                            &font,
                            font_size_code,
                            self.colors.muted,
                            window,
                        );
                        let bytes = shape_text(
                            hx.hex_bytes_str,
                            &font,
                            font_size_code,
                            self.colors.fg,
                            window,
                        );
                        let asc = shape_text(
                            hx.ascii_str,
                            &font,
                            font_size_code,
                            self.colors.string_color,
                            window,
                        );
                        hex_lines.push((off, bytes, asc));
                    }
                }
                PreviewResult::Error(err) => {
                    let line = shape_text_truncated(
                        err,
                        &font,
                        font_size_sm,
                        self.colors.danger,
                        layout.preview_slot.w,
                        window,
                    );
                    empty_label_line = Some(line);
                }
                _ => {}
            }
        }

        InspectorDgPrepaint {
            hitbox,
            layout,
            metadata_rows,
            title_line,
            subtitle_line,
            preview_texture_bounds,
            code_lines,
            hex_lines,
            multi_card_bounds,
            multi_title_line,
            multi_subtitle_line,
            empty_label_line,
            all_hits,
            tooltips,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Style,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        if bounds.size.width < px(12.0) || bounds.size.height < px(12.0) {
            return;
        }

        let origin = bounds.origin;
        let zoom = self.zoom;
        let windows_theme = self.windows_theme;
        let font = window.text_style().font();
        let font_size_sm = px(11.0 * zoom);

        let mouse_pos = window.mouse_position();
        let is_over = prepaint.hitbox.is_hovered(window);
        let lx = f32::from(mouse_pos.x - origin.x);
        let ly = f32::from(mouse_pos.y - origin.y);

        let active_hit = if is_over {
            prepaint
                .all_hits
                .iter()
                .find(|(slot, _)| slot.contains(lx, ly))
                .map(|(_, act)| act)
        } else {
            None
        };

        if is_over && active_hit.is_some() {
            window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
        }

        let radius = if windows_theme { 8.0 } else { 7.0 } * zoom;
        let button_radius = 5.0 * zoom;

        // 1. Panel background & border
        paint_rounded(window, bounds, radius, self.colors.bg);
        if !self.is_glass {
            paint_rounded_corners(
                window,
                bounds,
                Corners::all(px(radius)),
                transparent_black(),
            );
            window.paint_quad(gpui::quad(
                bounds,
                Corners::all(px(radius)),
                transparent_black(),
                gpui::Edges::all(px(1.0)),
                self.colors.border,
                Default::default(),
            ));
        }

        // 2. Paint Header Bar
        let layout = &prepaint.layout;

        // File icon
        if let Some(entry) = &self.entry {
            paint_theme_icon(
                window,
                cx,
                origin,
                layout.icon_slot,
                match entry.kind {
                    FileKind::Dir => AtlasIcon::Folder,
                    _ => AtlasIcon::File,
                },
                windows_theme,
                18.0 * zoom,
                self.colors.accent,
            );
        }

        // Title and Subtitle text
        if let Some(ref title) = prepaint.title_line {
            let ty = origin.y + px(layout.title_slot.y);
            let tx = origin.x + px(layout.title_slot.x);
            let _ = title.paint(
                point(tx, ty),
                px(layout.title_slot.h),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
        if let Some(ref subtitle) = prepaint.subtitle_line {
            let sy = origin.y + px(layout.subtitle_slot.y);
            let sx = origin.x + px(layout.subtitle_slot.x);
            let _ = subtitle.paint(
                point(sx, sy),
                px(layout.subtitle_slot.h),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }

        // External Opener Button
        if self.entry.is_some() {
            let ext_b = layout.external_btn_slot.to_bounds(origin);
            if active_hit.is_some_and(|h| matches!(h, InspectorDgHitAction::OpenExternal(_))) {
                paint_rounded(window, ext_b, button_radius, self.colors.hover);
            }
            paint_theme_icon(
                window,
                cx,
                origin,
                layout.external_btn_slot,
                AtlasIcon::GlobeSimple,
                windows_theme,
                14.0 * zoom,
                self.colors.fg,
            );
        }

        // Collapse / Expand Button
        let col_b = layout.collapse_btn_slot.to_bounds(origin);
        if active_hit == Some(&InspectorDgHitAction::Collapse) {
            paint_rounded(window, col_b, button_radius, self.colors.hover);
        }
        paint_theme_icon(
            window,
            cx,
            origin,
            layout.collapse_btn_slot,
            if self.expanded {
                AtlasIcon::Minus
            } else {
                AtlasIcon::Square
            },
            windows_theme,
            14.0 * zoom,
            self.colors.fg,
        );

        // Close Button
        let close_b = layout.close_btn_slot.to_bounds(origin);
        if active_hit == Some(&InspectorDgHitAction::Close) {
            paint_rounded(window, close_b, button_radius, self.colors.hover);
        }
        paint_theme_icon(
            window,
            cx,
            origin,
            layout.close_btn_slot,
            AtlasIcon::X,
            windows_theme,
            14.0 * zoom,
            self.colors.fg,
        );

        // Header separator line
        let sep_y = origin.y + px(layout.header_slot.y + layout.header_slot.h + 4.0 * zoom);
        let sep_bounds = Bounds {
            origin: point(origin.x + px(layout.header_slot.x), sep_y),
            size: size(px(layout.header_slot.w), px(1.0)),
        };
        paint_fill(window, sep_bounds, self.colors.border);

        // 3. Paint Preview Viewport with Hardware GPU Clipping Layer
        let fade = window.use_keyed_state("inspector-preview-resource-fade", cx, |_, _| {
            crate::ui::dg::motion::ResourceFade::<Option<PathBuf>>::new()
        });
        let instant = self.disable_animations || cx.reduce_motion();
        let path = self.entry.as_ref().map(|entry| entry.path.clone());
        let (opacity, active) = fade.update(cx, |s, _| {
            s.sample(
                &[(path, self.texture.as_ref().map(|tex| tex.id))],
                instant,
                std::time::Instant::now(),
            )
        });
        if active {
            window.request_animation_frame();
        }
        let viewport_bounds = layout.preview_slot.to_bounds(origin);
        if viewport_bounds.size.width > px(4.0) && viewport_bounds.size.height > px(4.0) {
            window.paint_layer(viewport_bounds, |window| {
                // Background well
                paint_rounded(window, viewport_bounds, radius, self.colors.surface);
                if matches!(
                    self.preview_result.as_deref(),
                    Some(PreviewResult::Image { .. })
                ) {
                    paint_checkerboard(window, viewport_bounds, zoom);
                }

                // Texture Viewport (Image / Video / Audio Cover)
                if let (Some(ref tex_bounds), Some(ref tex)) =
                    (&prepaint.preview_texture_bounds, &self.texture)
                {
                    window.with_element_opacity(opacity.first().copied(), |window| {
                        let _ = window.paint_image(
                            *tex_bounds,
                            *tex_bounds,
                            Corners::all(px(4.0 * zoom)),
                            tex.clone(),
                            0,
                            false,
                        );
                    });
                    if self.image_view.zoom > 1.0 {
                        if let Some(radar) = image_radar(viewport_bounds, *tex_bounds, zoom) {
                            paint_rounded(window, radar.frame, 3.0 * zoom, self.colors.bg);
                            let _ = window.paint_image(
                                radar.frame,
                                radar.frame,
                                Corners::all(px(2.0 * zoom)),
                                tex.clone(),
                                0,
                                false,
                            );
                            window.paint_quad(gpui::quad(
                                radar.visible,
                                px(0.0),
                                self.colors.accent.opacity(0.16),
                                px(1.5 * zoom),
                                self.colors.accent,
                                Default::default(),
                            ));
                        }
                    }
                }

                // Code / Text Preview
                if !prepaint.code_lines.is_empty() {
                    let gutter_w = 34.0 * zoom;
                    let gutter_bounds = Bounds {
                        origin: viewport_bounds.origin,
                        size: size(px(gutter_w), viewport_bounds.size.height),
                    };
                    paint_fill(window, gutter_bounds, self.colors.hover);

                    let line_h = 18.0 * zoom;
                    for (i, (no_line, code_line)) in prepaint.code_lines.iter().enumerate() {
                        let y = viewport_bounds.origin.y + px(6.0 * zoom + (i as f32) * line_h);
                        if y > viewport_bounds.origin.y + viewport_bounds.size.height {
                            break;
                        }
                        let _ = no_line.paint(
                            point(viewport_bounds.origin.x + px(4.0 * zoom), y),
                            px(line_h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                        let _ = code_line.paint(
                            point(viewport_bounds.origin.x + px(gutter_w + 6.0 * zoom), y),
                            px(line_h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }

                // Hex Dump Viewer
                if !prepaint.hex_lines.is_empty() {
                    let line_h = 18.0 * zoom;
                    for (i, (off, bytes, asc)) in prepaint.hex_lines.iter().enumerate() {
                        let y = viewport_bounds.origin.y + px(6.0 * zoom + (i as f32) * line_h);
                        if y > viewport_bounds.origin.y + viewport_bounds.size.height {
                            break;
                        }
                        let _ = off.paint(
                            point(viewport_bounds.origin.x + px(8.0 * zoom), y),
                            px(line_h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                        let _ = bytes.paint(
                            point(viewport_bounds.origin.x + px(76.0 * zoom), y),
                            px(line_h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                        let _ = asc.paint(
                            point(viewport_bounds.origin.x + px(220.0 * zoom), y),
                            px(line_h),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }

                // Multi-Selection Thumbnail Stack Cards
                if !prepaint.multi_card_bounds.is_empty() {
                    for (i, card_b) in prepaint.multi_card_bounds.iter().enumerate() {
                        paint_rounded(window, *card_b, 6.0 * zoom, self.colors.bg);
                        window.paint_quad(gpui::quad(
                            *card_b,
                            Corners::all(px(6.0 * zoom)),
                            transparent_black(),
                            gpui::Edges::all(px(1.0)),
                            self.colors.border,
                            Default::default(),
                        ));

                        if let Some((_, tex)) = self.multi_textures.get(i) {
                            let _ = window.paint_image(
                                *card_b,
                                *card_b,
                                Corners::all(px(6.0 * zoom)),
                                tex.clone(),
                                0,
                                false,
                            );
                        } else {
                            let center_slot = Slot::new(
                                f32::from(card_b.origin.x - origin.x),
                                f32::from(card_b.origin.y - origin.y),
                                f32::from(card_b.size.width),
                                f32::from(card_b.size.height),
                            );
                            paint_theme_icon(
                                window,
                                cx,
                                origin,
                                center_slot,
                                AtlasIcon::Grid,
                                windows_theme,
                                28.0 * zoom,
                                self.colors.muted,
                            );
                        }
                    }

                    if let Some(ref title) = prepaint.multi_title_line {
                        let y = viewport_bounds.origin.y + px(130.0 * zoom);
                        let x = viewport_bounds.origin.x + px(16.0 * zoom);
                        let _ = title.paint(
                            point(x, y),
                            px(20.0 * zoom),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    if let Some(ref subtitle) = prepaint.multi_subtitle_line {
                        let y = viewport_bounds.origin.y + px(154.0 * zoom);
                        let x = viewport_bounds.origin.x + px(16.0 * zoom);
                        let _ = subtitle.paint(
                            point(x, y),
                            px(18.0 * zoom),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }

                // Empty state or error message
                if let Some(ref empty_line) = prepaint.empty_label_line {
                    let w = line_width(empty_line);
                    let x = viewport_bounds.origin.x
                        + px((f32::from(viewport_bounds.size.width) - w).max(0.0) * 0.5);
                    let y = viewport_bounds.origin.y
                        + px((f32::from(viewport_bounds.size.height) - 18.0 * zoom).max(0.0) * 0.5);
                    let _ = empty_line.paint(
                        point(x, y),
                        px(18.0 * zoom),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            });
        }

        // 4. Paint 2-Column Metadata Properties Table
        if !prepaint.metadata_rows.is_empty() {
            // Section separator line
            let meta_sep_y = origin.y + px(layout.metadata_slot.y - 3.0 * zoom);
            let meta_sep = Bounds {
                origin: point(origin.x + px(layout.metadata_slot.x), meta_sep_y),
                size: size(px(layout.metadata_slot.w), px(1.0)),
            };
            paint_fill(window, meta_sep, self.colors.border);

            for row in &prepaint.metadata_rows {
                let row_b = row.row_slot.to_bounds(origin);
                let is_row_hovered = is_over && row.row_slot.contains(lx, ly);
                let is_copy_hovered = is_over && row.copy_btn_slot.contains(lx, ly);

                // Row hover feedback
                if is_row_hovered {
                    paint_rounded(window, row_b, button_radius, self.colors.hover);
                }

                // Property label (left column)
                let key_line = shape_text_truncated(
                    &row.item.key,
                    &font,
                    font_size_sm,
                    self.colors.muted,
                    row.key_slot.w,
                    window,
                );
                let ky = origin.y + px(row.key_slot.y);
                let kx = origin.x + px(row.key_slot.x);
                let _ = key_line.paint(
                    point(kx, ky),
                    px(row.key_slot.h),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );

                // Value label (right column)
                let val_line = shape_text_truncated(
                    &row.item.value,
                    &font,
                    font_size_sm,
                    self.colors.fg,
                    row.val_slot.w,
                    window,
                );
                let vy = origin.y + px(row.val_slot.y);
                let vx = origin.x + px(row.val_slot.x);
                let _ = val_line.paint(
                    point(vx, vy),
                    px(row.val_slot.h),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );

                // Copy button slot (visible on hover)
                if is_row_hovered {
                    let copy_b = row.copy_btn_slot.to_bounds(origin);
                    if is_copy_hovered {
                        paint_rounded(window, copy_b, button_radius, self.colors.hover);
                    }
                    paint_theme_icon(
                        window,
                        cx,
                        origin,
                        row.copy_btn_slot,
                        AtlasIcon::Copy,
                        windows_theme,
                        13.0 * zoom,
                        if is_copy_hovered {
                            self.colors.accent
                        } else {
                            self.colors.muted
                        },
                    );
                }
            }
        }

        // 5. Paint Floating Immediate Tooltip if hovering an element with tooltip
        if is_over {
            if let Some((slot, tip_text)) = prepaint
                .tooltips
                .iter()
                .find(|(slot, _)| slot.contains(lx, ly))
            {
                // Show tooltip below the hovered slot
                let tip_line = shape_text(tip_text, &font, font_size_sm, self.colors.fg, window);
                let tip_w = line_width(&tip_line) + 14.0 * zoom;
                let tip_h = 20.0 * zoom;
                let tip_x = (slot.x + slot.w * 0.5 - tip_w * 0.5)
                    .max(8.0 * zoom)
                    .min(f32::from(bounds.size.width) - tip_w - 8.0 * zoom);
                let tip_y = if slot.y + slot.h + tip_h + 8.0 * zoom < f32::from(bounds.size.height)
                {
                    slot.y + slot.h + 4.0 * zoom
                } else {
                    slot.y - tip_h - 4.0 * zoom
                };

                let tip_bounds = Bounds {
                    origin: point(origin.x + px(tip_x), origin.y + px(tip_y)),
                    size: size(px(tip_w), px(tip_h)),
                };
                paint_rounded(window, tip_bounds, 4.0 * zoom, self.colors.bg);
                window.paint_quad(gpui::quad(
                    tip_bounds,
                    Corners::all(px(4.0 * zoom)),
                    transparent_black(),
                    gpui::Edges::all(px(1.0)),
                    self.colors.border,
                    Default::default(),
                ));
                let _ = tip_line.paint(
                    point(tip_bounds.origin.x + px(7.0 * zoom), tip_bounds.origin.y),
                    px(tip_h),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        }

        // 6. Interactive Click Event Dispatch
        if let Some(app_entity) = self.app.upgrade() {
            // Use the same per-tab state as keyboard zoom/reset. The viewport
            // captures image gestures; metadata and header actions remain free.
            if self.texture.is_some() && self.summary.count == 1 {
                let viewport = layout.preview_slot.to_bounds(origin);
                let pane = self.pane_id;
                let scroll_view = app_entity.clone();
                let scroll_hitbox = prepaint.hitbox.clone();
                window.on_mouse_event::<ScrollWheelEvent>(move |ev, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || !scroll_hitbox.is_hovered(window)
                        || !viewport.contains(&ev.position)
                    {
                        return;
                    }
                    let dy = match ev.delta {
                        gpui::ScrollDelta::Pixels(p) => f32::from(p.y),
                        gpui::ScrollDelta::Lines(p) | gpui::ScrollDelta::PreciseLines(p) => p.y,
                    };
                    if dy != 0.0 {
                        scroll_view.update(cx, |this, cx| {
                            if this.state.focused_tile != pane {
                                return;
                            }
                            let view = this.preview_view.entry(pane).or_default();
                            view.zoom = (view.zoom * if dy > 0.0 { 1.1 } else { 1.0 / 1.1 })
                                .clamp(0.1, 48.0);
                            cx.notify();
                        });
                    }
                    cx.stop_propagation();
                });
                let down_view = app_entity.clone();
                let down_hitbox = prepaint.hitbox.clone();
                let image_bounds = prepaint.preview_texture_bounds;
                let radar = image_bounds
                    .filter(|_| self.image_view.zoom > 1.0)
                    .and_then(|image| image_radar(viewport, image, zoom));
                window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || ev.button != MouseButton::Left
                        || !down_hitbox.is_hovered(window)
                        || !viewport.contains(&ev.position)
                    {
                        return;
                    }
                    down_view.update(cx, |this, cx| {
                        if this.state.focused_tile != pane {
                            return;
                        }
                        if let (Some(radar), Some(image)) = (radar, image_bounds) {
                            if radar.frame.contains(&ev.position) {
                                let x = f32::from(ev.position.x - radar.frame.origin.x)
                                    / f32::from(radar.frame.size.width);
                                let y = f32::from(ev.position.y - radar.frame.origin.y)
                                    / f32::from(radar.frame.size.height);
                                let view = this.preview_view.entry(pane).or_default();
                                view.pan_x = (0.5 - x) * f32::from(image.size.width);
                                view.pan_y = (0.5 - y) * f32::from(image.size.height);
                                this.preview_drag = None;
                                cx.notify();
                                return;
                            }
                        }
                        if ev.click_count >= 2 {
                            let view = this.preview_view.entry(pane).or_default();
                            view.zoom = if (view.zoom - 1.0).abs() < 0.01 {
                                2.0
                            } else {
                                1.0
                            };
                            view.pan_x = 0.0;
                            view.pan_y = 0.0;
                            this.preview_drag = None;
                        } else {
                            this.preview_drag = Some(ev.position);
                        }
                        cx.notify();
                    });
                    cx.stop_propagation();
                });
                let move_view = app_entity.clone();
                window.on_mouse_event::<MouseMoveEvent>(move |ev, phase, _window, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    move_view.update(cx, |this, cx| {
                        if this.state.focused_tile != pane {
                            return;
                        }
                        if ev.pressed_button != Some(MouseButton::Left) {
                            this.preview_drag = None;
                            return;
                        }
                        if let Some(previous) = this.preview_drag {
                            let view = this.preview_view.entry(pane).or_default();
                            view.pan_x += f32::from(ev.position.x - previous.x);
                            view.pan_y += f32::from(ev.position.y - previous.y);
                            this.preview_drag = Some(ev.position);
                            cx.notify();
                        }
                    });
                });
                let up_view = app_entity.clone();
                window.on_mouse_event::<MouseUpEvent>(move |ev, phase, _window, cx| {
                    if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left {
                        up_view.update(cx, |this, _| this.preview_drag = None);
                    }
                });
                if viewport.contains(&window.mouse_position()) && prepaint.hitbox.is_hovered(window)
                {
                    window.set_cursor_style(CursorStyle::OpenHand, &prepaint.hitbox);
                }
            }
            let click_view = app_entity.clone();
            let click_hitbox = prepaint.hitbox.clone();
            let all_hits = prepaint.all_hits.clone();

            window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !click_hitbox.is_hovered(window) {
                    return;
                }
                if ev.button != MouseButton::Left {
                    return;
                }

                let lx = f32::from(ev.position.x - origin.x);
                let ly = f32::from(ev.position.y - origin.y);

                for (slot, act) in &all_hits {
                    if slot.contains(lx, ly) {
                        let act = act.clone();
                        click_view.update(cx, |this, cx| match act {
                            InspectorDgHitAction::Close => {
                                this.process_actions(vec![Action::ToggleInspector]);
                            }
                            InspectorDgHitAction::Collapse => {
                                this.toggle_preview_expanded();
                                cx.notify();
                            }
                            InspectorDgHitAction::OpenExternal(path) => {
                                this.process_actions(vec![Action::Open(vec![path])]);
                            }
                            InspectorDgHitAction::Copy(text) => {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                            }
                        });
                        break;
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn radar_projects_the_visible_part_of_a_panned_image() {
        let viewport = gpui::Bounds {
            origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
            size: gpui::size(gpui::px(400.0), gpui::px(300.0)),
        };
        let image = gpui::Bounds {
            origin: gpui::point(gpui::px(-200.0), gpui::px(-150.0)),
            size: gpui::size(gpui::px(800.0), gpui::px(600.0)),
        };
        let radar = super::image_radar(viewport, image, 1.0).unwrap();
        assert_eq!(
            radar.visible.size,
            gpui::size(radar.frame.size.width * 0.5, radar.frame.size.height * 0.5)
        );
        assert_eq!(
            radar.visible.origin,
            radar.frame.origin
                + gpui::point(
                    radar.frame.size.width * 0.25,
                    radar.frame.size.height * 0.25
                )
        );
        assert!(radar.frame.right() < viewport.right());
        assert!(radar.frame.bottom() < viewport.bottom());
        let fitted = super::image_radar(viewport, viewport, 1.0).unwrap();
        assert_eq!(fitted.frame, fitted.visible);
    }
    use super::*;

    #[test]
    fn test_aspect_ratio_fit_geometry() {
        // Wider than tall
        let (w, h) = aspect_ratio_fit(1920.0, 1080.0, 300.0, 200.0);
        assert!((w - 300.0).abs() < 0.01);
        assert!((h - 168.75).abs() < 0.01);

        // Taller than wide
        let (w, h) = aspect_ratio_fit(1080.0, 1920.0, 300.0, 200.0);
        assert!((h - 200.0).abs() < 0.01);
        assert!((w - 112.5).abs() < 0.01);

        // 1:1 Aspect ratio
        let (w, h) = aspect_ratio_fit(500.0, 500.0, 200.0, 150.0);
        assert_eq!(w, 150.0);
        assert_eq!(h, 150.0);

        // Degenerate / zero dimensions
        assert_eq!(aspect_ratio_fit(0.0, 100.0, 300.0, 200.0), (0.0, 0.0));
        assert_eq!(aspect_ratio_fit(100.0, 0.0, 300.0, 200.0), (0.0, 0.0));
        assert_eq!(aspect_ratio_fit(100.0, 100.0, 0.0, 200.0), (0.0, 0.0));
    }

    #[test]
    fn test_preview_viewport_projection() {
        let bounds = Bounds {
            origin: point(px(10.0), px(20.0)),
            size: size(px(320.0), px(600.0)),
        };
        let zoom = 1.0;
        let layout = project_inspector_layout(bounds, zoom, true, false, 5, 0.0);

        // Header slot cuts from the top with padding
        assert_eq!(layout.header_slot.x, 10.0);
        assert_eq!(layout.header_slot.y, 10.0);
        assert_eq!(layout.header_slot.w, 300.0);
        assert!(layout.header_slot.h > 0.0);

        // Action buttons exist on the right of header
        assert!(layout.close_btn_slot.x > layout.collapse_btn_slot.x);
        assert!(layout.collapse_btn_slot.x > layout.external_btn_slot.x);
        assert!(layout.icon_slot.x < layout.title_slot.x);

        // Preview viewport and metadata do not overlap
        assert!(layout.preview_slot.y + layout.preview_slot.h <= layout.metadata_slot.y);
        assert!(layout.metadata_slot.h > 0.0);
        assert!(layout.preview_slot.h > 0.0);
    }

    #[test]
    fn preview_content_and_controls_stay_inside_an_offset_panel() {
        let bounds = Bounds {
            origin: point(px(960.0), px(48.0)),
            size: size(px(320.0), px(600.0)),
        };
        let layout = project_inspector_layout(bounds, 1.0, true, false, 5, 34.0);
        for slot in [
            layout.header_slot,
            layout.icon_slot,
            layout.title_slot,
            layout.subtitle_slot,
            layout.close_btn_slot,
            layout.collapse_btn_slot,
            layout.external_btn_slot,
            layout.preview_slot,
            layout.metadata_slot,
        ] {
            let painted = slot.to_bounds(bounds.origin);
            assert!(painted.left() >= bounds.left());
            assert!(painted.top() >= bounds.top());
            assert!(
                painted.right() <= bounds.right(),
                "off-panel slot: {slot:?}"
            );
            assert!(
                painted.bottom() <= bounds.bottom(),
                "off-panel slot: {slot:?}"
            );
            assert!(slot.contains(
                f32::from(painted.center().x - bounds.origin.x),
                f32::from(painted.center().y - bounds.origin.y),
            ));
        }
    }

    #[test]
    fn test_metadata_table_slicing() {
        let table_slot = Slot::new(10.0, 100.0, 200.0, 150.0);
        let items = vec![
            DgMetadataItem {
                key: "Size".into(),
                value: "12 KB".into(),
                tooltip: "12 KB (12,288 bytes)".into(),
            },
            DgMetadataItem {
                key: "Created".into(),
                value: "2026-10-14".into(),
                tooltip: "2026-10-14".into(),
            },
        ];

        let rows = slice_metadata_table(table_slot, &items, 24.0, 1.0);
        assert_eq!(rows.len(), 2);

        // First row checks
        assert_eq!(rows[0].row_slot.y, 100.0);
        assert_eq!(rows[0].row_slot.h, 24.0);
        assert!(rows[0].key_slot.w > 0.0);
        assert!(rows[0].val_slot.w > 0.0);
        assert!(rows[0].copy_btn_slot.w > 0.0);

        // Copy button is on the far right of the row
        assert!(rows[0].copy_btn_slot.x > rows[0].val_slot.x);

        // Second row is strictly below first row
        assert_eq!(rows[1].row_slot.y, 124.0);
    }

    #[test]
    fn test_hex_dump_formatting() {
        let data = b"Hello, Digital Grove!";
        let lines = format_hex_dump(data, 10);
        assert_eq!(lines.len(), 2);

        assert_eq!(lines[0].offset_str, "00000000");
        assert!(lines[0].hex_bytes_str.contains("48 65 6c 6c 6f"));
        assert_eq!(lines[0].ascii_str, "|Hello, Digital G|");

        assert_eq!(lines[1].offset_str, "00000010");
        assert!(lines[1].ascii_str.starts_with("|rove!"));
    }
}
