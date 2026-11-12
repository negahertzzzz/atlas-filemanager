#![allow(clippy::disallowed_methods)]
// reason: image ops run on blocking workers, sync fs is intentional.

use image::ImageDecoder as _;
use std::path::Path;

use atlas_model::preview_cache::{ImageData, PreviewResult};

use super::{MAX_IMAGE_PREVIEW_EDGE, MAX_MULTI_THUMBNAIL_EDGE};

pub fn decode_svg(path: &Path, max_edge: u32) -> Option<(Vec<u8>, usize, usize)> {
    if super::is_cloud_placeholder(path) {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let opts = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(&data, &opts).ok()?;
    let size = tree.size();
    let (w, h) = (size.width(), size.height());
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let scale = (max_edge as f32 / w).min(max_edge as f32 / h).max(0.01);
    let target_w = (w * scale).round().max(1.0) as u32;
    let target_h = (h * scale).round().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(target_w, target_h)?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Some((pixmap.take(), target_w as usize, target_h as usize))
}

pub(super) fn preview_image(path: &Path) -> PreviewResult {
    if super::is_cloud_placeholder(path) {
        return super::offline_preview();
    }
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return PreviewResult::Error(e.to_string()),
    };
    if meta.len() == 0 {
        return PreviewResult::Error("El archivo está vacío (0 bytes)".to_string());
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    if ext == "svg" {
        return match decode_svg(path, MAX_IMAGE_PREVIEW_EDGE) {
            Some((rgba, width, height)) => PreviewResult::Image {
                rgba,
                width,
                height,
                original_width: width,
                original_height: height,
                exif: Vec::new(),
            },
            None => PreviewResult::Error("Could not render SVG".into()),
        };
    }

    let img = match oriented_image(path) {
        Ok(i) => i,
        Err(e) => return PreviewResult::Error(e.to_string()),
    };
    let original_width = img.width() as usize;
    let original_height = img.height() as usize;
    let img = img.thumbnail(MAX_IMAGE_PREVIEW_EDGE, MAX_IMAGE_PREVIEW_EDGE);
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let exif = read_exif(path);
    PreviewResult::Image {
        rgba: rgba.into_raw(),
        width: width as usize,
        height: height as usize,
        original_width,
        original_height,
        exif,
    }
}

pub(super) fn preview_thumbnail(path: &Path) -> Result<(Vec<u8>, usize, usize), image::ImageError> {
    if super::is_cloud_placeholder(path) {
        return Err(
            std::io::Error::new(std::io::ErrorKind::WouldBlock, "Cloud placeholder").into(),
        );
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "svg" {
        if let Some(res) = decode_svg(path, MAX_MULTI_THUMBNAIL_EDGE) {
            return Ok(res);
        }
    }
    let img = oriented_image(path)?.thumbnail(MAX_MULTI_THUMBNAIL_EDGE, MAX_MULTI_THUMBNAIL_EDGE);
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok((rgba.into_raw(), width as usize, height as usize))
}

fn oriented_image(path: &Path) -> Result<image::DynamicImage, image::ImageError> {
    // Decoder metadata includes EXIF tag 274 for JPEG/TIFF and orientation in
    // other supported containers. Apply it exactly once, before sizing/upload.
    let mut decoder = image::ImageReader::open(path)?
        .with_guessed_format()?
        .into_decoder()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = image::DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(image)
}

pub(super) fn decode_cover(data: &[u8]) -> Option<ImageData> {
    let img = image::load_from_memory(data)
        .ok()?
        .thumbnail(MAX_MULTI_THUMBNAIL_EDGE, MAX_MULTI_THUMBNAIL_EDGE);
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(ImageData {
        rgba: rgba.into_raw(),
        width: width as usize,
        height: height as usize,
    })
}

fn read_exif(path: &Path) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let Ok(file) = std::fs::File::open(path) else {
        return result;
    };
    let mut reader = std::io::BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else {
        return result;
    };
    for field in exif.fields() {
        let tag = field.tag.to_string();
        let value = field.display_value().to_string();
        if !value.is_empty() && result.len() < 30 {
            result.push((tag, value));
        }
    }
    result
}

/// Rotate RGBA8 image by `angle_deg` onto transparent padded canvas (bilinear).
/// Returns `(rgba, new_w, new_h)`.
// reason: affine + bilinear math is clearer as plain arithmetic; runs once per thumbnail off render thread.
#[allow(clippy::suboptimal_flops)]
pub(super) fn rotate_rgba(
    rgba: &[u8],
    w: usize,
    h: usize,
    angle_deg: f32,
) -> (Vec<u8>, usize, usize) {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return (rgba.to_vec(), w, h);
    }
    let (sin, cos) = angle_deg.to_radians().sin_cos();
    let (wf, hf) = (w as f32, h as f32);
    let new_w = (wf * cos.abs() + hf * sin.abs()).ceil().max(1.0) as usize;
    let new_h = (wf * sin.abs() + hf * cos.abs()).ceil().max(1.0) as usize;
    let mut out = vec![0u8; new_w * new_h * 4];
    let (cx, cy) = (wf / 2.0, hf / 2.0);
    let (ncx, ncy) = (new_w as f32 / 2.0, new_h as f32 / 2.0);
    let idx = |x: usize, y: usize| (y * w + x) * 4;
    for oy in 0..new_h {
        for ox in 0..new_w {
            let dx = ox as f32 + 0.5 - ncx;
            let dy = oy as f32 + 0.5 - ncy;
            let sx = cos * dx + sin * dy + cx;
            let sy = -sin * dx + cos * dy + cy;
            if sx < 0.0 || sy < 0.0 || sx >= wf || sy >= hf {
                continue;
            }
            let x0 = sx.floor() as usize;
            let y0 = sy.floor() as usize;
            let x1 = (x0 + 1).min(w - 1);
            let y1 = (y0 + 1).min(h - 1);
            let fx = sx - x0 as f32;
            let fy = sy - y0 as f32;
            let o = (oy * new_w + ox) * 4;
            for c in 0..4 {
                let p00 = f32::from(rgba[idx(x0, y0) + c]);
                let p10 = f32::from(rgba[idx(x1, y0) + c]);
                let p01 = f32::from(rgba[idx(x0, y1) + c]);
                let p11 = f32::from(rgba[idx(x1, y1) + c]);
                let top = p00 + (p10 - p00) * fx;
                let bot = p01 + (p11 - p01) * fx;
                out[o + c] = (top + (bot - top) * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    (out, new_w, new_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_exif_rotation_and_mirroring_are_applied_once_before_sizing() {
        use image::GenericImageView as _;
        let pixels = image::RgbImage::from_fn(40, 20, |x, y| {
            image::Rgb([(x * 6) as u8, (y * 12) as u8, 80])
        });
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode_image(&pixels)
            .unwrap();
        let original = image::load_from_memory(&jpeg).unwrap();
        let dir = tempfile::tempdir().unwrap();
        for orientation in [2u8, 6] {
            let mut exif = b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
            exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
            let mut fixture = jpeg[..2].to_vec();
            fixture.extend_from_slice(&[0xff, 0xe1]);
            fixture.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
            fixture.extend_from_slice(&exif);
            fixture.extend_from_slice(&jpeg[2..]);
            let path = dir.path().join(format!("orientation-{orientation}.jpg"));
            std::fs::write(&path, fixture).unwrap();
            let decoded = oriented_image(&path).unwrap();
            let expected = if orientation == 2 {
                original.fliph()
            } else {
                original.rotate90()
            };
            assert_eq!(decoded.dimensions(), expected.dimensions());
            assert_eq!(decoded.to_rgba8(), expected.to_rgba8());
            let PreviewResult::Image {
                original_width,
                original_height,
                ..
            } = preview_image(&path)
            else {
                panic!("image preview");
            };
            assert_eq!(
                (original_width, original_height),
                (expected.width() as usize, expected.height() as usize)
            );
        }
    }

    #[test]
    fn test_decode_svg_valid() {
        let dir = tempfile::tempdir().unwrap();
        let svg_path = dir.path().join("icon.svg");
        let svg_content = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><rect width="100" height="100" fill="red"/></svg>"#;
        std::fs::write(&svg_path, svg_content).unwrap();

        let decoded = decode_svg(&svg_path, 64);
        assert!(decoded.is_some());
        let (rgba, w, h) = decoded.unwrap();
        assert_eq!(w, 64);
        assert_eq!(h, 64);
        assert_eq!(rgba.len(), 64 * 64 * 4);
        // Red color (255, 0, 0, 255)
        assert_eq!(rgba[0], 255);
        assert_eq!(rgba[1], 0);
        assert_eq!(rgba[2], 0);
        assert_eq!(rgba[3], 255);
    }
}
