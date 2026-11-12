// reason: rclone binary download/extract + daemon spawn run one-shot on the cloud
// worker, never on the gpui render thread; the sync-fs ban targets UI blocking.
#![allow(clippy::disallowed_methods)]
//! rclone binary lifecycle: locate or download it, then run a long-lived
//! `rclone rcd` daemon the rest of the app drives over the rc HTTP API.
//!
//! Store packages bundle a pinned binary. Other distributions fall back to a
//! system `rclone` or a SHA256-verified first-use download.

use anyhow::{anyhow, Context};
use futures_util::StreamExt as _;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

/// Progress callback for the binary download: `(bytes_done, bytes_total)`.
pub type DownloadProgress = Box<dyn Fn(u64, u64) + Send + Sync>;

fn rclone_bin_name() -> &'static str {
    if cfg!(windows) {
        "rclone.exe"
    } else {
        "rclone"
    }
}

/// Map the host platform to rclone's `<os>-<arch>` asset suffix. Covers every
/// target rclone publishes so the download works on any system.
fn os_arch() -> anyhow::Result<(&'static str, &'static str)> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "osx",
        "windows" => "windows",
        "freebsd" => "freebsd",
        "netbsd" => "netbsd",
        "openbsd" => "openbsd",
        "solaris" | "illumos" => "solaris",
        other => return Err(anyhow!("unsupported OS for rclone download: {other}")),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        // rclone ships arm-v6/arm-v7/arm; the generic "arm" build runs everywhere.
        "arm" => "arm",
        "mips" | "mips64" => "mips",
        "powerpc64" => "ppc64",
        other => return Err(anyhow!("unsupported arch for rclone download: {other}")),
    };
    Ok((os, arch))
}

/// Resolve the current rclone version string (e.g. `v1.74.2`) from the published
/// `version.txt` so downloads always track the latest release.
async fn current_version(client: &reqwest::Client) -> anyhow::Result<String> {
    let text = client
        .get("https://downloads.rclone.org/version.txt")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await
        .context("fetch version.txt")?;
    text.split_whitespace()
        .find(|t| t.starts_with('v'))
        .map(str::to_string)
        .ok_or_else(|| anyhow!("could not parse version from version.txt: {text:?}"))
}

/// Where we keep a downloaded rclone binary.
fn managed_binary_path() -> PathBuf {
    crate::util::config::data_dir()
        .join("bin")
        .join(rclone_bin_name())
}

/// Find a usable rclone: prefer the Store-bundled binary, then `$PATH`, then our
/// managed copy if already present.
fn find_existing() -> Option<PathBuf> {
    if let Some(bundled) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(rclone_bin_name())))
        .filter(|path| path.is_file())
    {
        return Some(bundled);
    }
    let mut cmd = std::process::Command::new("rclone");
    cmd.arg("version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    if let Ok(out) = cmd.output() {
        if out.status.success() {
            return Some(PathBuf::from("rclone"));
        }
    }
    let managed = managed_binary_path();
    managed.is_file().then_some(managed)
}

/// Return the path of the currently usable rclone binary (PATH or managed copy),
/// without downloading. Returns `None` if rclone is not present.
pub fn binary_path() -> Option<PathBuf> {
    find_existing()
}

/// Ensure an rclone binary exists, downloading + verifying it if necessary.
/// Returns the path to invoke.
pub async fn ensure_binary(progress: DownloadProgress) -> anyhow::Result<PathBuf> {
    if let Some(p) = find_existing() {
        return Ok(p);
    }
    download_and_verify(progress).await?;
    Ok(managed_binary_path())
}

