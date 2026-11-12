use super::{Command as Conversion, Plan, Tool};
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub(super) fn run(command: Command, cancel: &AtomicBool) -> Result<()> {
    run_captured(command, cancel, None)
}

fn run_captured(command: Command, cancel: &AtomicBool, capture: Option<&File>) -> Result<()> {
    run_stdio(command, cancel, None, capture)
}

fn run_stdio(
    mut command: Command,
    cancel: &AtomicBool,
    input: Option<&File>,
    capture: Option<&File>,
) -> Result<()> {
    let stderr = tempfile::tempfile()?;
    command
        .stdin(match input {
            Some(file) => Stdio::from(file.try_clone()?),
            None => Stdio::null(),
        })
        .stdout(match capture {
            Some(file) => Stdio::from(file.try_clone()?),
            None => Stdio::null(),
        })
        .stderr(stderr.try_clone()?);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(crate::resources::tool_creation_flags(0x0800_0000)); // CREATE_NO_WINDOW
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("Cancelled");
    }
    let mut child = command
        .spawn()
        .context("Could not start conversion engine")?;
    crate::resources::contain_child(&child);
    let started = Instant::now();
    let status = loop {
        if cancel.load(Ordering::Relaxed) || started.elapsed() > Duration::from_secs(3600) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Conversion cancelled or exceeded the one-hour limit");
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(60)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
    };
    if !status.success() {
        // stderr goes to a file, never a pipe that could deadlock a busy codec.
        // Keep only its tail in the diagnostic channel.
        let mut stderr = stderr;
        let length = stderr.metadata()?.len();
        stderr.seek(SeekFrom::Start(length.saturating_sub(4096)))?;
        let mut tail = Vec::new();
        stderr.take(4096).read_to_end(&mut tail)?;
        bail!("{}: {}", status, String::from_utf8_lossy(&tail).trim());
    }
    Ok(())
}

pub(super) fn convert(
    tool: Tool,
    conversion: &Conversion,
    plan: &Plan,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut command = Command::new(
        plan.tools
            .get(tool)
            .context("Conversion engine is no longer available")?,
    );
    let source = &conversion.source;
    let target = conversion.target.extension;
    match tool {
        Tool::Opus => opus(command, conversion, plan, output, cancel),
        Tool::Ffmpeg => {
            command
                .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
                .arg(source);
            match target {
                "mp3" => {
                    command.args(["-vn", "-c:a", "libmp3lame", "-q:a", "2"]);
                }
                "m4a" | "aac" => {
                    command.args(["-vn", "-c:a", "aac", "-b:a", "192k"]);
                }
                "wav" => {
                    command.args(["-vn", "-c:a", "pcm_s16le"]);
                }
                "aiff" => {
                    command.args(["-vn", "-c:a", "pcm_s16be"]);
                }
                "flac" => {
                    command.args(["-vn", "-c:a", "flac"]);
                }
                "ogg" => {
                    command.args(["-vn", "-c:a", "libvorbis", "-q:a", "5"]);
                }
                "opus" => {
                    command.args(["-vn", "-c:a", "libopus", "-b:a", "128k"]);
                }
                "webm" => {
                    command.args([
                        "-c:v",
                        "libvpx-vp9",
                        "-crf",
                        "32",
                        "-b:v",
                        "0",
                        "-c:a",
                        "libopus",
                    ]);
                }
                "mp4" | "mov" | "mkv" => {
                    command.args([
                        "-c:v", "libx264", "-crf", "23", "-preset", "medium", "-pix_fmt",
                        "yuv420p", "-c:a", "aac",
                    ]);
                    if target != "mkv" {
                        command.args(["-movflags", "+faststart"]);
                    }
                }
                "avi" => {
                    command.args(["-c:v", "mpeg4", "-q:v", "4", "-c:a", "libmp3lame"]);
                }
                "gif" => {
                    command.args(["-vf", "fps=15,scale=640:-1:flags=lanczos", "-loop", "0"]);
                }
                "srt" | "vtt" | "ass" => {
                    command.args(["-map", "0:s:0"]);
                }
                _ => bail!("Unsupported FFmpeg output"),
            }
            command.arg(output);
            run(command, cancel)
        }
        Tool::Magick => {
            command.args(["-limit", "memory", "256MiB", "-limit", "map", "512MiB"]);
            // Multipage TIFF/PDF outputs preserve all pages. Single-image
            // formats intentionally take page/frame 1, disclosed in the UI.
            if !["gif", "tiff", "pdf"].contains(&target) {
                let mut input = source.as_os_str().to_os_string();
                input.push("[0]");
                command.arg(input);
            } else {
                command.arg(source);
            }
            command.arg("-auto-orient");
            if let Some(edge) = plan.settings.max_edge {
                command.args(["-resize", &format!("{edge}x{edge}>")]);
            }
            if target == "jpg" {
                command.args(["-background", "white", "-alpha", "remove", "-alpha", "off"]);
            }
            if target == "ico" {
                command.args(["-resize", "256x256>"]);
            }
            command
                .args(["-quality", &plan.settings.quality.clamp(1, 100).to_string()])
                .arg(output);
            run(command, cancel)
        }
        Tool::Pandoc => {
            // PDF deliberately goes through LibreOffice, with a dedicated
            // disposable profile, so no LaTeX runtime is implied or bundled.
            let reader = match super::catalog::extension(source).as_str() {
                "txt" => "markdown",
                "md" => "markdown",
                "html" => "html",
                "rst" => "rst",
                "tex" => "latex",
                "org" => "org",
                "docx" | "docm" | "dotx" => "docx",
                "odt" => "odt",
                "rtf" => "rtf",
                "epub" => "epub",
                "fb2" => "fb2",
                _ => bail!("Pandoc cannot read this document"),
            };
            let writer = match target {
                "md" => "gfm",
                "txt" => "plain",
                "html" => "html5",
                other => other,
            };
            command
                .args([
                    "--sandbox",
                    "--standalone",
                    "--from",
                    reader,
                    "--to",
                    writer,
                    "--output",
                ])
                .arg(output)
                .arg("--")
                .arg(source);
            run(command, cancel)
        }
        Tool::Office => office(command, conversion, output, cancel),
        Tool::PdfText => {
            let stage = tempfile::tempdir()?;
            let extracted = stage.path().join("extracted.txt");
            command
                .args(["-layout", "-enc", "UTF-8"])
                .arg(source)
                .arg(&extracted);
            run(command, cancel)?;
            let text = std::fs::read_to_string(&extracted)?;
            if text.trim().is_empty() {
                bail!("This PDF has no extractable text; OCR is required");
            }
            super::native::extracted_document(&text, target, output)
        }
        Tool::Calibre => {
            command.arg(source).arg(output);
            run(command, cancel)
        }
        Tool::SevenZip => seven_zip(conversion, plan, output, cancel),
        Tool::Ghostscript => bail!("Ghostscript is not a conversion engine"),
    }
}

