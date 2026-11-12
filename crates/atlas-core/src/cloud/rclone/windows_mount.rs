//! Windows-specific cloud mount helpers.

/// True if the WinFsp kernel driver is ready for rclone mounts.
pub fn ensure_winfsp_ready() -> bool {
    if winfsp_service_running() {
        return true;
    }
    let mut cmd = std::process::Command::new("sc.exe");
    cmd.args(["start", "WinFsp"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.status();
    if winfsp_service_running() {
        return true;
    }
    false
}

fn winfsp_service_running() -> bool {
    let mut cmd = std::process::Command::new("sc.exe");
    cmd.args(["query", "WinFsp"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let Ok(output) = cmd.output() else {
        return false;
    };
    output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .to_ascii_uppercase()
            .contains("RUNNING")
}

/// Download and install the pinned, SHA-256-verified WinFsp release. Windows
/// shows its own UAC prompt for the kernel driver.
/// `progress` is called with `(bytes_done, bytes_total)` during download.
#[cfg(windows)]
pub async fn install_winfsp(progress: impl Fn(u64, u64) + Send + 'static) -> anyhow::Result<()> {
    // The older unverified MSI that earlier builds left in the data folder.
    let _ =
        tokio::fs::remove_file(crate::util::config::data_dir().join("winfsp-installer.msi")).await;
    match crate::util::winfsp::install(progress).await? {
        Some(0) => Ok(()),
        Some(1641 | 3010) => Err(anyhow::anyhow!(
            "WinFsp was installed. Restart Windows, then mount the cloud drive again."
        )),
        code => Err(anyhow::anyhow!(
            "WinFsp installer exited with code {}",
            code.unwrap_or(-1)
        )),
    }
}
