//! Downscaled grid thumbnails.
//!
//! The Grid view used to paint image cells with `img(path)`, which makes gpui decode
//! every file at full resolution and keep a full-size GPU texture — brutal for a
//! folder of hundreds of large photos. Instead, the visible cells request a small
//! thumbnail that is decoded + resized off-thread (`request_thumbnail`), uploaded to
//! a cached `Arc<RenderImage>` (UI-side only — `state` never holds a renderer handle),
//! and reused on every later frame. Requests are virtualized: only the cells the grid
//! actually builds ask for a thumbnail.

use crate::app::App;
use crate::ui::inspector::rgba_to_render_image;
use atlas_model::app_state::BgResult;
use gpui::ColorExt as _;
use gpui::{point, px, size, Bounds, Corners, Pixels, Point, RenderImage};
use gpui_component::ActiveTheme as _;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// UI-side cache entry for one path's grid thumbnail.
#[derive(Clone)]
pub enum ThumbState {
    /// A decode task is in flight; render the placeholder glyph meanwhile.
    Loading,
    /// Ready to paint.
    Ready(Arc<RenderImage>),
    /// Decode failed (or produced nothing); never re-requested.
    Missing,
}

/// Longest edge (px) of the decoded thumbnail. Generous vs. the largest zoom step so
/// it still looks crisp, but a tiny fraction of a full-res photo's pixels.
// With global zoom (up to 3x) stacked on per-pane thumbnail size (up to
// 384px), a cell can now render well past this cap and look soft. 256 halves the
// worst-case blur cheaply; a fully dynamic decode resolution is a bigger change.
const THUMB_MAX_EDGE: u32 = 256;

/// Max concurrent *extension* thumbnailer processes (ffmpeg/magick/…). A large folder
/// of videos must not spawn hundreds of processes at once; over-cap requests are simply
/// dropped and re-issued on the next frame (the cell isn't cached yet).
pub(crate) const MAX_EXT_THUMB_INFLIGHT: usize = 8;

/// Max concurrent native image decodes. Over-cap requests are dropped and re-issued
/// next frame (no Loading marker inserted), keeping the blocking pool free for
/// directory listing while preventing a burst of hundreds of simultaneous decodes.
pub(crate) const MAX_NATIVE_THUMB_INFLIGHT: usize = 16;

/// Aspect-fit without upscaling; leave ten percent of the slot height as air.
pub(crate) fn thumbnail_size(
    natural_w: f32,
    natural_h: f32,
    slot_w: f32,
    slot_h: f32,
) -> (f32, f32) {
    let scale = (slot_w.max(0.0) / natural_w.max(1.0))
        .min(slot_h.max(0.0) * 0.90 / natural_h.max(1.0))
        .min(1.0);
    (
        (natural_w * scale).round().max(1.0),
        (natural_h * scale).round().max(1.0),
    )
}

/// Largest rect with the texture's own aspect that fits `slot`, centered. Shell
/// icons are cropped to their visible art (a folder is wider than tall), so an
/// icon slot must contain the texture, never stretch it to a square.
pub(crate) fn contain_bounds(texture: &RenderImage, slot: Bounds<Pixels>) -> Bounds<Pixels> {
    let natural = texture.size(0);
    let (nw, nh) = (
        i32::from(natural.width).max(1) as f32,
        i32::from(natural.height).max(1) as f32,
    );
    let (sw, sh) = (f32::from(slot.size.width), f32::from(slot.size.height));
    let scale = (sw / nw).min(sh / nh);
    let (w, h) = (nw * scale, nh * scale);
    Bounds {
        origin: point(
            slot.origin.x + px((sw - w) * 0.5),
            slot.origin.y + px((sh - h) * 0.5),
        ),
        size: size(px(w), px(h)),
    }
}

