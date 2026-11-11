//! Parsers for the machine-readable output of the git commands the panel runs.
use atlas_model::git::{
    Branch, ChangeKind, Commit, CommitFile, FileChange, RefKind, RefLabel, Remote, RepoHead, Stash,
    Status, Tag,
};

/// Field and record separators requested in every `--format`.
pub(crate) const FIELD: char = '\u{1f}';
pub(crate) const RECORD: char = '\u{1e}';

fn kind(code: char) -> Option<ChangeKind> {
    Some(match code {
        'M' => ChangeKind::Modified,
        'A' => ChangeKind::Added,
        'D' => ChangeKind::Deleted,
        'R' => ChangeKind::Renamed,
        'C' => ChangeKind::Copied,
        'T' => ChangeKind::TypeChanged,
        'U' => ChangeKind::Conflicted,
        _ => return None,
    })
}

/// `git status --porcelain=v2 --branch -z`.
pub(crate) fn status(output: &str) -> Status {
    let mut head = RepoHead::default();
    let mut changes = Vec::new();
    let mut fields = output.split('\0');
    while let Some(field) = fields.next() {
        if let Some(header) = field.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" if value != "(initial)" => {
                    head.head = value.get(..8).unwrap_or(value).to_owned();
                }
                "branch.head" if value != "(detached)" => head.branch = Some(value.to_owned()),
                "branch.upstream" => head.upstream = Some(value.to_owned()),
                "branch.ab" => {
                    for part in value.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            head.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            head.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let mut parts = field.splitn(2, ' ');
        let (Some(tag), Some(rest)) = (parts.next(), parts.next()) else {
            continue;
        };
        match tag {
            "?" => changes.push(FileChange {
                path: rest.to_owned(),
                original: None,
                staged: None,
                unstaged: Some(ChangeKind::Untracked),
            }),
            "1" | "2" => {
                // XY sub mH mI mW hH hI [Xscore] path
                let skip = if tag == "1" { 7 } else { 8 };
                let mut columns = rest.splitn(skip + 1, ' ');
                let xy: Vec<char> = columns.next().unwrap_or("..").chars().collect();
                let Some(path) = columns.nth(skip - 1) else {
                    continue;
                };
                // A rename's source is the next NUL-separated field.
                let original = (tag == "2")
                    .then(|| fields.next().map(str::to_owned))
                    .flatten();
                changes.push(FileChange {
                    path: path.to_owned(),
                    original,
                    staged: xy.first().copied().and_then(kind),
                    unstaged: xy.get(1).copied().and_then(kind),
                });
            }
            "u" => {
                // XY sub m1 m2 m3 mW h1 h2 h3 path
                if let Some(path) = rest.splitn(10, ' ').nth(9) {
                    head.conflicts += 1;
                    changes.push(FileChange {
                        path: path.to_owned(),
                        original: None,
                        staged: None,
                        unstaged: Some(ChangeKind::Conflicted),
                    });
                }
            }
            _ => {}
        }
    }
    head.changes = u32::try_from(changes.len()).unwrap_or(u32::MAX);
    Status { head, changes }
}

/// `%D` with `--decorate=full`: `HEAD -> refs/heads/main, tag: refs/tags/v1, …`.
pub(crate) fn decorations(text: &str) -> Vec<RefLabel> {
    let mut labels = Vec::new();
    for item in text.split(", ").map(str::trim).filter(|s| !s.is_empty()) {
        let item = if let Some(branch) = item.strip_prefix("HEAD -> ") {
            labels.push(RefLabel {
                name: "HEAD".into(),
                kind: RefKind::Head,
            });
            branch
        } else {
            item
        };
        let label = if let Some(tag) = item.strip_prefix("tag: ") {
            RefLabel {
                name: tag.trim_start_matches("refs/tags/").to_owned(),
                kind: RefKind::Tag,
            }
        } else if let Some(branch) = item.strip_prefix("refs/heads/") {
            RefLabel {
                name: branch.to_owned(),
                kind: RefKind::Branch,
            }
        } else if let Some(remote) = item.strip_prefix("refs/remotes/") {
            // `origin/HEAD` only repeats the remote's default branch.
            if remote.ends_with("/HEAD") {
                continue;
            }
            RefLabel {
                name: remote.to_owned(),
                kind: RefKind::Remote,
            }
        } else if item == "HEAD" {
            RefLabel {
                name: "HEAD".into(),
                kind: RefKind::Head,
            }
        } else {
            continue;
        };
        labels.push(label);
    }
    labels
}

