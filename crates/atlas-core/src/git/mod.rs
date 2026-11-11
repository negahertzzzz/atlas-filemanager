//! Git integration through the `git` command line: repository detection, the
//! reads behind the Git panel (status, history, branches, stashes, tags,
//! diffs) and the actions it runs. Everything here blocks; call it off the UI
//! thread.
//!
//! Reads are hardened for repositories the user merely browses into:
//! `core.fsmonitor` is disabled (a repository's own config could otherwise
//! run a program on every status), external diff drivers and textconv
//! filters are never used, and optional index locks are skipped so a
//! background refresh never collides with the user's own git commands.
// reason: this module only runs on background threads (probe, panel loads).
#![allow(clippy::disallowed_methods)]

mod graph;
mod parse;

use atlas_model::git::{
    CommitFile, GitAction, History, Overview, PullMode, RepoHead, RepoOperation, ResetMode,
};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Diffs longer than this are cut: the panel is a viewer, not an editor.
pub const DIFF_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub enum GitError {
    /// No `git` executable on this machine.
    Missing,
    Cancelled,
    /// git ran and failed; its own message.
    Failed(String),
    Io(std::io::Error),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => f.write_str("git was not found"),
            Self::Cancelled => f.write_str("cancelled"),
            Self::Failed(message) => f.write_str(message),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for GitError {}

pub type Result<T> = std::result::Result<T, GitError>;

/// The `git` executable: the first absolute `PATH` entry that has one, then
/// the usual Git for Windows locations. Relative `PATH` entries are skipped
/// so a browsed folder can never supply its own `git.exe`.
pub fn git_binary() -> Option<&'static Path> {
    static GIT: OnceLock<Option<PathBuf>> = OnceLock::new();
    GIT.get_or_init(|| {
        let name = if cfg!(windows) { "git.exe" } else { "git" };
        let from_path = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .filter(|dir| dir.is_absolute())
                .map(|dir| dir.join(name))
                .find(|candidate| candidate.is_file())
        });
        from_path.or_else(|| {
            if !cfg!(windows) {
                return None;
            }
            [
                std::env::var_os("ProgramFiles").map(PathBuf::from),
                std::env::var_os("ProgramW6432").map(PathBuf::from),
                std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("Programs")),
            ]
            .into_iter()
            .flatten()
            .map(|base| base.join("Git").join("cmd").join("git.exe"))
            .find(|candidate| candidate.is_file())
        })
    })
    .as_deref()
}

/// The working-tree root of the repository `dir` is in: the nearest folder
/// with a `.git` directory or file (worktrees and submodules use a file).
/// Never looks into `.git` itself.
pub fn find_repo(dir: &Path) -> Option<PathBuf> {
    if dir
        .components()
        .any(|c| c.as_os_str().eq_ignore_ascii_case(".git"))
    {
        return None;
    }
    let mut current = Some(dir);
    while let Some(folder) = current {
        if folder.join(".git").exists() {
            return Some(folder.to_path_buf());
        }
        current = folder.parent();
    }
    None
}

/// The repository's git directory (`.git`, or where a `.git` file points).
fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    let target = PathBuf::from(target);
    Some(if target.is_absolute() {
        target
    } else {
        root.join(target)
    })
}

/// A merge, rebase… left in progress, from the marker files git keeps.
fn operation(root: &Path) -> Option<RepoOperation> {
    let dir = git_dir(root)?;
    if dir.join("rebase-merge").is_dir() || dir.join("rebase-apply").is_dir() {
        Some(RepoOperation::Rebase)
    } else if dir.join("MERGE_HEAD").is_file() {
        Some(RepoOperation::Merge)
    } else if dir.join("CHERRY_PICK_HEAD").is_file() {
        Some(RepoOperation::CherryPick)
    } else if dir.join("REVERT_HEAD").is_file() {
        Some(RepoOperation::Revert)
    } else if dir.join("BISECT_LOG").is_file() {
        Some(RepoOperation::Bisect)
    } else {
        None
    }
}

fn command(root: &Path, read_only: bool) -> Result<Command> {
    let git = git_binary().ok_or(GitError::Missing)?;
    let mut cmd = Command::new(git);
    cmd.current_dir(root)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "color.ui=never",
            "-c",
            "core.quotepath=false",
            "-c",
            "i18n.logOutputEncoding=UTF-8",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        // Never wait on an editor nobody can see: messages are given up front.
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if read_only {
        cmd.env("GIT_OPTIONAL_LOCKS", "0");
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: no console flashes for git or its helpers.
        cmd.creation_flags(0x0800_0000);
    }
    Ok(cmd)
}

