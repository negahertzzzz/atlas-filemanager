//! rclone-backed implementation of the cloud provider abstraction.

pub mod oauth;
mod objects;
pub mod provider;
pub mod rc_api;
pub mod sidecar;
#[cfg(windows)]
pub mod windows_mount;

pub use provider::RcloneProvider;