/// Format string matching [`commits`].
pub(crate) const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1e";

pub(crate) fn commits(output: &str) -> Vec<Commit> {
    output
        .split(RECORD)
        .filter_map(|record| {
            let record = record.trim_start_matches(['\n', '\r']);
            let mut f = record.split(FIELD);
            let id = f.next().filter(|id| id.len() >= 7)?.to_owned();
            let parents = f
                .next()
                .unwrap_or("")
                .split(' ')
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect();
            Some(Commit {
                id,
                parents,
                author: f.next().unwrap_or("").to_owned(),
                email: f.next().unwrap_or("").to_owned(),
                time: f.next().and_then(|t| t.parse().ok()).unwrap_or(0),
                refs: decorations(f.next().unwrap_or("")),
                subject: f.next().unwrap_or("").trim_end().to_owned(),
            })
        })
        .collect()
}

/// Format matching [`branches`].
pub(crate) const BRANCH_FORMAT: &str = "--format=%(refname)%1f%(objectname:short)%1f%(upstream:short)%1f%(upstream:track,nobracket)%1f%(HEAD)%1f%(committerdate:unix)%1f%(contents:subject)%1e";

pub(crate) fn branches(output: &str) -> Vec<Branch> {
    let mut list: Vec<Branch> = output
        .split(RECORD)
        .filter_map(|record| {
            let record = record.trim_start_matches(['\n', '\r']);
            let mut f = record.split(FIELD);
            let refname = f.next()?;
            let (name, remote) = if let Some(name) = refname.strip_prefix("refs/heads/") {
                (name, false)
            } else if let Some(name) = refname.strip_prefix("refs/remotes/") {
                if name.ends_with("/HEAD") {
                    return None;
                }
                (name, true)
            } else {
                return None;
            };
            let target = f.next().unwrap_or("").to_owned();
            let upstream = f.next().filter(|u| !u.is_empty()).map(str::to_owned);
            let track = f.next().unwrap_or("");
            let (mut ahead, mut behind) = (0, 0);
            for part in track.split(", ") {
                if let Some(n) = part.strip_prefix("ahead ") {
                    ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix("behind ") {
                    behind = n.parse().unwrap_or(0);
                }
            }
            Some(Branch {
                name: name.to_owned(),
                remote,
                current: f.next() == Some("*"),
                upstream,
                ahead,
                behind,
                gone: track == "gone",
                target,
                time: f.next().and_then(|t| t.parse().ok()).unwrap_or(0),
                subject: f.next().unwrap_or("").trim_end().to_owned(),
            })
        })
        .collect();
    // Current branch first, then local before remote, newest first.
    list.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then(a.remote.cmp(&b.remote))
            .then(b.time.cmp(&a.time))
    });
    list
}

pub(crate) const TAG_FORMAT: &str = "--format=%(refname:short)%1f%(objectname:short)%1f%(*objectname:short)%1f%(creatordate:unix)%1f%(contents:subject)%1e";

pub(crate) fn tags(output: &str) -> Vec<Tag> {
    output
        .split(RECORD)
        .filter_map(|record| {
            let record = record.trim_start_matches(['\n', '\r']);
            let mut f = record.split(FIELD);
            let name = f.next().filter(|n| !n.is_empty())?.to_owned();
            let object = f.next().unwrap_or("");
            // Annotated tags point at a tag object; `*objectname` is the commit.
            let peeled = f.next().filter(|p| !p.is_empty()).unwrap_or(object);
            Some(Tag {
                name,
                target: peeled.to_owned(),
                time: f.next().and_then(|t| t.parse().ok()).unwrap_or(0),
                message: f.next().unwrap_or("").trim_end().to_owned(),
            })
        })
        .collect()
}

