//! Atlas-owned disk analysis. No external cleaner or shell scripts are executed.
mod duplicates;
mod execution;
#[cfg(windows)]
mod mft;
mod policy;
mod scan;
mod tree;
mod walk;

pub use execution::{execute, prepare_plan, prepare_plan_in};
pub use policy::{categories, is_cleanup_root, is_drive_root, protected, user_temp_root, Category};
pub use scan::{ScanJob, ScanMode};
use std::path::PathBuf;
pub use duplicates::{DuplicateJob, DEFAULT_MIN_BYTES as DUPLICATE_MIN_BYTES};
pub use tree::{cached_snapshot, cached_treemap, cached_types, invalidate as invalidate_tree};

/// Open and identify the given files in parallel; files that vanished, are
/// locked or are otherwise unsafe are simply left out.
pub fn inspect_candidates(
    paths: &[PathBuf],
) -> Vec<atlas_model::cleanup::CleanupCandidate> {
    #[cfg(windows)]
    {
        use rayon::prelude::*;
        paths
            .par_iter()
            .filter_map(|p| atlas_win_space::cleanup::inspect(p).ok())
            .collect()
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        Vec::new()
    }
}

/// Roots the analyzer opens on: a drive (local or mapped network drive) or an
/// SMB share (`\server\share`).
pub fn is_analysis_root(path: &std::path::Path) -> bool {
    policy::is_drive_root(path) || crate::fs::network::is_share_root(path)
}

/// Move one analyzed entry to the Recycle Bin and update the cached totals.
pub fn move_to_trash(path: &std::path::Path) -> std::io::Result<()> {
    // SMB shares have no Recycle Bin: Windows would delete permanently.
    if crate::fs::network::is_network_path(path) {
        return Err(std::io::Error::other("cleanup_network_no_trash"));
    }
    let blocked = policy::is_drive_root(path)
        || policy::protected(path)
        || crate::vault::is_private_path(path)
        || crate::vault::live::is_private(path)
        || system_entry(path);
    if blocked {
        return Err(std::io::Error::other("cleanup_scope_changed"));
    }
    std::fs::symlink_metadata(path)?;
    trash::delete(path).map_err(std::io::Error::other)?;
    tree::remove(path);
    Ok(())
}

/// Operating-system folders (and the Users root itself) are never offered.
fn system_entry(path: &std::path::Path) -> bool {
    use std::path::Component;
    let mut parts = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        });
    match parts.next().as_deref() {
        Some(
            "windows" | "program files" | "program files (x86)" | "programdata" | "$recycle.bin"
            | "system volume information" | "recovery" | "boot",
        ) => true,
        Some("users") => parts.next().is_none(),
        _ => false,
    }
}

/// Large read-only indices are released away from the GUI thread.
pub fn release_analysis_cache() {
    tree::invalidate();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trash_moves_a_real_file_and_refuses_system_roots() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("atlas-trash-test.bin");
        std::fs::write(&file, [1u8; 64]).unwrap();
        move_to_trash(&file).unwrap();
        assert!(!file.exists());
        assert!(move_to_trash(std::path::Path::new("C:\\")).is_err());
        assert!(move_to_trash(std::path::Path::new(r"C:\Windows\System32")).is_err());
        assert!(move_to_trash(std::path::Path::new(r"C:\Users")).is_err());
        assert!(move_to_trash(&dir.path().join("missing")).is_err());
    }
}
