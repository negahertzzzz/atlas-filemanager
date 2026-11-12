//! Cloud / network mounting extension (feature `cloud`).
//!
//! Architecture mirrors the tag-db worker: the UI never touches rclone directly.
//! It sends [`CloudCmd`] over a channel to a background worker that owns the
//! rclone daemon + provider, and receives answers back through the shared
//! `BgResult` channel drained on the UI thread.
//!
//! Two ways to reach a remote (see [`remote::AccessKind`]):
//! * **Access** — list/transfer via the provider; entries flow through the
//!   normal `BgResult::DirListed` path as a virtual `TabSource::Remote` tab.
//! * **Mount** — FUSE-mounted via rclone so it appears as a real path and every
//!   existing file-view code path works unchanged.

pub mod control;
mod installation;
mod journal;
pub mod preview;
pub mod provider;
#[cfg(feature = "cloud-backend")]
pub mod rclone;
pub mod remote;
pub mod sidecar_provider;
mod staging;
pub mod transfer;
mod worker;

pub use remote::{AccessKind, CloudRemote, RemoteId, RemoteKind, RemoteStatus};
pub use transfer::TransferId;
pub use worker::{spawn, CloudCmd};

use std::path::PathBuf;

/// Directory under which remotes are FUSE-mounted (`<data>/mounts/<id>`).
pub fn mounts_dir() -> PathBuf {
    crate::util::config::data_dir().join("mounts")
}

pub fn mount_point_for(id: &str) -> PathBuf {
    mounts_dir().join(id)
}

/// Local directory where Access-mode downloads land by default.
pub fn downloads_dir() -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| crate::util::config::data_dir().join("downloads"))
}

/// Opening a remote object materializes an isolated copy. Two accounts or two
/// homonymous objects never overwrite or open each other's local materialization.
pub fn open_cache_dir(account: &str, generation: u64, source: &str) -> PathBuf {
    use sha2::{Digest as _, Sha256};
    let mut digest = Sha256::new();
    digest.update(account.as_bytes());
    digest.update([0]);
    digest.update(generation.to_le_bytes());
    digest.update(source.as_bytes());
    crate::util::config::data_dir()
        .join("cloud-open")
        .join(format!("{:x}", digest.finalize()))
}
