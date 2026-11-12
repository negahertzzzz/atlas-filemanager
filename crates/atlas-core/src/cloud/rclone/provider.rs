//! [`CloudProvider`] implementation backed by a running rclone daemon.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use serde_json::{json, Value};

use super::rc_api::AsyncJob;
use super::rc_api::RcClient;
use crate::cloud::control::{OutcomeUnknown, TransferCancelled, TransferControl};
use crate::cloud::provider::{Capabilities, CloudProvider, ProgressSink, RemoteEntry};

pub struct RcloneProvider {
    rc: RcClient,
    cache_dir: PathBuf,
}

/// True for `\\server\share` style paths (WSL shares, SMB). Never matches on
/// Unix, where paths can't start with a backslash.
fn is_unc(p: &Path) -> bool {
    p.to_string_lossy().starts_with(r"\\")
}

fn speedup_config() -> Value {
    json!({
        "Immutable": true,
        "MultiThreadCutoff": "64Mi",
        "MultiThreadStreams": crate::resources::scaled(4),
    })
}

impl RcloneProvider {
    pub const fn new(rc: RcClient, cache_dir: PathBuf) -> Self {
        Self { rc, cache_dir }
    }

    /// Split a local file path into the `(dstFs, dstRemote)` pair rclone expects:
    /// the containing directory as the fs, the file name as the remote.
    fn local_split(p: &Path) -> anyhow::Result<(String, String)> {
        let parent = p
            .parent()
            .ok_or_else(|| anyhow!("path has no parent: {}", p.display()))?;
        let name = p
            .file_name()
            .ok_or_else(|| anyhow!("path has no file name: {}", p.display()))?;
        Ok((
            parent.to_string_lossy().into_owned(),
            name.to_string_lossy().into_owned(),
        ))
    }

    /// Unique staging path inside the cache dir for a transfer whose local end
    /// is a UNC path rclone can't be trusted with.
    async fn stage_path(&self, target: &Path) -> anyhow::Result<PathBuf> {
        let dir = self.cache_dir.join("unc-staging");
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating staging dir {}", dir.display()))?;
        let name = target
            .file_name()
            .ok_or_else(|| anyhow!("path has no file name: {}", target.display()))?;
        Ok(dir.join(format!(
            "{}-{}",
            uuid::Uuid::new_v4().simple(),
            name.to_string_lossy()
        )))
    }

    /// Drive an async copy job to completion, forwarding byte progress.
    async fn run_copy(
        &self,
        src_fs: &str,
        src_remote: &str,
        dst_fs: &str,
        dst_remote: &str,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        if control.is_cancelled() {
            return Err(TransferCancelled.into());
        }
        self.rc.require_missing(dst_fs, dst_remote).await?;
        if let Some(limit) = control.byte_limit() {
            let metadata = self.rc.operations_stat(src_fs, src_remote).await?;
            anyhow::ensure!(
                metadata
                    .get("Size")
                    .and_then(Value::as_u64)
                    .is_none_or(|size| size <= limit),
                "Remote file exceeds the download byte limit"
            );
        }
        let group = format!("fp-{}", uuid::Uuid::new_v4().simple());
        let mut config = speedup_config();
        // Older supported rclone builds ignore Immutable in copyfile. This
        // additionally protects a destination created after the preflight.
        config["IgnoreExisting"] = json!(true);
        if let Some(limit) = control.byte_limit() {
            config["MaxTransfer"] = json!(limit.to_string());
        }
        let job = self
            .rc
            .copyfile_async(src_fs, src_remote, dst_fs, dst_remote, &group, Some(config))
            .await
            .map_err(transfer_start_error)?;
        self.wait_job(job, progress, control).await
    }

    async fn run_directory_copy(
        &self,
        src: &str,
        dst: &str,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        if control.is_cancelled() {
            return Err(TransferCancelled.into());
        }
        let group = format!("directory-{}", uuid::Uuid::new_v4().simple());
        let job = self
            .rc
            .copy_directory_async(src, dst, &group)
            .await
            .map_err(transfer_start_error)?;
        self.wait_job(job, progress, control).await
    }

