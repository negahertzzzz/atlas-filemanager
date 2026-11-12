//! Real Win32 I/O through WinFsp. Uses only fixtures under the requested directory.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use atlas_core::vault::{self, mount::Mounted, Control};
    use std::{
        fs,
        io::{Read, Seek, SeekFrom, Write},
        path::PathBuf,
    };
    let directory = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or_else(|| anyhow::anyhow!("output directory required"))?,
    );
    fs::create_dir_all(&directory)?;
    if std::env::args().any(|arg| arg == "--install-driver") {
        tokio::runtime::Runtime::new()?.block_on(vault::mount::install_driver())?;
    }
    let source = directory.join("Normal folder");
    fs::create_dir_all(source.join("Nested"))?;
    fs::write(source.join("Notes.txt"), b"private sample content")?;
    let profile = uuid::Uuid::new_v4().to_string();
    let s = vault::protect(
        &source,
        "real mount test password",
        vec![profile.clone()],
        &profile,
        &Control::default(),
    )?;
    let root = s.root().to_path_buf();
    let mounted = Mounted::start(s).map_err(|(_, error)| anyhow::anyhow!(error))?;
    let path = mounted.path().to_path_buf();
    anyhow::ensure!(
        fs::read(path.join("Notes.txt"))? == b"private sample content",
        "native read"
    );
    anyhow::ensure!(
        fs::read_dir(&path)?.count() == 2,
        "native directory enumeration"
    );
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("Notes.txt"))?;
    anyhow::ensure!(mounted.has_open_files(), "native handle tracking");
    file.seek(SeekFrom::Start(8))?;
    file.write_all(b"edited")?;
    file.sync_all()?;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes == b"private edited content", "native overwrite");
    file.set_len(8)?;
    drop(file);
    fs::write(path.join("Office save.tmp"), b"new saved document")?;
    fs::rename(path.join("Office save.tmp"), path.join("Saved.txt"))?;
    fs::create_dir(path.join("New folder"))?;
    fs::rename(
        path.join("Saved.txt"),
        path.join("New folder").join("Saved.txt"),
    )?;
    anyhow::ensure!(
        fs::read(path.join("New folder").join("Saved.txt"))? == b"new saved document",
        "native save and move"
    );
    fs::remove_file(path.join("New folder").join("Saved.txt"))?;
    fs::remove_dir(path.join("New folder"))?;
    fs::write(path.join("Replace.txt"), b"old version")?;
    let mut old = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.join("Replace.txt"))?;
    fs::write(path.join("Replace.tmp"), b"new version")?;
    let sharing = fs::rename(path.join("Replace.tmp"), path.join("Replace.txt"));
    anyhow::ensure!(
        sharing
            .as_ref()
            .err()
            .and_then(std::io::Error::raw_os_error)
            == Some(5),
        "replacement must match NTFS sharing rules"
    );
    let mut original = Vec::new();
    old.read_to_end(&mut original)
        .context("read the old native handle")?;
    anyhow::ensure!(
        original == b"old version",
        "replacement invalidated open inode"
    );
    old.seek(SeekFrom::Start(0))?;
    old.write_all(b"OLD")
        .context("write the old native handle")?;
    old.sync_all().context("flush the old native handle")?;
    drop(old);
    fs::rename(path.join("Replace.tmp"), path.join("Replace.txt"))
        .context("replace after closing the target")?;
    anyhow::ensure!(
        fs::read(path.join("Replace.txt"))? == b"new version",
        "old handle overwrote replacement"
    );
    fs::write(path.join("Mapped.bin"), vec![123; 2 * 1024 * 1024 + 97])?;
    anyhow::ensure!(
        fs::metadata(path.join("Mapped.bin"))?.len() == 2 * 1024 * 1024 + 97,
        "native large write"
    );
    let session = mounted.into_session();
    anyhow::ensure!(!path.exists(), "mount survived lock");
    session.verify(&Control::default())?;
    drop(session);
    let mounted = Mounted::start(vault::unlock(&root, "real mount test password", &profile)?)
        .map_err(|(_, error)| anyhow::anyhow!(error))?;
    anyhow::ensure!(
        fs::read(mounted.path().join("Notes.txt"))? == b"private ",
        "edit lost after lock"
    );
    anyhow::ensure!(
        fs::read(mounted.path().join("Mapped.bin"))? == vec![123; 2 * 1024 * 1024 + 97],
        "large write lost"
    );
    drop(mounted);
    fs::write(directory.join("passed.txt"), "Native list; read; write; flush; truncate; create; rename; move; delete; Windows replacement sharing rules; large file; lock; unlock; durable edits: passed.\n")?;
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    panic!("Windows native integration diagnostic");
}
