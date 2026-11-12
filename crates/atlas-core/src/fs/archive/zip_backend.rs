//! ZIP archive provider.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::common::{build_synthetic_ancestors, ensure_parent_dir, path_to_dir_key};
use super::{make_entry, ArchiveProvider};
use atlas_model::file_entry::FileEntry;

/// Entries whose header declares at most this many bytes are read into
/// memory; larger ones are streamed so a preview or copy never holds a whole
/// video, or the size a forged header claims, in RAM.
const IN_MEMORY_ENTRY: u64 = 32 * 1024 * 1024;
/// Chunk size and queue depth of the streaming reader (≈ 256 KiB in flight).
const STREAM_CHUNK: usize = 64 * 1024;
const STREAM_DEPTH: usize = 4;

/// `Read + Seek` over the archive file that opens it on first use. Clones
/// start closed, so the cached provider keeps the parsed central directory
/// without holding the archive open: it can still be moved, renamed or
/// deleted while the user browses it.
struct ReopeningFile {
    path: Arc<Path>,
    len: u64,
    pos: u64,
    file: Option<File>,
}

impl ReopeningFile {
    fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self {
            path: Arc::from(path),
            len,
            pos: 0,
            file: Some(file),
        })
    }

    fn file(&mut self) -> io::Result<&mut File> {
        match &mut self.file {
            Some(file) => Ok(file),
            slot => {
                let mut file = File::open(&*self.path)?;
                file.seek(SeekFrom::Start(self.pos))?;
                Ok(slot.insert(file))
            }
        }
    }
}

impl Clone for ReopeningFile {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            len: self.len,
            pos: self.pos,
            file: None,
        }
    }
}

impl Read for ReopeningFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.file()?.read(buf)?;
        self.pos += read as u64;
        Ok(read)
    }
}

impl Seek for ReopeningFile {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek in archive"))?;
        if let Some(file) = &mut self.file {
            file.seek(SeekFrom::Start(target))?;
        }
        self.pos = target;
        Ok(target)
    }
}

fn oversized_entry() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "ZIP entry is larger than its header declares",
    )
}

/// Streams an entry produced on a worker thread. Dropping the reader stops
/// the worker at its next chunk, so a caller that aborts (cancel, bomb guard)
/// never inflates the rest of the entry.
struct StreamedEntry {
    chunks: std::sync::mpsc::Receiver<io::Result<Vec<u8>>>,
    current: Vec<u8>,
    offset: usize,
}

impl Read for StreamedEntry {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.offset >= self.current.len() {
            match self.chunks.recv() {
                Ok(Ok(chunk)) => {
                    self.current = chunk;
                    self.offset = 0;
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => return Ok(0),
            }
        }
        let count = out.len().min(self.current.len() - self.offset);
        out[..count].copy_from_slice(&self.current[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}

/// Pre-indexed, read-only view of a `.zip` file.
///
/// The central directory is parsed once at construction; subsequent `list()`
/// calls are O(entries in directory) with no disk I/O, and reads clone the
/// parsed archive instead of re-reading the central directory each time.
pub struct ZipProvider {
    path: PathBuf,
    /// `inner_dir` (with trailing '/') → direct children entries
    index: HashMap<String, Vec<FileEntry>>,
    /// Parsed archive with a closed reader; clone it to read an entry.
    archive: zip::ZipArchive<ReopeningFile>,
}

impl ZipProvider {
    pub fn new(path: &Path) -> anyhow::Result<Self> {
        let mut zip = zip::ZipArchive::new(ReopeningFile::open(path)?)?;
        let mut index: HashMap<String, Vec<FileEntry>> = HashMap::new();

        // Seed the root so `list("")` works even for empty archives.
        index.entry(String::new()).or_default();

        for i in 0..zip.len() {
            let entry = zip.by_index(i)?;
            // enclosed_name() rejects zip-slip paths (../, absolute, etc.).
            let mut inner = match entry.enclosed_name() {
                Some(n) => n.to_path_buf(),
                None => continue,
            };
            let s = inner.to_string_lossy();
            if s.ends_with('/') || s.ends_with('\\') {
                let mut s = s.into_owned();
                s.pop();
                if s.is_empty() {
                    continue;
                }
                inner = PathBuf::from(s);
            }
            // Names that are not valid text cannot be shown or opened.
            let Ok(raw_name) = entry.name() else { continue };
            // enclosed_name() turns "/x" or "C:/x" into "x", a name the entry
            // cannot be opened by; such entries stay hidden as before.
            let stored = Path::new(&*raw_name);
            if stored.has_root()
                || matches!(
                    stored.components().next(),
                    Some(std::path::Component::Prefix(_))
                )
            {
                continue;
            }
            // Skip __MACOSX cruft and other meta entries.
            if raw_name.starts_with("__MACOSX/") || raw_name.ends_with(".DS_Store") {
                continue;
            }
            let is_dir = entry.is_dir();
            let size = if is_dir { None } else { Some(entry.size()) };
            let name = inner
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }

            let parent_key = inner.parent().map(path_to_dir_key).unwrap_or_default();
            build_synthetic_ancestors(path, &inner, &mut index);

            let file_entry = make_entry(path, inner.clone(), name, is_dir, size);
            index.entry(parent_key).or_default().push(file_entry);
        }

        // A clone starts with a closed reader: keep it, release the handle.
        let archive = zip.clone();
        drop(zip);
        Ok(Self {
            path: path.to_path_buf(),
            index,
            archive,
        })
    }

    fn entry_name(inner_path: &Path) -> String {
        inner_path.to_string_lossy().replace('\\', "/")
    }

    /// Stream a large entry from a worker thread with bounded memory.
    fn stream_entry(&self, name: String, declared: u64) -> anyhow::Result<Box<dyn Read + Send>> {
        let (tx, chunks) = std::sync::mpsc::sync_channel(STREAM_DEPTH);
        let mut zip = self.archive.clone();
        std::thread::Builder::new()
            .name("atlas-zip-entry".into())
            .spawn(move || {
                let result = (|| -> io::Result<()> {
                    let mut entry = zip.by_name(&name).map_err(io::Error::other)?;
                    let mut sent = 0_u64;
                    loop {
                        let mut chunk = vec![0; STREAM_CHUNK];
                        let read = entry.read(&mut chunk)?;
                        if read == 0 {
                            return Ok(());
                        }
                        sent += read as u64;
                        if sent > declared {
                            return Err(oversized_entry());
                        }
                        chunk.truncate(read);
                        if tx.send(Ok(chunk)).is_err() {
                            return Ok(()); // The reader was dropped.
                        }
                    }
                })();
                if let Err(error) = result {
                    let _ = tx.send(Err(error));
                }
            })?;
        Ok(Box::new(StreamedEntry {
            chunks,
            current: Vec::new(),
            offset: 0,
        }))
    }
}

impl ArchiveProvider for ZipProvider {
    fn list(&self, inner_dir: &Path) -> anyhow::Result<Vec<FileEntry>> {
        let key = path_to_dir_key(inner_dir);
        Ok(self.index.get(&key).cloned().unwrap_or_default())
    }

