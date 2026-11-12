//! App methods for the colored file-type icon cache.
//!
//! Rasterises SVGs with resvg into multiple resolution tiers (32, 64, 128, 256 px)
//! and applies the premultiplied-alpha → `straight-BGRA` conversion with edge color bleed
//! that GPUI's `img()` pipeline expects, then wraps into `Arc<RenderImage>`.
//! Results flow through a dedicated `icon_tx / icon_rx` channel so RenderImage
//! (a GPUI type) never touches atlas-model.

use std::path::Path;
use std::sync::Arc;

use gpui::RenderImage;

use crate::icon_theme::{icon_key, IconState, MultiResIcon};
use atlas_model::file_entry::FileKind;

use super::App;

pub(crate) const TIER_SMALL: u32 = 32;
pub(crate) const TIER_MEDIUM: u32 = 64;
pub(crate) const TIER_LARGE: u32 = 128;
pub(crate) const TIER_FULL: u32 = 256;

impl App {
    /// Enqueue a background rasterisation for `key` if not already cached.
    pub(crate) fn request_icon(&mut self, key: String) {
        if self.icon_cache.contains_key(&key) {
            return;
        }
        let Some(ref theme) = self.icon_theme else {
            return;
        };
        let svg_path = if key == "/" {
            theme.default_folder.clone()
        } else if key.is_empty() {
            theme.default_file.clone()
        } else {
            theme.svg_for_ext(&key).map(std::path::Path::to_path_buf)
        };
        let Some(svg_path) = svg_path else {
            return;
        };

        self.icon_cache.insert(key.clone(), IconState::Loading);

        let icon_tx = self.icon_tx.clone();
        self.rt.spawn(async move {
            let result: Option<Arc<MultiResIcon>> =
                tokio::task::spawn_blocking(move || rasterize_multi(&svg_path))
                    .await
                    .ok()
                    .flatten();
            icon_tx.send((key, result)).ok();
        });
    }

    /// Return the cached icon texture for `key`, defaulting to list/compact size.
    pub(crate) fn icon_texture(&self, key: &str) -> Option<Arc<RenderImage>> {
        self.icon_texture_for_size(key, 24.0)
    }

    /// Return the cached icon texture for `key` calibrated to `size_px`.
    pub(crate) fn icon_texture_for_size(
        &self,
        key: &str,
        size_px: f32,
    ) -> Option<Arc<RenderImage>> {
        match self.icon_cache.get(key) {
            Some(IconState::Ready(multi)) => Some(multi.for_size(size_px)),
            _ => None,
        }
    }

    /// Return the cached `MultiResIcon` for `key` if ready.
    pub(crate) fn multi_res_icon(&self, key: &str) -> Option<Arc<MultiResIcon>> {
        match self.icon_cache.get(key) {
            Some(IconState::Ready(multi)) => Some(multi.clone()),
            _ => None,
        }
    }

    /// Resolve `(kind, ext)` → cache key → texture for default list size.
    pub(crate) fn icon_tex_for_entry(
        &self,
        kind: &FileKind,
        ext: Option<&str>,
    ) -> Option<Arc<RenderImage>> {
        self.icon_texture(&icon_key(kind, ext))
    }

    /// Resolve `(kind, ext)` → cache key → texture calibrated to `size_px`.
    #[allow(dead_code)]
    pub(crate) fn icon_tex_for_entry_sized(
        &self,
        kind: &FileKind,
        ext: Option<&str>,
        size_px: f32,
    ) -> Option<Arc<RenderImage>> {
        self.icon_texture_for_size(&icon_key(kind, ext), size_px)
    }

    /// Resolve `(kind, ext)` → cache key → `MultiResIcon`.
    pub(crate) fn multi_res_icon_for_entry(
        &self,
        kind: &FileKind,
        ext: Option<&str>,
    ) -> Option<Arc<MultiResIcon>> {
        self.multi_res_icon(&icon_key(kind, ext))
    }

    /// Resolve `(kind, ext)` to default Icons8 Liquid Glass texture.
    pub(crate) fn liquid_glass_icon(&self, kind: &FileKind, ext: Option<&str>) -> Arc<RenderImage> {
        self.liquid_glass
            .texture_for_entry_on(kind, ext, self.light_surfaces)
    }
}

