use super::{Tool, Tools};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub extension: &'static str,
    pub label: &'static str,
}

macro_rules! formats { ($($ext:literal => $label:literal),* $(,)?) => { pub const FORMATS: &[Format] = &[$(Format { extension: $ext, label: $label }),*]; }; }
formats! {
    "png" => "PNG", "jpg" => "JPG", "webp" => "WebP", "pdf" => "PDF",
    "docx" => "DOCX", "txt" => "TXT", "md" => "Markdown", "html" => "HTML",
    "epub" => "EPUB", "odt" => "ODT", "rtf" => "RTF", "pptx" => "PPTX", "odp" => "ODP",
    "xlsx" => "XLSX", "ods" => "ODS", "csv" => "CSV", "tsv" => "TSV",
    "json" => "JSON", "jsonl" => "JSONL", "toml" => "TOML",
    "vcf" => "vCard",
    "bmp" => "BMP", "tiff" => "TIFF", "gif" => "GIF", "ico" => "ICO", "avif" => "AVIF", "heic" => "HEIC", "jxl" => "JXL",
    "svg" => "SVG", "svgz" => "SVGZ",
    "mp3" => "MP3", "wav" => "WAV", "flac" => "FLAC", "m4a" => "M4A", "ogg" => "OGG", "opus" => "Opus", "aac" => "AAC", "aiff" => "AIFF", "au" => "AU",
    "mp4" => "MP4", "webm" => "WebM", "mkv" => "MKV", "mov" => "MOV", "avi" => "AVI",
    "srt" => "SRT", "vtt" => "VTT", "ass" => "ASS", "lrc" => "LRC", "smi" => "SAMI", "ttml" => "TTML",
    "zip" => "ZIP", "7z" => "7Z", "tar" => "TAR", "tar.gz" => "TAR.GZ",
    "mobi" => "MOBI", "azw3" => "AZW3", "fb2" => "FB2",
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Image,
    Data,
    Sheet,
    Markdown,
    Text,
    Archive,
    Subtitles,
    Pcm,
    Flac,
    Vector,
    Contacts,
    Book,
    SystemAudio,
    SystemVideo,
    PdfPages,
    SystemPdfText,
    OfficePackage,
    External(Tool),
}
impl Backend {
    pub fn available(self, tools: &Tools) -> bool {
        match self {
            Self::External(t) => tools.has(t),
            Self::PdfPages => self.required_tools().iter().all(|tool| tools.has(*tool)),
            Self::SystemPdfText => tools.1.ocr,
            Self::SystemAudio => tools.1.wave,
            Self::SystemVideo => tools.1.h264,
            _ => true,
        }
    }
    pub fn required_tools(self) -> &'static [Tool] {
        match self {
            Self::External(Tool::Magick) => &[Tool::Magick],
            Self::External(Tool::Ffmpeg) => &[Tool::Ffmpeg],
            Self::External(Tool::Opus) => &[Tool::Opus],
            Self::External(Tool::Pandoc) => &[Tool::Pandoc],
            Self::External(Tool::Office) => &[Tool::Office],
            Self::External(Tool::PdfText) => &[Tool::PdfText],
            Self::External(Tool::SevenZip) => &[Tool::SevenZip],
            Self::External(Tool::Calibre) => &[Tool::Calibre],
            Self::PdfPages if !cfg!(windows) => &[Tool::Magick],
            _ => &[],
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::External(t) => t.name(),
            Self::SystemAudio | Self::SystemVideo => "Windows",
            Self::PdfPages if cfg!(windows) => "Windows",
            Self::PdfPages => "ImageMagick",
            Self::SystemPdfText => "Windows OCR",
            _ => "Atlas",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConversionOption {
    pub target: &'static Format,
    pub missing: Vec<Tool>,
    pub engines: Vec<&'static str>,
}
impl ConversionOption {
    pub fn available(&self) -> bool {
        self.missing.is_empty()
    }
}

pub fn extension(path: &Path) -> String {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    for extension in ["tar.gz", "tar.bz2", "tar.xz"] {
        if name.ends_with(&format!(".{extension}")) {
            return extension.to_owned();
        }
    }
    match path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "jpeg" => "jpg".into(),
        "tif" => "tiff".into(),
        "markdown" => "md".into(),
        "htm" => "html".into(),
        "ndjson" => "jsonl".into(),
        "aif" => "aiff".into(),
        "wave" => "wav".into(),
        "snd" => "au".into(),
        "dfxp" => "ttml".into(),
        e => e.to_owned(),
    }
}

pub fn options(paths: &[PathBuf], tools: &Tools) -> Vec<ConversionOption> {
    if paths.is_empty() {
        return Vec::new();
    }
    let mut result: Vec<_> = FORMATS
        .iter()
        .filter_map(|target| {
            let mut missing = Vec::new();
            let mut engines = Vec::new();
            let mut changed = false;
            for path in paths {
                if extension(path) == target.extension {
                    continue;
                }
                let backend = route(path, target, tools)?;
                changed = true;
                if !engines.contains(&backend.name()) {
                    engines.push(backend.name());
                }
                for tool in backend.required_tools() {
                    if !tools.has(*tool) && !missing.contains(tool) {
                        missing.push(*tool);
                    }
                }
            }
            changed.then_some(ConversionOption {
                target,
                missing,
                engines,
            })
        })
        .collect();
    let source = extension(&paths[0]);
    let preferred: &[&str] = match source.as_str() {
        "pdf" => &["docx", "jpg", "png", "txt", "md", "html", "tiff"],
        "mp4" | "mov" | "webm" | "mkv" | "avi" => &["mp4", "webm", "gif", "mp3"],
        "mp3" | "wav" | "m4a" | "flac" | "ogg" | "opus" => &["mp3", "wav", "flac", "m4a"],
        "xlsx" | "xls" | "ods" => &["xlsx", "csv", "json", "pdf"],
        "docx" | "doc" | "odt" | "md" | "txt" | "rtf" | "epub" => &["pdf", "docx", "txt", "html"],
        _ => &[],
    };
    result.sort_by_key(|option| {
        preferred
            .iter()
            .position(|extension| *extension == option.target.extension)
            .unwrap_or(preferred.len())
    });
    result
}

/// Only routes implemented by Atlas, the OS, or the optional Opus encoder.
pub fn native_options(paths: &[PathBuf], tools: &Tools) -> Vec<ConversionOption> {
    wheel_options(paths, tools, &[])
}

/// Wheel routes: Atlas, the OS, the optional Opus encoder and the `extra`
/// engines an enabled tool provider has installed. Routes needing any other
/// engine are hidden, never offered with a missing-tool chip.
pub fn wheel_options(paths: &[PathBuf], tools: &Tools, extra: &[Tool]) -> Vec<ConversionOption> {
    let allowed = tools.clone().native_and(extra);
    options(paths, &allowed)
        .into_iter()
        .filter(|option| {
            paths.iter().all(|path| {
                extension(path) == option.target.extension
                    || route(path, option.target, &allowed).is_some_and(|backend| {
                        backend
                            .required_tools()
                            .iter()
                            .all(|tool| *tool == Tool::Opus || allowed.has(*tool))
                    })
            })
        })
        .collect()
}

/// Engines a provider package could install that would add wheel routes for
/// `paths`. The wheel hides those routes; the side panel offers the package.
pub fn locked_engines(
    paths: &[PathBuf],
    tools: &Tools,
    extra: &[Tool],
    installable: &[Tool],
) -> Vec<Tool> {
    let allowed = tools.clone().native_and(extra);
    let mut locked = Vec::new();
    for option in options(paths, &allowed) {
        if option.missing.is_empty() || option.missing.contains(&Tool::Opus) {
            continue;
        }
        if option.missing.iter().all(|tool| installable.contains(tool)) {
            for tool in option.missing {
                if !locked.contains(&tool) {
                    locked.push(tool);
                }
            }
        }
    }
    locked
}

pub fn route(source: &Path, target: &Format, tools: &Tools) -> Option<Backend> {
    let input = extension(source);
    let src = input.as_str();
    let dst = target.extension;
    let images = ["png", "jpg", "webp", "bmp", "tiff", "gif", "ico"];
    let all_images = [
        "png", "jpg", "webp", "bmp", "tiff", "gif", "ico", "avif", "heic", "heif", "jxl", "svg",
        "svgz", "psd", "psb", "tga", "dds", "ppm", "pgm", "pbm", "exr", "hdr", "cr2", "cr3", "nef",
        "arw", "dng", "orf", "raf", "rw2", "xcf", "jp2", "j2k", "pcx",
    ];
    let audio = [
        "mp3", "wav", "flac", "m4a", "aac", "ogg", "opus", "aiff", "aifc", "au", "wma", "amr",
        "ape", "ac3", "alac", "wv",
    ];
    let video = [
        "mp4", "webm", "mkv", "mov", "avi", "wmv", "flv", "mpeg", "mpg", "m4v", "ts", "m2ts",
        "3gp", "ogv", "vob",
    ];
    let documents = [
        "doc", "docx", "docm", "dotx", "odt", "rtf", "txt", "html", "md", "epub", "rst", "tex",
        "org", "fb2",
    ];
    let office_docs = ["doc", "docx", "docm", "dotx", "odt", "rtf", "txt", "html"];
    let sheets = ["xlsx", "xlsm", "xls", "xlsb", "ods", "csv", "tsv"];
    let slides = ["ppt", "pptx", "pptm", "ppsx", "odp"];
    let archives = [
        "zip", "7z", "rar", "tar", "tar.gz", "tar.bz2", "tar.xz", "gz", "bz2", "xz", "cab", "iso",
    ];
    let external = |t| Some(Backend::External(t));
    if dst == "opus"
        && (["wav", "aiff", "aifc", "au"].contains(&src) || tools.1.supports(src, "wav"))
        && (tools.has(Tool::Opus) || !tools.has(Tool::Ffmpeg))
    {
        return external(Tool::Opus);
    }
    if tools.1.h264 && ["mov", "m4v", "avi", "wmv", "3gp"].contains(&src) && dst == "mp4" {
        return Some(Backend::SystemVideo);
    }
    if ["wav", "aiff", "aifc", "au"].contains(&src) && dst == "flac" {
        return Some(Backend::Flac);
    }
    if ["wav", "aiff", "aifc", "au"].contains(&src) && ["wav", "aiff", "au"].contains(&dst) {
        return Some(Backend::Pcm);
    }
    if ["svg", "svgz"].contains(&src)
        && (images.contains(&dst) || ["pdf", "svg", "svgz"].contains(&dst))
    {
        return Some(Backend::Vector);
    }
    if (src == "vcf" && ["csv", "tsv", "json"].contains(&dst))
        || (["csv", "tsv", "json"].contains(&src) && dst == "vcf")
    {
        return Some(Backend::Contacts);
    }
    if tools.1.supports(src, dst) {
        return Some(Backend::SystemAudio);
    }
    if (["epub", "fb2"].contains(&src)
        && ["epub", "fb2", "txt", "md", "html", "docx"].contains(&dst))
        || (["txt", "md", "docx", "docm", "dotx"].contains(&src) && ["epub", "fb2"].contains(&dst))
    {
        return Some(Backend::Book);
    }
    if images.contains(&src) && (images.contains(&dst) || dst == "pdf") {
        // Native still-image conversion is tiny; animated/multi-page inputs go
        // through ImageMagick so frames/pages aren't silently discarded.
        if matches!(src, "gif" | "tiff") {
            return external(Tool::Magick);
        }
        return Some(Backend::Image);
    }
    if all_images.contains(&src) && (all_images.contains(&dst) || dst == "pdf") {
        return external(Tool::Magick);
    }
    if (audio.contains(&src) && audio.contains(&dst))
        || (video.contains(&src) && (video.contains(&dst) || audio.contains(&dst) || dst == "gif"))
    {
        return external(Tool::Ffmpeg);
    }
    if ["srt", "vtt", "ass", "ssa", "smi", "lrc", "ttml"].contains(&src)
        && ["srt", "vtt", "ass", "lrc", "smi", "ttml", "txt"].contains(&dst)
    {
        return Some(Backend::Subtitles);
    }
    if ["json", "jsonl", "toml", "csv", "tsv"].contains(&src)
        && ["json", "jsonl", "toml", "csv", "tsv"].contains(&dst)
    {
        return Some(Backend::Data);
    }
    if ["xlsx", "xlsm", "xls", "xlsb", "ods"].contains(&src)
        && ["csv", "tsv", "json"].contains(&dst)
    {
        return Some(Backend::Sheet);
    }
    if src == "md" && ["html", "txt"].contains(&dst) {
        return Some(Backend::Markdown);
    }
    if ["docm", "dotx"].contains(&src) && dst == "docx" {
        return Some(Backend::OfficePackage);
    }
    if ["txt", "docx", "docm", "dotx"].contains(&src)
        && ["txt", "md", "html", "docx"].contains(&dst)
    {
        return Some(Backend::Text);
    }
    if src == "pdf" && ["docx", "txt", "md", "html"].contains(&dst) {
        #[cfg(windows)]
        return tools.1.ocr.then_some(Backend::SystemPdfText);
        #[cfg(not(windows))]
        return external(Tool::PdfText);
    }
    if src == "pdf" && ["png", "jpg"].contains(&dst) {
        return Some(Backend::PdfPages);
    }
    if src == "pdf" && dst == "tiff" {
        return external(Tool::Magick);
    }
    if sheets.contains(&src) && ["xlsx", "ods", "pdf", "html", "csv", "tsv"].contains(&dst) {
        return external(Tool::Office);
    }
    if slides.contains(&src) && ["pptx", "odp", "pdf"].contains(&dst) {
        return external(Tool::Office);
    }
    if ["mobi", "azw", "azw3", "fb2", "epub"].contains(&src)
        && ["mobi", "azw3", "fb2", "epub", "pdf", "docx", "txt"].contains(&dst)
    {
        if tools.has(Tool::Calibre)
            || !["epub", "fb2"].contains(&src)
            || ["mobi", "azw3", "pdf"].contains(&dst)
        {
            return external(Tool::Calibre);
        }
    }
    if documents.contains(&src)
        && ["pdf", "docx", "odt", "rtf", "txt", "html", "md", "epub"].contains(&dst)
    {
        // Pandoc cannot read legacy DOC; those files stay on Office's exports.
        if src == "doc" && !["pdf", "docx", "odt", "rtf", "txt", "html"].contains(&dst) {
            return None;
        }
        if dst == "pdf" && src == "md" {
            return external(Tool::Office);
        }
        if dst == "pdf" && !office_docs.contains(&src) {
            return None;
        }
        if office_docs.contains(&src)
            && ["pdf", "docx", "odt", "rtf", "txt", "html"].contains(&dst)
            && (tools.has(Tool::Office) || src == "doc" || dst == "pdf")
        {
            return external(Tool::Office);
        }
        return external(Tool::Pandoc);
    }
    if archives.contains(&src) && ["zip", "7z", "tar", "tar.gz"].contains(&dst) {
        if ["zip", "tar", "tar.gz", "tar.bz2", "tar.xz"].contains(&src) && dst != "7z" {
            return Some(Backend::Archive);
        }
        return external(Tool::SevenZip);
    }
    None
}