    async fn wait_job(
        &self,
        job: AsyncJob,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        loop {
            // Cancellation has an explicit backend acknowledgement. Never drop
            // the job future and claim that its remote writes stopped.
            if control.is_cancelled() {
                self.rc.job_stop(job.jobid).await.map_err(|error| {
                    OutcomeUnknown(format!("Cannot confirm job cancellation: {error}"))
                })?;
                for _ in 0..120 {
                    match self.rc.job_status(job.jobid).await {
                        Ok(Some(Ok(()))) => return Ok(()), // completion won the race
                        Ok(Some(Err(_))) => return Err(TransferCancelled.into()),
                        _ => tokio::time::sleep(Duration::from_millis(100)).await,
                    }
                }
                return Err(OutcomeUnknown(
                    "Backend did not confirm that the cancelled job stopped".into(),
                )
                .into());
            }
            match self.rc.job_status(job.jobid).await {
                Ok(Some(result)) => return result,
                Ok(None) => {}
                Err(error) => {
                    return Err(OutcomeUnknown(format!(
                        "Transfer outcome could not be confirmed: {error}"
                    ))
                    .into())
                }
            }
            if let Ok((done, total)) = self.rc.core_stats(&job.group).await {
                progress(done, total);
            }
            tokio::select! {
                () = control.cancelled() => {},
                () = tokio::time::sleep(Duration::from_millis(250)) => {},
            }
        }
    }
}

