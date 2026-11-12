//! Install the provider shipped with this build without leaving partial binaries.
use anyhow::Context as _;
use sha2::{Digest as _, Sha256};
use std::{
    io::Read as _,
    path::{Path, PathBuf},
};

fn digest(path: &Path) -> anyhow::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().to_vec())
}

fn install_binary(source: &Path, target: &Path) -> anyhow::Result<()> {
    if target.is_file() && digest(source)? == digest(target)? {
        return Ok(());
    }
    let parent = target
        .parent()
        .context("provider has no installation directory")?;
    std::fs::create_dir_all(parent)?;
    let temporary = tempfile::NamedTempFile::new_in(parent)?;
    std::fs::copy(source, temporary.path())?;
    std::fs::set_permissions(temporary.path(), std::fs::metadata(source)?.permissions())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(target)
        .map_err(|error| error.error)
        .context("Cannot update the Cloud extension; close other Atlas instances and retry")?;
    Ok(())
}

pub fn prepare() -> anyhow::Result<(PathBuf, atlas_model::ext_model::Sidecar)> {
    let directory = crate::ext::extensions_dir().join("cloud");
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let resources = workspace.join("extensions/cloud");
    std::fs::create_dir_all(&directory)?;
    if !directory.join("extension.toml").is_file() && resources.join("extension.toml").is_file() {
        std::fs::copy(
            resources.join("extension.toml"),
            directory.join("extension.toml"),
        )?;
        if resources.join("icon.svg").is_file() {
            std::fs::copy(resources.join("icon.svg"), directory.join("icon.svg"))?;
        }
    }
    let binary = if cfg!(windows) {
        "atlas-cloud-extension.exe"
    } else {
        "atlas-cloud-extension"
    };
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join(binary)));
    let development = cfg!(debug_assertions).then(|| workspace.join("target/debug").join(binary));
    if let Some(source) = sibling
        .into_iter()
        .chain(development)
        .find(|path| path.is_file())
    {
        if source.canonicalize().ok() != directory.join(binary).canonicalize().ok() {
            install_binary(&source, &directory.join(binary))?;
            // The provider resolves rclone beside itself. A host-side companion
            // alone is invisible after copying the provider to extensions/cloud.
            let rclone = if cfg!(windows) {
                "rclone.exe"
            } else {
                "rclone"
            };
            if let Some(bundled) = source
                .parent()
                .map(|parent| parent.join(rclone))
                .filter(|path| path.is_file())
            {
                install_binary(&bundled, &directory.join(rclone))?;
            }
        }
    }
    let manifest = atlas_model::ext_model::Manifest::parse(
        &std::fs::read_to_string(directory.join("extension.toml"))
            .context("Cloud extension is not installed")?,
    )?;
    Ok((
        directory,
        manifest.sidecar.context("Cloud extension has no sidecar")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installing_a_new_build_replaces_an_old_protocol_binary_completely() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("new-provider");
        let target = directory.path().join("installed-provider");
        std::fs::write(&source, b"protocol-two-build").unwrap();
        std::fs::write(&target, b"old-protocol-build").unwrap();
        install_binary(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"protocol-two-build");
        install_binary(&source, &target).unwrap();
    }
}
