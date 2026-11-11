//! Git repository state for the Git panel: working-tree changes, commit
//! history with graph lanes, branches, stashes, tags and the actions the
//! panel can run. Produced by `atlas_core::git` off the UI thread.
use std::path::PathBuf;

/// What a pane's folder knows about the repository it sits in; drives the
/// branch chip in the pane header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoHead {
    /// Working-tree root (the folder that holds `.git`).
    pub root: PathBuf,
    /// Current branch; `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Short id of HEAD; empty in a repository without commits.
    pub head: String,
    /// Upstream of the current branch, e.g. `origin/main`.
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Changed paths: staged, unstaged, untracked and conflicted.
    pub changes: u32,
    pub conflicts: u32,
    /// A merge, rebase, cherry-pick… waiting to be continued or aborted.
    pub operation: Option<RepoOperation>,
}

impl RepoHead {
    /// Label for the branch chip: the branch, or the detached commit.
    pub fn label(&self) -> String {
        match &self.branch {
            Some(branch) => branch.clone(),
            None if self.head.is_empty() => "HEAD".to_owned(),
            None => format!("({})", self.head),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoOperation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    Bisect,
}

/// How a path differs, in the index (staged) or the working tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    /// One-letter badge, as `git status --short` prints it.
    pub const fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "U",
            Self::Conflicted => "!",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Repository-relative path with `/` separators.
    pub path: String,
    /// Source of a rename or copy.
    pub original: Option<String>,
    /// Index vs HEAD.
    pub staged: Option<ChangeKind>,
    /// Working tree vs index (untracked and conflicted paths live here).
    pub unstaged: Option<ChangeKind>,
}

impl FileChange {
    pub fn conflicted(&self) -> bool {
        self.unstaged == Some(ChangeKind::Conflicted)
    }
    pub fn untracked(&self) -> bool {
        self.unstaged == Some(ChangeKind::Untracked)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub head: RepoHead,
    pub changes: Vec<FileChange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// `HEAD ->` marker of the current branch, or a detached HEAD.
    Head,
    Branch,
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    /// Author date, seconds since the Unix epoch.
    pub time: i64,
    pub refs: Vec<RefLabel>,
    pub subject: String,
}

impl Commit {
    pub fn short_id(&self) -> &str {
        self.id.get(..8).unwrap_or(&self.id)
    }
}

/// Which part of a history row a graph line covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphSpan {
    /// Row top to the commit dot's height.
    Top,
    /// The commit dot's height to the row bottom.
    Bottom,
    /// Straight through the whole row.
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphLine {
    pub from: u16,
    pub to: u16,
    pub span: GraphSpan,
    /// Palette index: lines of one lane keep one colour.
    pub color: u16,
}

/// Graph cell of one history row: the commit dot's lane and the lines around it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphRow {
    pub lane: u16,
    pub lines: Vec<GraphLine>,
    /// Lanes in use on this row, for the cell width.
    pub width: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    pub commits: Vec<Commit>,
    pub graph: Vec<GraphRow>,
    /// More commits exist past the requested limit.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// `main` for local branches, `origin/main` for remote ones.
    pub name: String,
    pub remote: bool,
    pub current: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// The upstream branch no longer exists.
    pub gone: bool,
    pub target: String,
    pub time: i64,
    pub subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    /// Position in `git stash list` (`stash@{index}`).
    pub index: usize,
    pub time: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// Commit the tag points at (peeled for annotated tags).
    pub target: String,
    pub time: i64,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

/// Everything the Git panel shows except the history, loaded together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overview {
    pub status: Status,
    pub branches: Vec<Branch>,
    pub stashes: Vec<Stash>,
    pub tags: Vec<Tag>,
    pub remotes: Vec<Remote>,
}

/// One file of a commit, as `git show --name-status` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFile {
    pub path: String,
    pub original: Option<String>,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullMode {
    /// Whatever `pull.rebase` / `pull.ff` say.
    Default,
    FastForwardOnly,
    Rebase,
}

/// A change the Git panel asks git to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitAction {
    Init,
    Stage(Vec<String>),
    StageAll,
    Unstage(Vec<String>),
    UnstageAll,
    /// Throw away working-tree changes (tracked) or delete files (untracked).
    Discard {
        tracked: Vec<String>,
        untracked: Vec<String>,
    },
    /// Settle conflicted paths with our or their version and stage them.
    Resolve {
        paths: Vec<String>,
        ours: bool,
    },
    Commit {
        message: String,
        amend: bool,
    },
    /// Switch to a local branch.
    Switch(String),
    /// Check out a commit or tag without a branch.
    Detach(String),
    /// Create a local branch tracking `origin/x` and switch to it.
    Track(String),
    CreateBranch {
        name: String,
        start: Option<String>,
        switch: bool,
    },
    RenameBranch {
        from: String,
        to: String,
    },
    DeleteBranch {
        name: String,
        force: bool,
    },
    /// Delete `origin/x` on the server.
    DeleteRemoteBranch {
        remote: String,
        branch: String,
    },
    Merge {
        name: String,
        no_ff: bool,
    },
    Rebase(String),
    Continue(RepoOperation),
    Abort(RepoOperation),
    Skip(RepoOperation),
    Fetch {
        remote: Option<String>,
        prune: bool,
    },
    Pull(PullMode),
    Push {
        remote: Option<String>,
        branch: Option<String>,
        set_upstream: bool,
        force_with_lease: bool,
    },
    PushTags,
    StashSave {
        message: String,
        include_untracked: bool,
        keep_index: bool,
    },
    StashApply(usize),
    StashPop(usize),
    StashDrop(usize),
    CreateTag {
        name: String,
        target: Option<String>,
        message: String,
    },
    DeleteTag(String),
    PushTag {
        remote: String,
        name: String,
    },
    CherryPick(String),
    Revert(String),
    Reset {
        target: String,
        mode: ResetMode,
    },
    AddRemote {
        name: String,
        url: String,
    },
    RemoveRemote(String),
}

impl GitAction {
    /// Network operations: they may ask for credentials and take a while.
    pub const fn is_network(&self) -> bool {
        matches!(
            self,
            Self::Fetch { .. }
                | Self::Pull(_)
                | Self::Push { .. }
                | Self::PushTags
                | Self::PushTag { .. }
                | Self::DeleteRemoteBranch { .. }
        )
    }

    /// Loses work or rewrites shared state: the panel asks before running it.
    pub const fn is_destructive(&self) -> bool {
        matches!(
            self,
            Self::Discard { .. }
                | Self::DeleteBranch { .. }
                | Self::DeleteRemoteBranch { .. }
                | Self::StashDrop(_)
                | Self::DeleteTag(_)
                | Self::RemoveRemote(_)
                | Self::Reset {
                    mode: ResetMode::Hard,
                    ..
                }
                | Self::Push {
                    force_with_lease: true,
                    ..
                }
        )
    }
}
