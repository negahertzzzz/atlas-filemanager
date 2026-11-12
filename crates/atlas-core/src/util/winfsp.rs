//! The pinned official WinFsp installer, shared by encrypted folders and cloud
//! mounts. The MSI only runs after its SHA-256 matches the pinned release, so
//! a tampered download or redirect can never reach Windows Installer.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::io::Write;

const MSI_URL: &str =
    "https://github.com/winfsp/winfsp/releases/download/v2.1/winfsp-2.1.25156.msi";
const MSI_SHA256: &str = "073a70e00f77423e34bed98b86e600def93393ba5822204fac57a29324db9f7a";
const MSI_MAX_BYTES: u64 = 32 * 1024 * 1024;

/// Whether `bytes` are the pinned installer.
fn is_pinned(bytes: &[u8]) -> bool {
    format!("{:x}", Sha256::digest(bytes)) == MSI_SHA256
}

/// Downloads the pinned MSI, verifies it and runs Windows Installer with a
/// progress window and no forced restart. `progress` receives
/// `(bytes_done, bytes_total)`. Returns msiexec's exit code.
pub async fn install(progress: impl Fn(u64, u64) + Send) -> Result<Option<i32>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .user_agent(concat!("Atlas/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut response = client
        .get(MSI_URL)
        .send()
        .await
        .context("download WinFsp")?
        .error_for_status()
        .context("WinFsp download HTTP error")?;
    ensure!(
        response.url().scheme() == "https",
        "WinFsp download redirected to an insecure URL"
    );
    let total = response.content_length().unwrap_or(0);
    ensure!(total <= MSI_MAX_BYTES, "WinFsp installer is too large");
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.context("WinFsp download chunk")? {
        ensure!(
            bytes.len() as u64 + chunk.len() as u64 <= MSI_MAX_BYTES,
            "WinFsp installer is too large"
        );
        bytes.extend_from_slice(&chunk);
        progress(bytes.len() as u64, total);
    }
    ensure!(
        is_pinned(&bytes),
        "WinFsp installer failed the integrity check"
    );

    // A fresh, unpredictable temp name: nothing else can swap the verified file.
    let mut installer = tempfile::Builder::new()
        .prefix("atlas-winfsp-")
        .suffix(".msi")
        .tempfile()?;
    installer.write_all(&bytes)?;
    installer.as_file().sync_all()?;
    // MSI opens the package itself; close the write handle before invoking it.
    let installer = installer.into_temp_path();
    let mut command = tokio::process::Command::new("msiexec.exe");
    command
        .arg("/i")
        .arg(&installer)
        .args(["/passive", "/norestart"]);
    command.creation_flags(0x0800_0000);
    let status = command.status().await.context("run msiexec")?;
    Ok(status.code())
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_the_pinned_installer_passes() {
        assert!(!super::is_pinned(b"not the installer"));
        assert_eq!(super::MSI_SHA256.len(), 64);
    }
}
