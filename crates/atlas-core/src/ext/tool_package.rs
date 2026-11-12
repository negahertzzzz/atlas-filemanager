//! Optional engines: bounded streaming download, verified staging and one rename.
//! Versioned package directories keep existing conversion plans stable on updates.
use anyhow::{bail, ensure, Context, Result};
use atlas_model::ext_model::ToolSpec;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const MAX_DOWNLOAD: u64 = 1024 * 1024 * 1024;
const MAX_EXPANDED: u64 = 2 * MAX_DOWNLOAD;
const MAX_ENTRIES: usize = 100_000;
const MARKER: &str = ".atlas-package-sha256";

#[derive(Clone, Copy, Debug)]
pub enum Progress {
    Download { bytes: u64, total: Option<u64> },
    Extracting,
}

fn check(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Cancelled");
    Ok(())
}

async fn cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub(crate) fn relative(value: &str) -> Result<PathBuf> {
    ensure!(
        !value.is_empty() && !value.contains('\\'),
        "Invalid package path"
    );
    let mut result = PathBuf::new();
    for part in value.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != "..",
            "Invalid package path"
        );
        ensure!(
            !part.ends_with(['.', ' '])
                && !part
                    .chars()
                    .any(|c| c.is_control() || ":<>\"|?*".contains(c)),
            "Invalid package filename"
        );
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        ensure!(
            ![
                "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
                "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
                "LPT9"
            ]
            .contains(&stem.as_str()),
            "Reserved package filename"
        );
        result.push(part);
    }
    Ok(result)
}

fn mkdir_inside(base: &Path, relative: &Path) -> Result<PathBuf> {
    let mut path = base.to_path_buf();
    for part in relative.components() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) => ensure!(
                meta.is_dir() && !meta.file_type().is_symlink(),
                "Package directory contains a link or file"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&path)?
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

