//! Remote marketplace client — fetches the R2 registry and downloads extension
//! zips for installation. The actual install step delegates to `ext::install`.

use anyhow::{ensure, Result};
use atlas_model::ext_model::{RemoteRegistry, ToolSpec};
use sha2::{Digest, Sha256};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

const REGISTRY_BASE: &str = "https://pub-89dd9e8d0b06444ca2f59e0dc245dbc9.r2.dev";

/// Upper bounds for marketplace downloads, so a hostile or broken server
/// cannot exhaust memory. Larger optional engines use `tool_package`.
const MAX_REGISTRY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_EXTENSION_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ICON_BYTES: u64 = 4 * 1024 * 1024;
const MAX_LEGACY_TOOL_BYTES: u64 = 512 * 1024 * 1024;

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(600))
        .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// GET an HTTPS URL into memory, refusing redirects to plain HTTP and bodies
/// over `limit` bytes.
async fn fetch_https(url: &str, limit: u64) -> Result<Vec<u8>> {
    let parsed = reqwest::Url::parse(url)?;
    ensure!(parsed.scheme() == "https", "download requires HTTPS: {url}");
    let mut response = client()?.get(parsed).send().await?.error_for_status()?;
    ensure!(
        response.url().scheme() == "https",
        "download redirected to an insecure URL"
    );
    ensure!(
        response.content_length().is_none_or(|len| len <= limit),
        "download is too large"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() as u64 + chunk.len() as u64 <= limit,
            "download is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Download the remote catalogue from `{REGISTRY_BASE}/registry.json`.
pub async fn fetch_registry() -> Result<RemoteRegistry> {
    let url = format!("{REGISTRY_BASE}/registry.json");
    let bytes = fetch_https(&url, MAX_REGISTRY_BYTES).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Download a zip from `download_url` to a temporary file, returning the path.
/// The caller is responsible for deleting the file after installation.
pub async fn download_zip(download_url: &str, expected_sha256: &str) -> Result<PathBuf> {
    ensure!(
        !expected_sha256.is_empty(),
        "extension has no SHA-256 digest"
    );
    let bytes = fetch_https(download_url, MAX_EXTENSION_BYTES).await?;
    verify_sha256(&bytes, expected_sha256)?;

    let mut tmp = tempfile::NamedTempFile::new()?;
    tmp.write_all(&bytes)?;

    // Persist so the path outlives this scope; caller installs then can clean up.
    let (_, path) = tmp.keep()?;
    Ok(path)
}

/// Download an icon from `url` to `dest`.
pub async fn download_icon(url: &str, dest: &std::path::Path) -> Result<()> {
    let bytes = fetch_https(url, MAX_ICON_BYTES).await?;
    let mut file = std::fs::File::create(dest)?;
    file.write_all(&bytes)?;
    Ok(())
}

pub async fn install_tool(provider_dir: PathBuf, tool: ToolSpec) -> Result<PathBuf> {
    if !tool.archive_root.is_empty() {
        return super::tool_package::install(
            provider_dir,
            tool,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            |_| {},
        )
        .await;
    }
    ensure!(
        !tool.sha256.is_empty(),
        "tool {} has no SHA-256 digest",
        tool.name
    );
    // The path comes from an extension manifest: keep it inside the provider.
    let dest = provider_dir.join(super::tool_package::relative(&tool.path)?);
    let bytes = fetch_https(&tool.download_url, MAX_LEGACY_TOOL_BYTES).await?;
    verify_sha256(&bytes, &tool.sha256)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match tool.archive_format.as_str() {
        "zip" => extract_zip_file(&bytes, &tool.archive_path, &dest)?,
        "tgz" => extract_tgz_file(&bytes, &tool.archive_path, &dest)?,
        #[cfg(feature = "sevenz")]
        "7z" => extract_7z_file(&bytes, &tool.archive_path, &dest)?,
        other => anyhow::bail!("unsupported tool archive format: {other}"),
    }
    Ok(dest)
}

fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = format!("{:x}", Sha256::digest(bytes));
    ensure!(
        actual.eq_ignore_ascii_case(expected.trim()),
        "SHA-256 mismatch: expected {expected}, got {actual}"
    );
    Ok(())
}

fn extract_zip_file(bytes: &[u8], archive_path: &str, dest: &Path) -> Result<()> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader)?;
    let mut copied = 0usize;
    for idx in 0..archive.len() {
        let mut file = archive.by_index(idx)?;
        if file.is_dir() {
            continue;
        }
        if let Some(out_path) = sibling_dest(&file.name()?, archive_path, dest) {
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(out_path)?;
            std::io::copy(&mut file, &mut out)?;
            copied += 1;
        }
    }
    if copied == 0 {
        anyhow::bail!("tool file not found in zip: {archive_path}");
    }
    Ok(())
}

