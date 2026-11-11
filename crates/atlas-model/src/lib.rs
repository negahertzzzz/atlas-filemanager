//! `atlas-model`: framework-neutral application state.
//!
//! IDs, serializable state, requests/results — no filesystem, cloud, extension or
//! UI runtime. Moved here from the legacy `src/state` tree (Phase J); the binary
//! re-exports it as `crate::state` via a shim so existing paths keep resolving.

pub mod app_state;
pub mod batch_rename;
pub mod clamp;
pub mod cleanup;
pub mod compare;
pub mod edition;
#[cfg(feature = "provider-host")]
pub mod cloud_model;
#[cfg(feature = "provider-host")]
pub mod remote_path;
#[cfg(feature = "provider-host")]
pub mod cloud_state;
pub mod ext_model;
pub mod ext_state;
pub mod file_entry;
pub mod grouping;
pub mod layout_tree;
pub mod location;
pub mod ops;
pub mod overlay;
pub mod pane;
pub mod preview_cache;
pub mod profiles;
pub mod provider_protocol;
pub mod settings;
pub mod sync;
pub mod tab;
pub mod tag_cache;
pub mod undo;
