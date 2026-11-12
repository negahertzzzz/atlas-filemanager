//! NTFS volume index turned into a `SpaceTree`. No MFT result is a cleanup candidate.
use super::{
    policy,
    tree::{SpaceTree, TreeBuilder, FILE_KEEP},
};
use atlas_model::cleanup::ScanBackend;
use atlas_win_space::mft_space::{Index, Record};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};

const MASK: u64 = (1 << 48) - 1;
const UNSAFE_ATTRIBUTES: u32 = 0x400 | 0x1000 | 0x40000 | 0x400000;

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Cancelled")
}

pub(super) fn scan(
    root: &Path,
    elevated: bool,
    mut control: impl FnMut() -> bool,
    mut progress: impl FnMut(u64, u64),
) -> io::Result<SpaceTree> {
    if !policy::is_drive_root(root) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "No volume index",
        ));
    }
    let index = if elevated {
        atlas_win_space::mft_space::helper::scan(root, &mut control, &mut progress)?
    } else {
        atlas_win_space::mft_space::scan(root, &mut control, &mut progress)?
    };
    build(index, root, FILE_KEEP, &mut control)
}

fn build(
    index: Index,
    root: &Path,
    keep: u64,
    control: &mut impl FnMut() -> bool,
) -> io::Result<SpaceTree> {
    let records = &index.records;
    let dirs: HashMap<u64, u32> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| r.directory)
        .map(|(i, r)| (r.reference, i as u32))
        .collect();
    let root_record = records
        .iter()
        .position(|r| r.directory && r.reference & MASK == 5)
        .ok_or_else(|| io::Error::other("NTFS root record could not be verified"))?;
    // Children of each directory record, as a compressed row layout.
    let mut parent_of = vec![u32::MAX; records.len()];
    let mut counts = vec![0u32; records.len() + 1];
    for (i, r) in records.iter().enumerate() {
        if r.reference == r.parent {
            continue;
        }
        if let Some(p) = dirs.get(&r.parent) {
            parent_of[i] = *p;
            counts[*p as usize] += 1;
        }
    }
    let mut starts = vec![0u32; records.len() + 1];
    for i in 0..records.len() {
        starts[i + 1] = starts[i] + counts[i];
    }
    let mut cursor = starts.clone();
    let mut children = vec![0u32; starts[records.len()] as usize];
    for (i, p) in parent_of.iter().enumerate() {
        if *p != u32::MAX {
            children[cursor[*p as usize] as usize] = i as u32;
            cursor[*p as usize] += 1;
        }
    }
    let mut builder = TreeBuilder::new(keep);
    builder.skipped = index.skipped;
    let mut queue: Vec<(u32, u32, PathBuf)> = vec![(root_record as u32, 0, root.to_owned())];
    let mut visited = 0u32;
    while let Some((record, id, path)) = queue.pop() {
        visited += 1;
        if visited % 4096 == 0 && !control() {
            return Err(interrupted());
        }
        let (s, e) = (starts[record as usize], starts[record as usize + 1]);
        let (mut loose, mut loose_files) = (0u64, 0u64);
        for child in &children[s as usize..e as usize] {
            let r: &Record = &records[*child as usize];
            if r.attributes & UNSAFE_ATTRIBUTES != 0 {
                builder.skipped += 1;
            } else if r.directory {
                let child_path = path.join(&r.name);
                if policy::protected(&child_path)
                    || crate::vault::is_private_path(&child_path)
                    || crate::vault::live::is_private(&child_path)
                {
                    builder.skipped += 1;
                } else {
                    let cid = builder.add_dir(id, &r.name);
                    queue.push((*child, cid, child_path));
                }
            } else if r.bytes >= keep && r.bytes > 0 {
                builder.add_file(id, &r.name, r.bytes);
            } else {
                loose += r.bytes;
                loose_files += 1;
            }
        }
        builder.fold(id, loose, loose_files);
    }
    Ok(builder.finish(root.to_owned(), ScanBackend::NtfsMft))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn index_becomes_a_navigable_tree_without_private_or_stale_entries() {
        let item = |reference, parent, name: &str, bytes, directory| Record {
            reference,
            parent,
            name: name.into(),
            bytes,
            attributes: 0,
            directory,
        };
        let index = Index {
            skipped: 0,
            records: vec![
                item(5, 5, ".", 0, true),
                item(10, 5, "A", 0, true),
                item(11, 10, "B", 0, true),
                item(20, 10, "x", 3, false),
                item(21, 11, "y", 7, false),
                item(22, 5, "z", 2, false),
                item(30, 5, ".ssh", 0, true),
                item(31, 30, "key", 9, false),
                item(40, 999, "stale", 0, true),
                item(41, 40, "lost", 5, false),
            ],
        };
        let root = PathBuf::from("C:\\");
        let tree = build(index, &root, 0, &mut || true).unwrap();
        let whole = tree.snapshot(&root, 1).unwrap();
        assert_eq!((whole.bytes, whole.entry_count), (12, 3));
        assert_eq!(whole.skipped, 1);
        let child = tree.snapshot(&root.join("A"), 2).unwrap();
        assert_eq!((child.bytes, child.entry_count), (10, 2));
    }
}