/// Bleed non-transparent edge colors into immediate 1-pixel orthogonal transparent neighbors.
///
/// In GPU hardware rendering, bilinear sampling interpolates texels along polygon boundaries.
/// If adjacent transparent texels have RGB = (0, 0, 0), bilinear filtering blends the edge
/// color with black, producing a dark, eaten-away fringe on rounded corners.
///
/// By padding the immediate 1-pixel transparent boundary with the straight color of its
/// non-transparent orthogonal neighbors (preserving alpha = 0), the GPU filtered color remains
/// constant while only alpha transitions smoothly. Outer pixels (like corner (0,0)) remain
/// clean (0, 0, 0, 0).
fn bleed_straight_edges(raw: &mut [u8], side: usize) {
    let copy = raw.to_vec();
    for y in 0..side {
        for x in 0..side {
            let i = (y * side + x) * 4;
            if copy[i + 3] != 0 {
                continue;
            }

            let mut sum_b = 0u32;
            let mut sum_g = 0u32;
            let mut sum_r = 0u32;
            let mut sum_w = 0u32;

            for (dx, dy) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx >= 0 && nx < side as i32 && ny >= 0 && ny < side as i32 {
                    let ni = (ny as usize * side + nx as usize) * 4;
                    let a = u32::from(copy[ni + 3]);
                    if a > 0 {
                        sum_b += u32::from(copy[ni]) * a;
                        sum_g += u32::from(copy[ni + 1]) * a;
                        sum_r += u32::from(copy[ni + 2]) * a;
                        sum_w += a;
                    }
                }
            }

            if sum_w > 0 {
                raw[i] = ((sum_b + sum_w / 2) / sum_w) as u8;
                raw[i + 1] = ((sum_g + sum_w / 2) / sum_w) as u8;
                raw[i + 2] = ((sum_r + sum_w / 2) / sum_w) as u8;
                // alpha remains 0
            }
        }
    }
}

fn rasterize_tier(tree: &resvg::usvg::Tree, target_px: u32) -> Option<Arc<RenderImage>> {
    let svg_w = tree.size().width();
    let svg_h = tree.size().height();
    let scale = target_px as f32 / svg_w.max(svg_h);
    let dx = (target_px as f32 - svg_w * scale) * 0.5;
    let dy = (target_px as f32 - svg_h * scale) * 0.5;

    let mut pixmap = resvg::tiny_skia::Pixmap::new(target_px, target_px)?;
    resvg::render(
        tree,
        resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy),
        &mut pixmap.as_mut(),
    );

    let mut raw = pixmap.take();

    // tiny_skia emits premultiplied RGBA. Un-premultiply and swap to straight BGRA for GPUI.
    for px in raw.chunks_exact_mut(4) {
        let a = px[3];
        if a > 0 {
            let af = f32::from(a) / 255.0;
            px[0] = (f32::from(px[0]) / af).min(255.0).round() as u8;
            px[1] = (f32::from(px[1]) / af).min(255.0).round() as u8;
            px[2] = (f32::from(px[2]) / af).min(255.0).round() as u8;
        }
        px.swap(0, 2); // R↔B → BGRA
    }

    bleed_straight_edges(&mut raw, target_px as usize);

    let buf = image::RgbaImage::from_raw(target_px, target_px, raw)?;
    let frame = image::Frame::new(buf);
    Some(Arc::new(RenderImage::new(vec![frame])))
}

fn rasterize_multi(svg_path: &Path) -> Option<Arc<MultiResIcon>> {
    #[allow(clippy::disallowed_methods)] // reason: runs inside spawn_blocking
    let data = std::fs::read(svg_path).ok()?;
    let opts = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(&data, &opts).ok()?;

    let small = rasterize_tier(&tree, TIER_SMALL)?;
    let medium = rasterize_tier(&tree, TIER_MEDIUM)?;
    let large = rasterize_tier(&tree, TIER_LARGE)?;
    let full = rasterize_tier(&tree, TIER_FULL)?;

    Some(Arc::new(MultiResIcon {
        small,
        medium,
        large,
        full,
    }))
}

