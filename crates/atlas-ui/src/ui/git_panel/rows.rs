//! Row models for the Git panel lists: names grouped into a folder tree on
//! `/` (branches and changed files), and the text filter of branches and
//! commits. Kept free of GPUI so it can be tested on its own.
use atlas_model::git::Commit;
use std::collections::{BTreeMap, HashSet};

/// One row of a folder tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TreeRow {
    Folder {
        /// Collapse-state key: the tree's prefix and the folder's full path.
        key: String,
        /// Shown name; chains of single folders read `a/b` when compacted.
        label: String,
        depth: u16,
        /// Every item below the folder, as indexes into the source list.
        items: Vec<usize>,
        collapsed: bool,
    },
    Leaf {
        index: usize,
        label: String,
        depth: u16,
    },
}

#[derive(Default)]
struct Node {
    folders: BTreeMap<String, Node>,
    leaves: Vec<(usize, String)>,
    items: Vec<usize>,
}

/// Group `items` (`(index, path)`) into folders on `/`, folders first and
/// both sorted case-insensitively. Folders whose key is in `collapsed` hide
/// their content unless `open_all` is set (while filtering). With `compact`,
/// a folder that holds only one folder is merged into it (`src/ui`). A path
/// ending in `/` (an untracked folder) is a leaf and keeps its slash.
pub(super) fn tree(
    prefix: &str,
    items: &[(usize, &str)],
    collapsed: &HashSet<String>,
    open_all: bool,
    compact: bool,
) -> Vec<TreeRow> {
    let mut root = Node::default();
    for &(index, path) in items {
        let folder_slash = path.ends_with('/');
        let trimmed = path.trim_end_matches('/');
        let mut segments: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
        let Some(last) = segments.pop() else {
            continue;
        };
        let mut node = &mut root;
        node.items.push(index);
        for segment in segments {
            node = node.folders.entry(segment.to_owned()).or_default();
            node.items.push(index);
        }
        let label = if folder_slash {
            format!("{last}/")
        } else {
            last.to_owned()
        };
        node.leaves.push((index, label));
    }
    let mut rows = Vec::new();
    let options = Options {
        prefix,
        collapsed,
        open_all,
        compact,
    };
    emit(&root, "", 0, &options, &mut rows);
    rows
}

struct Options<'a> {
    prefix: &'a str,
    collapsed: &'a HashSet<String>,
    open_all: bool,
    compact: bool,
}

fn emit(node: &Node, path: &str, depth: u16, options: &Options<'_>, rows: &mut Vec<TreeRow>) {
    let join = |base: &str, name: &str| {
        if base.is_empty() {
            name.to_owned()
        } else {
            format!("{base}/{name}")
        }
    };
    let mut folders: Vec<(&String, &Node)> = node.folders.iter().collect();
    folders.sort_by_cached_key(|(name, _)| name.to_lowercase());
    for (name, mut child) in folders {
        let mut label = name.clone();
        let mut full = join(path, name);
        if options.compact {
            while child.leaves.is_empty() && child.folders.len() == 1 {
                let Some((next, grandchild)) = child.folders.iter().next() else {
                    break;
                };
                label = format!("{label}/{next}");
                full = join(&full, next);
                child = grandchild;
            }
        }
        let key = format!("{}{full}", options.prefix);
        let collapsed = !options.open_all && options.collapsed.contains(&key);
        rows.push(TreeRow::Folder {
            key,
            label,
            depth,
            items: child.items.clone(),
            collapsed,
        });
        if !collapsed {
            emit(child, &full, depth + 1, options, rows);
        }
    }
    let mut leaves: Vec<&(usize, String)> = node.leaves.iter().collect();
    leaves.sort_by_cached_key(|(_, label)| label.to_lowercase());
    rows.extend(leaves.into_iter().map(|(index, label)| TreeRow::Leaf {
        index: *index,
        label: label.clone(),
        depth,
    }));
}

/// Lower-cased words of a filter box; empty when there is nothing to filter.
pub(super) fn terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// True when every term is found in `text` (already lower-cased).
pub(super) fn matches_all(text: &str, terms: &[String]) -> bool {
    terms.iter().all(|term| text.contains(term.as_str()))
}