/// Run git in `root` and return stdout. `ok_codes` lists exit codes besides 0
/// that still mean success (`diff --no-index` exits 1 on differences).
fn run_with(
    root: &Path,
    args: &[&str],
    read_only: bool,
    input: Option<&str>,
    ok_codes: &[i32],
    cancel: Option<&AtomicBool>,
) -> Result<String> {
    let mut cmd = command(root, read_only)?;
    cmd.args(args);
    let mut child = cmd.spawn().map_err(GitError::Io)?;
    if let Some(mut stdin) = child.stdin.take() {
        if let Some(input) = input {
            let _ = stdin.write_all(input.as_bytes());
        }
    }
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe
                .by_ref()
                .take(DIFF_LIMIT as u64 * 4)
                .read_to_end(&mut buffer);
            // Drain the rest so git is never blocked on a full pipe.
            let _ = std::io::copy(pipe, &mut std::io::sink());
        }
        buffer
    });
    let err = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    });
    let status = loop {
        if let Some(status) = child.try_wait().map_err(GitError::Io)? {
            break status;
        }
        if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitError::Cancelled);
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let code = status.code().unwrap_or(-1);
    if status.success() || ok_codes.contains(&code) {
        return Ok(String::from_utf8_lossy(&stdout).into_owned());
    }
    let message = String::from_utf8_lossy(&stderr);
    let message = message.trim();
    Err(GitError::Failed(if message.is_empty() {
        String::from_utf8_lossy(&stdout).trim().to_owned()
    } else {
        message.to_owned()
    }))
}

fn read(root: &Path, args: &[&str]) -> Result<String> {
    run_with(root, args, true, None, &[], None)
}

/// Branch, upstream and change counts for the pane header.
pub fn head(root: &Path) -> Result<RepoHead> {
    let mut status = status(root)?;
    status.head.root = root.to_path_buf();
    Ok(status.head)
}

pub fn status(root: &Path) -> Result<atlas_model::git::Status> {
    let output = read(root, &["status", "--porcelain=v2", "--branch", "-z"])?;
    let mut status = parse::status(&output);
    status.head.root = root.to_path_buf();
    status.head.operation = operation(root);
    Ok(status)
}

pub fn overview(root: &Path) -> Result<Overview> {
    let status = status(root)?;
    let branches = parse::branches(&read(
        root,
        &[
            "for-each-ref",
            parse::BRANCH_FORMAT,
            "refs/heads",
            "refs/remotes",
        ],
    )?);
    let tags = parse::tags(&read(
        root,
        &[
            "for-each-ref",
            "--sort=-creatordate",
            parse::TAG_FORMAT,
            "refs/tags",
        ],
    )?);
    // A repository without stashes has no `refs/stash`; that is not an error.
    let stashes = read(root, &["stash", "list", parse::STASH_FORMAT])
        .map(|out| parse::stashes(&out))
        .unwrap_or_default();
    let remotes = parse::remotes(&read(root, &["remote", "-v"])?);
    Ok(Overview {
        status,
        branches,
        stashes,
        tags,
        remotes,
    })
}

/// Newest `limit` commits of every branch (or only HEAD), with graph lanes.
pub fn history(root: &Path, limit: usize, all: bool) -> Result<History> {
    let count = format!("--max-count={}", limit + 1);
    let mut args = vec![
        "log",
        "--date-order",
        "--decorate=full",
        parse::LOG_FORMAT,
        &count,
    ];
    if all {
        args.push("--all");
    }
    log(root, &args, limit, true)
}

/// Newest `limit` commits that touched `path`, following renames of a file.
/// Parents are not rewritten to the filtered list, so there is no graph.
pub fn file_history(root: &Path, path: &str, limit: usize) -> Result<History> {
    let count = format!("--max-count={}", limit + 1);
    let mut args = vec!["log", "--decorate=full", parse::LOG_FORMAT, &count];
    // `--follow` takes a single file; a folder is listed without it.
    if !path.ends_with('/') {
        args.push("--follow");
    }
    args.extend(["--", path]);
    log(root, &args, limit, false)
}