    fn read_bytes(&self, inner_path: &Path) -> anyhow::Result<Box<dyn Read + Send>> {
        let name = Self::entry_name(inner_path);
        let mut zip = self.archive.clone();
        // Resolve the entry here so a missing name or directory fails now.
        let mut entry = zip.by_name(&name)?;
        let declared = entry.size();
        if declared > IN_MEMORY_ENTRY {
            drop(entry);
            return self.stream_entry(name, declared);
        }
        // Never trust the header for the allocation beyond the in-memory cap.
        let mut buf = Vec::with_capacity(declared as usize);
        (&mut entry).take(declared + 1).read_to_end(&mut buf)?;
        anyhow::ensure!(buf.len() as u64 <= declared, oversized_entry());
        Ok(Box::new(Cursor::new(buf)))
    }

    fn extract_to(&self, inner_path: &Path, dest: &Path) -> anyhow::Result<()> {
        let name = Self::entry_name(inner_path);
        let mut zip = self.archive.clone();
        let mut entry = zip.by_name(&name)?;
        let declared = entry.size();
        ensure_parent_dir(dest)?;
        let mut out = File::create(dest)?;
        let copied = io::copy(&mut (&mut entry).take(declared.saturating_add(1)), &mut out)?;
        if copied > declared {
            drop(out);
            #[allow(clippy::disallowed_methods)] // reason: trait is sync; runs under spawn_blocking
            let _ = std::fs::remove_file(dest);
            return Err(oversized_entry().into());
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
    use std::io::Write as _;

    fn make_test_zip(entries: &[(&str, Option<&str>)]) -> tempfile::NamedTempFile {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let file = std::fs::File::create(tmp.path()).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, content) in entries {
            if let Some(data) = content {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(data.as_bytes()).unwrap();
            } else {
                zip.add_directory(*name, opts).unwrap();
            }
        }
        zip.finish().unwrap();
        tmp
    }

    #[test]
    fn subdir_implicit_listed() {
        // Zip created with only files (no explicit dir entries).
        let tmp = make_test_zip(&[
            ("top.txt", Some("hello")),
            ("sub/a.txt", Some("aaa")),
            ("sub/b.txt", Some("bbb")),
            ("sub/deep/c.txt", Some("ccc")),
        ]);
        let prov = ZipProvider::new(tmp.path()).unwrap();

        // Root should show top.txt + sub/
        let root = prov.list(Path::new("")).unwrap();
        let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"top.txt"),
            "root missing top.txt: {names:?}"
        );
        assert!(names.contains(&"sub"), "root missing sub/: {names:?}");

        // sub/ should show a.txt, b.txt, deep/
        let sub = prov.list(Path::new("sub")).unwrap();
        let sub_names: Vec<_> = sub.iter().map(|e| e.name.as_str()).collect();
        assert!(
            sub_names.contains(&"a.txt"),
            "sub missing a.txt: {sub_names:?}"
        );
        assert!(
            sub_names.contains(&"b.txt"),
            "sub missing b.txt: {sub_names:?}"
        );
        assert!(
            sub_names.contains(&"deep"),
            "sub missing deep/: {sub_names:?}"
        );