async fn download_and_verify(progress: DownloadProgress) -> anyhow::Result<()> {
    let (os, arch) = os_arch()?;
    let client = reqwest::Client::new();
    let version = current_version(&client).await?;
    let asset = format!("rclone-{version}-{os}-{arch}.zip");
    let base = format!("https://downloads.rclone.org/{version}");
    let zip_url = format!("{base}/{asset}");
    let sums_url = format!("{base}/SHA256SUMS");

    // Expected checksum for our asset, from the release's SHA256SUMS.
    let sums = client
        .get(&sums_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await
        .context("fetch SHA256SUMS")?;
    let expected = sums
        .lines()
        .find_map(|line| {
            let (hash, name) = line.split_once("  ")?;
            (name.trim() == asset).then(|| hash.trim().to_ascii_lowercase())
        })
        .ok_or_else(|| anyhow!("no checksum for {asset} in SHA256SUMS"))?;

    // Stream the zip to a temp file while hashing + reporting progress.
    let resp = client.get(&zip_url).send().await?.error_for_status()?;
    let total = resp.content_length().unwrap_or(0);
    let mut hasher = Sha256::new();
    let tmp = std::env::temp_dir().join(format!("atlas-{asset}"));
    let mut file = tokio::fs::File::create(&tmp).await?;
    let mut done: u64 = 0;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("download chunk")?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        done += chunk.len() as u64;
        progress(done, total);
    }
    file.flush().await?;
    drop(file);

    let got = format!("{:x}", hasher.finalize());
    if got != expected {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(anyhow!(
            "rclone checksum mismatch: expected {expected}, got {got}"
        ));
    }

    extract_binary(&tmp, &managed_binary_path())?;
    let _ = tokio::fs::remove_file(&tmp).await;
    Ok(())
}

/// Pull the single `rclone` binary out of the release zip into `dest`.
fn extract_binary(zip_path: &Path, dest: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let bin_name = rclone_bin_name();

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let name = entry.name()?.into_owned();
        if name.ends_with(&format!("/{bin_name}")) || name == bin_name {
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            std::fs::write(dest, &bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
            }
            return Ok(());
        }
    }
    Err(anyhow!(
        "rclone binary not found inside {}",
        zip_path.display()
    ))
}

/// PID of the live `rclone rcd` child, so the app can hard-kill it on exit even
/// though the daemon handle lives on a never-joined runtime thread. 0 = none.
pub static RCD_PID: AtomicU32 = AtomicU32::new(0);

/// Connection details for a spawned daemon.
pub struct Daemon {
    pub child: Child,
    pub addr: String,
    pub user: String,
    pub pass: String,
    #[cfg(windows)]
    _job: atlas_win_space::process_job::ChildJob,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Best-effort: stop the daemon (and thus any mounts) when the app exits.
        let _ = self.child.start_kill();
    }
}

/// Pick a free localhost port by binding to :0 and reading the assigned port.
fn free_port() -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// Spawn `rclone rcd` bound to a random localhost port with random basic-auth
/// credentials. The caller polls `core/version` to confirm readiness.
pub fn spawn_daemon(binary: &Path) -> anyhow::Result<Daemon> {
    let port = free_port()?;
    let addr = format!("127.0.0.1:{port}");
    let user = format!("fp-{}", uuid::Uuid::new_v4().simple());
    let pass = uuid::Uuid::new_v4().simple().to_string();
    let cache_dir = crate::util::config::data_dir().join("rclone-cache");
    let _ = std::fs::create_dir_all(&cache_dir);

    let mut cmd = Command::new(binary);
    cmd.arg("rcd")
        .arg("--rc-addr")
        .arg(&addr)
        .arg("--rc-user")
        .arg(&user)
        .arg("--rc-pass")
        .arg(&pass)
        .arg("--cache-dir")
        .arg(&cache_dir)
        .args(["--low-level-retries", "2", "--retries", "2"])
        .arg("--transfers")
        .arg(crate::resources::scaled(4).to_string())
        .arg("--checkers")
        .arg(crate::resources::scaled(8).to_string())
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);

    let mut child = cmd.spawn().context("spawn rclone rcd")?;
    #[cfg(windows)]
    let job = match child
        .raw_handle()
        .map(|handle| atlas_win_space::process_job::ChildJob::contain(handle as isize))
    {
        Some(Ok(job)) => job,
        Some(Err(error)) => {
            let _ = child.start_kill();
            return Err(error).context("contain rclone process lifetime");
        }
        None => {
            let _ = child.start_kill();
            return Err(anyhow!("rclone process handle unavailable"));
        }
    };
    RCD_PID.store(child.id().unwrap_or(0), Ordering::SeqCst);

    Ok(Daemon {
        child,
        addr,
        user,
        pass,
        #[cfg(windows)]
        _job: job,
    })
}