/// Shared by retained and DG surfaces: every preview texture (including PDF,
/// video, RAW and extension previews) gets the same inner one-pixel outline.
pub(crate) fn paint_thumbnail(
    window: &mut gpui::Window,
    cx: &gpui::App,
    texture: &Arc<RenderImage>,
    center: Point<Pixels>,
    slot_w: f32,
    slot_h: f32,
) {
    paint_thumbnail_state(window, cx, texture, center, slot_w, slot_h, false);
}

/// Same as [`paint_thumbnail`] with a hover state. The frame is two hairlines so it
/// reads on any theme and any image edge: an inner neutral line that separates the
/// picture from its frame, and an outer ring tinted from the theme (foreground at
/// rest, accent on hover).
pub(crate) fn paint_thumbnail_state(
    window: &mut gpui::Window,
    cx: &gpui::App,
    texture: &Arc<RenderImage>,
    center: Point<Pixels>,
    slot_w: f32,
    slot_h: f32,
    hovered: bool,
) {
    let natural = texture.size(0);
    let (width, height) = thumbnail_size(
        i32::from(natural.width) as f32,
        i32::from(natural.height) as f32,
        slot_w,
        slot_h,
    );
    let bounds = Bounds {
        origin: point(
            px((f32::from(center.x) - width * 0.5).round()),
            px((f32::from(center.y) - height * 0.5).round()),
        ),
        size: size(px(width), px(height)),
    };
    let corners = Corners::all(px(3.0));
    let _ = window.paint_image(bounds, bounds, corners, texture.clone(), 0, false);
    let theme = cx.theme();
    let dark = theme.is_dark();
    let inner = gpui::black().opacity(if dark { 0.35 } else { 0.12 });
    window.paint_quad(gpui::outline(bounds, inner, Default::default()).corner_radii(corners));
    let ring_bounds = bounds.dilate(px(1.0));
    let ring = if hovered {
        theme.accent.opacity(if dark { 0.75 } else { 0.65 })
    } else {
        theme.foreground.opacity(if dark { 0.16 } else { 0.14 })
    };
    window.paint_quad(
        gpui::outline(ring_bounds, ring, Default::default()).corner_radii(Corners::all(px(4.0))),
    );
}

#[cfg(test)]
mod geometry_tests {
    use super::thumbnail_size;
    #[test]
    fn landscape_portrait_and_small_previews_keep_their_proportions() {
        assert_eq!(thumbnail_size(400.0, 200.0, 100.0, 100.0), (100.0, 50.0));
        assert_eq!(thumbnail_size(200.0, 400.0, 100.0, 100.0), (45.0, 90.0));
        assert_eq!(thumbnail_size(20.0, 10.0, 100.0, 100.0), (20.0, 10.0));
    }
}