pub(crate) const STASH_FORMAT: &str = "--format=%gd%x1f%ct%x1f%gs%x1e";

pub(crate) fn stashes(output: &str) -> Vec<Stash> {
    output
        .split(RECORD)
        .filter_map(|record| {
            let record = record.trim_start_matches(['\n', '\r']);
            let mut f = record.split(FIELD);
            let selector = f.next()?;
            let index = selector
                .strip_prefix("stash@{")?
                .strip_suffix('}')?
                .parse()
                .ok()?;
            Some(Stash {
                index,
                time: f.next().and_then(|t| t.parse().ok()).unwrap_or(0),
                message: f.next().unwrap_or("").trim_end().to_owned(),
            })
        })
        .collect()
}

/// `git remote -v`: one entry per remote (its fetch URL).
pub(crate) fn remotes(output: &str) -> Vec<Remote> {
    let mut list: Vec<Remote> = Vec::new();
    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let (Some(name), Some(url)) = (parts.next(), parts.next()) else {
            continue;
        };
        if !list.iter().any(|r| r.name == name) {
            list.push(Remote {
                name: name.to_owned(),
                url: url.to_owned(),
            });
        }
    }
    list
}

/// `git show --name-status -z --format=`: `M\0path\0R100\0old\0new\0…`.
pub(crate) fn commit_files(output: &str) -> Vec<CommitFile> {
    let mut files = Vec::new();
    let mut fields = output
        .split('\0')
        .map(|f| f.trim_start_matches(['\n', '\r']));
    while let Some(status) = fields.next() {
        let Some(code) = status.chars().next() else {
            continue;
        };
        let Some(kind) = kind(code) else {
            continue;
        };
        let first = fields.next().unwrap_or("").to_owned();
        if matches!(kind, ChangeKind::Renamed | ChangeKind::Copied) {
            let path = fields.next().unwrap_or("").to_owned();
            files.push(CommitFile {
                path,
                original: Some(first),
                kind,
            });
        } else {
            files.push(CommitFile {
                path: first,
                original: None,
                kind,
            });
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_v2_status_with_branch_rename_conflict_and_untracked() {
        let out = concat!(
            "# branch.oid 40a3e92384304ac8fd2d7d3ece1d96d7c5418a4a\0",
            "# branch.head main\0",
            "# branch.upstream origin/main\0",
            "# branch.ab +2 -1\0",
            "1 .M N... 100644 100644 100644 aaaaaaa bbbbbbb src/a file.rs\0",
            "1 A. N... 000000 100644 100644 0000000 ccccccc new.txt\0",
            "2 R. N... 100644 100644 100644 ddddddd ddddddd R100 docs/new name.md\0docs/old.md\0",
            "u UU N... 100644 100644 100644 100644 e f g conflict.rs\0",
            "? notes/\0",
        );
        let status = status(out);
        assert_eq!(status.head.branch.as_deref(), Some("main"));
        assert_eq!(status.head.head, "40a3e923");
        assert_eq!(status.head.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.head.ahead, status.head.behind), (2, 1));
        assert_eq!((status.head.changes, status.head.conflicts), (5, 1));
        let c = &status.changes;
        assert_eq!(c[0].path, "src/a file.rs");
        assert_eq!(
            (c[0].staged, c[0].unstaged),
            (None, Some(ChangeKind::Modified))
        );
        assert_eq!(c[1].staged, Some(ChangeKind::Added));
        assert_eq!(c[2].path, "docs/new name.md");
        assert_eq!(c[2].original.as_deref(), Some("docs/old.md"));
        assert_eq!(c[2].staged, Some(ChangeKind::Renamed));
        assert!(c[3].conflicted());
        assert_eq!(c[3].path, "conflict.rs");
        assert!(c[4].untracked());
    }

    #[test]
    fn detached_and_initial_heads() {
        let detached = status("# branch.oid 1234567890abcdef\0# branch.head (detached)\0");
        assert_eq!(detached.head.branch, None);
        assert_eq!(detached.head.label(), "(12345678)");
        let initial = status("# branch.oid (initial)\0# branch.head main\0");
        assert_eq!(initial.head.head, "");
        assert_eq!(initial.head.label(), "main");
    }

    #[test]
    fn log_records_and_decorations() {
        let out = "40a3e92384304ac8fd2d7d3ece1d96d7c5418a4a\u{1f}c2b83f97c22f8d42cdb125c9dcc324695124ab69 9efa80df2ca8ce061cc0414880f35bf47310ea3b\u{1f}Ada\u{1f}ada@example.com\u{1f}1794424043\u{1f}HEAD -> refs/heads/main, refs/remotes/origin/main, refs/remotes/origin/HEAD, tag: refs/tags/v1.0\u{1f}Merge branch 'x'\u{1e}\n9efa80df2ca8ce061cc0414880f35bf47310ea3b\u{1f}\u{1f}Bob\u{1f}b@example.com\u{1f}1793925694\u{1f}\u{1f}root\u{1e}\n";
        let commits = commits(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].parents.len(), 2);
        assert_eq!(commits[0].short_id(), "40a3e923");
        let kinds: Vec<_> = commits[0]
            .refs
            .iter()
            .map(|r| (r.kind, r.name.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (RefKind::Head, "HEAD"),
                (RefKind::Branch, "main"),
                (RefKind::Remote, "origin/main"),
                (RefKind::Tag, "v1.0"),
            ]
        );
        assert!(commits[1].parents.is_empty());
        assert_eq!(commits[1].subject, "root");
    }

    #[test]
    fn branches_tags_stashes_remotes_and_commit_files() {
        let out = "refs/heads/feature\u{1f}abc1234\u{1f}origin/feature\u{1f}ahead 1, behind 3\u{1f} \u{1f}100\u{1f}wip\u{1e}\nrefs/heads/main\u{1f}def5678\u{1f}origin/main\u{1f}\u{1f}*\u{1f}50\u{1f}init\u{1e}\nrefs/remotes/origin/HEAD\u{1f}def5678\u{1f}\u{1f}\u{1f} \u{1f}50\u{1f}init\u{1e}\nrefs/remotes/origin/main\u{1f}def5678\u{1f}\u{1f}\u{1f} \u{1f}50\u{1f}init\u{1e}\nrefs/heads/old\u{1f}0000000\u{1f}origin/old\u{1f}gone\u{1f} \u{1f}10\u{1f}x\u{1e}\n";
        let list = branches(out);
        let names: Vec<_> = list.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["main", "feature", "old", "origin/main"]);
        assert!(list[0].current);
        assert_eq!((list[1].ahead, list[1].behind), (1, 3));
        assert!(list[2].gone);
        assert!(list[3].remote);

        let tags = tags("v1\u{1f}aaaaaaa\u{1f}bbbbbbb\u{1f}20\u{1f}Release\u{1e}\nlight\u{1f}ccccccc\u{1f}\u{1f}10\u{1f}msg\u{1e}\n");
        assert_eq!(tags[0].target, "bbbbbbb");
        assert_eq!(tags[1].target, "ccccccc");

        let stashes = stashes(
            "stash@{0}\u{1f}30\u{1f}On main: wip\u{1e}\nstash@{1}\u{1f}20\u{1f}WIP on x\u{1e}\n",
        );
        assert_eq!(stashes[1].index, 1);
        assert_eq!(stashes[0].message, "On main: wip");

        let remotes = remotes("origin\thttps://example.com/a.git (fetch)\norigin\thttps://example.com/a.git (push)\nfork\tgit@example.com:b.git (fetch)\n");
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[1].name, "fork");

        let files = commit_files("\nM\0src/a.rs\0R087\0old.rs\0new.rs\0A\0b c.txt\0");
        assert_eq!(files.len(), 3);
        assert_eq!(files[1].path, "new.rs");
        assert_eq!(files[1].original.as_deref(), Some("old.rs"));
        assert_eq!(files[2].path, "b c.txt");
    }
}
