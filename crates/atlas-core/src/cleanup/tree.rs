//! Immutable in-memory size tree. One scan produces one `SpaceTree`; navigating
//! to any folder is then a lookup, never another disk read.
use atlas_model::cleanup::{
    DiskEntry, DiskSnapshot, ScanBackend, ScanStatus, TreemapNode, TypeStat,
};
use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};

/// Files smaller than this are folded into their folder's `loose` bytes so a
/// multi-million file volume only keeps directories and meaningful files.
pub const FILE_KEEP: u64 = 256 * 1024;
const MAX_ROWS: usize = 512;
const MAX_FILES: usize = 256;

struct TNode {
    parent: u32,
    name: Box<str>,
    /// Subtree bytes (own loose bytes + children).
    size: u64,
    files: u64,
    dir: bool,
    kids: (u32, u32),
}

pub struct SpaceTree {
    root: PathBuf,
    nodes: Vec<TNode>,
    kids: Vec<u32>,
    pub backend: ScanBackend,
    pub skipped: u64,
    pub errors: u64,
    pub limited: bool,
}

pub struct TreeBuilder {
    nodes: Vec<TNode>,
    keep: u64,
    pub skipped: u64,
    pub errors: u64,
    pub limited: bool,
}

impl TreeBuilder {
    pub fn new(keep: u64) -> Self {
        Self {
            nodes: vec![TNode {
                parent: u32::MAX,
                name: "".into(),
                size: 0,
                files: 0,
                dir: true,
                kids: (0, 0),
            }],
            keep,
            skipped: 0,
            errors: 0,
            limited: false,
        }
    }
    pub fn add_dir(&mut self, parent: u32, name: &str) -> u32 {
        self.nodes.push(TNode {
            parent,
            name: name.into(),
            size: 0,
            files: 0,
            dir: true,
            kids: (0, 0),
        });
        (self.nodes.len() - 1) as u32
    }
    pub fn add_file(&mut self, parent: u32, name: &str, size: u64) {
        if size >= self.keep && size > 0 {
            self.nodes.push(TNode {
                parent,
                name: name.into(),
                size,
                files: 1,
                dir: false,
                kids: (0, 0),
            });
        } else {
            self.fold(parent, size, 1);
        }
    }
    /// Account for files that are not individually retained.
    pub fn fold(&mut self, parent: u32, bytes: u64, files: u64) {
        let node = &mut self.nodes[parent as usize];
        node.size = node.size.saturating_add(bytes);
        node.files += files;
    }
    pub fn finish(mut self, root: PathBuf, backend: ScanBackend) -> SpaceTree {
        let n = self.nodes.len();
        // Children always have a larger id than their parent.
        for i in (1..n).rev() {
            let (size, files, parent) = {
                let node = &self.nodes[i];
                (node.size, node.files, node.parent as usize)
            };
            let p = &mut self.nodes[parent];
            p.size = p.size.saturating_add(size);
            p.files += files;
        }
        let mut counts = vec![0u32; n + 1];
        for node in &self.nodes[1..] {
            counts[node.parent as usize] += 1;
        }
        let mut start = 0u32;
        for (i, count) in counts.iter().take(n).enumerate() {
            self.nodes[i].kids = (start, *count);
            start += count;
        }
        let mut cursor: Vec<u32> = self.nodes.iter().map(|n| n.kids.0).collect();
        let mut kids = vec![0u32; start as usize];
        for i in 1..n {
            let p = self.nodes[i].parent as usize;
            kids[cursor[p] as usize] = i as u32;
            cursor[p] += 1;
        }
        for i in 0..n {
            let (s, c) = self.nodes[i].kids;
            let nodes = &self.nodes;
            kids[s as usize..(s + c) as usize].sort_unstable_by(|a, b| {
                let (a, b) = (&nodes[*a as usize], &nodes[*b as usize]);
                b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name))
            });
        }
        SpaceTree {
            root,
            nodes: self.nodes,
            kids,
            backend,
            skipped: self.skipped,
            errors: self.errors,
            limited: self.limited,
        }
    }
}