/// A snapshot ToolSpec owns the exact URL, bytes and digest for this operation.
/// Cancellation drops the download and staging guards; publication is all-or-none.
pub async fn install(
    provider_dir: PathBuf,
    tool: ToolSpec,
    cancel: Arc<AtomicBool>,
    progress: impl Fn(Progress) + Send + Sync + 'static,
) -> Result<PathBuf> {
    check(&cancel)?;
    ensure!(
        tool.sha256.len() == 64 && tool.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "Package needs an exact SHA-256 digest"
    );
    ensure!(
        !tool.archive_root.is_empty(),
        "Package needs an explicit archive root"
    );
    ensure!(
        matches!(tool.archive_format.as_str(), "zip" | "tgz"),
        "Unsupported package archive"
    );
    ensure!(
        tool.download_bytes <= MAX_DOWNLOAD,
        "Package download is too large"
    );
    ensure!(
        tool.installed_bytes <= MAX_EXPANDED,
        "Expanded package is too large"
    );
    let url = reqwest::Url::parse(&tool.download_url)?;
    ensure!(url.scheme() == "https", "Package download requires HTTPS");
    let binary = relative(&tool.path)?;
    let package_dir = binary
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .context("Package requires its own versioned directory")?;
    let executable = binary.file_name().context("Missing package executable")?;
    let archive_binary = relative(&tool.archive_path)?;
    let archive_root = if tool.archive_root == "." {
        PathBuf::new()
    } else {
        relative(&tool.archive_root)?
    };
    ensure!(
        archive_binary.strip_prefix(&archive_root)? == Path::new(executable),
        "Package executable must be directly inside its declared root"
    );
    ensure!(
        tool.archive_files.len() <= MAX_ENTRIES,
        "Too many selected package files"
    );
    let selected: Vec<_> = tool
        .archive_files
        .iter()
        .map(|name| relative(name))
        .collect::<Result<_>>()?;
    ensure!(
        selected.is_empty() || selected.iter().any(|path| path == Path::new(executable)),
        "Selected package files omit the executable"
    );
    let base = provider_dir
        .canonicalize()
        .context("Tool provider no longer exists")?;
    let container = mkdir_inside(&base, package_dir.parent().unwrap_or(Path::new("")))?;
    let destination = base.join(package_dir);
    if destination.exists() {
        ensure!(
            !std::fs::symlink_metadata(&destination)?
                .file_type()
                .is_symlink(),
            "Package destination is a link"
        );
        if std::fs::read_to_string(destination.join(MARKER))
            .ok()
            .is_some_and(|hash| hash == tool.sha256.to_ascii_lowercase())
            && destination.join(executable).is_file()
        {
            return Ok(destination.join(executable));
        }
        bail!("Package directory already exists; use a new version or remove this package first");
    }
    let stage = tempfile::Builder::new()
        .prefix(".atlas-package-")
        .tempdir_in(&container)?;
    let download = stage.path().join("download");
    let payload = stage.path().join("payload");
    std::fs::create_dir(&payload)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .build()?;
    let mut response = tokio::select! {
        result = client.get(url).send() => result?.error_for_status()?,
        _ = cancelled(&cancel) => bail!("Cancelled"),
    };
    ensure!(
        response.url().scheme() == "https",
        "Package redirected to an insecure URL"
    );
    let length = response.content_length();
    ensure!(
        length.is_none_or(|bytes| bytes <= MAX_DOWNLOAD),
        "Package download is too large"
    );
    if tool.download_bytes != 0 {
        ensure!(
            length.is_none_or(|bytes| bytes == tool.download_bytes),
            "Package download size changed"
        );
    }
    let mut file = File::create(&download)?;
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut last = Instant::now();
    progress(Progress::Download {
        bytes,
        total: length.or((tool.download_bytes != 0).then_some(tool.download_bytes)),
    });
    let transfer = async {
        while let Some(chunk) = response.chunk().await? {
            check(&cancel)?;
            bytes += chunk.len() as u64;
            ensure!(
                bytes <= MAX_DOWNLOAD && (tool.download_bytes == 0 || bytes <= tool.download_bytes),
                "Package exceeds its download size"
            );
            file.write_all(&chunk)?;
            hash.update(&chunk);
            if last.elapsed() >= Duration::from_millis(200) {
                progress(Progress::Download {
                    bytes,
                    total: length,
                });
                last = Instant::now();
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    tokio::select! {
        result = transfer => result?,
        _ = cancelled(&cancel) => bail!("Cancelled"),
    }
    ensure!(
        tool.download_bytes == 0 || bytes == tool.download_bytes,
        "Incomplete package download"
    );
    let actual = format!("{:x}", hash.finalize());
    ensure!(
        actual.eq_ignore_ascii_case(&tool.sha256),
        "Package SHA-256 mismatch"
    );
    file.sync_all()?;
    drop(file);
    check(&cancel)?;
    progress(Progress::Extracting);
    tokio::task::spawn_blocking(move || {
        let expanded = extract_selected(
            &download,
            &payload,
            &tool.archive_format,
            &archive_root,
            &selected,
            &cancel,
        )?;
        ensure!(
            tool.installed_bytes == 0 || expanded == tool.installed_bytes,
            "Expanded package size changed"
        );
        let executable = binary.file_name().context("Missing package executable")?;
        let path = payload.join(executable);
        ensure!(
            path.is_file() && path.metadata()?.len() > 0,
            "Package executable is missing or empty"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::write(payload.join(MARKER), actual)?;
        check(&cancel)?;
        // Same-volume rename publishes the complete package; no merge/overwrite.
        // The marker makes the directory nonempty, so concurrent winners survive.
        match std::fs::rename(&payload, &destination) {
            Ok(()) => {}
            Err(error) => {
                ensure!(
                    std::fs::read_to_string(destination.join(MARKER))
                        .ok()
                        .is_some_and(|hash| hash == tool.sha256.to_ascii_lowercase())
                        && destination.join(executable).is_file(),
                    "Publish package: {error}"
                );
            }
        }
        drop(stage);
        Ok(destination.join(executable))
    })
    .await?
}

struct Extractor<'a> {
    output: &'a Path,
    root: &'a Path,
    selected: &'a [PathBuf],
    cancel: &'a AtomicBool,
    seen: HashSet<String>,
    bytes: u64,
}

impl Extractor<'_> {
    fn copy(&mut self, name: &str, reader: &mut impl Read) -> Result<()> {
        check(self.cancel)?;
        let path = relative(name)?;
        let Ok(path) = path.strip_prefix(self.root) else {
            return Ok(());
        };
        if !self.selected.is_empty() && !self.selected.iter().any(|selected| selected == path) {
            return Ok(());
        }
        ensure!(
            !path.as_os_str().is_empty(),
            "Package entry is its own root"
        );
        ensure!(
            self.seen
                .insert(path.to_string_lossy().to_ascii_lowercase()),
            "Duplicate package entry"
        );
        ensure!(self.seen.len() <= MAX_ENTRIES, "Too many package entries");
        ensure!(path != Path::new(MARKER), "Reserved package marker");
        let parent = mkdir_inside(self.output, path.parent().unwrap_or(Path::new("")))?;
        let mut output = File::options()
            .create_new(true)
            .write(true)
            .open(parent.join(path.file_name().context("Invalid package file")?))?;
        let mut buffer = [0; 64 * 1024];
        loop {
            check(self.cancel)?;
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            self.bytes += count as u64;
            ensure!(self.bytes <= MAX_EXPANDED, "Expanded package exceeds 2 GiB");
            output.write_all(&buffer[..count])?;
        }
        output.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
fn extract(
    archive: &Path,
    output: &Path,
    format: &str,
    root: &Path,
    cancel: &AtomicBool,
) -> Result<u64> {
    extract_selected(archive, output, format, root, &[], cancel)
}

fn extract_selected(
    archive: &Path,
    output: &Path,
    format: &str,
    root: &Path,
    selected: &[PathBuf],
    cancel: &AtomicBool,
) -> Result<u64> {
    let mut state = Extractor {
        output,
        root,
        selected,
        cancel,
        seen: HashSet::new(),
        bytes: 0,
    };
    let file = File::open(archive)?;
    match format {
        "zip" => {
            let mut zip = zip::ZipArchive::new(file)?;
            ensure!(zip.len() <= MAX_ENTRIES, "Too many package entries");
            for index in 0..zip.len() {
                check(cancel)?;
                let mut entry = zip.by_index(index)?;
                ensure!(
                    entry.unix_mode().is_none_or(|mode| mode & 0o170000 == 0
                        || [0o100000, 0o040000].contains(&(mode & 0o170000))),
                    "Package contains links or special files"
                );
                if entry.is_dir() {
                    continue;
                }
                let name = entry.name()?.into_owned();
                state.copy(&name, &mut entry)?;
            }
        }
        "tgz" => {
            let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
            for (index, entry) in tar.entries()?.enumerate() {
                check(cancel)?;
                ensure!(index < MAX_ENTRIES, "Too many package entries");
                let mut entry = entry?;
                let kind = entry.header().entry_type();
                ensure!(
                    kind.is_file() || kind.is_dir(),
                    "Package contains links or special files"
                );
                if kind.is_dir() {
                    continue;
                }
                let name = entry
                    .path()?
                    .to_str()
                    .context("Non-UTF-8 package path")?
                    .to_owned();
                state.copy(&name, &mut entry)?;
            }
        }
        _ => bail!("Unsupported package archive"),
    }
    ensure!(
        selected.iter().all(|path| state
            .seen
            .contains(&path.to_string_lossy().to_ascii_lowercase())),
        "A selected package file is missing"
    );
    Ok(state.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &Path, entries: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, bytes) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn extracts_the_complete_package_and_keeps_licenses_and_nested_data() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("fixture.zip");
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        fixture(
            &archive,
            &[
                ("package/tool.exe", b"binary"),
                ("package/COPYING.txt", b"license"),
                ("package/data/templates/default.txt", b"template"),
                ("other/ignored", b"ignored"),
            ],
        );
        extract(
            &archive,
            &output,
            "zip",
            Path::new("package"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(std::fs::read(output.join("tool.exe")).unwrap(), b"binary");
        assert_eq!(
            std::fs::read(output.join("COPYING.txt")).unwrap(),
            b"license"
        );
        assert_eq!(
            std::fs::read(output.join("data/templates/default.txt")).unwrap(),
            b"template"
        );
        assert!(!output.join("ignored").exists());
    }

    #[test]
    fn rejects_escaping_duplicate_and_reserved_package_entries() {
        for (index, entries) in [
            vec![("package/../escape", b"bad".as_slice())],
            vec![
                ("package/tool.exe", b"one".as_slice()),
                ("package/TOOL.exe", b"two".as_slice()),
            ],
            vec![("package/C:/escape", b"bad".as_slice())],
            vec![("package/CON.txt", b"bad".as_slice())],
            vec![("package/.atlas-package-sha256", b"bad".as_slice())],
        ]
        .into_iter()
        .enumerate()
        {
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("fixture.zip");
            let output = dir.path().join("output");
            std::fs::create_dir(&output).unwrap();
            fixture(&archive, &entries);
            assert!(
                extract(
                    &archive,
                    &output,
                    "zip",
                    Path::new("package"),
                    &AtomicBool::new(false)
                )
                .is_err(),
                "{index}"
            );
            assert!(!dir.path().join("escape").exists());
        }
    }

    #[test]
    fn extraction_cancels_before_creating_files() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("fixture.zip");
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        fixture(&archive, &[("package/tool.exe", b"binary")]);
        assert!(extract(
            &archive,
            &output,
            "zip",
            Path::new("package"),
            &AtomicBool::new(true)
        )
        .is_err());
        assert_eq!(std::fs::read_dir(output).unwrap().count(), 0);
    }

    #[test]
    fn reduced_packages_extract_only_explicit_files_and_require_every_selected_file() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("fixture.zip");
        let output = dir.path().join("output");
        std::fs::create_dir(&output).unwrap();
        fixture(
            &archive,
            &[
                ("opusenc.exe", b"encoder"),
                ("LICENSE", b"license"),
                ("opusinfo.exe", b"unused"),
            ],
        );
        let selected = [PathBuf::from("opusenc.exe"), PathBuf::from("LICENSE")];
        assert_eq!(
            extract_selected(
                &archive,
                &output,
                "zip",
                Path::new(""),
                &selected,
                &AtomicBool::new(false)
            )
            .unwrap(),
            14
        );
        assert!(output.join("opusenc.exe").is_file() && output.join("LICENSE").is_file());
        assert!(!output.join("opusinfo.exe").exists());
        let missing = dir.path().join("missing");
        std::fs::create_dir(&missing).unwrap();
        assert!(extract_selected(
            &archive,
            &missing,
            "zip",
            Path::new(""),
            &[PathBuf::from("missing.exe")],
            &AtomicBool::new(false)
        )
        .is_err());
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn real_reduced_opus_package_encodes_without_ffmpeg_when_enabled() {
        if std::env::var_os("ATLAS_REAL_PACKAGE_TEST").is_none() {
            return;
        }
        use crate::convert::{self, Backend, Plan, Settings, Tool, Tools};
        let manifest = atlas_model::ext_model::Manifest::parse(include_str!(
            "../../../../extensions/conversion-opus/extension.toml"
        ))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let provider = dir.path().join("provider");
        std::fs::create_dir(&provider).unwrap();
        let tool = manifest.tools[0].clone();
        let path = install(
            provider,
            tool.clone(),
            Arc::new(AtomicBool::new(false)),
            |_| {},
        )
        .await
        .unwrap();
        assert!(path.parent().unwrap().join("LICENSE").is_file());
        assert!(path.parent().unwrap().join("opusenc.html").is_file());
        assert!(!path.parent().unwrap().join("opusinfo.exe").exists());
        assert!(!path.parent().unwrap().join("opusdec.exe").exists());
        let mut tools = Tools::discover_with([("opusenc".into(), path.clone())]);
        let reader = tools.get(Tool::Ffmpeg).map(Path::to_path_buf);
        tools.0.retain(|tool, _| *tool == Tool::Opus);
        let folder = dir.path().join("日本語 y español");
        std::fs::create_dir(&folder).unwrap();
        let source = folder.join("canción.wav");
        let samples: Vec<_> = (0..22050)
            .map(|index| {
                ((f64::from(index) * 440.0 * std::f64::consts::TAU / 44100.0).sin() * 16000.0)
                    as i16
            })
            .flat_map(i16::to_le_bytes)
            .collect();
        let mut wave = b"RIFF".to_vec();
        wave.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
        wave.extend_from_slice(
            b"WAVEfmt \x10\0\0\0\x01\0\x01\0\x44\xac\0\0\x88\x58\x01\0\x02\0\x10\0data",
        );
        wave.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        wave.extend_from_slice(&samples);
        std::fs::write(&source, &wave).unwrap();
        let mut cases = vec![source.clone()];
        for extension in ["aiff", "au"] {
            let plan = Plan::build(
                &[source.clone()],
                extension,
                Settings::default(),
                Tools::default(),
            )
            .unwrap();
            let report = convert::execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
            assert!(report.errors().is_empty(), "{:?}", report.errors());
            cases.extend(report.outputs());
        }
        if tools.1.mp3 {
            let plan =
                Plan::build(&[source.clone()], "mp3", Settings::default(), tools.clone()).unwrap();
            let report = convert::execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
            assert!(report.errors().is_empty(), "{:?}", report.errors());
            cases.extend(report.outputs());
        }
        for source in &cases {
            let plan = Plan::build(
                &[source.clone()],
                "opus",
                Settings::default(),
                tools.clone(),
            )
            .unwrap();
            assert_eq!(plan.commands[0].backend, Backend::External(Tool::Opus));
            assert!(!plan.tools.has(Tool::Ffmpeg));
            let report = convert::execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
            assert!(report.errors().is_empty(), "{:?}", report.errors());
            let output = report.outputs()[0].clone();
            let bytes = std::fs::read(&output).unwrap();
            assert_eq!(&bytes[..4], b"OggS");
            assert!(bytes.windows(8).any(|bytes| bytes == b"OpusHead"));
            if let Some(reader) = &reader {
                let result = std::process::Command::new(reader)
                    .args(["-v", "error", "-xerror", "-i"])
                    .arg(&output)
                    .args(["-f", "f32le", "-acodec", "pcm_f32le", "pipe:1"])
                    .output()
                    .unwrap();
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                let samples: Vec<_> = result
                    .stdout
                    .chunks_exact(4)
                    .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                    .collect();
                assert!(
                    (23990..=30000).contains(&samples.len()),
                    "Unexpected Opus duration: {}",
                    samples.len()
                );
                let energy = samples
                    .iter()
                    .map(|sample| f64::from(*sample).powi(2))
                    .sum::<f64>()
                    / samples.len() as f64;
                assert!(
                    energy > 0.02 && energy < 0.3,
                    "Unexpected Opus signal: {energy}"
                );
            }
        }
        assert_eq!(std::fs::read(&source).unwrap(), wave);
        let version = std::process::Command::new(&path)
            .arg("--version")
            .output()
            .unwrap();
        assert!(version.status.success());
        let evidence = serde_json::json!({"package": "opusenc", "version": "0.2 (libopus 1.3-rc2)", "download_bytes": tool.download_bytes, "installed_payload_bytes": tool.installed_bytes, "sha256": tool.sha256, "selected_files": tool.archive_files, "unused_binaries_excluded": ["opusinfo.exe", "opusdec.exe"], "inputs_verified": cases.iter().map(|source| source.extension().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>(), "unicode_directory": true, "ffmpeg_used_as_engine": false, "independent_decoder": reader.is_some(), "version_output": String::from_utf8_lossy(&version.stdout)});
        std::fs::write(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../artifacts/handmade-conversion-validation/opus-package.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn refuses_unpinned_packages_and_cancelled_jobs_without_downloading() {
        let dir = tempfile::tempdir().unwrap();
        for cancel in [false, true] {
            let result = install(
                dir.path().to_path_buf(),
                ToolSpec::default(),
                Arc::new(AtomicBool::new(cancel)),
                |_| {},
            )
            .await;
            assert!(result.is_err());
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "windows")]
    #[tokio::test]
    async fn real_pandoc_package_download_and_conversion_when_enabled() {
        if std::env::var_os("ATLAS_REAL_PACKAGE_TEST").is_none() {
            return;
        }
        let manifest = atlas_model::ext_model::Manifest::parse(include_str!(
            "../../../../extensions/conversion-pandoc/extension.toml"
        ))
        .unwrap();
        let tool = manifest.tools[0].clone();
        let dir = tempfile::tempdir().unwrap();
        let provider = dir.path().join("provider");
        std::fs::create_dir(&provider).unwrap();
        let stages = Arc::new(std::sync::Mutex::new(Vec::new()));
        let events = stages.clone();
        let path = install(
            provider.clone(),
            tool.clone(),
            Arc::new(AtomicBool::new(false)),
            move |progress| events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(progress),
        )
        .await
        .unwrap();
        assert!(path.parent().unwrap().join("COPYING.rtf").is_file());
        assert!(stages
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|progress| matches!(progress, Progress::Extracting)));
        let metadata = std::fs::metadata(&path).unwrap();
        let reused = install(
            provider.clone(),
            tool,
            Arc::new(AtomicBool::new(false)),
            |_| panic!("An installed version must be reused without downloading"),
        )
        .await
        .unwrap();
        assert_eq!(path, reused);
        assert_eq!(
            metadata.modified().unwrap(),
            std::fs::metadata(&path).unwrap().modified().unwrap()
        );
        let tools = crate::convert::Tools::discover_with([("pandoc".into(), path.clone())]);
        assert_eq!(
            tools.get(crate::convert::Tool::Pandoc),
            Some(path.as_path())
        );
        let source = dir.path().join("document.rst");
        std::fs::write(
            &source,
            "Document\n========\n\nTexto con Unicode: María.\n\n* Uno\n* Dos\n",
        )
        .unwrap();
        let plan = crate::convert::Plan::build(
            &[source],
            "docx",
            crate::convert::Settings::default(),
            tools,
        )
        .unwrap();
        let report =
            crate::convert::execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
        assert!(report.errors().is_empty(), "{:?}", report.errors());
        let plan = crate::convert::Plan::build(
            &report.outputs(),
            "txt",
            crate::convert::Settings::default(),
            crate::convert::Tools::default(),
        )
        .unwrap();
        let report =
            crate::convert::execute(&plan, &Arc::new(AtomicBool::new(false)), |_, _, _| {});
        assert!(report.errors().is_empty(), "{:?}", report.errors());
        let text = std::fs::read_to_string(&report.outputs()[0]).unwrap();
        assert!(
            text.contains("María") && text.contains("Uno") && text.contains("Dos"),
            "{text}"
        );
        let version = std::process::Command::new(&path)
            .arg("--version")
            .output()
            .unwrap();
        assert!(version.status.success());
        let evidence = serde_json::json!({"package": "pandoc", "version": "3.12.1", "download_bytes": 42154914_u64, "installed_payload_bytes": 236017543_u64, "sha256": "6ef431cb20b2c24a0371fc19200dc49d452be54b1b5c9239f3462841ef53e587", "binary_bytes": metadata.len(), "license_preserved": true, "reuse_without_download": true, "rst_docx_txt": text, "version_output": String::from_utf8_lossy(&version.stdout)});
        let artifacts = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../artifacts/handmade-conversion-validation");
        std::fs::write(
            artifacts.join("pandoc-package.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
    }
}