/// Whether a commit matches every term: in its subject, author, e-mail, ref
/// names, or as the start of its id.
pub(super) fn commit_matches(commit: &Commit, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let mut text = format!(
        "{}\n{}\n{}",
        commit.subject.to_lowercase(),
        commit.author.to_lowercase(),
        commit.email.to_lowercase()
    );
    for label in &commit.refs {
        text.push('\n');
        text.push_str(&label.name.to_lowercase());
    }
    terms
        .iter()
        .all(|term| text.contains(term.as_str()) || commit.id.starts_with(term.as_str()))
}

#[cfg(test)]
mod tests {
    use super::{commit_matches, terms, tree, TreeRow};
    use atlas_model::git::{Commit, RefKind, RefLabel};
    use std::collections::HashSet;

    fn labels(rows: &[TreeRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                TreeRow::Folder {
                    label,
                    depth,
                    items,
                    collapsed,
                    ..
                } => format!(
                    "{}{label}/ ({}){}",
                    "  ".repeat(usize::from(*depth)),
                    items.len(),
                    if *collapsed { " +" } else { "" }
                ),
                TreeRow::Leaf { label, depth, .. } => {
                    format!("{}{label}", "  ".repeat(usize::from(*depth)))
                }
            })
            .collect()
    }

    #[test]
    fn branches_group_into_sorted_folders_before_leaves() {
        let names = ["main", "feat/git", "Feat-b", "feat/tree/x", "fix/a", "dev"];
        let items: Vec<(usize, &str)> = names.iter().copied().enumerate().collect();
        let rows = tree("L:", &items, &HashSet::new(), false, false);
        assert_eq!(
            labels(&rows),
            [
                "feat/ (2)",
                "  tree/ (1)",
                "    x",
                "  git",
                "fix/ (1)",
                "  a",
                "dev",
                "Feat-b",
                "main"
            ]
        );
        let TreeRow::Leaf { index, .. } = rows[2] else {
            panic!("leaf expected");
        };
        assert_eq!(names[index], "feat/tree/x");
    }

    #[test]
    fn collapsed_folders_hide_content_unless_filtering() {
        let items = [(0, "feat/a"), (1, "feat/b"), (2, "main")];
        let collapsed: HashSet<String> = ["L:feat".to_owned()].into();
        let rows = tree("L:", &items, &collapsed, false, false);
        assert_eq!(labels(&rows), ["feat/ (2) +", "main"]);
        let open = tree("L:", &items, &collapsed, true, false);
        assert_eq!(labels(&open), ["feat/ (2)", "  a", "  b", "main"]);
        // Another tree prefix keeps its own state.
        let remote = tree("R:", &items, &collapsed, false, false);
        assert_eq!(remote.len(), 4);
    }

    #[test]
    fn file_trees_compact_single_folder_chains() {
        let items = [
            (0, "crates/ui/src/a.rs"),
            (1, "crates/ui/src/b.rs"),
            (2, "crates/core/lib.rs"),
            (3, "new/"),
            (4, "README.md"),
        ];
        let rows = tree("U:", &items, &HashSet::new(), false, true);
        assert_eq!(
            labels(&rows),
            [
                "crates/ (3)",
                "  core/ (1)",
                "    lib.rs",
                "  ui/src/ (2)",
                "    a.rs",
                "    b.rs",
                "new/",
                "README.md"
            ]
        );
        let TreeRow::Folder { key, .. } = &rows[3] else {
            panic!("folder expected");
        };
        assert_eq!(key, "U:crates/ui/src");
    }

    #[test]
    fn commits_match_subject_author_refs_and_id_prefix() {
        let commit = Commit {
            id: "40a3e92384304ac8fd2d7d3ece1d96d7c5418a4a".into(),
            parents: Vec::new(),
            author: "Ada Lovelace".into(),
            email: "ada@example.com".into(),
            time: 0,
            refs: vec![RefLabel {
                kind: RefKind::Branch,
                name: "feat/git".into(),
            }],
            subject: "Native SMB mapping".into(),
        };
        for query in ["smb", "ADA native", "40a3e9", "feat/git", "example.com"] {
            assert!(commit_matches(&commit, &terms(query)), "{query}");
        }
        assert!(commit_matches(&commit, &terms("  ")));
        assert!(!commit_matches(&commit, &terms("smb bob")));
        assert!(!commit_matches(&commit, &terms("e92384")));
    }
}