impl SpaceTree {
    fn kids_of(&self, id: u32) -> &[u32] {
        let (s, c) = self.nodes[id as usize].kids;
        &self.kids[s as usize..(s + c) as usize]
    }
    fn find(&self, path: &Path) -> Option<u32> {
        self.locate(path, true)
    }
    /// `dir_only` restricts every component to directories; otherwise the last may be a file.
    fn locate(&self, path: &Path, dir_only: bool) -> Option<u32> {
        let rel = path.strip_prefix(&self.root).ok()?;
        let mut current = 0u32;
        let count = rel.components().count();
        for (i, part) in rel.components().enumerate() {
            let Component::Normal(name) = part else {
                return None;
            };
            let name = name.to_string_lossy();
            current = *self.kids_of(current).iter().find(|k| {
                let n = &self.nodes[**k as usize];
                (n.dir || (!dir_only && i + 1 == count)) && n.name.eq_ignore_ascii_case(&name)
            })?;
        }
        Some(current)
    }
    fn path_of(&self, id: u32, base: &Path, base_id: u32) -> PathBuf {
        let mut names = Vec::new();
        let mut at = id;
        while at != base_id && at != u32::MAX {
            names.push(&*self.nodes[at as usize].name);
            at = self.nodes[at as usize].parent;
        }
        let mut path = base.to_owned();
        for name in names.into_iter().rev() {
            path.push(name);
        }
        path
    }
    fn entry(&self, id: u32, base: &Path, base_id: u32) -> DiskEntry {
        let node = &self.nodes[id as usize];
        DiskEntry {
            path: self.path_of(id, base, base_id),
            bytes: node.size,
            directory: node.dir,
        }
    }

