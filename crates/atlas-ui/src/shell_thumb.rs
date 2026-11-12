#![allow(unsafe_code)]
//! Fast thumbnail extraction via the Windows Shell thumbnail cache.
//!
//! Replaces the per-file ffmpeg subprocess approach with
//! `IShellItemImageFactory::GetImage`, which reads from (and writes into) the
//! same `thumbcache_*.db` that Explorer uses.  Second visits are instant; no
//! temp files; no spawned processes; Windows manages eviction.
//!
//! **Call only from `tokio::task::spawn_blocking`** — `GetImage` may block
//! while generating the thumbnail for the first visit.

use std::mem::size_of;
use std::path::Path;

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::{IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName};

// SIIGBF flag constants (from Windows SDK ShObjIdl_core.h).
// Declare locally — named constants appeared later in windows-rs; avoids tracking
//           which exact crate release exports them.
const SIIGBF_THUMBNAILONLY: i32 = 0x0000_0008;
const SIIGBF_ICONONLY: i32 = 0x0000_0004;

thread_local! {
    /// COM is initialized once per blocking-pool thread.  Each tokio blocking
    /// thread is an independent STA, so all concurrent `GetImage` calls proceed
    /// in parallel without serialization.
    static COM_INITED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ensure_com_init() {
    COM_INITED.with(|flag| {
        if !flag.get() {
            // S_FALSE (already initialized on this thread) is fine — we ignore it.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            flag.set(true);
        }
    });
}

/// Query the current user's aggregate bin state without enumerating its files.
pub(crate) fn recycle_bin_contains_items() -> Option<bool> {
    use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    ensure_com_init();
    let mut info = SHQUERYRBINFO {
        cbSize: size_of::<SHQUERYRBINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        SHQueryRecycleBinW(PCWSTR::null(), &mut info).ok()?;
    }
    Some(info.i64NumItems > 0)
}

/// Extract a thumbnail for `path` from the Windows Shell thumbnail cache.
///
/// Returns `(RGBA pixels, width, height)` where `width * height * 4 == pixels.len()`.
/// The longest edge is at most `max_edge` pixels (Windows scales down if needed).
///
/// Returns `None` when:
/// - The path is invalid / not found.
/// - No thumbnail provider is registered for this file type.
/// - `IShellItemImageFactory::GetImage` fails for any other reason.
///
/// The caller falls back to the file-type glyph on `None`, matching the
/// existing behavior for unknown formats.
pub fn shell_thumbnail(path: &Path, max_edge: u32) -> Option<(Vec<u8>, usize, usize)> {
    if atlas_core::vault::live::is_private(path) {
        return None;
    }
    let (pixels, w, h) = shell_image(path, max_edge, SIIGBF_THUMBNAILONLY)?;
    Some(trim_black_bars(pixels, w, h, &path.display().to_string()))
}

/// Extract the best Shell image for a folder: thumbnail/custom icon when Windows
/// has one, otherwise the Shell icon.
pub fn shell_thumbnail_or_icon(path: &Path, max_edge: u32) -> Option<(Vec<u8>, usize, usize)> {
    if atlas_core::vault::live::is_private(path) {
        return None;
    }
    if atlas_core::fs::preview::is_cloud_placeholder(path) {
        return None;
    }
    let (pixels, w, h) = shell_image_from_parsing_name(&path.to_string_lossy(), max_edge, 0)?;
    Some(clean_shell_image(pixels, w, h, &path.display().to_string()))
}

/// Extract the plain Shell *file-type icon* for `path` (never a thumbnail or a
/// folder content peek). Returned untrimmed so the bitmap stays square — cropping
/// to the visible bbox made folder icons render narrow.
pub fn shell_icon_only(path: &Path, max_edge: u32) -> Option<(Vec<u8>, usize, usize)> {
    if atlas_core::fs::preview::is_cloud_placeholder(path) {
        return None;
    }
    shell_image_from_parsing_name(&path.to_string_lossy(), max_edge, SIIGBF_ICONONLY)
}

/// Shell icon at four crisp tiers (32/64/128/256). 32 is requested natively so the
/// hinted 32px icon is used; 64/128 are Lanczos-downscaled from the 256 jumbo icon
/// instead of letting the Shell upscale its 48px bitmap.
///
/// `compact` (per-file icons: exe/lnk/no-extension…) produces only 32 + 64 to keep
/// memory and CPU small when a directory has thousands of unique icons.
pub fn shell_icon_tiers(path: &Path, compact: bool) -> Vec<(Vec<u8>, usize, usize)> {
    let Some((full, fw, fh)) = shell_icon_only(path, 256) else {
        return Vec::new();
    };
    let Some(src) = image::RgbaImage::from_raw(fw as u32, fh as u32, full.clone()) else {
        return Vec::new();
    };
    let small = shell_icon_only(path, 32).unwrap_or_else(|| resize_tier(&src, 32));
    if compact {
        return vec![small, resize_tier(&src, 64)];
    }
    vec![
        small,
        resize_tier(&src, 64),
        resize_tier(&src, 128),
        (full, fw, fh),
    ]
}

fn resize_tier(src: &image::RgbaImage, edge: u32) -> (Vec<u8>, usize, usize) {
    // Premultiply before filtering so transparent edges don't halo.
    let mut pm = src.clone();
    for px in pm.pixels_mut() {
        let a = px[3] as u32;
        for c in 0..3 {
            px[c] = ((px[c] as u32 * a + 127) / 255) as u8;
        }
    }
    let mut out = image::imageops::resize(&pm, edge, edge, image::imageops::FilterType::Lanczos3);
    for px in out.pixels_mut() {
        let a = px[3] as u32;
        if a > 0 {
            for c in 0..3 {
                px[c] = ((px[c] as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    let (w, h) = (out.width() as usize, out.height() as usize);
    (out.into_raw(), w, h)
}

/// Extract the Shell icon for a virtual shell object such as Recycle Bin.
#[cfg(target_os = "windows")]
pub fn shell_virtual_icon(parsing_name: &str, max_edge: u32) -> Option<(Vec<u8>, usize, usize)> {
    let (pixels, w, h) = shell_image_from_parsing_name(parsing_name, max_edge, 0)?;
    Some(clean_shell_image(pixels, w, h, parsing_name))
}

fn trim_black_bars(pixels: Vec<u8>, w: usize, h: usize, label: &str) -> (Vec<u8>, usize, usize) {
    let Some(matte) = corner_matte(&pixels, w, h) else {
        return (pixels, w, h);
    };
    let Some((x, y, visible_w, visible_h)) = non_matte_bbox(&pixels, w, h, matte) else {
        return (pixels, w, h);
    };
    let cropped_w = visible_w * 100 <= w * 96;
    let cropped_h = visible_h * 100 <= h * 96;
    if !cropped_w && !cropped_h {
        return (pixels, w, h);
    }
    tracing::debug!(
        path = %label,
        width = w,
        height = h,
        visible_w,
        visible_h,
        "shell thumbnail had matte bars; cropped them"
    );
    (
        crop_rgba(&pixels, w, x, y, visible_w, visible_h),
        visible_w,
        visible_h,
    )
}

fn clean_shell_image(
    mut pixels: Vec<u8>,
    w: usize,
    h: usize,
    label: &str,
) -> (Vec<u8>, usize, usize) {
    if strip_black_shell_matte(&mut pixels, w, h) {
        tracing::debug!(
            path = %label,
            width = w,
            height = h,
            "shell thumbnail had opaque black matte; stripped it"
        );
    }
    if let Some((x, y, visible_w, visible_h)) = visible_bbox(&pixels, w, h) {
        let tiny_w = visible_w * 3 < w;
        let tiny_h = visible_h * 3 < h;
        if tiny_w && tiny_h {
            tracing::debug!(
                path = %label,
                width = w,
                height = h,
                visible_w,
                visible_h,
                "shell thumbnail visible content is tiny"
            );
        }
        if (x > 0 || y > 0 || visible_w < w || visible_h < h) && visible_w * visible_h > 0 {
            pixels = crop_rgba(&pixels, w, x, y, visible_w, visible_h);
            return (pixels, visible_w, visible_h);
        }
    }
    (pixels, w, h)
}

fn corner_matte(pixels: &[u8], w: usize, h: usize) -> Option<[u8; 3]> {
    if w == 0 || h == 0 || pixels.len() != w * h * 4 {
        return None;
    }
    let samples = [
        &pixels[0..4],
        &pixels[(w - 1) * 4..(w - 1) * 4 + 4],
        &pixels[(h - 1) * w * 4..(h - 1) * w * 4 + 4],
        &pixels[((h - 1) * w + w - 1) * 4..((h - 1) * w + w - 1) * 4 + 4],
    ];
    let mut r = 0usize;
    let mut g = 0usize;
    let mut b = 0usize;
    for px in samples {
        if px[3] < 240 {
            return None;
        }
        r += px[0] as usize;
        g += px[1] as usize;
        b += px[2] as usize;
    }
    Some([(r / 4) as u8, (g / 4) as u8, (b / 4) as u8])
}

fn non_matte_bbox(
    pixels: &[u8],
    w: usize,
    h: usize,
    matte: [u8; 3],
) -> Option<(usize, usize, usize, usize)> {
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            if !is_matte_pixel(&pixels[idx..idx + 4], matte) {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    found.then_some((min_x, min_y, max_x - min_x + 1, max_y - min_y + 1))
}

fn strip_black_shell_matte(pixels: &mut [u8], w: usize, h: usize) -> bool {
    if w == 0 || h == 0 || pixels.len() != w * h * 4 {
        return false;
    }
    let corner = w.min(h).min(12);
    if corner == 0 {
        return false;
    }
    let mut total = 0usize;
    let mut black = 0usize;
    for y in 0..corner {
        for x in 0..corner {
            total += sample_corner_pixel(pixels, w, x, y, &mut black);
            total += sample_corner_pixel(pixels, w, w - 1 - x, y, &mut black);
            total += sample_corner_pixel(pixels, w, x, h - 1 - y, &mut black);
            total += sample_corner_pixel(pixels, w, w - 1 - x, h - 1 - y, &mut black);
        }
    }
    if total == 0 || black * 100 / total < 75 {
        return false;
    }
    let mut changed = false;
    for px in pixels.chunks_exact_mut(4) {
        if is_opaque_black(px) {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            px[3] = 0;
            changed = true;
        }
    }
    changed
}

fn sample_corner_pixel(pixels: &[u8], w: usize, x: usize, y: usize, black: &mut usize) -> usize {
    let idx = (y * w + x) * 4;
    if is_opaque_black(&pixels[idx..idx + 4]) {
        *black += 1;
    }
    1
}

fn is_opaque_black(px: &[u8]) -> bool {
    px[3] >= 240 && px[0] <= 8 && px[1] <= 8 && px[2] <= 8
}

fn is_matte_pixel(px: &[u8], matte: [u8; 3]) -> bool {
    px[3] >= 240
        && px[0].abs_diff(matte[0]) <= 24
        && px[1].abs_diff(matte[1]) <= 24
        && px[2].abs_diff(matte[2]) <= 24
}

fn visible_bbox(pixels: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut found = false;
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) * 4;
            if pixels[idx + 3] > 16 {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    found.then_some((min_x, min_y, max_x - min_x + 1, max_y - min_y + 1))
}

fn crop_rgba(pixels: &[u8], w: usize, x: usize, y: usize, crop_w: usize, crop_h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(crop_w * crop_h * 4);
    for row in y..y + crop_h {
        let start = (row * w + x) * 4;
        let end = start + crop_w * 4;
        out.extend_from_slice(&pixels[start..end]);
    }
    out
}

fn shell_image(path: &Path, max_edge: u32, flags: i32) -> Option<(Vec<u8>, usize, usize)> {
    if atlas_core::fs::preview::is_cloud_placeholder(path) {
        return None;
    }
    shell_image_from_parsing_name(&path.to_string_lossy(), max_edge, flags)
}

fn shell_image_from_parsing_name(
    parsing_name: &str,
    max_edge: u32,
    flags: i32,
) -> Option<(Vec<u8>, usize, usize)> {
    ensure_com_init();

    // Encode path as null-terminated UTF-16 for the Shell API.
    let wide: Vec<u16> = parsing_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        // Create an IShellItem for the path, then QI for IShellItemImageFactory.
        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None).ok()?;
        let factory: IShellItemImageFactory = item.cast().ok()?;

        // SIIGBF_THUMBNAILONLY: fail (→ None) rather than returning a generic icon.
        // Windows scales the result to fit within `max_edge × max_edge`.
        let size = SIZE {
            cx: max_edge as i32,
            cy: max_edge as i32,
        };
        let flags = windows::Win32::UI::Shell::SIIGBF(flags);
        let hbitmap = factory.GetImage(size, flags).ok()?;

        // --- HBITMAP → RGBA pixels ---

        // Obtain the actual pixel dimensions (may differ from the requested size).
        let mut bm = BITMAP::default();
        let got = GetObjectW(
            HGDIOBJ(hbitmap.0),
            size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut _),
        );
        if got == 0 {
            let _ = DeleteObject(HGDIOBJ(hbitmap.0));
            return None;
        }
        let w = bm.bmWidth as usize;
        let h = bm.bmHeight.unsigned_abs() as usize;
        if w == 0 || h == 0 {
            let _ = DeleteObject(HGDIOBJ(hbitmap.0));
            return None;
        }

        // GetDIBits needs any valid HDC (used only for palette operations with
        // DIB_PAL_COLORS; harmless for our 32bpp BI_RGB request).
        let hdc = CreateCompatibleDC(None);

        // Request 32bpp top-down DIB (negative biHeight = top-down scanlines).
        let mut bi_header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: 0, // BI_RGB
            ..Default::default()
        };
        let mut pixels = vec![0u8; w * h * 4];
        // BITMAPINFO = BITMAPINFOHEADER + RGBQUAD[1]; no RGBQUAD needed for 32bpp BI_RGB.
        let bi_ptr = &mut bi_header as *mut BITMAPINFOHEADER as *mut BITMAPINFO;
        let rows = GetDIBits(
            hdc,
            hbitmap,
            0,
            h as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            bi_ptr,
            DIB_RGB_COLORS,
        );

        let _ = DeleteDC(hdc);
        let _ = DeleteObject(HGDIOBJ(hbitmap.0));

        if rows == 0 {
            return None;
        }

        // Windows DIBs with BI_RGB store pixels as BGRA. Shell icons often arrive
        // premultiplied; return straight RGBA because the UI upload path handles
        // the final GPUI channel swap.
        let has_alpha = pixels.chunks_exact(4).any(|chunk| chunk[3] != 0);
        for chunk in pixels.chunks_exact_mut(4) {
            let b = chunk[0];
            let g = chunk[1];
            let r = chunk[2];
            let a = if has_alpha { chunk[3] } else { 255 };
            let (r, g, b) = if has_alpha && a != 0 {
                let un = |c: u8| ((c as u32 * 255) / a as u32).min(255) as u8;
                (un(r), un(g), un(b))
            } else {
                (r, g, b)
            };
            chunk[0] = r;
            chunk[1] = g;
            chunk[2] = b;
            chunk[3] = a;
        }

        Some((pixels, w, h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_thumbnail_video() {
        let video = Path::new(r"C:\Users\fzemi\Videos\2025-06-30 11-20-28.mp4");
        if !video.exists() {
            return;
        }
        let t0 = std::time::Instant::now();
        let thumb = shell_thumbnail(video, 256);
        let elapsed = t0.elapsed();
        println!("shell_thumbnail took: {:?}", elapsed);
        assert!(
            thumb.is_some(),
            "Expected thumbnail from Windows Shell for video"
        );
        let (rgba, w, h) = thumb.unwrap();
        println!("Thumb dims: {}x{}, bytes: {}", w, h, rgba.len());
        assert!(w > 0 && h > 0);
        assert_eq!(rgba.len(), w * h * 4);
    }
}