        // sub/deep/ should show c.txt
        let deep = prov.list(Path::new("sub/deep")).unwrap();
        let deep_names: Vec<_> = deep.iter().map(|e| e.name.as_str()).collect();
        assert!(
            deep_names.contains(&"c.txt"),
            "deep missing c.txt: {deep_names:?}"
        );
    }

    #[test]
    fn subdir_explicit_listed() {
        // Zip with explicit directory entries.
        let tmp = make_test_zip(&[
            ("sub/", None),
            ("sub/deep/", None),
            ("sub/a.txt", Some("aaa")),
            ("sub/deep/c.txt", Some("ccc")),
        ]);
        let prov = ZipProvider::new(tmp.path()).unwrap();

        let root = prov.list(Path::new("")).unwrap();
        let root_names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
        assert!(
            root_names.contains(&"sub"),
            "root missing sub: {root_names:?}"
        );

        let sub = prov.list(Path::new("sub")).unwrap();
        let sub_names: Vec<_> = sub.iter().map(|e| e.name.as_str()).collect();
        assert!(
            sub_names.contains(&"a.txt"),
            "sub missing a.txt: {sub_names:?}"
        );
        assert!(
            sub_names.contains(&"deep"),
            "sub missing deep: {sub_names:?}"
        );

        let deep = prov.list(Path::new("sub/deep")).unwrap();
        let deep_names: Vec<_> = deep.iter().map(|e| e.name.as_str()).collect();
        assert!(
            deep_names.contains(&"c.txt"),
            "deep missing c.txt: {deep_names:?}"
        );
    }

    #[test]
    fn reads_reuse_the_index_without_holding_the_archive_open() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("a.zip");
        let tmp = make_test_zip(&[("one.txt", Some("first")), ("sub/two.txt", Some("second"))]);
        std::fs::copy(tmp.path(), &original).unwrap();
        let provider = ZipProvider::new(&original).unwrap();
        // No handle is held between reads: the archive can be renamed freely.
        let renamed = dir.path().join("b.zip");
        std::fs::rename(&original, &renamed).unwrap();
        std::fs::rename(&renamed, &original).unwrap();
        for _ in 0..3 {
            let mut text = String::new();
            provider
                .read_bytes(Path::new("sub/two.txt"))
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            assert_eq!(text, "second");
        }
        let dest = dir.path().join("out/one.txt");
        provider.extract_to(Path::new("one.txt"), &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest).unwrap(), "first");
        assert!(provider.read_bytes(Path::new("missing.txt")).is_err());
    }

    #[test]
    fn large_entries_stream_with_bounded_memory() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut zip = zip::ZipWriter::new(std::fs::File::create(tmp.path()).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(true);
        zip.start_file("big.bin", opts).unwrap();
        let block = vec![7_u8; 1024 * 1024];
        let total = IN_MEMORY_ENTRY as usize + 3 * block.len();
        for _ in 0..total / block.len() {
            zip.write_all(&block).unwrap();
        }
        zip.finish().unwrap();
        let provider = ZipProvider::new(tmp.path()).unwrap();
        let mut reader = provider.read_bytes(Path::new("big.bin")).unwrap();
        let mut seen = 0_usize;
        let mut buf = vec![0_u8; 256 * 1024];
        loop {
            let read = reader.read(&mut buf).unwrap();
            if read == 0 {
                break;
            }
            assert!(buf[..read].iter().all(|&b| b == 7));
            seen += read;
        }
        assert_eq!(seen, total);
        // Dropping a half-read stream must not hang or leak the worker.
        let mut partial = provider.read_bytes(Path::new("big.bin")).unwrap();
        partial.read_exact(&mut buf).unwrap();
        drop(partial);
    }

    #[cfg(windows)]
    #[test]
    fn extracted_files_keep_the_archive_mark_of_the_web() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("download.zip");
        let tmp = make_test_zip(&[("setup.exe", Some("MZ"))]);
        std::fs::copy(tmp.path(), &archive).unwrap();
        let mut zone = archive.as_os_str().to_os_string();
        zone.push(":Zone.Identifier");
        std::fs::write(&zone, "[ZoneTransfer]\r\nZoneId=3\r\n").unwrap();
        let provider = ZipProvider::new(&archive).unwrap();
        let dest = dir.path().join("setup.exe");
        super::super::extract_for_export(&provider, Path::new("setup.exe"), &dest).unwrap();
        let mut copied = dest.as_os_str().to_os_string();
        copied.push(":Zone.Identifier");
        assert!(std::fs::read_to_string(copied)
            .unwrap()
            .contains("ZoneId=3"));
        // Archives without a mark produce files without one.
        let clean = dir.path().join("clean.zip");
        std::fs::copy(tmp.path(), &clean).unwrap();
        let provider = ZipProvider::new(&clean).unwrap();
        let dest = dir.path().join("clean.exe");
        super::super::extract_for_export(&provider, Path::new("setup.exe"), &dest).unwrap();
        let mut stream = dest.as_os_str().to_os_string();
        stream.push(":Zone.Identifier");
        assert!(std::fs::metadata(stream).is_err());
    }
}
