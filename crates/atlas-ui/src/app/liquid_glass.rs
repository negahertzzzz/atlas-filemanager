//! GPU texture cache for Icons8 Liquid Glass default file and folder icons.
//!
//! Pre-decodes embedded PNG assets into Arc<RenderImage> with GPUI-expected
//! straight BGRA channel ordering, enabling zero-allocation instant rendering
//! in both high-performance custom virtual elements (DetailsElement, GridSurfaceElement)
//! and standard GPUI views (miller, list, inspector, etc.).

use std::collections::HashMap;
use std::sync::Arc;

use atlas_model::file_entry::FileKind;
use gpui::RenderImage;

use crate::assets::liquid_glass_filetype;
use crate::ui::icons::{file_type_icon_kind, FileTypeIcon};

const ALL_ICONS: &[FileTypeIcon] = &[
    FileTypeIcon::Folder,
    FileTypeIcon::File,
    FileTypeIcon::Document,
    FileTypeIcon::Pdf,
    FileTypeIcon::Image,
    FileTypeIcon::Audio,
    FileTypeIcon::Video,
    FileTypeIcon::Code,
    FileTypeIcon::Python,
    FileTypeIcon::Archive,
    FileTypeIcon::Presentation,
];

pub(crate) struct LiquidGlassCache {
    textures: HashMap<FileTypeIcon, Arc<RenderImage>>,
    /// Same art with darkened ink: the white glass vanishes on light surfaces.
    light_textures: HashMap<FileTypeIcon, Arc<RenderImage>>,
    fallback: Arc<RenderImage>,
}

impl LiquidGlassCache {
    pub(crate) fn new() -> Self {
        let mut textures = HashMap::with_capacity(ALL_ICONS.len());
        let mut light_textures = HashMap::with_capacity(ALL_ICONS.len());

        for &kind in ALL_ICONS {
            let path = kind.path();
            if let Some(bytes) = liquid_glass_filetype(path) {
                if let Some(tex) =
                    decode_png_to_render_image(bytes, [1.0; 3], AlphaMap::Gain(DARK_SURFACE_ALPHA))
                {
                    textures.insert(kind, tex);
                }
                if let Some(tex) = decode_png_to_render_image(
                    bytes,
                    LIGHT_SURFACE_INK,
                    AlphaMap::Curve(LIGHT_SURFACE_ALPHA_CURVE),
                ) {
                    light_textures.insert(kind, tex);
                }
            }
        }

        let fallback = textures
            .get(&FileTypeIcon::File)
            .cloned()
            .unwrap_or_else(create_dummy_texture);

        Self {
            textures,
            light_textures,
            fallback,
        }
    }

    #[inline]
    pub(crate) fn texture_for(&self, icon: FileTypeIcon) -> Arc<RenderImage> {
        self.textures
            .get(&icon)
            .cloned()
            .unwrap_or_else(|| self.fallback.clone())
    }

    #[inline]
    pub(crate) fn texture_for_entry(
        &self,
        kind: &FileKind,
        extension: Option<&str>,
    ) -> Arc<RenderImage> {
        let icon_kind = file_type_icon_kind(kind, extension);
        self.texture_for(icon_kind)
    }

    /// Variant for light surfaces (darkened ink so the glass stays visible).
    #[inline]
    pub(crate) fn texture_for_entry_on(
        &self,
        kind: &FileKind,
        extension: Option<&str>,
        light_surface: bool,
    ) -> Arc<RenderImage> {
        let icon_kind = file_type_icon_kind(kind, extension);
        if light_surface {
            if let Some(tex) = self.light_textures.get(&icon_kind) {
                return tex.clone();
            }
        }
        self.texture_for(icon_kind)
    }
}

/// The art is pure white at ~50% alpha: crisp on black, gray-on-gray on the
/// mid-tone surfaces of Graphite/Nord/glass. Denser glass keeps it legible.
const DARK_SURFACE_ALPHA: f32 = 1.4;
/// Per-channel RGB ink for the light-surface variant: the white glass becomes
/// a cool slate. A uniform gray scale read muddy on every light theme.
const LIGHT_SURFACE_INK: [f32; 3] = [0.42, 0.48, 0.57];
/// Light variant alpha = alpha^CURVE: lifts the faint folder front so the
/// gradient stays soft instead of a dark tab fading into a gray slab.
const LIGHT_SURFACE_ALPHA_CURVE: f32 = 0.6;

#[derive(Clone, Copy)]
enum AlphaMap {
    Gain(f32),
    Curve(f32),
}

/// Decode raw PNG bytes to an Arc<RenderImage> with straight BGRA format,
/// scaling RGB per channel by `ink` (1.0 = original art) and remapping alpha.
fn decode_png_to_render_image(
    bytes: &[u8],
    ink: [f32; 3],
    alpha: AlphaMap,
) -> Option<Arc<RenderImage>> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut raw = img.into_raw();

    // Swap R <-> B for GPUI's straight-BGRA GPU pipeline.
    for px in raw.chunks_exact_mut(4) {
        px.swap(0, 2);
        // Now BGRA, while `ink` is RGB.
        for (c, scale) in px[..3].iter_mut().zip([ink[2], ink[1], ink[0]]) {
            if scale < 1.0 {
                *c = (f32::from(*c) * scale).round() as u8;
            }
        }
        let a = f32::from(px[3]) / 255.0;
        let a = match alpha {
            AlphaMap::Gain(gain) => a * gain,
            AlphaMap::Curve(exp) => a.powf(exp),
        };
        px[3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
    }

    let buf = image::RgbaImage::from_raw(w, h, raw)?;
    let frame = image::Frame::new(buf);
    Some(Arc::new(RenderImage::new(vec![frame])))
}

fn create_dummy_texture() -> Arc<RenderImage> {
    let buf = image::RgbaImage::from_raw(1, 1, vec![128, 128, 128, 255]).expect("1x1 buffer");
    Arc::new(RenderImage::new(vec![image::Frame::new(buf)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_all_eleven_liquid_glass_icons() {
        let cache = LiquidGlassCache::new();
        assert_eq!(
            cache.textures.len(),
            11,
            "All 11 Liquid Glass PNGs must decode"
        );

        for &kind in ALL_ICONS {
            let tex = cache.texture_for(kind);
            let sz = tex.size(0);
            assert!(
                sz.width.0 > 0 && sz.height.0 > 0,
                "Icon {:?} has invalid dimensions",
                kind
            );
        }

        // Test entry resolution
        let folder_tex = cache.texture_for_entry(&FileKind::Dir, None);
        assert_eq!(
            folder_tex.size(0),
            cache.texture_for(FileTypeIcon::Folder).size(0)
        );

        let code_tex = cache.texture_for_entry(&FileKind::File, Some("rs"));
        assert_eq!(
            code_tex.size(0),
            cache.texture_for(FileTypeIcon::Code).size(0)
        );
    }
}