fn log(root: &Path, args: &[&str], limit: usize, with_graph: bool) -> Result<History> {
    let output = match read(root, args) {
        Ok(output) => output,
        // A new repository has no commits yet.
        Err(GitError::Failed(message))
            if message.contains("does not have any commits")
                || message.contains("bad default revision") =>
        {
            String::new()
        }
        Err(error) => return Err(error),
    };
    let mut commits = parse::commits(&output);
    let truncated = commits.len() > limit;
    commits.truncate(limit);
    let graph = if with_graph {
        graph::layout(&commits)
    } else {
        Vec::new()
    };
    Ok(History {
        commits,
        graph,
        truncated,
    })
}

/// Full message, author and committer of one commit.
pub fn commit_message(root: &Path, id: &str) -> Result<String> {
    read(root, &["show", "--no-patch", "--format=%B", id]).map(|text| text.trim_end().to_owned())
}

pub fn commit_files(root: &Path, id: &str) -> Result<Vec<CommitFile>> {
    let output = read(
        root,
        &[
            "show",
            "--diff-merges=first-parent",
            "--name-status",
            "-z",
            "--format=",
            id,
        ],
    )?;
    Ok(parse::commit_files(&output))
}

/// What to show a diff of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffTarget {
    /// Working tree vs index.
    Unstaged(String),
    /// Index vs HEAD.
    Staged(String),
    /// A new file git does not track yet.
    Untracked(String),
    /// One file of a commit, against its first parent.
    Commit { id: String, path: Option<String> },
    /// What a stash entry holds.
    Stash(usize),
}

pub fn diff(root: &Path, target: &DiffTarget) -> Result<String> {
    const FLAGS: [&str; 3] = ["--no-ext-diff", "--no-textconv", "--no-color"];
    let mut text = match target {
        DiffTarget::Unstaged(path) => {
            let mut args = vec!["diff"];
            args.extend(FLAGS);
            args.extend(["--", path.as_str()]);
            read(root, &args)?
        }
        DiffTarget::Staged(path) => {
            let mut args = vec!["diff", "--cached"];
            args.extend(FLAGS);
            args.extend(["--", path.as_str()]);
            read(root, &args)?
        }
        DiffTarget::Untracked(path) => {
            let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
            let mut args = vec!["diff", "--no-index"];
            args.extend(FLAGS);
            args.extend(["--", null, path.as_str()]);
            run_with(root, &args, true, None, &[1], None)?
        }
        DiffTarget::Commit { id, path } => {
            let mut args = vec![
                "show",
                "--diff-merges=first-parent",
                "--format=",
                "--stat=120",
                "--patch",
            ];
            args.extend(FLAGS);
            args.push(id.as_str());
            if let Some(path) = path {
                args.extend(["--", path.as_str()]);
            }
            read(root, &args)?
        }
        DiffTarget::Stash(index) => {
            let name = format!("stash@{{{index}}}");
            let mut args = vec!["stash", "show", "--stat=120", "--patch"];
            args.extend(FLAGS);
            args.push(name.as_str());
            read(root, &args)?
        }
    };
    if text.len() > DIFF_LIMIT {
        let mut cut = DIFF_LIMIT;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n…\n");
    }
    Ok(text)
}

fn owned(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_owned()).collect()
}

fn with_paths(mut args: Vec<String>, paths: &[String]) -> Vec<String> {
    args.push("--".into());
    args.extend(paths.iter().cloned());
    args
}

fn operation_command(op: RepoOperation) -> &'static str {
    match op {
        RepoOperation::Merge => "merge",
        RepoOperation::Rebase => "rebase",
        RepoOperation::CherryPick => "cherry-pick",
        RepoOperation::Revert => "revert",
        RepoOperation::Bisect => "bisect",
    }
}

