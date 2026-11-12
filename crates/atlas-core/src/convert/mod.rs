//! Local, non-destructive conversion pipeline. Catalog -> immutable plan -> workers.
//! Engines are discovered off-thread; no engine or codec is bundled by this module.
mod books;
mod catalog;
mod contacts;
#[cfg(test)]
mod edition_tests;
pub mod actions;
mod flac;
mod native;
mod office;
#[cfg(all(test, target_os = "windows"))]
mod page_tests;
mod pages;
mod pcm;
mod process;
mod subtitles;
mod tools;
mod vector;
#[cfg(all(test, target_os = "windows"))]
mod video_tests;

pub use catalog::{locked_engines, native_options, options, route, wheel_options, Backend, ConversionOption, Format, FORMATS};
pub use tools::{Tool, Tools};

use anyhow::{bail, Context, Result};
use atlas_model::edition::{self, Feature};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

#[derive(Clone, Debug)]
pub struct Settings {
    pub quality: u8,
    /// Maximum image edge; None preserves the original dimensions.
    pub max_edge: Option<u32>,
    /// None saves beside each source.
    pub output_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            quality: 90,
            max_edge: None,
            output_dir: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Command {
    pub source: PathBuf,
    pub target: &'static Format,
    pub backend: Backend,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub commands: Vec<Command>,
    pub settings: Settings,
    pub tools: Tools,
}

impl Plan {
    pub fn build_native(
        paths: &[PathBuf],
        extension: &str,
        settings: Settings,
        tools: Tools,
    ) -> Result<Self> {
        Self::build_wheel(paths, extension, settings, tools, &[])
    }
    /// Wheel plans: native routes, Opus and the provider-installed `extra` engines.
    pub fn build_wheel(
        paths: &[PathBuf],
        extension: &str,
        settings: Settings,
        tools: Tools,
        extra: &[Tool],
    ) -> Result<Self> {
        edition::current().check(Feature::BatchConversion, paths.len())?;
        let tools = tools.native_and(extra);
        let target = FORMATS
            .iter()
            .find(|format| format.extension == extension)
            .context("Unknown output format")?;
        for source in paths {
            if catalog::extension(source) != extension
                && catalog::route(source, target, &tools).is_some_and(|backend| {
                    backend
                        .required_tools()
                        .iter()
                        .any(|tool| *tool != Tool::Opus && !extra.contains(tool))
                })
            {
                bail!("This route is not implemented by Atlas or Windows; only the Opus encoder is enabled");
            }
        }
        Self::build(paths, extension, settings, tools)
    }
    pub fn build(
        paths: &[PathBuf],
        extension: &str,
        settings: Settings,
        tools: Tools,
    ) -> Result<Self> {
        edition::current().check(Feature::BatchConversion, paths.len())?;
        let target = FORMATS
            .iter()
            .find(|f| f.extension == extension)
            .context("Unknown output format")?;
        if paths.is_empty() {
            bail!("Select at least one file");
        }
        let mut commands = Vec::with_capacity(paths.len());
        for source in paths {
            if !source.is_file() {
                bail!("Not a local file: {}", source.display());
            }
            if catalog::extension(source) == target.extension {
                continue;
            }
            let backend =
                catalog::route(source, target, &tools).context("Incompatible conversion")?;
            if !backend.available(&tools) {
                bail!("Install {} to convert this file", backend.name());
            }
            commands.push(Command {
                source: std::path::absolute(source)?,
                target,
                backend,
            });
        }
        if commands.is_empty() {
            bail!("These files already use the selected format");
        }
        if settings.output_dir.as_ref().is_some_and(|p| !p.is_dir()) {
            bail!("Output folder does not exist");
        }
        Ok(Self {
            commands,
            settings,
            tools,
        })
    }
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Created { source: PathBuf, output: PathBuf },
    Failed { source: PathBuf, error: String },
    Cancelled { source: PathBuf },
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
}

impl Report {
    pub fn outputs(&self) -> Vec<PathBuf> {
        self.outcomes
            .iter()
            .filter_map(|r| {
                if let Outcome::Created { output, .. } = r {
                    Some(output.clone())
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn errors(&self) -> Vec<String> {
        self.outcomes
            .iter()
            .filter_map(|r| {
                if let Outcome::Failed { source, error } = r {
                    Some(format!("{}: {error}", source.display()))
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn cancelled(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|r| matches!(r, Outcome::Cancelled { .. }))
            .count()
    }
}

/// Two isolated lanes cap simultaneous decoders/processes and peak memory. Results
/// retain input order; only the monotonic completion counter is shared by workers.
pub fn execute(
    plan: &Plan,
    cancel: &Arc<AtomicBool>,
    progress: impl Fn(usize, usize, &Path) + Sync,
) -> Report {
    // Plans are public and can be constructed without the builders. Enforce the
    // edition before starting any worker or creating any output file.
    let edition = edition::current();
    let free = edition == edition::Edition::Free;
    let planned = plan.commands.len();
    let used_today = if free { crate::usage::conversions_today() } else { 0 };
    let allowed = edition
        .check(Feature::BatchConversion, planned)
        .and_then(|()| edition.check(Feature::ConversionsPerDay, used_today + planned));
    if let Err(limit) = allowed {
        return Report {
            outcomes: plan
                .commands
                .iter()
                .enumerate()
                .map(|(index, command)| {
                    progress(index + 1, plan.commands.len(), &command.source);
                    Outcome::Failed {
                        source: command.source.clone(),
                        error: limit.to_string(),
                    }
                })
                .collect(),
        };
    }
    let done = AtomicUsize::new(0);
    let total = plan.commands.len();
    let run = |command: &Command| {
        let outcome = if cancel.load(Ordering::Relaxed) {
            Outcome::Cancelled {
                source: command.source.clone(),
            }
        } else {
            match execute_one(command, plan, cancel) {
                Ok(output) => Outcome::Created {
                    source: command.source.clone(),
                    output,
                },
                Err(_) if cancel.load(Ordering::Relaxed) => Outcome::Cancelled {
                    source: command.source.clone(),
                },
                Err(error) => Outcome::Failed {
                    source: command.source.clone(),
                    error: format!("{error:#}"),
                },
            }
        };
        progress(
            done.fetch_add(1, Ordering::Relaxed) + 1,
            total,
            &command.source,
        );
        outcome
    };
    let outcomes: Vec<Outcome> = match rayon::ThreadPoolBuilder::new()
        .num_threads(crate::resources::scaled(2))
        .thread_name(|n| format!("atlas-convert-{n}"))
        .start_handler(|_| crate::resources::lower_current_thread())
        .build()
    {
        Ok(pool) => pool.install(|| plan.commands.par_iter().map(run).collect()),
        Err(_) => plan.commands.iter().map(run).collect(),
    };
    if free {
        let created = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Outcome::Created { .. }))
            .count();
        crate::usage::record_conversions(created);
    }
    Report { outcomes }
}

fn execute_one(command: &Command, plan: &Plan, cancel: &AtomicBool) -> Result<PathBuf> {
    let parent = plan
        .settings
        .output_dir
        .as_deref()
        .or_else(|| command.source.parent())
        .context("Missing output directory")?;
    if command.backend == Backend::PdfPages {
        return pages::execute(command, plan, parent, cancel);
    }
    // Keep a path guard rather than an open file handle: system codecs can
    // require exclusive access to their destination. The guard still removes
    // every failed/cancelled temporary and publishes with noclobber atomically.
    let output = tempfile::Builder::new()
        .prefix(".atlas-convert-")
        .suffix(&format!(".{}", command.target.extension))
        .tempfile_in(parent)?
        .into_temp_path();
    match command.backend {
        Backend::Image => native::image(command, &plan.settings, &output)?,
        Backend::Data => native::data(command, &output)?,
        Backend::Sheet => native::sheet(command, &output)?,
        Backend::Markdown => native::markdown(command, &output)?,
        Backend::Text => native::text(command, &output)?,
        Backend::OfficePackage => office::normalize(command, &output, cancel)?,
        Backend::Archive => native::archive(command, &output, cancel)?,
        Backend::Subtitles => subtitles::convert(command, &output)?,
        Backend::Pcm => pcm::convert(command, &output, cancel)?,
        Backend::Flac => flac::convert(command, &output, cancel)?,
        Backend::Vector => vector::convert(command, &plan.settings, &output)?,
        Backend::Contacts => contacts::convert(command, &output)?,
        Backend::Book => books::convert(command, &output)?,
        Backend::SystemPdfText => {
            #[cfg(windows)]
            {
                let stage = tempfile::Builder::new()
                    .prefix(".atlas-ocr-")
                    .tempdir_in(parent)?;
                let text =
                    atlas_win_space::pdf::recognize_pages(&command.source, stage.path(), cancel)
                        .map_err(anyhow::Error::msg)?;
                native::extracted_document(&text, command.target.extension, &output)?;
            }
            #[cfg(not(windows))]
            bail!("Windows OCR is unavailable on this platform");
        }
        Backend::SystemVideo => {
            #[cfg(target_os = "windows")]
            atlas_win_space::media::convert_video(
                &command.source,
                &output,
                plan.settings.quality,
                plan.settings.max_edge,
                cancel,
            )
            .map_err(anyhow::Error::msg)?;
            #[cfg(not(target_os = "windows"))]
            bail!("Windows video conversion is unavailable on this platform");
        }
        Backend::SystemAudio => {
            #[cfg(target_os = "windows")]
            {
                use atlas_win_space::media::{convert_audio, AudioTarget};
                let target = match command.target.extension {
                    "wav" | "aiff" | "au" | "flac" => AudioTarget::Wave,
                    "mp3" => AudioTarget::Mp3,
                    "aac" => AudioTarget::Aac,
                    "m4a" => AudioTarget::M4a,
                    _ => bail!("Unsupported Windows audio target"),
                };
                let bridge;
                let source = if ["aiff", "aifc", "au"]
                    .contains(&catalog::extension(&command.source).as_str())
                {
                    bridge = tempfile::Builder::new()
                        .prefix(".atlas-pcm-")
                        .suffix(".wav")
                        .tempfile_in(parent)?
                        .into_temp_path();
                    let pcm = Command {
                        source: command.source.clone(),
                        target: FORMATS
                            .iter()
                            .find(|f| f.extension == "wav")
                            .context("Missing WAV format")?,
                        backend: Backend::Pcm,
                    };
                    pcm::convert(&pcm, &bridge, cancel)?;
                    bridge.as_ref()
                } else {
                    command.source.as_path()
                };
                if ["aiff", "au", "flac"].contains(&command.target.extension) {
                    let decoded = tempfile::Builder::new()
                        .prefix(".atlas-decoded-")
                        .suffix(".wav")
                        .tempfile_in(parent)?
                        .into_temp_path();
                    convert_audio(source, &decoded, target, plan.settings.quality, cancel)
                        .map_err(anyhow::Error::msg)?;
                    let native = Command {
                        source: decoded.to_path_buf(),
                        target: command.target,
                        backend: if command.target.extension == "flac" {
                            Backend::Flac
                        } else {
                            Backend::Pcm
                        },
                    };
                    if native.backend == Backend::Flac {
                        flac::convert(&native, &output, cancel)?;
                    } else {
                        pcm::convert(&native, &output, cancel)?;
                    }
                } else {
                    convert_audio(source, &output, target, plan.settings.quality, cancel)
                        .map_err(anyhow::Error::msg)?;
                }
            }
            #[cfg(not(target_os = "windows"))]
            bail!("Windows audio conversion is unavailable on this platform");
        }
        Backend::External(tool) => process::convert(tool, command, plan, &output, cancel)?,
        Backend::PdfPages => unreachable!("Page export uses a directory"),
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }
    publish(output, |index| {
        output_name(&command.source, parent, command.target.extension, index)
    })
}

/// Flush a finished temporary and claim the first free `name(index)`.
pub(crate) fn publish(
    mut output: tempfile::TempPath,
    name: impl Fn(usize) -> PathBuf,
) -> Result<PathBuf> {
    if std::fs::metadata(&output)?.len() == 0 {
        bail!("The engine produced an empty file");
    }
    std::fs::File::options()
        .read(true)
        .write(true)
        .open(&output)?
        .sync_all()?;
    // persist_noclobber atomically claims a destination, including races with
    // other workers/applications. Never truncate an existing file or a symlink.
    for index in 0..10_000 {
        let destination = name(index);
        match output.persist_noclobber(&destination) {
            Ok(()) => return Ok(destination),
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                output = error.path
            }
            Err(error) => return Err(error.error.into()),
        }
    }
    bail!("Too many files with the same output name")
}

fn output_name(source: &Path, parent: &Path, extension: &str, index: usize) -> PathBuf {
    let mut stem = source.file_stem().unwrap_or_default().to_os_string();
    if catalog::extension(source).starts_with("tar.") {
        stem = Path::new(&stem)
            .file_stem()
            .unwrap_or_default()
            .to_os_string();
    }
    if index > 0 {
        stem.push(format!(" ({index})"));
    }
    stem.push(format!(".{extension}"));
    parent.join(stem)
}

#[cfg(test)]
mod tests;
