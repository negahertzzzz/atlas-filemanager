//! Same-format file actions for the Actions ring: resize, rotate, strip
//! metadata and compress. Everything is native except "Compress PDF", which is
//! only offered when Ghostscript is installed. Outputs never replace the
//! source: they land beside it (or in the chosen folder) with a suffix.
use super::{catalog, native, process, publish, Backend, Command, Outcome, Report, Settings};
use super::{Tool, Tools, FORMATS};
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Resize,
    RotateRight,
    RotateLeft,
    StripMetadata,
    Compress,
    CompressPdf,
}

/// Raster formats the native encoders round-trip without dropping frames/pages.
const STILL_IMAGES: [&str; 4] = ["png", "jpg", "webp", "bmp"];

impl Action {
    pub const ALL: [Self; 6] = [
        Self::Resize,
        Self::RotateRight,
        Self::RotateLeft,
        Self::StripMetadata,
        Self::Compress,
        Self::CompressPdf,
    ];

    /// Stable id persisted in settings.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Resize => "resize",
            Self::RotateRight => "rotate-right",
            Self::RotateLeft => "rotate-left",
            Self::StripMetadata => "strip-metadata",
            Self::Compress => "compress",
            Self::CompressPdf => "compress-pdf",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    /// `extension` as normalised by the conversion catalog (`jpeg` → `jpg`).
    pub fn accepts(self, extension: &str) -> bool {
        match self {
            Self::Resize | Self::RotateRight | Self::RotateLeft => {
                STILL_IMAGES.contains(&extension)
            }
            Self::StripMetadata | Self::Compress => ["jpg", "png"].contains(&extension),
            Self::CompressPdf => extension == "pdf",
        }
    }

    /// External engine the action needs; `None` means Atlas does it natively.
    pub const fn tool(self) -> Option<Tool> {
        match self {
            Self::CompressPdf => Some(Tool::Ghostscript),
            _ => None,
        }
    }