/// Command lines for an action, run in order. Commit messages travel on
/// stdin, so they are not part of the arguments.
pub fn action_args(action: &GitAction) -> Vec<Vec<String>> {
    let one = |args: Vec<String>| vec![args];
    match action {
        GitAction::Init => one(owned(&["init"])),
        GitAction::Stage(paths) => one(with_paths(owned(&["add", "-A"]), paths)),
        GitAction::StageAll => one(owned(&["add", "-A"])),
        // Without `HEAD`, so it also works before the first commit.
        GitAction::Unstage(paths) => one(with_paths(owned(&["reset", "-q"]), paths)),
        GitAction::UnstageAll => one(owned(&["reset", "-q"])),
        // Written to `.gitignore` by `run`, no git command.
        GitAction::Ignore(_) => Vec::new(),
        GitAction::Discard { tracked, .. } if tracked.is_empty() => Vec::new(),
        // Back to the staged version: staged changes are kept.
        GitAction::Discard { tracked, .. } => {
            one(with_paths(owned(&["restore", "--worktree"]), tracked))
        }
        GitAction::Resolve { paths, ours } => vec![
            with_paths(
                owned(&["checkout", if *ours { "--ours" } else { "--theirs" }]),
                paths,
            ),
            with_paths(owned(&["add"]), paths),
        ],
        GitAction::Commit { amend, .. } => {
            let mut args = owned(&["commit", "-F", "-"]);
            if *amend {
                args.push("--amend".into());
            }
            one(args)
        }
        GitAction::Switch(name) => one(owned(&["switch", name])),
        GitAction::Detach(target) => one(owned(&["switch", "--detach", target])),
        GitAction::Track(remote) => one(owned(&["switch", "--track", remote])),
        GitAction::CreateBranch {
            name,
            start,
            switch,
        } => {
            let mut args = if *switch {
                owned(&["switch", "-c", name])
            } else {
                owned(&["branch", name])
            };
            args.extend(start.iter().cloned());
            one(args)
        }
        GitAction::RenameBranch { from, to } => one(owned(&["branch", "-m", from, to])),
        GitAction::DeleteBranch { name, force } => {
            one(owned(&["branch", if *force { "-D" } else { "-d" }, name]))
        }
        GitAction::DeleteRemoteBranch { remote, branch } => {
            one(owned(&["push", remote, "--delete", branch]))
        }
        GitAction::Merge { name, no_ff } => {
            let mut args = owned(&["merge", "--no-edit"]);
            if *no_ff {
                args.push("--no-ff".into());
            }
            args.push(name.clone());
            one(args)
        }
        GitAction::Rebase(onto) => one(owned(&["rebase", onto])),
        GitAction::Continue(op) => match op {
            // `merge --continue` would open an editor for the message.
            RepoOperation::Merge => one(owned(&["commit", "--no-edit"])),
            RepoOperation::Bisect => one(owned(&["bisect", "good"])),
            op => one(owned(&[operation_command(*op), "--continue"])),
        },
        GitAction::Abort(op) => match op {
            RepoOperation::Bisect => one(owned(&["bisect", "reset"])),
            op => one(owned(&[operation_command(*op), "--abort"])),
        },
        GitAction::Skip(op) => match op {
            RepoOperation::Bisect => one(owned(&["bisect", "skip"])),
            op => one(owned(&[operation_command(*op), "--skip"])),
        },
        GitAction::Fetch { remote, prune } => {
            let mut args = owned(&["fetch"]);
            if *prune {
                args.push("--prune".into());
            }
            match remote {
                Some(remote) => args.push(remote.clone()),
                None => args.push("--all".into()),
            }
            one(args)
        }
        GitAction::Pull(mode) => one(match mode {
            PullMode::Default => owned(&["pull"]),
            PullMode::FastForwardOnly => owned(&["pull", "--ff-only"]),
            PullMode::Rebase => owned(&["pull", "--rebase"]),
        }),
        GitAction::Push {
            remote,
            branch,
            set_upstream,
            force_with_lease,
        } => {
            let mut args = owned(&["push"]);
            if *set_upstream {
                args.push("-u".into());
            }
            if *force_with_lease {
                args.push("--force-with-lease".into());
            }
            args.extend(remote.iter().cloned());
            args.extend(branch.iter().cloned());
            one(args)
        }
        GitAction::PushTags => one(owned(&["push", "--tags"])),
        GitAction::StashSave {
            message,
            include_untracked,
            keep_index,
        } => {
            let mut args = owned(&["stash", "push"]);
            if *include_untracked {
                args.push("--include-untracked".into());
            }
            if *keep_index {
                args.push("--keep-index".into());
            }
            if !message.trim().is_empty() {
                args.extend(["-m".into(), message.trim().to_owned()]);
            }
            one(args)
        }
        GitAction::StashApply(i) => one(owned(&["stash", "apply", &format!("stash@{{{i}}}")])),
        GitAction::StashPop(i) => one(owned(&["stash", "pop", &format!("stash@{{{i}}}")])),
        GitAction::StashDrop(i) => one(owned(&["stash", "drop", &format!("stash@{{{i}}}")])),
        GitAction::CreateTag {
            name,
            target,
            message,
        } => {
            let mut args = owned(&["tag"]);
            if !message.trim().is_empty() {
                args.extend(["-a".into(), "-m".into(), message.trim().to_owned()]);
            }
            args.push(name.clone());
            args.extend(target.iter().cloned());
            one(args)
        }
        GitAction::DeleteTag(name) => one(owned(&["tag", "-d", name])),
        GitAction::PushTag { remote, name } => {
            one(owned(&["push", remote, &format!("refs/tags/{name}")]))
        }
        GitAction::CherryPick(id) => one(owned(&["cherry-pick", id])),
        GitAction::Revert(id) => one(owned(&["revert", "--no-edit", id])),
        GitAction::Reset { target, mode } => one(owned(&[
            "reset",
            match mode {
                ResetMode::Soft => "--soft",
                ResetMode::Mixed => "--mixed",
                ResetMode::Hard => "--hard",
            },
            target,
        ])),
        GitAction::AddRemote { name, url } => one(owned(&["remote", "add", name, url])),
        GitAction::RemoveRemote(name) => one(owned(&["remote", "remove", name])),
    }
}

