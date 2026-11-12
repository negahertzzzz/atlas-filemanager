//! TAR archive provider (supports plain tar, .tar.gz, .tar.bz2, .tar.xz).

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use bzip2::read::BzDecoder;
use xz2::read::XzDecoder;

use super::bomb_guard::BombGuard;
use super::common::{build_synthetic_ancestors, path_to_dir_key};
use super::sanitize::sanitize_inner_path;
use super::{make_entry, ArchiveProvider};
use atlas_model::file_entry::FileEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Bzip2,
    Xz,
    Zst,
}

#[derive(Debug, Clone)]
struct TarEntryMeta {
    is_dir: bool,
    size: Option<u64>,
    entry_index: usize,
    raw_file_pos: u64,
}

pub struct TarProvider {
    path: PathBuf,
    compression: Compression,
    /// `inner_dir` key → direct children `FileEntry` list
    index: HashMap<String, Vec<FileEntry>>,
    /// `inner_path` string → metadata for on-demand extraction / reading
    entries_meta: HashMap<String, TarEntryMeta>,
}

impl TarProvider {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        Self::new(path, Compression::None)
    }

    pub fn new(path: &Path, compression: Compression) -> anyhow::Result<Self> {
        let mut index: HashMap<String, Vec<FileEntry>> = HashMap::new();
        let mut entries_meta: HashMap<String, TarEntryMeta> = HashMap::new();
        index.entry(String::new()).or_default();

        let mut guard = BombGuard::default();
        let file = std::fs::File::open(path)?;
        let reader: Box<dyn Read + Send> = match compression {
            Compression::None => Box::new(file),
            Compression::Gzip => Box::new(flate2::read::GzDecoder::new(file)),
            Compression::Bzip2 => Box::new(BzDecoder::new(file)),
            Compression::Xz => Box::new(XzDecoder::new(file)),
            Compression::Zst => anyhow::bail!("zstd tar archives are not supported yet"),
        };
        let mut ar = tar::Archive::new(reader);

        for (idx, entry_result) in ar.entries()?.enumerate() {
            let entry = match entry_result {
                Ok(e) => e,
                Err(_) => break,
            };
            let raw_path = entry.path()?.into_owned();

            // SEC-05: Sanitize path using sanitize_inner_path to avoid traversal/tar bomb
            let Some(inner) = sanitize_inner_path(&raw_path) else {
                continue;
            };

            let is_dir = entry.header().entry_type().is_dir();
            let size = if is_dir {
                None
            } else {
                entry.header().size().ok()
            };

            let depth = inner.components().count() as u32;
            guard
                .check_entry(0, depth)
                .map_err(|e| anyhow::anyhow!("{e}"))?;

            let name = inner
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }

            let parent_key = inner.parent().map(path_to_dir_key).unwrap_or_default();
            build_synthetic_ancestors(path, &inner, &mut index);

            let inner_key = inner.to_string_lossy().replace('\\', "/");
            let raw_file_pos = entry.raw_file_position();

            entries_meta.insert(
                inner_key,
                TarEntryMeta {
                    is_dir,
                    size,
                    entry_index: idx,
                    raw_file_pos,
                },
            );

            let file_entry = make_entry(path, inner, name, is_dir, size);
            index.entry(parent_key).or_default().push(file_entry);
        }

        Ok(Self {
            path: path.to_path_buf(),
            compression,
            index,
            entries_meta,
        })
    }

    fn open_reader(&self) -> anyhow::Result<Box<dyn Read + Send>> {
        let file = std::fs::File::open(&self.path)?;
        let reader: Box<dyn Read + Send> = match self.compression {
            Compression::None => Box::new(file),
            Compression::Gzip => Box::new(flate2::read::GzDecoder::new(file)),
            Compression::Bzip2 => Box::new(BzDecoder::new(file)),
            Compression::Xz => Box::new(XzDecoder::new(file)),
            Compression::Zst => anyhow::bail!("zstd tar archives are not supported yet"),
        };
        Ok(reader)
    }
}

impl ArchiveProvider for TarProvider {
    fn list(&self, inner_dir: &Path) -> anyhow::Result<Vec<FileEntry>> {
        let key = path_to_dir_key(inner_dir);
        Ok(self.index.get(&key).cloned().unwrap_or_default())
    }

    fn read_bytes(&self, inner_path: &Path) -> anyhow::Result<Box<dyn Read + Send>> {
        let key = inner_path.to_string_lossy().replace('\\', "/");
        let meta = self
            .entries_meta
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("entry not found: {key}"))?;

        if meta.is_dir {
            return Ok(Box::new(Cursor::new(Vec::new())));
        }

        // Fast path for uncompressed tar: seek directly without loading full file into RAM
        if self.compression == Compression::None {
            use std::io::Seek;
            let mut file = std::fs::File::open(&self.path)?;
            file.seek(std::io::SeekFrom::Start(meta.raw_file_pos))?;
            let take_len = meta.size.unwrap_or(0);
            let mut guard = BombGuard::default();
            let mut buf = Vec::with_capacity(std::cmp::min(take_len, 8 * 1024 * 1024) as usize);
            let mut chunk = [0u8; 64 * 1024];
            let mut limited = file.take(take_len);
            loop {
                let n = limited.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                guard
                    .check_chunk(n as u64)
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                buf.extend_from_slice(&chunk[..n]);
            }
            return Ok(Box::new(Cursor::new(buf)));
        }

