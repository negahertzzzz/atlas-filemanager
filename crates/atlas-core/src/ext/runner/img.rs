#![allow(clippy::disallowed_methods)]
// reason: preview functions run on blocking workers and must touch files.

use std::path::Path;

use atlas_model::preview_cache::PreviewResult;

pub(crate) fn image_file_to_preview(path: &Path) -> PreviewResult {
    match image::open(path) {
        Ok(img) => {
            let original_width = img.width() as usize;
            let original_height = img.height() as usize;
            let img = if img.width() > 1600 || img.height() > 1600 {
                img.thumbnail(1600, 1600)
            } else {
                img
            }
            .to_rgba8();
            let (width, height) = img.dimensions();
            PreviewResult::Image {
                rgba: img.into_raw(),
                width: width as usize,
                height: height as usize,
                original_width,
                original_height,
                exif: Vec::new(),
            }
        }
        Err(e) => PreviewResult::Error(e.to_string()),
    }
}

pub(crate) fn run_magick_preview(path: &Path, magick: &Path) -> PreviewResult {
    let output =
        std::env::temp_dir().join(format!("atlas-magick-preview-{}.png", uuid::Uuid::new_v4()));
    let input = format!("{}[0]", path.to_string_lossy());
    let mut cmd = std::process::Command::new(magick);
    cmd.args([
        input.as_str(),
        "-flatten",
        "-resize",
        "1600x1600>",
        output.to_string_lossy().as_ref(),
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(crate::resources::tool_creation_flags(0x0800_0000));
    }
    let ran = cmd.output();
    let result = match ran {
        Ok(out) if out.status.success() => image_file_to_preview(&output),
        Ok(out) => PreviewResult::Error(String::from_utf8_lossy(&out.stderr).trim().to_string()),
        Err(e) => PreviewResult::Error(e.to_string()),
    };
    let _ = std::fs::remove_file(output);
    result
}