    fn suffix(self, settings: &ActionSettings) -> String {
        match self {
            Self::Resize => format!("-{}px", settings.edge),
            Self::RotateRight | Self::RotateLeft => "-rotated".into(),
            Self::StripMetadata => "-clean".into(),
            Self::Compress | Self::CompressPdf => "-compressed".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ActionSettings {
    /// Longest edge for Resize, in pixels.
    pub edge: u32,
    /// JPEG quality for Compress.
    pub quality: u8,
    /// None saves beside each source.
    pub output_dir: Option<PathBuf>,
}

impl Default for ActionSettings {
    fn default() -> Self {
        Self {
            edge: 1920,
            quality: 80,
            output_dir: None,
        }
    }
}

/// Actions from `enabled` (kept in that order) that apply to every path and
/// whose engine is installed. Missing engines hide the action entirely.
pub fn available(paths: &[PathBuf], tools: &Tools, enabled: &[Action]) -> Vec<Action> {
    if paths.is_empty() {
        return Vec::new();
    }
    enabled
        .iter()
        .copied()
        .filter(|action| action.tool().is_none_or(|tool| tools.has(tool)))
        .filter(|action| {
            paths
                .iter()
                .all(|path| action.accepts(&catalog::extension(path)))
        })
        .collect()
}

/// Run `action` over `paths` one file at a time, reporting like conversions.
pub fn execute(
    action: Action,
    paths: &[PathBuf],
    settings: &ActionSettings,
    tools: &Tools,
    cancel: &AtomicBool,
    progress: impl Fn(usize, usize, &Path),
) -> Report {
    let total = paths.len();
    let mut outcomes = Vec::with_capacity(total);
    for (index, source) in paths.iter().enumerate() {
        let outcome = if cancel.load(Ordering::Relaxed) {
            Outcome::Cancelled {
                source: source.clone(),
            }
        } else {
            match execute_one(action, source, settings, tools, cancel) {
                Ok(output) => Outcome::Created {
                    source: source.clone(),
                    output,
                },
                Err(_) if cancel.load(Ordering::Relaxed) => Outcome::Cancelled {
                    source: source.clone(),
                },
                Err(error) => Outcome::Failed {
                    source: source.clone(),
                    error: format!("{error:#}"),
                },
            }
        };
        progress(index + 1, total, source);
        outcomes.push(outcome);
    }
    Report { outcomes }
}

fn execute_one(
    action: Action,
    source: &Path,
    settings: &ActionSettings,
    tools: &Tools,
    cancel: &AtomicBool,
) -> Result<PathBuf> {
    if !source.is_file() {
        bail!("Not a local file: {}", source.display());
    }
    let extension = catalog::extension(source);
    if !action.accepts(&extension) {
        bail!("This action does not apply to .{extension} files");
    }
    let parent = settings
        .output_dir
        .as_deref()
        .or_else(|| source.parent())
        .context("Missing output directory")?;
    let raw_extension = source
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let output = tempfile::Builder::new()
        .prefix(".atlas-action-")
        .suffix(&format!(".{raw_extension}"))
        .tempfile_in(parent)?
        .into_temp_path();
    match action {
        Action::Resize => {
            let image = native::decode_oriented(source)?;
            let edge = settings.edge.max(1);
            if image.width() <= edge && image.height() <= edge {
                bail!("Already {edge} px or smaller");
            }
            write_same_format(image, source, &extension, 92, Some(edge), &output)?;
        }
        Action::RotateRight | Action::RotateLeft => {
            let image = native::decode_oriented(source)?;
            let image = if action == Action::RotateRight {
                image.rotate90()
            } else {
                image.rotate270()
            };
            write_same_format(image, source, &extension, 95, None, &output)?;
        }
        Action::StripMetadata => strip(source, &extension, &output)?,
        Action::Compress => {
            compress_image(source, &extension, settings.quality, &output)?;
            ensure_smaller(source, &output)?;
        }
        Action::CompressPdf => {
            compress_pdf(source, tools, &output, cancel)?;
            ensure_smaller(source, &output)?;
        }
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }
    let stem = source
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let suffix = action.suffix(settings);
    publish(output, |index| {
        let mut name = format!("{stem}{suffix}");
        if index > 0 {
            name.push_str(&format!(" ({index})"));
        }
        if !raw_extension.is_empty() {
            name.push('.');
            name.push_str(&raw_extension);
        }
        parent.join(name)
    })
}

fn write_same_format(
    image: image::DynamicImage,
    source: &Path,
    extension: &str,
    quality: u8,
    max_edge: Option<u32>,
    output: &Path,
) -> Result<()> {
    let target = FORMATS
        .iter()
        .find(|format| format.extension == extension)
        .context("Unsupported image format")?;
    let command = Command {
        source: source.to_path_buf(),
        target,
        backend: Backend::Image,
    };
    let settings = Settings {
        quality,
        max_edge,
        output_dir: None,
    };
    native::write_image(image, &command, &settings, output)
}

fn ensure_smaller(source: &Path, output: &Path) -> Result<()> {
    if std::fs::metadata(output)?.len() >= std::fs::metadata(source)?.len() {
        bail!("Already optimized: the compressed file would not be smaller");
    }
    Ok(())
}

/// Lossless where the format allows it: metadata segments/chunks are dropped
/// and the pixel data is copied untouched. A rotated JPEG (EXIF orientation)
/// is re-encoded upright instead, since dropping the tag would turn it.
fn strip(source: &Path, extension: &str, output: &Path) -> Result<()> {
    let bytes = std::fs::read(source)?;
    let stripped = match extension {
        "jpg" => {
            if jpeg_orientation(source)? != image::metadata::Orientation::NoTransforms {
                let image = native::decode_oriented(source)?;
                return write_same_format(image, source, extension, 95, None, output);
            }
            strip_jpeg(&bytes)?
        }
        "png" => strip_png(&bytes)?,
        _ => bail!("Metadata removal supports JPEG and PNG"),
    };
    std::fs::write(output, stripped)?;
    Ok(())
}

fn jpeg_orientation(source: &Path) -> Result<image::metadata::Orientation> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::open(source)?
        .with_guessed_format()?
        .into_decoder()?;
    Ok(decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms))
}

/// Copy a JPEG without APP1 (EXIF/XMP), APP13 (IPTC/Photoshop) and comments.
/// ICC profiles (APP2), JFIF and Adobe segments are kept so colours don't shift.
pub(crate) fn strip_jpeg(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() < 4 || bytes[..2] != [0xFF, 0xD8] {
        bail!("Not a JPEG file");
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&[0xFF, 0xD8]);
    let mut i = 2;
    loop {
        if bytes.get(i) != Some(&0xFF) {
            bail!("Malformed JPEG");
        }
        while bytes.get(i) == Some(&0xFF) {
            i += 1;
        }
        let marker = *bytes.get(i).context("Truncated JPEG")?;
        i += 1;
        match marker {
            0xD9 => {
                out.extend_from_slice(&[0xFF, 0xD9]);
                return Ok(out);
            }
            0x01 | 0xD0..=0xD7 => {
                out.extend_from_slice(&[0xFF, marker]);
                continue;
            }
            _ => {}
        }
        let length = bytes
            .get(i..i + 2)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
            .context("Truncated JPEG")?;
        if length < 2 || i + length > bytes.len() {
            bail!("Malformed JPEG segment");
        }
        if marker == 0xDA {
            // Start of scan: entropy-coded data and everything after it is kept.
            out.extend_from_slice(&[0xFF, 0xDA]);
            out.extend_from_slice(&bytes[i..]);
            return Ok(out);
        }
        if !matches!(marker, 0xE1 | 0xED | 0xFE) {
            out.extend_from_slice(&[0xFF, marker]);
            out.extend_from_slice(&bytes[i..i + length]);
        }
        i += length;
    }
}

/// Copy a PNG without text, EXIF and timestamp chunks.
pub(crate) fn strip_png(bytes: &[u8]) -> Result<Vec<u8>> {
    const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
    if bytes.len() < 8 || bytes[..8] != SIGNATURE {
        bail!("Not a PNG file");
    }
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(&SIGNATURE);
    let mut i = 8;
    while i + 12 <= bytes.len() {
        let length =
            u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        let kind = &bytes[i + 4..i + 8];
        let end = i
            .checked_add(12)
            .and_then(|n| n.checked_add(length))
            .filter(|end| *end <= bytes.len())
            .context("Malformed PNG chunk")?;
        if !matches!(kind, b"tEXt" | b"zTXt" | b"iTXt" | b"eXIf" | b"tIME") {
            out.extend_from_slice(&bytes[i..end]);
        }
        if kind == b"IEND" {
            return Ok(out);
        }
        i = end;
    }
    bail!("Truncated PNG")
}

fn compress_image(source: &Path, extension: &str, quality: u8, output: &Path) -> Result<()> {
    let image = native::decode_oriented(source)?;
    match extension {
        "jpg" => write_same_format(
            image,
            source,
            extension,
            quality.clamp(1, 100),
            None,
            output,
        ),
        "png" => {
            use image::codecs::png::{CompressionType, FilterType, PngEncoder};
            use image::ImageEncoder;
            let file = std::fs::File::create(output)?;
            PngEncoder::new_with_quality(file, CompressionType::Best, FilterType::Adaptive)
                .write_image(
                    image.as_bytes(),
                    image.width(),
                    image.height(),
                    image.color().into(),
                )?;
            Ok(())
        }
        _ => bail!("Compression supports JPEG and PNG"),
    }
}

fn compress_pdf(source: &Path, tools: &Tools, output: &Path, cancel: &AtomicBool) -> Result<()> {
    let ghostscript = tools
        .get(Tool::Ghostscript)
        .context("Ghostscript is not installed")?;
    // Ghostscript expands `%d` in output names; escape literal percent signs.
    let output = output.to_string_lossy().replace('%', "%%");
    let mut command = std::process::Command::new(ghostscript);
    command
        .args([
            "-sDEVICE=pdfwrite",
            "-dCompatibilityLevel=1.5",
            "-dPDFSETTINGS=/ebook",
            "-dSAFER",
            "-dNOPAUSE",
            "-dQUIET",
            "-dBATCH",
        ])
        .arg(format!("-sOutputFile={output}"))
        .arg(source);
    process::run(command, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(dir: &Path, name: &str, width: u32, height: u32) -> PathBuf {
        let path = dir.join(name);
        image::RgbaImage::from_fn(width, height, |x, y| {
            image::Rgba([(x * 7) as u8, (y * 5) as u8, 90, 255])
        })
        .save(&path)
        .unwrap();
        path
    }

    fn jpeg_with_exif(dir: &Path) -> PathBuf {
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(16, 8, image::Rgb([200, 30, 30]))
            .write_to(&mut encoded, image::ImageFormat::Jpeg)
            .unwrap();
        let encoded = encoded.into_inner();
        // Big-endian TIFF with an empty IFD: EXIF present, no orientation.
        let payload = b"Exif\0\0MM\0\x2a\0\0\0\x08\0\0\0\0\0\0";
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE1];
        bytes.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&[0xFF, 0xFE, 0x00, 0x07]);
        bytes.extend_from_slice(b"hello");
        bytes.extend_from_slice(&encoded[2..]);
        let path = dir.join("photo.jpg");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn availability_follows_enabled_order_selection_and_installed_engines() {
        let all = Action::ALL;
        let images = [PathBuf::from("a.png"), PathBuf::from("b.JPEG")];
        assert_eq!(
            available(&images, &Tools::default(), &all),
            [
                Action::Resize,
                Action::RotateRight,
                Action::RotateLeft,
                Action::StripMetadata,
                Action::Compress
            ]
        );
        let order = [Action::Compress, Action::Resize];
        assert_eq!(available(&images, &Tools::default(), &order), order);
        let mixed = [PathBuf::from("a.png"), PathBuf::from("b.pdf")];
        assert!(available(&mixed, &Tools::default(), &all).is_empty());
        let pdf = [PathBuf::from("doc.pdf")];
        assert!(available(&pdf, &Tools::default(), &all).is_empty());
        let mut tools = Tools::default();
        tools.0.insert(Tool::Ghostscript, PathBuf::from("gs"));
        assert_eq!(available(&pdf, &tools, &all), [Action::CompressPdf]);
        assert!(available(&[PathBuf::from("anim.gif")], &tools, &all).is_empty());
    }

    #[test]
    fn ids_round_trip() {
        for action in Action::ALL {
            assert_eq!(Action::from_id(action.id()), Some(action));
        }
        assert_eq!(Action::from_id("nope"), None);
    }

    #[test]
    fn resize_keeps_format_aspect_and_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = png(dir.path(), "wide.png", 400, 200);
        let settings = ActionSettings {
            edge: 100,
            ..ActionSettings::default()
        };
        let cancel = AtomicBool::new(false);
        let report = execute(
            Action::Resize,
            &[source.clone()],
            &settings,
            &Tools::default(),
            &cancel,
            |_, _, _| {},
        );
        let outputs = report.outputs();
        assert_eq!(
            outputs,
            [dir.path().join("wide-100px.png")],
            "{:?}",
            report.errors()
        );
        let resized = image::open(&outputs[0]).unwrap();
        assert_eq!((resized.width(), resized.height()), (100, 50));
        assert_eq!(image::open(&source).unwrap().width(), 400);
        // A second run never overwrites the first output.
        let again = execute(
            Action::Resize,
            &[source.clone()],
            &settings,
            &Tools::default(),
            &cancel,
            |_, _, _| {},
        );
        assert_eq!(again.outputs(), [dir.path().join("wide-100px (1).png")]);
        // Smaller images are reported, not upscaled.
        let small = png(dir.path(), "small.png", 40, 20);
        let skipped = execute(
            Action::Resize,
            &[small],
            &settings,
            &Tools::default(),
            &cancel,
            |_, _, _| {},
        );
        assert!(skipped.outputs().is_empty());
        assert_eq!(skipped.errors().len(), 1);
    }