/// Standalone rasterisation to 256px for tests or direct single-texture usage.
#[allow(dead_code)]
pub fn rasterize(svg_path: &Path) -> Option<Arc<RenderImage>> {
    #[allow(clippy::disallowed_methods)]
    let data = std::fs::read(svg_path).ok()?;
    let opts = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(&data, &opts).ok()?;
    rasterize_tier(&tree, TIER_FULL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rasterize_dimensions_and_clean_edges() {
        let temp_dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let test_svg_path = temp_dir.join(format!("test_icon_{nanos}.svg"));
        let svg_data = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100">
            <circle cx="50" cy="50" r="40" fill="#ff8000" />
        </svg>"##;
        std::fs::write(&test_svg_path, svg_data).expect("write temp svg");

        let image = rasterize(&test_svg_path).expect("rasterize must succeed");
        std::fs::remove_file(&test_svg_path).ok();

        let sz = image.size(0);
        assert_eq!(sz.width.0, 256, "Width must be TARGET_PX = 256");
        assert_eq!(sz.height.0, 256, "Height must be TARGET_PX = 256");

        let bytes = image.as_bytes(0).expect("frame bytes must exist");
        // Far corner (0,0) is far away from the circle (r=40 in 100x100) -> must be completely transparent (0,0,0,0)
        assert_eq!(bytes[3], 0, "Corner pixel must be transparent");
        assert_eq!(
            bytes[0], 0,
            "Blue channel must be 0 in far transparent pixel"
        );
        assert_eq!(
            bytes[1], 0,
            "Green channel must be 0 in far transparent pixel"
        );
        assert_eq!(
            bytes[2], 0,
            "Red channel must be 0 in far transparent pixel"
        );

        // Center pixel (128,128) must be opaque and have expected orange color (#ff8000 -> BGRA: B=0, G=128, R=255, A=255)
        let center_idx = (128 * 256 + 128) * 4;
        assert_eq!(bytes[center_idx + 3], 255, "Center pixel must be opaque");
        assert!(
            bytes[center_idx + 2] > 240,
            "Red channel should be ~255 in straight BGRA"
        );
        assert!(
            bytes[center_idx + 1] > 110 && bytes[center_idx + 1] < 145,
            "Green channel should be ~128 in straight BGRA"
        );
        assert!(
            bytes[center_idx] < 10,
            "Blue channel should be ~0 in straight BGRA"
        );
    }

    #[test]
    fn test_rasterize_multi_generates_all_tiers_cleanly() {
        let temp_dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let test_svg_path = temp_dir.join(format!("test_multi_{nanos}.svg"));
        // Folder-shaped SVG with rounded corners
        let svg_data = r##"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
            <path d="M10 4H4c-1.11 0-2 .89-2 2v12a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V8c0-1.11-.9-2-2-2h-8l-2-2z" fill="#45403d" />
        </svg>"##;
        std::fs::write(&test_svg_path, svg_data).expect("write temp svg");

        let multi = rasterize_multi(&test_svg_path).expect("rasterize_multi must succeed");
        std::fs::remove_file(&test_svg_path).ok();

        assert_eq!(multi.small.size(0).width.0, 32);
        assert_eq!(multi.medium.size(0).width.0, 64);
        assert_eq!(multi.large.size(0).width.0, 128);
        assert_eq!(multi.full.size(0).width.0, 256);

        // Verify tier selection
        assert_eq!(multi.for_size(16.0).size(0).width.0, 32);
        assert_eq!(multi.for_size(24.0).size(0).width.0, 32);
        assert_eq!(multi.for_size(42.0).size(0).width.0, 64);
        assert_eq!(multi.for_size(53.0).size(0).width.0, 64);
        assert_eq!(multi.for_size(63.0).size(0).width.0, 64);
        assert_eq!(multi.for_size(64.0).size(0).width.0, 64);
        assert_eq!(multi.for_size(65.0).size(0).width.0, 128);
        assert_eq!(multi.for_size(84.0).size(0).width.0, 128);
        assert_eq!(multi.for_size(120.0).size(0).width.0, 128);
        assert_eq!(multi.for_size(128.0).size(0).width.0, 128);
        assert_eq!(multi.for_size(129.0).size(0).width.0, 256);
        assert_eq!(multi.for_size(200.0).size(0).width.0, 256);
    }

    #[test]
    fn test_corner_anti_aliasing_and_straight_colors() {
        let temp_dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let test_svg_path = temp_dir.join(format!("test_corners_{nanos}.svg"));
        let svg_data = r##"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
            <path d="M10 4H4c-1.11 0-2 .89-2 2v12a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2V8c0-1.11-.9-2-2-2h-8l-2-2z" fill="#45403d" />
        </svg>"##;
        std::fs::write(&test_svg_path, svg_data).expect("write temp svg");

        let multi = rasterize_multi(&test_svg_path).expect("rasterize_multi must succeed");
        std::fs::remove_file(&test_svg_path).ok();

        // Check medium tier (64x64, used for ~42-56px thumbnails)
        let medium_bytes = multi.medium.as_bytes(0).expect("medium bytes");
        let mut semi_transparent_count = 0;
        let mut color_bled_transparent_count = 0;

        for px in medium_bytes.chunks_exact(4) {
            let b = px[0];
            let g = px[1];
            let r = px[2];
            let a = px[3];

            if a > 16 && a < 255 {
                semi_transparent_count += 1;
                // Anti-aliased boundary pixels must preserve the #45403d hue without dark degradation
                assert!(
                    r >= 55 && r <= 80,
                    "Red should be around 69 on boundary pixels, got {r}"
                );
                assert!(
                    g >= 50 && g <= 75,
                    "Green should be around 64 on boundary pixels, got {g}"
                );
                assert!(
                    b >= 45 && b <= 70,
                    "Blue should be around 61 on boundary pixels, got {b}"
                );
            } else if a == 0 && (r > 0 || g > 0 || b > 0) {
                color_bled_transparent_count += 1;
                // The 1-pixel bleed buffer must preserve the true straight color so GPU interpolation doesn't darken
                assert!(r >= 55 && r <= 80, "Red bleed must be around 69, got {r}");
                assert!(g >= 50 && g <= 75, "Green bleed must be around 64, got {g}");
                assert!(b >= 45 && b <= 70, "Blue bleed must be around 61, got {b}");
            }
        }

        // Curves must have anti-aliased subpixel transitions
        assert!(semi_transparent_count > 40, "Expected anti-aliased subpixel pixels on rounded corners, got {semi_transparent_count}");
        // 1px padding must protect edges from dark fringing
        assert!(
            color_bled_transparent_count > 20,
            "Expected 1-pixel color-bleed border pixels, got {color_bled_transparent_count}"
        );
        // Far corner (0,0) must still be pure zero
        assert_eq!(medium_bytes[0], 0);
        assert_eq!(medium_bytes[1], 0);
        assert_eq!(medium_bytes[2], 0);
        assert_eq!(medium_bytes[3], 0);
    }
}