fn extract_tgz_file(bytes: &[u8], archive_path: &str, dest: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut copied = 0usize;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().replace('\\', "/");
        if let Some(out_path) = sibling_dest(&path, archive_path, dest) {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(out_path, buf)?;
            copied += 1;
        }
    }
    if copied == 0 {
        anyhow::bail!("tool file not found in tgz: {archive_path}");
    }
    Ok(())
}

#[cfg(feature = "sevenz")]
fn extract_7z_file(bytes: &[u8], archive_path: &str, dest: &Path) -> Result<()> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = sevenz_rust2::ArchiveReader::new(reader, sevenz_rust2::Password::empty())?;
    let mut copied = 0usize;
    let mut target_seen = false;
    let extract_result = archive.for_each_entries(|entry, reader| {
        if entry.is_directory() {
            return Ok(true);
        }
        if let Some(out_path) = sibling_dest(entry.name(), archive_path, dest) {
            target_seen |= out_path == dest;
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(sevenz_rust2::Error::from)?;
            }
            let mut out = std::fs::File::create(out_path).map_err(sevenz_rust2::Error::from)?;
            std::io::copy(reader, &mut out).map_err(sevenz_rust2::Error::from)?;
            copied += 1;
        }
        Ok(true)
    });
    if let Err(err) = extract_result {
        if !target_seen || !dest.is_file() {
            return Err(err.into());
        }
    }
    if copied == 0 && (!target_seen || !dest.is_file()) {
        anyhow::bail!("tool file not found in 7z: {archive_path}");
    }
    Ok(())
}

fn archive_entry_matches(entry: &str, wanted: &str) -> bool {
    let entry = entry.replace('\\', "/");
    let wanted = wanted.replace('\\', "/");
    entry == wanted || entry.ends_with(&format!("/{wanted}"))
}

