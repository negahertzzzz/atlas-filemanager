//! Backend-agnostic provider abstraction.
//!
//! The rest of the app talks to a `dyn CloudProvider`, never to rclone directly,
//! so an OpenDAL/native implementation can be slotted in later without touching
//! the UI or worker. v1 ships a single impl: [`super::rclone::RcloneProvider`].
//!
//! Deliberately complete abstraction; some methods/fields are consumed only by
//! later phases (write ops, capability-gated UI), hence the module-level allow.
#![allow(dead_code)]

use super::control::TransferControl;
use std::path::Path;

/// A single entry returned by [`CloudProvider::list`] / `stat`.
#[derive(Debug, Clone)]
pub struct RemoteEntry {
    /// File/dir name (last path component).
    pub name: String,
    /// Remote-relative path from the remote root.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Modification time as a unix timestamp (seconds), if known.
    pub modified: Option<i64>,
    pub mime: Option<String>,
}

/// What a backend can do — lets the UI hide unsupported actions.
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub can_mount: bool,
    pub can_write: bool,
    /// True if random reads are cheap (FUSE/VFS cached) vs. whole-file fetch.
    pub random_read: bool,
}

/// Progress callback for a transfer: `(bytes_done, bytes_total)`.
pub type ProgressSink = std::sync::Arc<dyn Fn(u64, u64) + Send + Sync>;

#[async_trait::async_trait]
pub trait CloudProvider: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    async fn prepare_shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Create/overwrite the backend config for remote `id`. `parameters` are
    /// backend-specific; secret values are obscured by the backend, not stored
    /// by the app.
    async fn configure(
        &self,
        id: &str,
        backend_type: &str,
        parameters: serde_json::Value,
    ) -> anyhow::Result<()>;

    /// Configure an OAuth backend, driving the interactive auth flow (opens the
    /// browser). Blocks until the user approves or it fails.
    async fn configure_oauth(
        &self,
        id: &str,
        backend_type: &str,
        parameters: serde_json::Value,
    ) -> anyhow::Result<()>;

    /// Remove remote `id`'s backend config.
    async fn deconfigure(&self, id: &str) -> anyhow::Result<()>;

    /// List the directory `path` (remote-relative) of remote `id`.
    async fn list(&self, id: &str, path: &str) -> anyhow::Result<Vec<RemoteEntry>>;

    /// Download a remote file or directory to a local path, reporting progress.
    async fn download(
        &self,
        id: &str,
        src: &str,
        dst: &Path,
        progress: ProgressSink,
    ) -> anyhow::Result<()> {
        self.download_controlled(id, src, dst, progress, TransferControl::default())
            .await
    }

    async fn download_controlled(
        &self,
        id: &str,
        src: &str,
        dst: &Path,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()>;

    /// Upload a local file or directory to a remote path, reporting progress.
    async fn upload(
        &self,
        id: &str,
        src: &Path,
        dst: &str,
        progress: ProgressSink,
    ) -> anyhow::Result<()> {
        self.upload_controlled(id, src, dst, progress, TransferControl::default())
            .await
    }

    async fn upload_controlled(
        &self,
        id: &str,
        src: &Path,
        dst: &str,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()>;

    async fn copy_between(
        &self,
        source_id: &str,
        source: &str,
        destination_id: &str,
        destination: &str,
        progress: ProgressSink,
        control: TransferControl,
    ) -> anyhow::Result<()>;

    /// Storage usage `(used, total)` in bytes for the remote, if reported.
    async fn about(&self, id: &str) -> anyhow::Result<(u64, u64)>;

    async fn mkdir(&self, id: &str, path: &str) -> anyhow::Result<()>;
    async fn delete(&self, id: &str, path: &str, is_dir: bool) -> anyhow::Result<()>;
    async fn rename(&self, id: &str, from: &str, to: &str) -> anyhow::Result<()>;
    /// Server-side copy (no local download). Falls back to download+upload if unsupported.
    async fn copy(&self, id: &str, from: &str, to: &str) -> anyhow::Result<()>;

    /// FUSE-mount the remote root at `mount_point` (requires `can_mount`).
    async fn mount(&self, id: &str, mount_point: &Path) -> anyhow::Result<()>;
    async fn unmount(&self, mount_point: &Path) -> anyhow::Result<()>;

    async fn install_mount_support(&self, progress: ProgressSink) -> anyhow::Result<()>;
}