fn opus(
    mut command: Command,
    conversion: &Conversion,
    plan: &Plan,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    anyhow::ensure!(
        conversion.target.extension == "opus",
        "Unsupported Opus output"
    );
    // Feed the small encoder a canonical WAV, with no untrusted metadata or
    // compressed chunks. PCM stays handmade; other input uses Windows decoding.
    let wave = tempfile::Builder::new()
        .prefix(".atlas-opus-")
        .suffix(".wav")
        .tempfile_in(output.parent().context("Missing Opus staging directory")?)?
        .into_temp_path();
    let input = Conversion {
        source: conversion.source.clone(),
        target: super::FORMATS
            .iter()
            .find(|format| format.extension == "wav")
            .context("Missing WAV format")?,
        backend: super::Backend::Pcm,
    };
    if let Err(error) = super::pcm::convert(&input, &wave, cancel) {
        anyhow::ensure!(!cancel.load(Ordering::Relaxed), "Cancelled");
        if !plan.tools.1.wave {
            return Err(error);
        }
        #[cfg(target_os = "windows")]
        atlas_win_space::media::convert_audio(
            &input.source,
            &wave,
            atlas_win_space::media::AudioTarget::Wave,
            plan.settings.quality,
            cancel,
        )
        .map_err(anyhow::Error::msg)?;
        #[cfg(not(target_os = "windows"))]
        return Err(error);
    }
    let (_, pcm) = super::pcm::open(&wave)?;
    anyhow::ensure!(
        pcm.channels <= 8,
        "Opus conversion accepts up to eight channels"
    );
    let bitrate = if plan.settings.quality >= 95 {
        224
    } else if plan.settings.quality >= 85 {
        160
    } else {
        96
    };
    let bitrate = bitrate * u32::from(pcm.channels) / 2;
    command
        .args([
            "--quiet",
            "--vbr",
            "--comp",
            "8",
            "--discard-comments",
            "--discard-pictures",
            "--bitrate",
        ])
        .arg(bitrate.to_string())
        .args(["-", "-"]);
    // Handles keep Unicode paths out of older engine argument parsers and avoid
    // buffering an audio stream in RAM. Both files stay owned by this worker.
    run_stdio(
        command,
        cancel,
        Some(&File::open(&wave)?),
        Some(&File::create(output)?),
    )
}