/// Run `action` in `root` and return git's combined output for the log.
/// Network actions stop when `cancel` is set.
pub fn run(root: &Path, action: &GitAction, cancel: &AtomicBool) -> Result<String> {
    let mut log = String::new();
    // Untracked files have nothing to restore: they go to the Recycle Bin.
    if let GitAction::Discard { untracked, .. } = action {
        for path in untracked {
            let relative = path
                .trim_end_matches('/')
                .replace('/', std::path::MAIN_SEPARATOR_STR);
            let full = root.join(relative);
            crate::cleanup::move_to_trash(&full).map_err(GitError::Io)?;
        }
    }
    if let GitAction::Ignore(paths) = action {
        append_ignore(root, paths)?;
    }
    let input = match action {
        GitAction::Commit { message, .. } => Some(message.as_str()),
        _ => None,
    };
    for args in action_args(action) {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = run_with(root, &refs, false, input, &[], Some(cancel))?;
        log.push_str(output.trim_end());
    }
    Ok(log)
}

/// A `.gitignore` line matching exactly `path` (relative, `/`-separated) from
/// the repository root: anchored with a leading `/`, with the characters git
/// reads as patterns escaped. Directories keep their trailing `/`.
pub fn ignore_pattern(path: &str) -> String {
    let mut line = String::with_capacity(path.len() + 2);
    line.push('/');
    for ch in path.trim_start_matches('/').chars() {
        if matches!(ch, '*' | '?' | '[' | ']' | '\\' | '!' | '#') {
            line.push('\\');
        }
        line.push(ch);
    }
    // Trailing spaces are dropped by git unless escaped.
    let trimmed = line.trim_end_matches(' ').len();
    let spaces = line.len() - trimmed;
    line.truncate(trimmed);
    for _ in 0..spaces {
        line.push_str("\\ ");
    }
    line
}

/// Append `paths` to the root `.gitignore`, skipping lines already there.
fn append_ignore(root: &Path, paths: &[String]) -> Result<()> {
    let file = root.join(".gitignore");
    let existing = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(GitError::Io(error)),
    };
    let known: std::collections::HashSet<&str> = existing.lines().map(str::trim_end).collect();
    let mut added = String::new();
    for path in paths {
        let line = ignore_pattern(path);
        if !known.contains(line.as_str()) && !added.lines().any(|l| l == line) {
            added.push_str(&line);
            added.push('\n');
        }
    }
    if added.is_empty() {
        return Ok(());
    }
    // Keep the file's line endings when it already uses CRLF.
    let crlf = existing.contains("\r\n");
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push_str(if crlf { "\r\n" } else { "\n" });
    }
    if crlf {
        added = added.replace('\n', "\r\n");
    }
    text.push_str(&added);
    std::fs::write(&file, text).map_err(GitError::Io)
}

