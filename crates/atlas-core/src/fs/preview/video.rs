#![allow(clippy::disallowed_methods)]
// reason: video frame extraction and ffprobe run on blocking workers.

use std::path::Path;
use std::process::Command;

use atlas_model::preview_cache::PreviewResult;

#[cfg(target_os = "linux")]
use super::MAX_IMAGE_PREVIEW_EDGE;

pub(super) fn preview_video(path: &Path) -> PreviewResult {
    if super::is_cloud_placeholder(path) { return super::offline_preview(); }
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return PreviewResult::Error(e.to_string()),
    };
    if meta.len() == 0 {
        return PreviewResult::Error("El archivo de vídeo está vacío (0 bytes)".to_string());
    }

    let output_prefix =
        std::env::temp_dir().join(format!("atlas-vidprev-{}", uuid::Uuid::new_v4()));
    // BMP: no compression pass, so ffmpeg finishes sooner and decoding is a memcpy.
    let png_path = output_prefix.with_extension("bmp");

    // ffprobe is independent of frame extraction: overlap them.
    let probe_path = path.to_path_buf();
    let probe = std::thread::spawn(move || read_video_info(&probe_path));

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-noaccurate_seek", "-ss", "0.5", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=min(1600\\,iw):-1",
            "-update",
            "1",
        ])
        .arg(&png_path);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(crate::resources::tool_creation_flags(0x0800_0000));
    }

    let mut ok = cmd
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    if !ok {
        let mut retry = Command::new("ffmpeg");
        retry
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(path)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=min(1600\\,iw):-1",
                "-update",
                "1",
            ])
            .arg(&png_path);

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            retry.creation_flags(crate::resources::tool_creation_flags(0x0800_0000));
        }
        ok = retry
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
    }
    if ok {
        if let Ok(img) = image::open(&png_path) {
            let _ = std::fs::remove_file(&png_path);
            let original_width = img.width() as usize;
            let original_height = img.height() as usize;
            let rgba = img.to_rgba8();
            let (width, height) = rgba.dimensions();

            let mut exif = Vec::new();
            if let Some((codec, orig_w, orig_h, dur)) = probe.join().ok().flatten() {
                if orig_w > 0 && orig_h > 0 {
                    exif.push(("Resolution".to_string(), format!("{orig_w} × {orig_h}")));
                }
                if let Some(dur) = dur {
                    let total_secs = dur.round() as u64;
                    let mins = total_secs / 60;
                    let secs = total_secs % 60;
                    exif.push(("Duration".to_string(), format!("{mins}:{secs:02}")));
                }
                if let Some(codec) = codec {
                    exif.push(("Codec".to_string(), codec));
                }
            }

            return PreviewResult::Image {
                rgba: rgba.into_raw(),
                width: width as usize,
                height: height as usize,
                original_width,
                original_height,
                exif,
            };
        }
    }
    let _ = std::fs::remove_file(&png_path);

    #[cfg(target_os = "linux")]
    if let Some((rgba, width, height)) =
        super::linux_thumb::freedesktop_thumbnail(path, MAX_IMAGE_PREVIEW_EDGE)
    {
        return PreviewResult::Image {
            rgba,
            width,
            height,
            original_width: width,
            original_height: height,
            exif: vec![("Source".to_string(), "Desktop Thumbnail".to_string())],
        };
    }

    PreviewResult::Binary { size: meta.len() }
}

/// Resolution / duration / codec rows for the inspector, from ffprobe.
pub(super) fn video_exif(path: &Path) -> Vec<(String, String)> {
    let mut exif = Vec::new();
    if let Some((codec, w, h, dur)) = read_video_info(path) {
        if w > 0 && h > 0 {
            exif.push(("Resolution".to_string(), format!("{w} × {h}")));
        }
        if let Some(dur) = dur {
            let total = dur.round() as u64;
            exif.push(("Duration".to_string(), format!("{}:{:02}", total / 60, total % 60)));
        }
        if let Some(codec) = codec {
            exif.push(("Codec".to_string(), codec));
        }
    }
    exif
}

fn read_video_info(path: &Path) -> Option<(Option<String>, usize, usize, Option<f64>)> {
    let mut cmd = Command::new("ffprobe");
    cmd.args([
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height,codec_name:format=duration",
        "-of",
        "default=noprint_wrappers=1",
    ])
    .arg(path);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(crate::resources::tool_creation_flags(0x0800_0000));
    }

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut codec = None;
    let mut width = 0;
    let mut height = 0;
    let mut duration = None;

    for line in text.lines() {
        if let Some(val) = line.strip_prefix("codec_name=") {
            codec = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("width=") {
            width = val.parse().unwrap_or(0);
        } else if let Some(val) = line.strip_prefix("height=") {
            height = val.parse().unwrap_or(0);
        } else if let Some(val) = line.strip_prefix("duration=") {
            duration = val.parse().ok();
        }
    }
    Some((codec, width, height, duration))
}