fn seven_zip(
    conversion: &Conversion,
    plan: &Plan,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let executable = plan
        .tools
        .get(Tool::SevenZip)
        .context("7-Zip is unavailable")?;
    let stage = tempfile::Builder::new().prefix("atlas-repack-").tempdir()?;
    let mut source = conversion.source.clone();
    let extension = super::catalog::extension(&source);
    if ["tar.gz", "tar.bz2", "tar.xz"].contains(&extension.as_str()) {
        let file = File::open(&source)?;
        let mut reader: Box<dyn Read> = match extension.as_str() {
            "tar.gz" => Box::new(flate2::read::GzDecoder::new(file)),
            "tar.bz2" => Box::new(bzip2::read::BzDecoder::new(file)),
            _ => Box::new(xz2::read::XzDecoder::new(file)),
        };
        source = stage.path().join("input.tar");
        let mut dest = File::create(&source)?;
        copy_cancelled(&mut reader, &mut dest, cancel)?;
    }
    let mut listing = tempfile::tempfile()?;
    let mut list = Command::new(executable);
    list.args(["l", "-slt", "-ba", "-sccUTF-8"]).arg(&source);
    run_captured(list, cancel, Some(&listing))?;
    if listing.metadata()?.len() > 32 * 1024 * 1024 {
        bail!("Archive directory is too large");
    }
    listing.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    listing.read_to_string(&mut text)?;
    let mut names = std::collections::BTreeSet::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("Path = ") {
            let normalized = name.replace('\\', "/");
            if !super::native::safe_member(Path::new(&normalized))
                || !names.insert(normalized.to_lowercase())
            {
                bail!("Unsafe or duplicate archive path: {name}");
            }
        }
        if line.starts_with("Symbolic Link = ")
            || line.starts_with("Hard Link = ")
            || line
                .strip_prefix("Attributes = ")
                .is_some_and(|s| s.split_whitespace().any(|a| a.starts_with('l')))
        {
            bail!("Links are not supported during archive conversion");
        }
    }
    if names.is_empty() {
        bail!("Archive is empty or its directory could not be read");
    }
    let contents = stage.path().join("contents");
    std::fs::create_dir(&contents)?;
    let mut extract = Command::new(executable);
    extract
        .args(["x", "-y", "-bd", "-p"])
        .arg(format!("-o{}", contents.display()))
        .arg(&source);
    run(extract, cancel)?;
    for entry in walkdir::WalkDir::new(&contents).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink() {
            bail!("Archive contains a symbolic link");
        }
        if !entry.file_type().is_file() && !entry.file_type().is_dir() {
            bail!("Archive contains a special file");
        }
    }
    // 7-Zip requires a nonexistent destination, isolated from the user's files.
    let target = conversion.target.extension;
    let packed = stage.path().join(if target == "tar.gz" {
        "output.tar"
    } else {
        "output.archive"
    });
    let mut pack = Command::new(executable);
    pack.current_dir(&contents)
        .args([
            "a",
            "-y",
            "-bd",
            &format!("-t{}", if target == "tar.gz" { "tar" } else { target }),
        ])
        .arg(&packed)
        .arg(".");
    run(pack, cancel)?;
    if target == "tar.gz" {
        let mut reader = File::open(packed)?;
        let mut encoder =
            flate2::write::GzEncoder::new(File::create(output)?, flate2::Compression::default());
        copy_cancelled(&mut reader, &mut encoder, cancel)?;
        encoder.finish()?;
    } else {
        std::fs::copy(packed, output)?;
    }
    Ok(())
}

fn copy_cancelled(
    reader: &mut dyn Read,
    writer: &mut dyn std::io::Write,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("Cancelled");
        }
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(());
        }
        writer.write_all(&buffer[..count])?;
    }
}

fn office(
    mut command: Command,
    conversion: &Conversion,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let stage = tempfile::Builder::new().prefix("atlas-office-").tempdir()?;
    let profile = stage.path().join("profile");
    let profile_url = url::Url::from_directory_path(&profile)
        .map_err(|()| anyhow::anyhow!("Invalid Office profile path"))?;
    let extension = conversion.target.extension;
    let source = if super::catalog::extension(&conversion.source) == "md" {
        let html = stage.path().join("markdown-source.html");
        let intermediate = Conversion {
            source: conversion.source.clone(),
            target: super::FORMATS
                .iter()
                .find(|f| f.extension == "html")
                .context("Missing HTML format")?,
            backend: super::Backend::Markdown,
        };
        super::native::markdown(&intermediate, &html)?;
        html
    } else {
        conversion.source.clone()
    };
    let format = match extension {
        "docx" => "docx:Office Open XML Text",
        "odt" => "odt:writer8",
        "rtf" => "rtf:Rich Text Format",
        "txt" => "txt:Text (encoded):UTF8",
        "xlsx" => "xlsx:Calc MS Excel 2007 XML",
        "ods" => "ods:calc8",
        "csv" => "csv:Text - txt - csv (StarCalc):44,34,76,1",
        "tsv" => "csv:Text - txt - csv (StarCalc):9,34,76,1",
        "pptx" => "pptx:Impress MS PowerPoint 2007 XML",
        "odp" => "odp:impress8",
        "pdf" => "pdf",
        "html" => "html",
        _ => bail!("Unsupported Office output"),
    };
    command.arg(format!("-env:UserInstallation={profile_url}"));
    command
        .args([
            "--headless",
            "--norestore",
            "--nodefault",
            "--nofirststartwizard",
            "--convert-to",
            format,
            "--outdir",
        ])
        .arg(stage.path())
        .arg(&source);
    run(command, cancel)?;
    let generated_ext = if extension == "tsv" { "csv" } else { extension };
    let mut filename = source
        .file_stem()
        .context("Missing source filename")?
        .to_os_string();
    filename.push(format!(".{generated_ext}"));
    let generated = stage.path().join(filename);
    if !generated.is_file() {
        bail!(
            "LibreOffice could not export this file to {}",
            conversion.target.label
        );
    }
    std::fs::copy(generated, output)?;
    Ok(())
}
