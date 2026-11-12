//! Process-only mount identities; never serialized and never used as key storage.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub const NAMESPACE: &str = "atlas-private-mounts";
fn mounts() -> &'static Mutex<HashMap<PathBuf, String>> {
    static MOUNTS: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    MOUNTS.get_or_init(Default::default)
}
pub fn is_private(path: &Path) -> bool {
    path.components().any(|part| {
        part.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(NAMESPACE)
    })
}
pub fn register(path: PathBuf, name: String) {
    mounts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(path, name);
}
pub fn unregister(path: &Path) {
    mounts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(path);
}
pub fn display_name(path: &Path) -> Option<String> {
    mounts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(path)
        .cloned()
}
pub fn mount_root(path: &Path) -> Option<PathBuf> {
    mounts()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .find(|root| path.starts_with(root))
        .cloned()
}