impl App {
    pub(crate) fn request_recycle_bin_status(&mut self, refresh: bool) {
        if !refresh && self.recycle_bin_status_checked {
            return;
        }
        if self.recycle_bin_status_inflight {
            self.recycle_bin_status_dirty |= refresh;
            return;
        }
        self.recycle_bin_status_inflight = true;
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let full = tokio::task::spawn_blocking(|| {
                #[cfg(windows)]
                {
                    crate::shell_thumb::recycle_bin_contains_items()
                }
                #[cfg(not(windows))]
                {
                    atlas_core::fs::recycle_bin::contains_items()
                }
            })
            .await
            .ok()
            .flatten();
            let _ = tx.send(BgResult::RecycleBinStatus(full));
        });
    }

    pub(crate) fn refresh_recycle_bin_feedback(&mut self) {
        self.request_recycle_bin_status(true);
        #[cfg(windows)]
        {
            let key = Self::shell_recycle_bin_icon_key();
            if self.ext_thumb_inflight.contains(&key) {
                self.recycle_bin_icon_dirty = true;
            } else if self.native_file_icons() {
                self.load_shell_recycle_bin_icon(true);
            }
        }
    }
    #[cfg(target_os = "windows")]
    pub(crate) fn shell_folder_icon_key() -> PathBuf {
        PathBuf::from("__atlas_shell_folder_icon__")
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn shell_recycle_bin_icon_key() -> PathBuf {
        PathBuf::from("__atlas_shell_recycle_bin_icon__")
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn shell_folder_icon(&self) -> Option<Arc<crate::icon_theme::MultiResIcon>> {
        self.shell_icons
            .get(&Self::shell_folder_icon_key())
            .cloned()
            .flatten()
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn shell_recycle_bin_icon(&self) -> Option<Arc<RenderImage>> {
        self.grid_thumbnail(&Self::shell_recycle_bin_icon_key())
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn request_shell_folder_icon(&mut self) {
        let folder = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        self.request_shell_icon_tiers(Self::shell_folder_icon_key(), folder, false);
    }

    #[cfg(target_os = "windows")]
    fn request_shell_icon_tiers(&mut self, key: PathBuf, source: PathBuf, per_path: bool) {
        /// Max concurrent Shell icon extractions; over-cap requests retry next frame.
        const MAX_SHELL_ICON_INFLIGHT: usize = 8;
        /// Max cached per-file icons (exe/lnk/…); oldest are evicted.
        const MAX_PER_PATH_ICONS: usize = 512;
        if self.shell_icons.contains_key(&key)
            || self
                .shell_icon_inflight
                .load(std::sync::atomic::Ordering::Relaxed)
                >= MAX_SHELL_ICON_INFLIGHT
        {
            return;
        }
        self.shell_icons.insert(key.clone(), None);
        if per_path {
            self.shell_icon_lru.push_back(key.clone());
            while self.shell_icon_lru.len() > MAX_PER_PATH_ICONS {
                if let Some(old) = self.shell_icon_lru.pop_front() {
                    self.shell_icons.remove(&old);
                }
            }
        }
        let tx = self.tx.clone();
        let inflight = self.shell_icon_inflight.clone();
        inflight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.rt.spawn(async move {
            let tiers = tokio::task::spawn_blocking(move || {
                crate::shell_thumb::shell_icon_tiers(&source, per_path)
            })
            .await
            .unwrap_or_default();
            inflight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            let _ = tx.send(BgResult::ShellIconReady { key, tiers });
        });
    }

    /// Apply a finished multi-tier Shell icon (called from the bg-result drain).
    #[cfg(target_os = "windows")]
    pub(crate) fn apply_shell_icon(&mut self, key: PathBuf, tiers: Vec<(Vec<u8>, usize, usize)>) {
        let imgs: Vec<_> = tiers
            .iter()
            .filter_map(|(rgba, w, h)| rgba_to_render_image(rgba, *w, *h))
            .collect();
        let icon = match imgs.len() {
            4 => Some(Arc::new(crate::icon_theme::MultiResIcon {
                small: imgs[0].clone(),
                medium: imgs[1].clone(),
                large: imgs[2].clone(),
                full: imgs[3].clone(),
            })),
            // Compact per-file icon: the 64px tier serves every larger size.
            2 => Some(Arc::new(crate::icon_theme::MultiResIcon {
                small: imgs[0].clone(),
                medium: imgs[1].clone(),
                large: imgs[1].clone(),
                full: imgs[1].clone(),
            })),
            _ => None,
        };
        // Evicted (per-path LRU) while in flight: drop the result.
        if self.shell_icons.contains_key(&key) {
            self.shell_icons.insert(key, icon);
        }
    }

    /// Key of the shared Windows file-type icon for `path` (one per extension; per
    /// path for types whose icon is embedded in the file).
    #[cfg(target_os = "windows")]
    pub(crate) fn shell_type_icon_key(path: &std::path::Path) -> PathBuf {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext.is_empty()
            || matches!(
                ext.as_str(),
                "exe" | "lnk" | "ico" | "scr" | "url" | "cur" | "msi"
            )
        {
            PathBuf::from(format!("__atlas_shell_icon__{}", path.display()))
        } else {
            PathBuf::from(format!("__atlas_shell_icon__.{ext}"))
        }
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn shell_type_icon(
        &self,
        path: &std::path::Path,
    ) -> Option<Arc<crate::icon_theme::MultiResIcon>> {
        self.shell_icons
            .get(&Self::shell_type_icon_key(path))
            .cloned()
            .flatten()
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn request_shell_type_icon(&mut self, path: &std::path::Path) {
        let key = Self::shell_type_icon_key(path);
        let per_path = key.to_string_lossy().starts_with("__atlas_shell_icon__")
            && !key.to_string_lossy().starts_with("__atlas_shell_icon__.");
        self.request_shell_icon_tiers(key, path.to_path_buf(), per_path);
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn request_shell_recycle_bin_icon(&mut self) {
        self.load_shell_recycle_bin_icon(false);
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn load_shell_recycle_bin_icon(&mut self, refresh: bool) {
        let key = Self::shell_recycle_bin_icon_key();
        if (!refresh && self.thumb_textures.contains_key(&key))
            || self.ext_thumb_inflight.contains(&key)
            || self.ext_thumb_inflight.len() >= MAX_EXT_THUMB_INFLIGHT
        {
            return;
        }
        if !self.thumb_textures.contains_key(&key) {
            self.thumb_textures.insert(key.clone(), ThumbState::Loading);
        }
        self.ext_thumb_inflight.insert(key.clone());
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                crate::shell_thumb::shell_virtual_icon(
                    "::{645FF040-5081-101B-9F08-00AA002F954E}",
                    THUMB_MAX_EDGE,
                )
            })
            .await
            .ok()
            .flatten();
            let bg = match result {
                Some((rgba, width, height)) => BgResult::GridThumbnailReady {
                    path: key,
                    rgba,
                    width,
                    height,
                },
                None => BgResult::GridThumbnailReady {
                    path: key,
                    rgba: Vec::new(),
                    width: 0,
                    height: 0,
                },
            };
            let _ = tx.send(bg);
        });
    }

    #[cfg(not(target_os = "windows"))]
    pub(crate) fn request_shell_recycle_bin_icon(&mut self) {}

    /// Ensure a thumbnail decode is requested for `path`. If the native-decode cap is
    /// reached the request is silently dropped (the cell will re-request next frame
    /// because no Loading marker is inserted). The task checks `thumb_wanted` before
    /// the expensive decode; if the path left the viewport it sends `GridThumbnailSkipped`
    /// so the Loading marker is cleared and the cell can re-request when visible again.
    pub(crate) fn request_thumbnail(&mut self, path: &std::path::Path) {
        if self.thumb_textures.contains_key(path) {
            return;
        }
        if self.native_thumb_inflight >= MAX_NATIVE_THUMB_INFLIGHT {
            return;
        }
        let path = path.to_path_buf();
        self.thumb_textures
            .insert(path.clone(), ThumbState::Loading);
        self.native_thumb_inflight += 1;
        let tx = self.tx.clone();
        let wanted = self.thumb_wanted.clone();
        self.rt.spawn(async move {
            if let Ok(bg) = tokio::task::spawn_blocking(move || {
                if !wanted
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .contains(&path)
                {
                    return BgResult::GridThumbnailSkipped { path };
                }
                decode_thumbnail(&path, THUMB_MAX_EDGE)
            })
            .await
            {
                let _ = tx.send(bg);
            }
        });
    }

    /// Remove `path` from the thumbnail cache (called when a decode was skipped because
    /// the path left the viewport), so the cell can re-request it when visible again.
    pub(crate) fn apply_thumb_skipped(&mut self, path: PathBuf) {
        if !self.ext_thumb_inflight.remove(&path) {
            self.native_thumb_inflight = self.native_thumb_inflight.saturating_sub(1);
        }
        self.thumb_textures.remove(&path);
    }

    /// Request a thumbnail produced by an extension thumbnailer (`run_tmpl` is the
    /// command template). Idempotent and throttled: skips if already known or if the
    /// external-process cap is reached (re-issued next frame). On completion it reports
    /// via the same `BgResult::GridThumbnailReady` path as native decodes.
    pub(crate) fn request_ext_thumbnail(
        &mut self,
        path: &std::path::Path,
        run_tmpl: String,
        tools: HashMap<String, PathBuf>,
    ) {
        if atlas_core::vault::live::is_private(path) {
            return;
        }
        if self.thumb_textures.contains_key(path)
            || self.ext_thumb_inflight.len() >= MAX_EXT_THUMB_INFLIGHT
        {
            return;
        }
        let path = path.to_path_buf();
        self.thumb_textures
            .insert(path.clone(), ThumbState::Loading);
        self.ext_thumb_inflight.insert(path.clone());
        let tx = self.tx.clone();
        let wanted = self.thumb_wanted.clone();
        self.rt.spawn(async move {
            let report = path.clone();
            if !wanted
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(&path)
            {
                let _ = tx.send(BgResult::GridThumbnailSkipped { path: report });
                return;
            }
            let wanted_check = wanted.clone();
            let result = tokio::task::spawn_blocking(move || {
                if atlas_core::fs::preview::is_cloud_placeholder(&path) {
                    return Ok(None);
                }
                if !wanted_check
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .contains(&path)
                {
                    return Err(());
                }
                #[cfg(target_os = "linux")]
                if let Some(thumb) =
                    atlas_core::fs::preview::freedesktop_thumbnail(&path, THUMB_MAX_EDGE)
                {
                    return Ok(Some(thumb));
                }
                let thumb = atlas_core::ext::runner::run_thumbnail(
                    &path,
                    &run_tmpl,
                    &tools,
                    THUMB_MAX_EDGE,
                );
                Ok(thumb)
            })
            .await;

            let bg = match result {
                Ok(Err(())) => BgResult::GridThumbnailSkipped { path: report },
                Ok(Ok(Some((rgba, width, height)))) => BgResult::GridThumbnailReady {
                    path: report,
                    rgba,
                    width,
                    height,
                },
                Ok(Ok(None)) => BgResult::GridThumbnailReady {
                    path: report,
                    rgba: Vec::new(),
                    width: 0,
                    height: 0,
                },
                Err(_) => BgResult::GridThumbnailSkipped { path: report },
            };
            let _ = tx.send(bg);
        });
    }

    /// Request a thumbnail produced by the Windows Shell thumbnail cache
    /// (`IShellItemImageFactory`).  Covers videos, RAW photos, HEIC, PDF, etc. — any
    /// format that has a registered Windows thumbnail provider.  Shares the extension
    /// inflight cap so we never exceed `MAX_EXT_THUMB_INFLIGHT` concurrent COM calls.
    #[cfg(target_os = "windows")]
    pub(crate) fn request_shell_thumbnail(&mut self, path: &std::path::Path) {
        self.request_shell_image(path, false, false);
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn request_shell_thumbnail_or_icon(&mut self, path: &std::path::Path) {
        self.request_shell_image(path, true, false);
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn request_visible_shell_thumbnail(&mut self, path: &std::path::Path) {
        self.request_shell_image(path, false, true);
    }

    #[cfg(target_os = "windows")]
    fn request_shell_image(
        &mut self,
        path: &std::path::Path,
        allow_icon: bool,
        visible_only: bool,
    ) {
        if atlas_core::vault::live::is_private(path) {
            return;
        }
        if self.thumb_textures.contains_key(path)
            || self.ext_thumb_inflight.len() >= MAX_EXT_THUMB_INFLIGHT
        {
            return;
        }
        let path = path.to_path_buf();
        self.thumb_textures
            .insert(path.clone(), ThumbState::Loading);
        self.ext_thumb_inflight.insert(path.clone());
        let tx = self.tx.clone();
        let wanted = self.thumb_wanted.clone();
        self.rt.spawn(async move {
            let report = path.clone();
            if visible_only
                && !wanted
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .contains(&path)
            {
                let _ = tx.send(BgResult::GridThumbnailSkipped { path: report });
                return;
            }
            let wanted_check = wanted.clone();
            let result = tokio::task::spawn_blocking(move || {
                if visible_only
                    && !wanted_check
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .contains(&path)
                {
                    return Err(());
                }
                let thumb = if allow_icon {
                    crate::shell_thumb::shell_thumbnail_or_icon(&path, THUMB_MAX_EDGE)
                } else {
                    crate::shell_thumb::shell_thumbnail(&path, THUMB_MAX_EDGE)
                };
                Ok(thumb)
            })
            .await;

            let bg = match result {
                Ok(Err(())) => BgResult::GridThumbnailSkipped { path: report },
                Ok(Ok(Some((rgba, width, height)))) => BgResult::GridThumbnailReady {
                    path: report,
                    rgba,
                    width,
                    height,
                },
                Ok(Ok(None)) => BgResult::GridThumbnailReady {
                    path: report,
                    rgba: Vec::new(),
                    width: 0,
                    height: 0,
                },
                Err(_) => BgResult::GridThumbnailSkipped { path: report },
            };
            let _ = tx.send(bg);
        });
    }

    /// Look up an already-decoded thumbnail texture for `path`, if ready.
    pub(crate) fn grid_thumbnail(&self, path: &std::path::Path) -> Option<Arc<RenderImage>> {
        match self.thumb_textures.get(path) {
            Some(ThumbState::Ready(tex)) => Some(tex.clone()),
            _ => None,
        }
    }

    /// Apply a finished thumbnail decode (called from the bg-result drain).
    pub(crate) fn apply_grid_thumbnail(
        &mut self,
        path: PathBuf,
        rgba: Vec<u8>,
        width: usize,
        height: usize,
    ) {
        // Free an extension-thumbnailer slot if this path used one; otherwise it was
        // a native decode — decrement the native inflight counter.
        let was_ext = self.ext_thumb_inflight.remove(&path);
        if !was_ext {
            self.native_thumb_inflight = self.native_thumb_inflight.saturating_sub(1);
        }
        let state = if rgba.is_empty() {
            ThumbState::Missing
        } else {
            match rgba_to_render_image(&rgba, width, height) {
                Some(tex) => ThumbState::Ready(tex),
                None => ThumbState::Missing,
            }
        };
        self.thumb_textures.insert(path, state);
    }
}

/// Decode `path` and box-resize so its longest edge is `max_edge`, returning packed
/// RGBA8. Returns an empty vec on any failure (cached as a permanent miss).
fn decode_thumbnail(path: &std::path::Path, max_edge: u32) -> BgResult {
    if atlas_core::fs::preview::is_cloud_placeholder(path) {
        return BgResult::GridThumbnailReady {
            path: path.to_path_buf(),
            rgba: Vec::new(),
            width: 0,
            height: 0,
        };
    }
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() == 0 {
            return BgResult::GridThumbnailReady {
                path: path.to_path_buf(),
                rgba: Vec::new(),
                width: 0,
                height: 0,
            };
        }
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let result = if atlas_model::preview_cache::is_audio_ext(&ext) {
        atlas_core::fs::preview::extract_audio_thumbnail(path, max_edge)
    } else if ext == "svg" {
        atlas_core::fs::preview::decode_svg(path, max_edge)
    } else if atlas_model::preview_cache::is_model_ext(&ext) {
        atlas_core::fs::preview::model_thumbnail(path, max_edge)
    } else {
        image::open(path).ok().map(|img| {
            let thumb = img.thumbnail(max_edge, max_edge).to_rgba8();
            let (w, h) = (thumb.width() as usize, thumb.height() as usize);
            (thumb.into_raw(), w, h)
        })
    };

    #[cfg(target_os = "linux")]
    let result = result.or_else(|| atlas_core::fs::preview::freedesktop_thumbnail(path, max_edge));

    match result {
        Some((rgba, width, height)) => BgResult::GridThumbnailReady {
            path: path.to_path_buf(),
            rgba,
            width,
            height,
        },
        None => BgResult::GridThumbnailReady {
            path: path.to_path_buf(),
            rgba: Vec::new(),
            width: 0,
            height: 0,
        },
    }
}