        // Compressed archives: stream on demand until the requested entry
        let reader = self.open_reader()?;
        let mut ar = tar::Archive::new(reader);
        for (idx, entry_result) in ar.entries()?.enumerate() {
            if idx == meta.entry_index {
                let mut entry = entry_result?;
                let size_hint = meta.size.unwrap_or(0);
                let mut buf =
                    Vec::with_capacity(std::cmp::min(size_hint, 8 * 1024 * 1024) as usize);
                let mut guard = BombGuard::default();
                let mut chunk = [0u8; 64 * 1024];
                loop {
                    let n = entry.read(&mut chunk)?;
                    if n == 0 {
                        break;
                    }
                    guard
                        .check_chunk(n as u64)
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    buf.extend_from_slice(&chunk[..n]);
                }
                return Ok(Box::new(Cursor::new(buf)));
            }
        }

        anyhow::bail!("entry not found in archive stream: {key}")
    }

    fn extract_to(&self, inner_path: &Path, dest: &Path) -> anyhow::Result<()> {
        let key = inner_path.to_string_lossy().replace('\\', "/");
        let meta = self
            .entries_meta
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("entry not found: {key}"))?;

        super::common::ensure_parent_dir(dest)?;
        if meta.is_dir {
            #[allow(clippy::disallowed_methods)]
            std::fs::create_dir_all(dest)?;
            return Ok(());
        }

        let mut out = std::fs::File::create(dest)?;
        let mut reader = self.read_bytes(inner_path)?;
        let mut guard = BombGuard::default();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            guard
                .check_chunk(n as u64)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            use std::io::Write as _;
            out.write_all(&chunk[..n])?;
        }
        Ok(())
    }

    fn archive_path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tar_backend_respects_size_limits() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_path = tmp.path().join("bomb.tar");

        let file = std::fs::File::create(&tar_path).unwrap();
        let mut builder = tar::Builder::new(file);

        let mut header = tar::Header::new_gnu();
        header.set_path("huge_file.bin").unwrap();
        header.set_size(10 * 1024 * 1024 * 1024); // 10 GB
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();
        builder.finish().unwrap();

        let provider = TarProvider::new(&tar_path, Compression::None);
        assert!(
            provider.is_ok(),
            "TarProvider::new must succeed without loading 10GB into RAM: {:?}",
            provider.err()
        );
        let p = provider.unwrap();
        let entries = p.list(Path::new("")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "huge_file.bin");
    }

    #[test]
    fn test_tar_backend_drops_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let tar_path = tmp.path().join("traversal.tar");

        let file = std::fs::File::create(&tar_path).unwrap();
        let mut builder = tar::Builder::new(file);

        let mut header = tar::Header::new_gnu();
        // Manually place traversal path into header bytes
        let path_bytes = b"../../etc/passwd\0";
        header.as_mut_bytes()[..path_bytes.len()].copy_from_slice(path_bytes);
        header.set_size(0);
        header.set_cksum();
        builder.append(&header, std::io::empty()).unwrap();

        let mut header2 = tar::Header::new_gnu();
        header2.set_path("safe.txt").unwrap();
        let content = b"hello world";
        header2.set_size(content.len() as u64);
        header2.set_cksum();
        builder.append(&header2, &content[..]).unwrap();
        builder.finish().unwrap();

        let provider = TarProvider::new(&tar_path, Compression::None).unwrap();
        let entries = provider.list(Path::new("")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "safe.txt");

        let mut read = provider.read_bytes(Path::new("safe.txt")).unwrap();
        let mut out = Vec::new();
        read.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"hello world");

        let dest = tmp.path().join("extracted_safe.txt");
        provider.extract_to(Path::new("safe.txt"), &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"hello world");
    }

    #[test]
    fn test_tar_gz_on_demand_read_and_extract() {
        let tmp = tempfile::tempdir().unwrap();
        let tgz_path = tmp.path().join("archive.tar.gz");

        let file = std::fs::File::create(&tgz_path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);

        let mut header = tar::Header::new_gnu();
        header.set_path("hello.txt").unwrap();
        let content = b"gz content on demand";
        header.set_size(content.len() as u64);
        header.set_cksum();
        builder.append(&header, &content[..]).unwrap();
        let enc = builder.into_inner().unwrap();
        enc.finish().unwrap();

        let provider = TarProvider::new(&tgz_path, Compression::Gzip).unwrap();
        let entries = provider.list(Path::new("")).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello.txt");

        let mut read = provider.read_bytes(Path::new("hello.txt")).unwrap();
        let mut out = Vec::new();
        read.read_to_end(&mut out).unwrap();
        assert_eq!(out, b"gz content on demand");

        let dest = tmp.path().join("extracted_hello.txt");
        provider.extract_to(Path::new("hello.txt"), &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"gz content on demand");
    }
}