    #[test]
    fn rotate_swaps_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let source = png(dir.path(), "tall.png", 30, 10);
        let cancel = AtomicBool::new(false);
        for action in [Action::RotateRight, Action::RotateLeft] {
            let report = execute(
                action,
                &[source.clone()],
                &ActionSettings::default(),
                &Tools::default(),
                &cancel,
                |_, _, _| {},
            );
            let rotated = image::open(&report.outputs()[0]).unwrap();
            assert_eq!((rotated.width(), rotated.height()), (10, 30));
        }
    }

    #[test]
    fn strip_removes_jpeg_exif_and_comments_losslessly() {
        let dir = tempfile::tempdir().unwrap();
        let source = jpeg_with_exif(dir.path());
        let original = std::fs::read(&source).unwrap();
        let stripped = strip_jpeg(&original).unwrap();
        assert!(!stripped.windows(4).any(|w| w == b"Exif"));
        assert!(!stripped.windows(5).any(|w| w == b"hello"));
        // Scan data is byte-identical.
        let scan = |b: &[u8]| {
            b.windows(2)
                .position(|w| w == [0xFF, 0xDA])
                .map(|p| b[p..].to_vec())
        };
        assert_eq!(scan(&original), scan(&stripped));
        let cancel = AtomicBool::new(false);
        let report = execute(
            Action::StripMetadata,
            &[source],
            &ActionSettings::default(),
            &Tools::default(),
            &cancel,
            |_, _, _| {},
        );
        let output = &report.outputs()[0];
        assert_eq!(output, &dir.path().join("photo-clean.jpg"));
        assert_eq!(image::open(output).unwrap().width(), 16);
        assert!(strip_jpeg(b"not a jpeg").is_err());
        assert!(strip_jpeg(&original[..30]).is_err());
    }

    #[test]
    fn strip_removes_png_text_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let source = png(dir.path(), "meta.png", 4, 4);
        let mut bytes = std::fs::read(&source).unwrap();
        // Insert a tEXt chunk right after IHDR (8 + 25 bytes).
        let data = b"Author\0Someone";
        let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
        chunk.extend_from_slice(b"tEXt");
        chunk.extend_from_slice(data);
        chunk.extend_from_slice(&[0, 0, 0, 0]);
        bytes.splice(33..33, chunk);
        let stripped = strip_png(&bytes).unwrap();
        assert!(!stripped.windows(4).any(|w| w == b"tEXt"));
        assert!(stripped.ends_with(&[0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82]));
        assert!(image::load_from_memory(&stripped).is_ok());
        assert!(strip_png(&bytes[..40]).is_err());
    }

    #[test]
    fn compress_never_publishes_a_bigger_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("noisy.jpg");
        image::RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([(x * 31 ^ y * 17) as u8, (x * y) as u8, (x + y) as u8])
        })
        .save(&path)
        .unwrap();
        let cancel = AtomicBool::new(false);
        let settings = ActionSettings {
            quality: 40,
            ..ActionSettings::default()
        };
        let report = execute(
            Action::Compress,
            &[path.clone()],
            &settings,
            &Tools::default(),
            &cancel,
            |_, _, _| {},
        );
        let output = &report.outputs()[0];
        assert!(std::fs::metadata(output).unwrap().len() < std::fs::metadata(&path).unwrap().len());
        // Recompressing Atlas's own best-effort PNG cannot shrink it: reported,
        // nothing published and no leftover temporaries.
        let flat = png(dir.path(), "flat.png", 32, 32);
        let first = execute(
            Action::Compress,
            &[flat],
            &settings,
            &Tools::default(),
            &cancel,
            |_, _, _| {},
        );
        let optimized = first
            .outputs()
            .first()
            .cloned()
            .unwrap_or_else(|| dir.path().join("flat.png"));
        let report = execute(
            Action::Compress,
            &[optimized],
            &settings,
            &Tools::default(),
            &cancel,
            |_, _, _| {},
        );
        assert!(report.outputs().is_empty());
        assert!(report.errors()[0].contains("Already optimized"));
        assert!(std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .all(|entry| !entry
                .file_name()
                .to_string_lossy()
                .starts_with(".atlas-action-")));
    }

    #[test]
    fn compress_pdf_requires_ghostscript() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.pdf");
        std::fs::write(&path, b"%PDF-1.4\n").unwrap();
        let cancel = AtomicBool::new(false);
        let report = execute(
            Action::CompressPdf,
            &[path],
            &ActionSettings::default(),
            &Tools::default(),
            &cancel,
            |_, _, _| {},
        );
        assert!(report.errors()[0].contains("Ghostscript"));
    }
}
