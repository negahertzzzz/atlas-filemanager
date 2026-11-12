use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tool {
    Magick,
    Ffmpeg,
    Opus,
    Pandoc,
    Office,
    PdfText,
    SevenZip,
    Calibre,
    /// Ghostscript: never a conversion route, only the optional "Compress PDF" action.
    Ghostscript,
}

impl Tool {
    pub const ALL: [Self; 9] = [
        Self::Magick,
        Self::Ffmpeg,
        Self::Opus,
        Self::Pandoc,
        Self::Office,
        Self::PdfText,
        Self::SevenZip,
        Self::Calibre,
        Self::Ghostscript,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Magick => "ImageMagick",
            Self::Ffmpeg => "FFmpeg",
            Self::Opus => "Opus",
            Self::Pandoc => "Pandoc",
            Self::Office => "LibreOffice",
            Self::PdfText => "Poppler",
            Self::SevenZip => "7-Zip",
            Self::Calibre => "Calibre",
            Self::Ghostscript => "Ghostscript",
        }
    }
    pub const fn executable(self) -> &'static str {
        match self {
            Self::Magick => "magick",
            Self::Ffmpeg => "ffmpeg",
            Self::Opus => "opusenc",
            Self::Pandoc => "pandoc",
            Self::Office => "soffice",
            Self::PdfText => "pdftotext",
            Self::SevenZip => "7z",
            Self::Calibre => "ebook-convert",
            Self::Ghostscript if cfg!(windows) => "gswin64c",
            Self::Ghostscript => "gs",
        }
    }
    pub const fn url(self) -> &'static str {
        match self {
            Self::Magick => "https://imagemagick.org/script/download.php",
            Self::Ffmpeg => "https://ffmpeg.org/download.html",
            Self::Opus => "https://opus-codec.org/downloads/",
            Self::Pandoc => "https://pandoc.org/installing.html",
            Self::Office => "https://www.libreoffice.org/download/download-libreoffice/",
            Self::PdfText => "https://poppler.freedesktop.org/",
            Self::SevenZip => "https://www.7-zip.org/",
            Self::Calibre => "https://calibre-ebook.com/download",
            Self::Ghostscript => "https://ghostscript.com/releases/gsdnld.html",
        }
    }
    /// Tool-provider manifests may use the portable name (`gs`) for Ghostscript.
    pub fn matches_name(self, name: &str) -> bool {
        name == self.executable() || (self == Self::Ghostscript && name == "gs")
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemAudio {
    pub wave: bool,
    pub mp3: bool,
    pub aac: bool,
    pub h264: bool,
    pub ocr: bool,
}

impl SystemAudio {
    pub fn supports(self, input: &str, output: &str) -> bool {
        self.wave
            && [
                "wav", "aiff", "aifc", "au", "mp3", "m4a", "aac", "wma", "flac", "mp4", "mov",
                "m4v",
            ]
            .contains(&input)
            && match output {
                "wav" | "aiff" | "au" | "flac" => true,
                "mp3" => self.mp3,
                "m4a" | "aac" => self.aac,
                _ => false,
            }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Tools(pub BTreeMap<Tool, PathBuf>, pub SystemAudio);

impl Tools {
    /// Conversion wheel policy: Atlas, Windows and the small Opus encoder only.
    /// Discovery for previews and independent validation can still see other tools.
    pub fn native_and_opus(self) -> Self {
        self.native_and(&[])
    }
    /// Wheel policy plus `extra` engines (those installed by enabled tool providers).
    pub fn native_and(mut self, extra: &[Tool]) -> Self {
        self.0
            .retain(|tool, _| *tool == Tool::Opus || extra.contains(tool));
        self
    }
    /// Tools whose discovered path is the one supplied by an extension package.
    pub fn managed_by<'a>(
        &self,
        managed: impl IntoIterator<Item = (&'a String, &'a PathBuf)>,
    ) -> Vec<Tool> {
        let mut found = Vec::new();
        for (name, path) in managed {
            for tool in Tool::ALL {
                if tool.matches_name(name)
                    && self.get(tool) == Some(path.as_path())
                    && !found.contains(&tool)
                {
                    found.push(tool);
                }
            }
        }
        found
    }
    pub fn has(&self, tool: Tool) -> bool {
        self.0.contains_key(&tool)
    }
    pub fn get(&self, tool: Tool) -> Option<&Path> {
        self.0.get(&tool).map(PathBuf::as_path)
    }

    /// Scan executable paths and query system codecs off the interactive lane.
    /// Never start an external process, download a payload or scan recursively.
    pub fn discover() -> Self {
        Self::discover_with(std::iter::empty())
    }

    /// Registry paths are an immutable snapshot of enabled tool providers.
    /// Validate them on this worker lane and prefer them over system installs.
    pub fn discover_with(managed: impl IntoIterator<Item = (String, PathBuf)>) -> Self {
        let mut directories: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| {
                std::env::split_paths(&p)
                    .filter(|p| p.is_absolute())
                    .collect()
            })
            .unwrap_or_default();
        for root in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(root).map(PathBuf::from) {
                for suffix in [
                    "LibreOffice/program",
                    "7-Zip",
                    "Calibre2",
                    "Pandoc",
                    "Programs/Pandoc",
                    "Microsoft/WinGet/Links",
                    "Atlas/tools",
                ] {
                    directories.push(base.join(suffix));
                }
                // Ghostscript installs into gs/gs<version>/bin.
                if let Ok(entries) = std::fs::read_dir(base.join("gs")) {
                    for entry in entries.flatten().take(64) {
                        directories.push(entry.path().join("bin"));
                    }
                }
                if let Ok(entries) = std::fs::read_dir(&base) {
                    for entry in entries.flatten().take(512) {
                        if entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with("ImageMagick-")
                        {
                            directories.push(entry.path());
                        }
                    }
                }
            }
        }
        #[cfg(target_os = "macos")]
        directories.extend([
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/Applications/LibreOffice.app/Contents/MacOS"),
            PathBuf::from("/Applications/calibre.app/Contents/MacOS"),
        ]);
        let mut tools = Self::default();
        #[cfg(target_os = "windows")]
        {
            let audio = atlas_win_space::media::audio_capabilities();
            tools.1 = SystemAudio {
                wave: audio.wave,
                mp3: audio.mp3,
                aac: audio.aac,
                h264: audio.h264,
                ocr: atlas_win_space::pdf::ocr_available(),
            };
        }
        for tool in Tool::ALL {
            let executable = format!("{}{}", tool.executable(), std::env::consts::EXE_SUFFIX);
            if let Some(path) = directories
                .iter()
                .map(|p| p.join(&executable))
                .find(|p| p.is_file())
            {
                tools.0.insert(tool, path);
            }
        }
        for (name, path) in managed {
            if let Some(tool) = Tool::ALL.into_iter().find(|tool| tool.matches_name(&name)) {
                if path.is_absolute() && path.is_file() {
                    tools.0.insert(tool, path);
                }
            }
        }
        tools
    }
}