#[async_trait::async_trait]
impl CloudProvider for RcloneProvider {
    async fn prepare_shutdown(&self) -> anyhow::Result<()> {
        self.rc.require_no_mounts().await
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            can_mount: true,
            can_write: true,
            random_read: false,
        }
    }

    async fn configure(
        &self,
        id: &str,
        backend_type: &str,
        parameters: Value,
    ) -> anyhow::Result<()> {
        self.rc.config_create(id, backend_type, parameters).await
    }

    async fn configure_oauth(
        &self,
        id: &str,
        backend_type: &str,
        parameters: Value,
    ) -> anyhow::Result<()> {
        super::oauth::run_config_machine(&self.rc, id, backend_type, parameters).await
    }

    async fn deconfigure(&self, id: &str) -> anyhow::Result<()> {
        self.rc.unmount_account(id).await?;
        self.rc.config_delete(id).await
    }

    async fn list(&self, id: &str, path: &str) -> anyhow::Result<Vec<RemoteEntry>> {
        let drive = self.rc.is_drive(id).await?;
        anyhow::ensure!(
            !drive
                || path.is_empty()
                || atlas_model::remote_path::last(path).is_some_and(|object| object.is_dir),
            "Refresh this account from its root: this folder has no immutable object ID"
        );
        let (fs, remote) = self.rc.scope(id, path).await?;
        let v = self.rc.operations_list(&fs, &remote).await?;
        let list = v
            .get("list")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("operations/list: missing list"))?;
        let entries = list
            .iter()
            .map(|e| RemoteEntry {
                name: e
                    .get("Name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                path: if drive && e.get("ID").and_then(Value::as_str).is_some() {
                    atlas_model::remote_path::join(
                        path,
                        e["ID"].as_str().unwrap_or_default(),
                        e["Name"].as_str().unwrap_or_default(),
                        e["IsDir"].as_bool().unwrap_or(false),
                    )
                } else {
                    e.get("Path")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                },
                is_dir: e.get("IsDir").and_then(Value::as_bool).unwrap_or(false),
                size: e.get("Size").and_then(Value::as_i64).unwrap_or(0).max(0) as u64,
                modified: e
                    .get("ModTime")
                    .and_then(Value::as_str)
                    .and_then(chrono_secs),
                mime: e
                    .get("MimeType")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
            .collect();
        Ok(entries)
    }

    async fn download_controlled(
        &self,
        id: &str,
        src: &str,
        dst: &Path,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        if control.is_cancelled() {
            return Err(TransferCancelled.into());
        }
        anyhow::ensure!(
            !self.rc.is_drive(id).await? || atlas_model::remote_path::last(src).is_some(),
            "Refresh this account from its root: this file has no immutable object ID"
        );
        let parent = dst
            .parent()
            .ok_or_else(|| anyhow!("download destination has no parent"))?;
        tokio::fs::create_dir_all(parent).await?;
        let destination = dst.to_path_buf();
        let mut stage = tokio::task::spawn_blocking(move || {
            super::super::staging::DownloadStage::new(&destination)
        })
        .await??;
        stage.set_byte_limit(control.byte_limit());
        let dst = stage.path.clone();
        let dst = dst.as_path();
        if let Some(object) = atlas_model::remote_path::last(src).filter(|object| !object.is_dir) {
            anyhow::ensure!(
                self.rc.is_drive(id).await?,
                "Object addresses require Google Drive"
            );
            let group = format!("copy-id-{}", uuid::Uuid::new_v4().simple());
            let result = async {
                let job = self
                    .rc
                    .copy_id_async(
                        id,
                        &object.id,
                        &dst.to_string_lossy(),
                        &group,
                        control.byte_limit(),
                    )
                    .await
                    .map_err(transfer_start_error)?;
                self.wait_job(job, progress, control).await
            }
            .await;
            stage.check(result)?;
            return tokio::task::spawn_blocking(move || stage.commit()).await?;
        }
        let (source_fs, source_path) = self.rc.scope(id, src).await?;
        if self
            .rc
            .operations_stat(&source_fs, &source_path)
            .await?
            .get("IsDir")
            .and_then(Value::as_bool)
            == Some(true)
        {
            let result = self
                .run_directory_copy(
                    &super::objects::rooted(&source_fs, &source_path),
                    &dst.to_string_lossy(),
                    progress,
                    control,
                )
                .await;
            stage.check(result)?;
            return tokio::task::spawn_blocking(move || stage.commit()).await?;
        }
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if crate::fs::paths::wsl_unc_parts(dst).is_some() {
            let (dst_fs, dst_remote) = Self::local_split(dst)?;
            let started = Instant::now();
            match self
                .run_copy(
                    &source_fs,
                    &source_path,
                    &dst_fs,
                    &dst_remote,
                    progress.clone(),
                    control.clone(),
                )
                .await
            {
                Ok(()) => {
                    tracing::debug!(
                        target = %dst.display(),
                        elapsed_ms = started.elapsed().as_millis(),
                        "cloud download used direct WSL copy"
                    );
                    return tokio::task::spawn_blocking(move || stage.commit()).await?;
                }
                Err(e) => {
                    if control.is_cancelled() || e.is::<OutcomeUnknown>() {
                        return stage.check(Err(e));
                    }
                    tracing::warn!(
                        target = %dst.display(),
                        error = %e,
                        "direct WSL download failed; falling back to staging"
                    );
                }
            }
        }
        // rclone's local backend chokes on some UNC destinations (WSL shares via
        // \\wsl.localhost, SMB): stage the download in our cache dir and finish
        // with a plain filesystem copy, which handles UNC fine.
        if is_unc(dst) {
            let staged = self.stage_path(dst).await?;
            let (dst_fs, dst_remote) = Self::local_split(&staged)?;
            let result = self
                .run_copy(
                    &source_fs,
                    &source_path,
                    &dst_fs,
                    &dst_remote,
                    progress,
                    control,
                )
                .await;
            stage.check(result)?;
            tokio::fs::copy(&staged, dst)
                .await
                .with_context(|| format!("copying staged download to {}", dst.display()))?;
            let _ = tokio::fs::remove_file(&staged).await;
            return tokio::task::spawn_blocking(move || stage.commit()).await?;
        }
        let (dst_fs, dst_remote) = Self::local_split(dst)?;
        let result = self
            .run_copy(
                &source_fs,
                &source_path,
                &dst_fs,
                &dst_remote,
                progress,
                control,
            )
            .await;
        stage.check(result)?;
        tokio::task::spawn_blocking(move || stage.commit()).await?
    }

    async fn upload_controlled(
        &self,
        id: &str,
        src: &Path,
        dst: &str,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        if control.is_cancelled() {
            return Err(TransferCancelled.into());
        }
        let (destination_fs, destination_path) = self.rc.scope(id, dst).await?;
        if tokio::fs::metadata(src).await?.is_dir() {
            return self
                .run_directory_copy(
                    &src.to_string_lossy(),
                    &super::objects::rooted(&destination_fs, &destination_path),
                    progress,
                    control,
                )
                .await;
        }
        if crate::fs::paths::wsl_unc_parts(src).is_some() {
            let (src_fs, src_remote) = Self::local_split(src)?;
            let started = Instant::now();
            match self
                .run_copy(
                    &src_fs,
                    &src_remote,
                    &destination_fs,
                    &destination_path,
                    progress.clone(),
                    control.clone(),
                )
                .await
            {
                Ok(()) => {
                    tracing::debug!(
                        source = %src.display(),
                        elapsed_ms = started.elapsed().as_millis(),
                        "cloud upload used direct WSL copy"
                    );
                    return Ok(());
                }
                Err(e) => {
                    if control.is_cancelled() || e.is::<OutcomeUnknown>() {
                        return Err(e);
                    }
                    tracing::warn!(
                        source = %src.display(),
                        error = %e,
                        "direct WSL upload failed; falling back to staging"
                    );
                }
            }
        }
        // Mirror of the download staging: UNC sources (WSL/SMB) go through the
        // cache dir so rclone only ever sees plain local paths.
        if is_unc(src) {
            let staged = self.stage_path(src).await?;
            tokio::fs::copy(src, &staged)
                .await
                .with_context(|| format!("staging UNC upload source {}", src.display()))?;
            let (src_fs, src_remote) = Self::local_split(&staged)?;
            let result = self
                .run_copy(
                    &src_fs,
                    &src_remote,
                    &destination_fs,
                    &destination_path,
                    progress,
                    control,
                )
                .await;
            let _ = tokio::fs::remove_file(&staged).await;
            return result;
        }
        let (src_fs, src_remote) = Self::local_split(src)?;
        self.run_copy(
            &src_fs,
            &src_remote,
            &destination_fs,
            &destination_path,
            progress,
            control,
        )
        .await
    }

    async fn about(&self, id: &str) -> anyhow::Result<(u64, u64)> {
        self.rc.about(&format!("{id}:")).await
    }

    async fn copy_between(
        &self,
        source_id: &str,
        source: &str,
        destination_id: &str,
        destination: &str,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()> {
        if control.is_cancelled() {
            return Err(TransferCancelled.into());
        }
        let (destination_fs, destination_path) = self.rc.scope(destination_id, destination).await?;
        if let Some(object) = atlas_model::remote_path::last(source) {
            anyhow::ensure!(
                self.rc.is_drive(source_id).await?,
                "Object addresses require Google Drive"
            );
            if object.is_dir {
                return self
                    .run_directory_copy(
                        &format!("{source_id},root_folder_id={}:", object.id),
                        &super::objects::rooted(&destination_fs, &destination_path),
                        progress,
                        control,
                    )
                    .await;
            }
            self.rc
                .require_missing(&destination_fs, &destination_path)
                .await?;
            let group = format!("cross-account-{}", uuid::Uuid::new_v4().simple());
            let job = self
                .rc
                .copy_id_async(
                    source_id,
                    &object.id,
                    &super::objects::rooted(&destination_fs, &destination_path),
                    &group,
                    control.byte_limit(),
                )
                .await
                .map_err(transfer_start_error)?;
            return self.wait_job(job, progress, control).await;
        }
        anyhow::ensure!(
            !self.rc.is_drive(source_id).await?,
            "Refresh the source account from its root to obtain object IDs"
        );
        let (source_fs, source_path) = self.rc.scope(source_id, source).await?;
        if self
            .rc
            .operations_stat(&source_fs, &source_path)
            .await?
            .get("IsDir")
            .and_then(Value::as_bool)
            == Some(true)
        {
            self.run_directory_copy(
                &super::objects::rooted(&source_fs, &source_path),
                &super::objects::rooted(&destination_fs, &destination_path),
                progress,
                control,
            )
            .await
        } else {
            self.run_copy(
                &source_fs,
                &source_path,
                &destination_fs,
                &destination_path,
                progress,
                control,
            )
            .await
        }
    }

    async fn mkdir(&self, id: &str, path: &str) -> anyhow::Result<()> {
        let (fs, path) = self.rc.scope(id, path).await?;
        self.rc.operations_mkdir(&fs, &path).await
    }

    async fn delete(&self, id: &str, path: &str, is_dir: bool) -> anyhow::Result<()> {
        if self.rc.is_drive(id).await? {
            return self.rc.trash_id(id, path).await;
        }
        let fs = format!("{id}:");
        if is_dir {
            self.rc.operations_purge(&fs, path).await
        } else {
            self.rc.operations_deletefile(&fs, path).await
        }
    }

    async fn rename(&self, id: &str, from: &str, to: &str) -> anyhow::Result<()> {
        if self.rc.is_drive(id).await? {
            return self.rc.rename_id(id, from, to).await;
        }
        let fs = format!("{id}:");
        self.rc.operations_movefile(&fs, from, &fs, to).await
    }

    async fn copy(&self, id: &str, from: &str, to: &str) -> anyhow::Result<()> {
        if let Some(object) = atlas_model::remote_path::last(from) {
            let (destination_fs, destination_path) = self.rc.scope(id, to).await?;
            let target = super::objects::rooted(&destination_fs, &destination_path);
            if object.is_dir {
                anyhow::ensure!(
                    self.rc.is_drive(id).await?,
                    "Object addresses require Google Drive"
                );
                return self
                    .run_directory_copy(
                        &format!("{id},root_folder_id={}:", object.id),
                        &target,
                        std::sync::Arc::new(|_, _| {}),
                        TransferControl::default(),
                    )
                    .await;
            }
            self.rc
                .require_missing(&destination_fs, &destination_path)
                .await?;
            let group = format!("copy-id-{}", uuid::Uuid::new_v4().simple());
            let job = self
                .rc
                .copy_id_async(id, &object.id, &target, &group, None)
                .await?;
            return self
                .wait_job(
                    job,
                    std::sync::Arc::new(|_, _| {}),
                    TransferControl::default(),
                )
                .await;
        }
        anyhow::ensure!(
            !self.rc.is_drive(id).await?,
            "Refresh this account from its root: the source has no immutable object ID"
        );
        let fs = format!("{id}:");
        if self
            .rc
            .operations_stat(&fs, from)
            .await?
            .get("IsDir")
            .and_then(Value::as_bool)
            == Some(true)
        {
            return self
                .run_directory_copy(
                    &format!("{id}:{from}"),
                    &format!("{id}:{to}"),
                    std::sync::Arc::new(|_, _| {}),
                    TransferControl::default(),
                )
                .await;
        }
        self.run_copy(
            &fs,
            from,
            &fs,
            to,
            std::sync::Arc::new(|_, _| {}),
            TransferControl::default(),
        )
        .await
    }

    async fn mount(&self, id: &str, mount_point: &Path) -> anyhow::Result<()> {
        tokio::fs::create_dir_all(mount_point)
            .await
            .context("create mount point")?;
        self.rc
            .mount(
                &format!("{id}:"),
                &mount_point.to_string_lossy(),
                &self.cache_dir.to_string_lossy(),
            )
            .await
    }

    async fn unmount(&self, mount_point: &Path) -> anyhow::Result<()> {
        self.rc.unmount(&mount_point.to_string_lossy()).await
    }

    async fn install_mount_support(&self, progress: ProgressSink) -> anyhow::Result<()> {
        #[cfg(windows)]
        {
            return super::windows_mount::install_winfsp(move |done, total| {
                progress(done, total);
            })
            .await;
        }
        #[cfg(not(windows))]
        anyhow::bail!("mount support is provided by the operating system")
    }
}

fn transfer_start_error(error: anyhow::Error) -> anyhow::Error {
    if error.is::<reqwest::Error>() {
        OutcomeUnknown(
            "Connection lost while starting a backend job. Verify the destination before retrying."
                .into(),
        )
        .into()
    } else {
        error
    }
}

/// Parse an RFC3339 timestamp into unix seconds without pulling in a date crate.
/// Falls back to `None` on anything unexpected; only the date/time prefix matters.
fn chrono_secs(s: &str) -> Option<i64> {
    // Format: 2026-06-02T13:45:30.000000000Z — parse the calendar fields directly.
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let min = num(14, 16)?;
    let sec = num(17, 19)?;
    Some(days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec)
}

/// Days since unix epoch for a civil date (Howard Hinnant's algorithm).
const fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