    /// Forget an entry that was deleted, keeping every ancestor total consistent.
    fn remove_node(&mut self, path: &Path) -> bool {
        let Some(id) = self.locate(path, false).filter(|id| *id != 0) else {
            return false;
        };
        let (size, files) = {
            let n = &mut self.nodes[id as usize];
            (std::mem::take(&mut n.size), std::mem::take(&mut n.files))
        };
        let mut at = self.nodes[id as usize].parent;
        while at != u32::MAX {
            let n = &mut self.nodes[at as usize];
            n.size = n.size.saturating_sub(size);
            n.files = n.files.saturating_sub(files);
            let (s, c) = n.kids;
            let nodes = &self.nodes;
            let mut range = self.kids[s as usize..(s + c) as usize].to_vec();
            range.sort_unstable_by(|a, b| {
                let (a, b) = (&nodes[*a as usize], &nodes[*b as usize]);
                b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name))
            });
            self.kids[s as usize..(s + c) as usize].copy_from_slice(&range);
            at = self.nodes[at as usize].parent;
        }
        true
    }

    /// Nested treemap of `path`, at most `budget` rectangles. The largest
    /// entries are expanded first, so a small budget still shows the big
    /// structure; anything below `min_fraction` of the whole is folded into its
    /// parent's `other` bytes.
    pub fn treemap(&self, path: &Path, budget: usize, min_fraction: f64) -> Option<TreemapNode> {
        let id = self.find(path)?;
        let total = self.nodes[id as usize].size.max(1);
        let floor = (total as f64 * min_fraction) as u64;
        // Breadth-first by size: decide which nodes are drawn before building.
        let mut drawn: std::collections::HashSet<u32> = std::collections::HashSet::new();
        let mut frontier: BinaryHeap<(u64, u32)> = BinaryHeap::new();
        frontier.push((self.nodes[id as usize].size, id));
        drawn.insert(id);
        while let Some((_, at)) = frontier.pop() {
            for kid in self.kids_of(at) {
                let node = &self.nodes[*kid as usize];
                // Kids are sorted by size: the rest are smaller still.
                if node.size <= floor || node.size == 0 || drawn.len() >= budget {
                    break;
                }
                drawn.insert(*kid);
                if node.dir {
                    frontier.push((node.size, *kid));
                }
            }
        }
        Some(self.treemap_node(id, path, &drawn))
    }

    fn treemap_node(
        &self,
        id: u32,
        path: &Path,
        drawn: &std::collections::HashSet<u32>,
    ) -> TreemapNode {
        let node = &self.nodes[id as usize];
        let children: Vec<TreemapNode> = self
            .kids_of(id)
            .iter()
            .filter(|kid| drawn.contains(kid))
            .map(|kid| self.treemap_node(*kid, &path.join(&*self.nodes[*kid as usize].name), drawn))
            .collect();
        let shown = children
            .iter()
            .fold(0u64, |sum, c| sum.saturating_add(c.bytes));
        TreemapNode {
            path: path.to_owned(),
            name: path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
            bytes: node.size,
            directory: node.dir,
            other: node.size.saturating_sub(shown),
            children,
        }
    }

    /// Space by file extension below `path`. Files under [`FILE_KEEP`] are not
    /// indexed one by one; their total is reported as one "small files" row.
    pub fn types(&self, path: &Path) -> Option<Vec<TypeStat>> {
        let id = self.find(path)?;
        let mut by_ext: std::collections::HashMap<String, (u64, u64)> =
            std::collections::HashMap::new();
        let mut stack = vec![id];
        let (mut kept_bytes, mut kept_files) = (0u64, 0u64);
        while let Some(at) = stack.pop() {
            for kid in self.kids_of(at) {
                let node = &self.nodes[*kid as usize];
                if node.dir {
                    stack.push(*kid);
                    continue;
                }
                let extension = Path::new(&*node.name)
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                let slot = by_ext.entry(extension).or_default();
                slot.0 = slot.0.saturating_add(node.size);
                slot.1 += node.files;
                kept_bytes = kept_bytes.saturating_add(node.size);
                kept_files += node.files;
            }
        }
        let root = &self.nodes[id as usize];
        let mut stats: Vec<TypeStat> = by_ext
            .into_iter()
            .map(|(extension, (bytes, files))| TypeStat {
                extension,
                bytes,
                files,
                small_files: false,
            })
            .collect();
        let small = root.size.saturating_sub(kept_bytes);
        if small > 0 {
            stats.push(TypeStat {
                extension: String::new(),
                bytes: small,
                files: root.files.saturating_sub(kept_files),
                small_files: true,
            });
        }
        stats.sort_by(|a, b| {
            b.bytes
                .cmp(&a.bytes)
                .then_with(|| a.extension.cmp(&b.extension))
        });
        Some(stats)
    }

    /// Every individually indexed file (at least [`FILE_KEEP`]) below `path`.
    pub fn files_under(&self, path: &Path) -> Option<Vec<(PathBuf, u64)>> {
        let id = self.find(path)?;
        let mut files = Vec::new();
        let mut stack = vec![(id, path.to_owned())];
        while let Some((at, base)) = stack.pop() {
            for kid in self.kids_of(at) {
                let node = &self.nodes[*kid as usize];
                let child = base.join(&*node.name);
                if node.dir {
                    stack.push((*kid, child));
                } else {
                    files.push((child, node.size));
                }
            }
        }
        Some(files)
    }

    /// Snapshot of any folder inside this tree; no I/O besides the capacity query.
    pub fn snapshot(&self, path: &Path, generation: u64) -> Option<DiskSnapshot> {
        let id = self.find(path)?;
        let node = &self.nodes[id as usize];
        let kids = self.kids_of(id);
        let entries: Vec<_> = kids
            .iter()
            .filter(|k| self.nodes[**k as usize].size > 0)
            .take(MAX_ROWS)
            .map(|k| self.entry(*k, path, id))
            .collect();
        let shown: u64 = entries.iter().fold(0, |n, e| n.saturating_add(e.bytes));
        let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::with_capacity(MAX_FILES + 1);
        let mut stack = vec![id];
        while let Some(at) = stack.pop() {
            for k in self.kids_of(at) {
                let n = &self.nodes[*k as usize];
                if heap.len() >= MAX_FILES && heap.peek().is_some_and(|m| n.size <= m.0 .0) {
                    // Kids are sorted by size: the rest cannot qualify either.
                    break;
                }
                if n.dir {
                    stack.push(*k);
                } else {
                    heap.push(Reverse((n.size, *k)));
                    if heap.len() > MAX_FILES {
                        heap.pop();
                    }
                }
            }
        }
        let mut large: Vec<_> = heap.into_iter().map(|Reverse((_, k))| k).collect();
        large.sort_unstable_by(|a, b| {
            let (a, b) = (&self.nodes[*a as usize], &self.nodes[*b as usize]);
            b.size.cmp(&a.size)
        });
        let limited = kids.len() > MAX_ROWS || self.limited;
        let mut snapshot = DiskSnapshot {
            generation,
            root: path.to_owned(),
            backend: self.backend,
            bytes: node.size,
            entry_count: node.files,
            skipped: self.skipped,
            errors: self.errors,
            limited,
            other_bytes: node.size.saturating_sub(shown),
            large_files: large.into_iter().map(|k| self.entry(k, path, id)).collect(),
            entries,
            ..Default::default()
        };
        #[cfg(windows)]
        {
            snapshot.capacity = atlas_win_space::cleanup::capacity(path);
            snapshot.fast_scan_available = !crate::fs::network::is_network_path(&self.root)
                && self
                    .root
                    .ancestors()
                    .last()
                    .is_some_and(atlas_win_space::mft_space::supported);
        }
        snapshot.status = if snapshot.skipped > 0 || snapshot.errors > 0 || limited {
            ScanStatus::Partial
        } else {
            ScanStatus::Complete
        };
        Some(snapshot)
    }
}