fn sibling_dest(entry: &str, wanted: &str, dest: &Path) -> Option<PathBuf> {
    let entry = entry.replace('\\', "/");
    let wanted = wanted.replace('\\', "/");
    let wanted_dir = wanted.rsplit_once('/').map_or("", |(dir, _)| dir);
    if wanted_dir.is_empty() {
        return archive_entry_matches(&entry, &wanted).then(|| dest.to_path_buf());
    }
    let marker = format!("{wanted_dir}/");
    let pos = if entry.starts_with(&marker) {
        0
    } else {
        entry.find(&format!("/{marker}"))? + 1
    };
    let relative = entry[pos + marker.len()..].trim();
    // Archive entry names are untrusted: reject `..`, absolute paths, drive
    // prefixes and reserved names instead of joining them onto `dest`.
    let relative = super::tool_package::relative(relative).ok()?;
    Some(dest.parent()?.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_sha256_before_installing() {
        let digest = format!("{:x}", Sha256::digest(b"atlas"));
        assert!(verify_sha256(b"atlas", &digest.to_uppercase()).is_ok());
        assert!(verify_sha256(b"tampered", &digest).is_err());
    }

    #[test]
    fn archive_entry_matches_nested_tool_path() {
        assert!(archive_entry_matches(
            "poppler-26.02.0/Library/bin/pdftoppm.exe",
            "Library/bin/pdftoppm.exe"
        ));
        assert!(archive_entry_matches("bin/pdfium.dll", "bin/pdfium.dll"));
    }

    #[test]
    fn sibling_dest_extracts_whole_tool_directory() {
        let dest = Path::new("tools/windows-x64/poppler/bin/pdftoppm.exe");
        assert_eq!(
            sibling_dest(
                "poppler-26.02.0/Library/bin/lcms2.dll",
                "Library/bin/pdftoppm.exe",
                dest
            )
            .unwrap(),
            PathBuf::from("tools/windows-x64/poppler/bin/lcms2.dll")
        );
    }

    #[test]
    fn sibling_dest_rejects_entries_that_escape_the_tool_directory() {
        let dest = Path::new("tools/windows-x64/poppler/bin/pdftoppm.exe");
        for entry in [
            "poppler/Library/bin/../../../../evil.dll",
            "poppler/Library/bin/C:/Windows/evil.dll",
            "poppler/Library/bin//etc/evil",
            "poppler/Library/bin/sub\\..\\..\\evil.dll",
            "poppler/Library/bin/..",
            "poppler/Library/bin/CON.dll",
        ] {
            assert!(
                sibling_dest(entry, "Library/bin/pdftoppm.exe", dest).is_none(),
                "{entry}"
            );
        }
    }

    #[tokio::test]
    async fn legacy_tools_refuse_paths_outside_the_provider_and_plain_http() {
        let dir = tempfile::tempdir().unwrap();
        for (path, url) in [
            ("../escape.exe", "https://example.invalid/tool.zip"),
            ("C:/Windows/escape.exe", "https://example.invalid/tool.zip"),
            ("tools/ok.exe", "http://example.invalid/tool.zip"),
        ] {
            let tool = ToolSpec {
                name: "x".into(),
                path: path.into(),
                download_url: url.into(),
                archive_path: "ok.exe".into(),
                archive_format: "zip".into(),
                sha256: "00".repeat(32),
                ..ToolSpec::default()
            };
            assert!(install_tool(dir.path().to_path_buf(), tool).await.is_err(), "{path} {url}");
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn real_ffmpeg_tool_download_when_enabled() {
        if std::env::var_os("ATLAS_REAL_TOOL_DOWNLOAD").is_none() {
            return;
        }
        let provider_dir =
            std::env::temp_dir().join(format!("atlas-real-ffmpeg-{}", uuid::Uuid::new_v4()));
        let tool = ToolSpec {
            name: "ffmpeg".into(),
            path: "tools/windows-x64/ffmpeg/ffmpeg.exe".into(),
            os: "windows".into(),
            arch: "x86_64".into(),
            download_url: "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-lgpl.zip".into(),
            archive_path: "bin/ffmpeg.exe".into(),
            archive_format: "zip".into(),
            signature_url: String::new(),
            sha256: String::new(),
            ..ToolSpec::default()
        };

        let installed = install_tool(provider_dir.clone(), tool).await.unwrap();
        assert!(installed.is_file());
        let output = std::process::Command::new(&installed)
            .arg("-version")
            .output()
            .unwrap();
        assert!(output.status.success());

        let _ = std::fs::remove_dir_all(provider_dir);
    }

    #[cfg(feature = "sevenz")]
    #[tokio::test]
    async fn real_imagemagick_tool_download_when_enabled() {
        if std::env::var_os("ATLAS_REAL_TOOL_DOWNLOAD").is_none() {
            return;
        }
        let provider_dir =
            std::env::temp_dir().join(format!("atlas-real-magick-{}", uuid::Uuid::new_v4()));
        let tool = ToolSpec {
            name: "magick".into(),
            path: "tools/windows-x64/imagemagick/magick.exe".into(),
            os: "windows".into(),
            arch: "x86_64".into(),
            download_url: "https://imagemagick.org/archive/binaries/ImageMagick-7.1.2-26-portable-Q16-HDRI-x64.7z".into(),
            archive_path: "magick.exe".into(),
            archive_format: "7z".into(),
            signature_url: String::new(),
            sha256: String::new(),
            ..ToolSpec::default()
        };

        let installed = install_tool(provider_dir.clone(), tool).await.unwrap();
        assert!(installed.is_file());
        let output = std::process::Command::new(&installed)
            .arg("-version")
            .output()
            .unwrap();
        assert!(output.status.success());

        let _ = std::fs::remove_dir_all(provider_dir);
    }
}