/// Check that a branch or tag name is one git accepts.
pub fn valid_ref_name(root: &Path, name: &str) -> bool {
    !name.trim().is_empty()
        && !name.starts_with('-')
        && read(root, &["check-ref-format", "--branch", name]).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_detection_walks_up_and_ignores_git_internals() {
        let base = std::env::temp_dir().join(format!("atlas-git-find-{}", std::process::id()));
        let nested = base.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(base.join(".git").join("objects")).unwrap();
        assert_eq!(find_repo(&nested), Some(base.clone()));
        assert_eq!(find_repo(&base.join(".git").join("objects")), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn actions_build_safe_argument_lists() {
        let args = action_args(&GitAction::Stage(vec!["-rf".into(), "a b.txt".into()]));
        assert_eq!(args, [["add", "-A", "--", "-rf", "a b.txt"]]);
        let args = action_args(&GitAction::Commit {
            message: "x".into(),
            amend: true,
        });
        assert_eq!(args, [["commit", "-F", "-", "--amend"]]);
        let args = action_args(&GitAction::Push {
            remote: Some("origin".into()),
            branch: Some("main".into()),
            set_upstream: true,
            force_with_lease: false,
        });
        assert_eq!(args, [["push", "-u", "origin", "main"]]);
        assert_eq!(
            action_args(&GitAction::StashPop(2)),
            [["stash", "pop", "stash@{2}"]]
        );
        assert!(action_args(&GitAction::Discard {
            tracked: Vec::new(),
            untracked: vec!["x".into()]
        })
        .is_empty());
        assert!(GitAction::Reset {
            target: "HEAD~1".into(),
            mode: ResetMode::Hard
        }
        .is_destructive());
        assert!(!GitAction::Reset {
            target: "HEAD~1".into(),
            mode: ResetMode::Soft
        }
        .is_destructive());
    }

    /// End to end against the real git, when the machine has one.
    #[test]
    fn status_history_and_actions_on_a_scratch_repository() {
        if git_binary().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!("atlas-git-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let never = AtomicBool::new(false);
        run(&root, &GitAction::Init, &never).unwrap();
        for (key, value) in [
            ("user.name", "Atlas Test"),
            ("user.email", "atlas@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            run_with(&root, &["config", key, value], false, None, &[], None).unwrap();
        }
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        let fresh = status(&root).unwrap();
        assert!(fresh.changes[0].untracked());
        run(&root, &GitAction::StageAll, &never).unwrap();
        run(
            &root,
            &GitAction::Commit {
                message: "first line\n\nbody".into(),
                amend: false,
            },
            &never,
        )
        .unwrap();
        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        let diff = diff(&root, &DiffTarget::Unstaged("a.txt".into())).unwrap();
        assert!(diff.contains("+two"));
        run(
            &root,
            &GitAction::CreateBranch {
                name: "topic".into(),
                start: None,
                switch: true,
            },
            &never,
        )
        .unwrap();
        let overview = overview(&root).unwrap();
        assert_eq!(overview.status.head.branch.as_deref(), Some("topic"));
        assert_eq!(overview.status.head.changes, 1);
        assert_eq!(overview.branches.len(), 2);
        let history = history(&root, 10, true).unwrap();
        assert_eq!(history.commits.len(), 1);
        assert_eq!(history.commits[0].subject, "first line");
        assert_eq!(
            commit_message(&root, &history.commits[0].id).unwrap(),
            "first line\n\nbody"
        );
        let files = commit_files(&root, &history.commits[0].id).unwrap();
        assert_eq!(files[0].path, "a.txt");
        run(
            &root,
            &GitAction::StashSave {
                message: "wip".into(),
                include_untracked: false,
                keep_index: false,
            },
            &never,
        )
        .unwrap();
        assert_eq!(overview_stashes(&root), 1);
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        run(&root, &GitAction::StageAll, &never).unwrap();
        run(
            &root,
            &GitAction::Commit {
                message: "second".into(),
                amend: false,
            },
            &never,
        )
        .unwrap();
        let file = file_history(&root, "a.txt", 10).unwrap();
        assert_eq!(file.commits.len(), 1);
        assert_eq!(file.commits[0].subject, "first line");
        assert!(file.graph.is_empty());
        assert_eq!(file_history(&root, "b.txt", 10).unwrap().commits.len(), 1);
        std::fs::write(root.join("junk.log"), "x").unwrap();
        run(&root, &GitAction::Ignore(vec!["junk.log".into()]), &never).unwrap();
        run(&root, &GitAction::Ignore(vec!["junk.log".into()]), &never).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join(".gitignore")).unwrap(),
            "/junk.log\n"
        );
        let after = status(&root).unwrap();
        assert!(after.changes.iter().all(|c| c.path != "junk.log"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ignore_patterns_match_only_the_given_path() {
        assert_eq!(ignore_pattern("build/"), "/build/");
        assert_eq!(ignore_pattern("a b/c.txt"), "/a b/c.txt");
        assert_eq!(ignore_pattern("#notes[1]*.md"), "/\\#notes\\[1\\]\\*.md");
        assert_eq!(ignore_pattern("odd "), "/odd\\ ");
    }

    fn overview_stashes(root: &Path) -> usize {
        overview(root).unwrap().stashes.len()
    }
}