static TREE: Mutex<Option<Arc<SpaceTree>>> = Mutex::new(None);

pub fn store(tree: Arc<SpaceTree>) {
    *TREE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tree);
}
pub fn cached(path: &Path) -> Option<Arc<SpaceTree>> {
    TREE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|t| t.find(path).is_some())
        .cloned()
}
/// Update the cached tree after `path` was deleted. False if nothing changed.
pub fn remove(path: &Path) -> bool {
    let mut guard = TREE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .as_mut()
        .and_then(Arc::get_mut)
        .is_some_and(|tree| tree.remove_node(path))
}
pub fn invalidate() {
    if let Some(old) = TREE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        std::thread::spawn(move || drop(old));
    }
}
/// Nested treemap of `path` from the last completed scan.
pub fn cached_treemap(path: &Path, budget: usize) -> Option<TreemapNode> {
    cached(path)?.treemap(path, budget, 0.0015)
}
/// File types below `path` from the last completed scan.
pub fn cached_types(path: &Path) -> Option<Vec<TypeStat>> {
    cached(path)?.types(path)
}
/// Indexed files below `path` from the last completed scan.
pub fn cached_files(path: &Path) -> Option<Vec<(PathBuf, u64)>> {
    cached(path)?.files_under(path)
}
/// Instant folder view from the last completed scan, if it covers `path`.
pub fn cached_snapshot(path: &Path, generation: u64) -> Option<DiskSnapshot> {
    cached(path)?.snapshot(path, generation)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn totals_navigation_and_large_files_come_from_one_tree() {
        let mut b = TreeBuilder::new(0);
        let a = b.add_dir(0, "A");
        let bb = b.add_dir(a, "B");
        b.add_file(a, "x", 3);
        b.add_file(bb, "y", 7);
        b.add_file(0, "z", 2);
        let root = PathBuf::from("C:\\");
        let t = b.finish(root.clone(), ScanBackend::NativeDirectory);
        let whole = t.snapshot(&root, 1).unwrap();
        assert_eq!((whole.bytes, whole.entry_count), (12, 3));
        assert_eq!(
            whole
                .large_files
                .iter()
                .map(|f| f.bytes)
                .collect::<Vec<_>>(),
            vec![7, 3, 2]
        );
        let child = t.snapshot(&root.join("a"), 2).unwrap();
        assert_eq!((child.bytes, child.entry_count), (10, 2));
        assert_eq!(
            child.entries.iter().map(|f| f.bytes).collect::<Vec<_>>(),
            vec![7, 3]
        );
        assert!(t.snapshot(&root.join("missing"), 3).is_none());
    }
    #[test]
    fn removing_an_entry_updates_every_ancestor() {
        let mut b = TreeBuilder::new(0);
        let a = b.add_dir(0, "A");
        b.add_file(a, "x", 3);
        b.add_file(a, "y", 7);
        let root = PathBuf::from("C:\\");
        let mut t = b.finish(root.clone(), ScanBackend::NativeDirectory);
        assert!(t.remove_node(&root.join("A").join("y")));
        let s = t.snapshot(&root, 1).unwrap();
        assert_eq!((s.bytes, s.entry_count), (3, 1));
        let child = t.snapshot(&root.join("A"), 1).unwrap();
        assert_eq!(child.entries.len(), 1);
        assert!(!t.remove_node(&root.join("missing")));
    }
    #[test]
    fn treemap_types_and_files_come_from_the_same_tree() {
        let mut b = TreeBuilder::new(100);
        let a = b.add_dir(0, "A");
        let deep = b.add_dir(a, "Deep");
        b.add_file(deep, "movie.MKV", 9_000);
        b.add_file(a, "song.mp3", 600);
        b.add_file(0, "notes.txt", 20);
        b.add_file(0, "disk.iso", 2_000);
        let root = PathBuf::from("C:\\");
        let t = b.finish(root.clone(), ScanBackend::NativeDirectory);

        let map = t.treemap(&root, 100, 0.0).unwrap();
        assert_eq!(map.bytes, 11_620);
        assert_eq!(map.children[0].name, "A");
        assert_eq!(map.children[0].children[0].name, "Deep");
        assert_eq!(map.children[0].children[0].children[0].bytes, 9_000);
        // The 20-byte file is folded: children plus `other` equal the total.
        let shown: u64 = map.children.iter().map(|c| c.bytes).sum();
        assert_eq!(shown + map.other, map.bytes);
        // A tight budget keeps only the largest rectangles.
        let small = t.treemap(&root, 2, 0.0).unwrap();
        assert_eq!(small.children.len(), 1);
        assert!(small.children[0].children.is_empty());

        let types = t.types(&root).unwrap();
        assert_eq!(types[0].extension, "mkv");
        assert_eq!(types[0].bytes, 9_000);
        assert!(types
            .iter()
            .any(|s| s.small_files && s.bytes == 20 && s.files == 1));
        assert_eq!(types.iter().map(|s| s.bytes).sum::<u64>(), 11_620);

        let mut files = t.files_under(&root.join("a")).unwrap();
        files.sort();
        assert_eq!(files.len(), 2);
        assert!(files
            .iter()
            .any(|(p, b)| p.ends_with("movie.MKV") && *b == 9_000));
    }
    #[test]
    fn small_files_fold_into_the_folder_total() {
        let mut b = TreeBuilder::new(100);
        b.add_file(0, "tiny", 10);
        b.add_file(0, "big", 500);
        let t = b.finish(PathBuf::from("C:\\"), ScanBackend::NativeDirectory);
        let s = t.snapshot(Path::new("C:\\"), 1).unwrap();
        assert_eq!((s.bytes, s.entry_count, s.entries.len()), (510, 2, 1));
        assert_eq!(s.other_bytes, 10);
    }
}
