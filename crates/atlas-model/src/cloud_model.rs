//! Cloud domain model.
//!
//! These are inert IDs, configuration records and transfer snapshots. Backend
//! behavior lives in `crate::cloud`; keeping the data here lets state/model stay
//! independent from rclone and filesystem implementation details.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// rclone remote name. Must be unique and free of spaces/`:`; used as the `name:`
/// fs prefix in every rc call.
pub type RemoteId = String;

/// Which protocol a remote speaks. Each variant carries the non-secret connection
/// fields needed to (re)build the rclone config; the password/key is supplied
/// once at creation time and handed straight to rclone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteKind {
    Sftp {
        host: String,
        port: u16,
        user: String,
    },
    Ftp {
        host: String,
        port: u16,
        user: String,
    },
    WebDav {
        url: String,
        vendor: String,
        user: String,
    },
    /// SMB/CIFS share. rclone lists shares as top-level directories.
    Smb {
        host: String,
        port: u16,
        user: String,
        /// Windows/Active Directory domain. Empty means rclone's default
        /// (`WORKGROUP`), which is right for NAS boxes and local accounts.
        #[serde(default)]
        domain: String,
    },
    /// Google Drive (OAuth; browser approval, no stored password).
    GoogleDrive,
    /// Microsoft `OneDrive` (OAuth).
    OneDrive,
}

/// Split an SMB login typed as `DOMAIN\user` (or `DOMAIN/user`) into
/// `(user, domain)`. `user@domain.tld` stays whole: NTLM accepts the UPN form.
pub fn split_smb_login(login: &str) -> (String, String) {
    let login = login.trim();
    match login.split_once(['\\', '/']) {
        Some((domain, user)) if !domain.is_empty() && !user.is_empty() => {
            (user.trim().to_owned(), domain.trim().to_owned())
        }
        _ => (login.to_owned(), String::new()),
    }
}

#[cfg(test)]
mod smb_login_tests {
    #[test]
    fn domain_logins_are_split_and_other_forms_kept() {
        use super::split_smb_login as split;
        assert_eq!(split(r"CONTOSO\mario"), ("mario".into(), "CONTOSO".into()));
        assert_eq!(split("CONTOSO/mario"), ("mario".into(), "CONTOSO".into()));
        assert_eq!(
            split(" mario@contoso.local "),
            ("mario@contoso.local".into(), String::new())
        );
        assert_eq!(split("guest"), ("guest".into(), String::new()));
        assert_eq!(split(r"\mario"), (r"\mario".into(), String::new()));
    }
}

impl RemoteKind {
    /// rclone backend type string (the `type` parameter of `config/create`).
    pub const fn rclone_type(&self) -> &'static str {
        match self {
            Self::Sftp { .. } => "sftp",
            Self::Ftp { .. } => "ftp",
            Self::WebDav { .. } => "webdav",
            Self::Smb { .. } => "smb",
            Self::GoogleDrive => "drive",
            Self::OneDrive => "onedrive",
        }
    }

    /// Short human label for the UI.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Sftp { .. } => "SFTP",
            Self::Ftp { .. } => "FTP",
            Self::WebDav { .. } => "WebDAV",
            Self::Smb { .. } => "SMB",
            Self::GoogleDrive => "Google Drive",
            Self::OneDrive => "OneDrive",
        }
    }

    /// OAuth backends authenticate via the browser instead of a password.
    pub const fn is_oauth(&self) -> bool {
        matches!(self, Self::GoogleDrive | Self::OneDrive)
    }
}

/// How the user wants to reach a remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AccessKind {
    /// Browse/upload/download through the provider; nothing is mounted.
    #[default]
    Access,
    /// FUSE-mounted via rclone so it appears as a real filesystem path.
    Mount,
}

/// Runtime mount/connection status (not persisted).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RemoteStatus {
    #[default]
    Idle,
    Connecting,
    Mounting,
    Mounted(PathBuf),
    /// Drive mounting not available (e.g. WinFsp absent); falling back to
    /// browse-via-API (Access mode). Not an error — click still browses.
    BrowseFallback(String),
    Error(String),
}

/// A configured remote as the app knows it. `status` is runtime-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudRemote {
    pub id: RemoteId,
    pub name: String,
    pub kind: RemoteKind,
    pub access: AccessKind,
    #[serde(default)]
    pub auto_mount: bool,
    #[serde(skip)]
    pub status: RemoteStatus,
    /// Storage usage `(used, total)` in bytes, fetched after connecting. Runtime only.
    #[serde(skip)]
    pub quota: Option<(u64, u64)>,
}

impl CloudRemote {
    /// The rclone fs prefix for this remote, e.g. `myserver:`.
    #[allow(dead_code)] // reason: mount/access UI uses this when cloud flows are enabled
    pub fn fs(&self) -> String {
        format!("{}:", self.id)
    }
}

pub type TransferId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferKind {
    Download,
    Upload,
    RemoteCopy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSource {
    pub id: RemoteId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransferState {
    Queued,
    Running,
    Paused,
    Pausing,
    Cancelling,
    Cancelled,
    /// The backend may have committed or still be writing. No blind retry.
    Interrupted(String),
    Done,
    Failed(String),
}

impl TransferState {
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed(_) | Self::Cancelled | Self::Interrupted(_)
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transfer {
    pub id: TransferId,
    pub kind: TransferKind,
    /// File name for display.
    #[allow(dead_code)] // reason: transfer manager UI uses this when cloud flows are enabled
    pub label: String,
    pub remote_id: RemoteId,
    pub remote_path: String,
    pub local_path: PathBuf,
    pub state: TransferState,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub retries: u32,
    /// After a download completes, open the file with the OS handler.
    pub open_on_done: bool,
    /// After an upload completes, move the local source to trash (move-semantics).
    pub delete_source: bool,
    #[serde(default)]
    pub source: Option<RemoteSource>,
}

impl Transfer {
    #[allow(dead_code)] // reason: transfer progress UI uses this when cloud flows are enabled
    pub fn fraction(&self) -> f32 {
        if self.bytes_total == 0 {
            return 0.0;
        }
        (self.bytes_done as f32 / self.bytes_total as f32).clamp(0.0, 1.0)
    }
}

/// Immutable list snapshot handed to the UI each time the manager changes.
pub type TransferSnapshot = Vec<Transfer>;

pub fn preview_key(account: &str, generation: u64, path: &std::path::Path) -> PathBuf {
    PathBuf::from(format!(
        "remote-preview:{account}/{generation}/{}",
        path.to_string_lossy()
    ))
}
