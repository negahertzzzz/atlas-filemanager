//! Git panel for the repository a folder belongs to: working-tree changes
//! with staging and commits, the history graph, branches, stashes, tags and
//! remotes, with diffs on the right. Every git call runs off the UI thread;
//! actions that lose work ask for confirmation first.
#![allow(clippy::too_many_lines)] // reason: one cohesive panel, split by tab below
use crate::app::App as AtlasApp;
use crate::ui::icons::AtlasIcon;
use atlas_core::git::{self, DiffTarget, GitError};
use atlas_model::git::{
    ChangeKind, CommitFile, GitAction, GraphRow, GraphSpan, History, Overview, PullMode, RefKind,
    RepoOperation, ResetMode,
};
use gpui::{prelude::*, *};
use gpui_component::{
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    h_flex,
    input::{InputEvent, InputState},
    menu::{DropdownMenu as _, PopupMenuItem},
    v_flex, ActiveTheme, Disableable, Icon, Sizable,
};
use rows::TreeRow;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod render;
mod rows;

const HISTORY_PAGE: usize = 1500;
const ROW_H: f32 = 28.;
const LANE_W: f32 = 12.;

fn tr(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}

fn icon(icon: AtlasIcon) -> Icon {
    Icon::empty().path(icon.path())
}

fn date(seconds: i64) -> String {
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds.max(0).unsigned_abs());
    atlas_core::util::display::format_date(at)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Changes,
    History,
    Branches,
    Stashes,
    Tags,
    Remotes,
}

impl Tab {
    const ALL: [Self; 6] = [
        Self::Changes,
        Self::History,
        Self::Branches,
        Self::Stashes,
        Self::Tags,
        Self::Remotes,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Changes => "git_tab_changes",
            Self::History => "git_tab_history",
            Self::Branches => "git_tab_branches",
            Self::Stashes => "git_tab_stashes",
            Self::Tags => "git_tab_tags",
            Self::Remotes => "git_tab_remotes",
        }
    }
}

enum Repo {
    Loading,
    /// No git executable on this machine.
    Missing,
    NotRepo,
    Failed(String),
    Ready(PathBuf),
}

enum Loaded {
    Missing,
    NotRepo,
    Failed(String),
    Ready(PathBuf, Overview),
}

#[derive(Clone, Default, PartialEq, Eq)]
enum Selection {
    #[default]
    None,
    Change {
        staged: bool,
        path: String,
    },
    Commit {
        id: String,
        file: Option<String>,
    },
    Branch(String),
    Stash(usize),
    Tag(String),
    Remote(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Meta,
    Header,
    Hunk,
    Add,
    Remove,
    Context,
}

type DiffLines = Arc<Vec<(LineKind, SharedString)>>;

#[derive(Default)]
struct Detail {
    loading: bool,
    message: String,
    files: Vec<CommitFile>,
    lines: DiffLines,
    error: Option<String>,
}

#[derive(Default)]
struct DetailData {
    message: String,
    files: Vec<CommitFile>,
    lines: Vec<(LineKind, SharedString)>,
    error: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
enum Prompt {
    NewBranch { start: Option<String> },
    RenameBranch(String),
    NewTag { target: Option<String> },
    Stash,
    AddRemote,
}

impl Prompt {
    fn label(&self) -> String {
        match self {
            Self::NewBranch { start: None } => tr("git_prompt_new_branch"),
            Self::NewBranch { start: Some(at) } => {
                rust_i18n::t!("git_prompt_new_branch_at", at = short(at)).into_owned()
            }
            Self::RenameBranch(from) => {
                rust_i18n::t!("git_prompt_rename_branch", name = from).into_owned()
            }
            Self::NewTag { target: None } => tr("git_prompt_new_tag"),
            Self::NewTag { target: Some(at) } => {
                rust_i18n::t!("git_prompt_new_tag_at", at = short(at)).into_owned()
            }
            Self::Stash => tr("git_prompt_stash"),
            Self::AddRemote => tr("git_prompt_add_remote"),
        }
    }
}

fn short(id: &str) -> &str {
    if id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        &id[..8]
    } else {
        id
    }
}

/// The three groups of the Changes list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Conflicts,
    Staged,
    Unstaged,
}

impl Section {
    /// Collapse-state prefix of the section's folder tree.
    const fn prefix(self) -> &'static str {
        match self {
            Self::Conflicts => "C:",
            Self::Staged => "S:",
            Self::Unstaged => "U:",
        }
    }
}

/// Rows of the Changes list.
#[derive(Clone)]
enum ChangeRow {
    Header(&'static str, usize),
    Folder {
        section: Section,
        key: String,
        label: String,
        depth: u16,
        /// Indexes of every change below the folder.
        items: Arc<Vec<usize>>,
        collapsed: bool,
    },
    /// `depth` is set in the tree view, which shows only the file name.
    File {
        staged: bool,
        index: usize,
        depth: Option<u16>,
    },
}

/// Rows of the Branches list.
#[derive(Clone)]
enum BranchRow {
    Header(String),
    Folder {
        key: String,
        label: String,
        depth: u16,
        count: usize,
        collapsed: bool,
    },
    /// `label` is set in the tree view: the last segment of the name.
    Branch {
        index: usize,
        label: Option<String>,
        depth: u16,
    },
}

/// How the panel lays out its lists; kept in the settings.
#[derive(Clone, Copy)]
pub(crate) struct GitView {
    pub(crate) branch_tree: bool,
    pub(crate) change_tree: bool,
}

fn diff_lines(text: &str) -> Vec<(LineKind, SharedString)> {
    const HEADERS: [&str; 11] = [
        "--- ",
        "+++ ",
        "index ",
        "new file",
        "deleted file",
        "similarity",
        "rename ",
        "old mode",
        "new mode",
        "Binary files",
        "copy ",
    ];
    let mut in_hunk = false;
    text.lines()
        .map(|line| {
            let kind = if line.starts_with("diff --git") || line.starts_with("diff --cc") {
                in_hunk = false;
                LineKind::Header
            } else if line.starts_with("@@") {
                in_hunk = true;
                LineKind::Hunk
            } else if in_hunk {
                match line.as_bytes().first() {
                    Some(b'+') => LineKind::Add,
                    Some(b'-') => LineKind::Remove,
                    Some(b'\\') => LineKind::Meta,
                    _ => LineKind::Context,
                }
            } else if HEADERS.iter().any(|h| line.starts_with(h)) {
                LineKind::Header
            } else {
                LineKind::Meta
            };
            let mut shown: String = line.chars().take(1200).collect();
            if shown.contains('\t') {
                shown = shown.replace('\t', "    ");
            }
            (kind, SharedString::from(shown))
        })
        .collect()
}

/// `git …` as the user would type it, for the status line.
fn action_label(action: &GitAction) -> String {
    if let GitAction::Ignore(paths) = action {
        return rust_i18n::t!("git_ignored", count = paths.len()).into_owned();
    }
    let args = git::action_args(action);
    match args.first() {
        Some(first) => format!("git {}", first.join(" ")),
        None => tr("git_discard"),
    }
}

pub(crate) struct GitPanel {
    pub(crate) focus: FocusHandle,
    parent: WeakEntity<AtlasApp>,
    dir: PathBuf,
    repo: Repo,
    tab: Tab,
    overview: Arc<Overview>,
    history: Arc<History>,
    graph_width: u16,
    history_limit: usize,
    history_all: bool,
    history_loading: bool,
    loads: u64,
    history_loads: u64,
    detail_loads: u64,
    selection: Selection,
    detail: Detail,
    running: Option<(String, Arc<AtomicBool>, bool)>,
    /// Last outcome: `(failed, text)`.
    message: Option<(bool, String)>,
    confirm: Option<(String, GitAction)>,
    prompt: Option<Prompt>,
    commit_input: Entity<InputState>,
    prompt_input: Entity<InputState>,
    prompt_extra: Entity<InputState>,
    amend: bool,
    clear_commit: bool,
    /// Push once the running commit succeeds.
    push_after_commit: bool,
    stash_untracked: bool,
    view: GitView,
    /// Collapsed tree folders, keyed by tree prefix and folder path.
    collapsed: HashSet<String>,
    branch_filter: Entity<InputState>,
    branch_terms: Vec<String>,
    history_filter: Entity<InputState>,
    history_terms: Vec<String>,
    /// Commits shown by the History list: all of them, or the filter's matches.
    history_rows: Arc<Vec<usize>>,
    /// History of one path (`git log --follow`) instead of the branches.
    history_path: Option<String>,
    list_scroll: UniformListScrollHandle,
    diff_scroll: UniformListScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl GitPanel {
    pub(crate) fn new(
        dir: PathBuf,
        parent: WeakEntity<AtlasApp>,
        view: GitView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let commit_input = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 8)
                .placeholder(tr("git_commit_placeholder"))
        });
        let prompt_input = cx.new(|cx| InputState::new(window, cx));
        let prompt_extra = cx.new(|cx| InputState::new(window, cx));
        let branch_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("git_filter_branches")));
        let history_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder(tr("git_filter_commits")));
        let submit = |panel: &mut Self,
                      _: &Entity<InputState>,
                      event: &InputEvent,
                      window: &mut Window,
                      cx: &mut Context<Self>| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                panel.submit_prompt(window, cx);
            }
        };
        let subscriptions = vec![
            cx.subscribe_in(&prompt_input, window, submit),
            cx.subscribe_in(&prompt_extra, window, submit),
            cx.subscribe_in(
                &branch_filter,
                window,
                |panel, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        panel.branch_terms = rows::terms(&input.read(cx).value());
                        panel.list_scroll = UniformListScrollHandle::new();
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &history_filter,
                window,
                |panel, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        panel.history_terms = rows::terms(&input.read(cx).value());
                        panel.refilter_history();
                        panel.list_scroll = UniformListScrollHandle::new();
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &commit_input,
                window,
                |panel, _, event: &InputEvent, window, cx| {
                    // Ctrl+Enter commits, as in most git clients.
                    if let InputEvent::PressEnter {
                        secondary: true, ..
                    } = event
                    {
                        panel.commit(window, cx);
                    }
                },
            ),
        ];
        let mut panel = Self {
            focus: cx.focus_handle(),
            parent,
            dir,
            repo: Repo::Loading,
            tab: Tab::Changes,
            overview: Arc::default(),
            history: Arc::default(),
            graph_width: 1,
            history_limit: HISTORY_PAGE,
            history_all: true,
            history_loading: false,
            loads: 0,
            history_loads: 0,
            detail_loads: 0,
            selection: Selection::None,
            detail: Detail::default(),
            running: None,
            message: None,
            confirm: None,
            prompt: None,
            commit_input,
            prompt_input,
            prompt_extra,
            amend: false,
            clear_commit: false,
            push_after_commit: false,
            stash_untracked: true,
            view,
            collapsed: HashSet::new(),
            branch_filter,
            branch_terms: Vec::new(),
            history_filter,
            history_terms: Vec::new(),
            history_rows: Arc::default(),
            history_path: None,
            list_scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        panel.reload(cx);
        panel
    }

    /// The repository shown, once it is known.
    pub(crate) fn repo_root(&self) -> Option<PathBuf> {
        self.root()
    }

    fn root(&self) -> Option<PathBuf> {
        match &self.repo {
            Repo::Ready(root) => Some(root.clone()),
            _ => None,
        }
    }

    /// Run `work` on a background thread and hand its result to `done`.
    fn spawn_work<T: Send + 'static>(
        cx: &mut Context<Self>,
        work: impl FnOnce() -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    ) {
        let (sender, receiver) = flume::bounded::<T>(1);
        let _ = std::thread::Builder::new()
            .name("atlas-git".into())
            .spawn(move || {
                let _ = sender.send(work());
            });
        cx.spawn(async move |this, cx| {
            if let Ok(value) = receiver.recv_async().await {
                let _ = this.update(cx, |panel, cx| {
                    done(panel, value, cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.loads += 1;
        let generation = self.loads;
        let dir = self.dir.clone();
        let known = self.root();
        Self::spawn_work(
            cx,
            move || {
                if git::git_binary().is_none() {
                    return Loaded::Missing;
                }
                let Some(root) = known.or_else(|| git::find_repo(&dir)) else {
                    return Loaded::NotRepo;
                };
                match git::overview(&root) {
                    Ok(overview) => Loaded::Ready(root, overview),
                    Err(error) => Loaded::Failed(error.to_string()),
                }
            },
            move |panel, loaded, cx| {
                if panel.loads != generation {
                    return;
                }
                match loaded {
                    Loaded::Ready(root, overview) => {
                        panel.repo = Repo::Ready(root);
                        panel.overview = Arc::new(overview);
                        if !panel.selection_exists() {
                            panel.selection = Selection::None;
                        }
                        panel.load_history(cx);
                        panel.load_detail(cx);
                    }
                    // Keep showing what was loaded; report the failure.
                    Loaded::Failed(error) if matches!(panel.repo, Repo::Ready(_)) => {
                        panel.message = Some((true, error));
                    }
                    Loaded::Failed(error) => panel.repo = Repo::Failed(error),
                    Loaded::Missing => panel.repo = Repo::Missing,
                    Loaded::NotRepo => panel.repo = Repo::NotRepo,
                }
            },
        );
        cx.notify();
    }

    fn load_history(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root() else {
            return;
        };
        self.history_loads += 1;
        let generation = self.history_loads;
        self.history_loading = true;
        let (limit, all) = (self.history_limit, self.history_all);
        let path = self.history_path.clone();
        Self::spawn_work(
            cx,
            move || match path {
                Some(path) => git::file_history(&root, &path, limit),
                None => git::history(&root, limit, all),
            },
            move |panel, result, _| {
                if panel.history_loads != generation {
                    return;
                }
                panel.history_loading = false;
                match result {
                    Ok(history) => {
                        panel.graph_width = history
                            .graph
                            .iter()
                            .map(|row| row.width)
                            .max()
                            .unwrap_or(1)
                            .max(1);
                        panel.history = Arc::new(history);
                        panel.refilter_history();
                    }
                    Err(error) => panel.message = Some((true, error.to_string())),
                }
            },
        );
    }

    fn selection_exists(&self) -> bool {
        let o = &self.overview;
        match &self.selection {
            Selection::None => true,
            Selection::Change { staged, path } => o.status.changes.iter().any(|c| {
                &c.path == path
                    && if *staged {
                        c.staged.is_some()
                    } else {
                        c.unstaged.is_some()
                    }
            }),
            // History reloads separately; a vanished commit just shows nothing.
            Selection::Commit { .. } => true,
            Selection::Branch(name) => o.branches.iter().any(|b| &b.name == name),
            Selection::Stash(index) => o.stashes.iter().any(|s| s.index == *index),
            Selection::Tag(name) => o.tags.iter().any(|t| &t.name == name),
            Selection::Remote(name) => o.remotes.iter().any(|r| &r.name == name),
        }
    }

    fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
        if self.selection == selection {
            return;
        }
        self.selection = selection;
        self.load_detail(cx);
        cx.notify();
    }

    fn load_detail(&mut self, cx: &mut Context<Self>) {
        self.detail_loads += 1;
        let generation = self.detail_loads;
        let Some(root) = self.root() else {
            return;
        };
        let target = match &self.selection {
            Selection::Change { staged, path } => {
                let change = self
                    .overview
                    .status
                    .changes
                    .iter()
                    .find(|c| &c.path == path);
                Some(if change.is_some_and(|c| c.untracked()) {
                    DiffTarget::Untracked(path.clone())
                } else if *staged {
                    DiffTarget::Staged(path.clone())
                } else {
                    DiffTarget::Unstaged(path.clone())
                })
            }
            Selection::Commit { id, file } => Some(DiffTarget::Commit {
                id: id.clone(),
                path: file.clone(),
            }),
            Selection::Stash(index) => Some(DiffTarget::Stash(*index)),
            _ => None,
        };
        let Some(target) = target else {
            self.detail = Detail::default();
            return;
        };
        self.detail.loading = true;
        let selection = self.selection.clone();
        // Choosing a file of the same commit keeps its message and file list.
        let reuse = matches!(
            (&selection, &target),
            (Selection::Commit { file: Some(_), .. }, _)
        ) && !self.detail.files.is_empty();
        Self::spawn_work(
            cx,
            move || {
                let mut data = DetailData::default();
                if let (Selection::Commit { id, .. }, false) = (&selection, reuse) {
                    data.message = git::commit_message(&root, id).unwrap_or_default();
                    data.files = git::commit_files(&root, id).unwrap_or_default();
                }
                match git::diff(&root, &target) {
                    Ok(text) => data.lines = diff_lines(&text),
                    Err(error) => data.error = Some(error.to_string()),
                }
                data
            },
            move |panel, data, _| {
                if panel.detail_loads != generation {
                    return;
                }
                let (message, files) = if reuse {
                    (
                        std::mem::take(&mut panel.detail.message),
                        std::mem::take(&mut panel.detail.files),
                    )
                } else {
                    (data.message, data.files)
                };
                panel.detail = Detail {
                    loading: false,
                    message,
                    files,
                    lines: Arc::new(data.lines),
                    error: data.error,
                };
                panel.diff_scroll = UniformListScrollHandle::new();
            },
        );
    }

    /// Ask before actions that lose work; run the others directly.
    fn request(&mut self, action: GitAction, cx: &mut Context<Self>) {
        if action.is_destructive() {
            let text = match &action {
                GitAction::Discard { tracked, untracked } => rust_i18n::t!(
                    "git_confirm_discard",
                    count = tracked.len() + untracked.len()
                )
                .into_owned(),
                GitAction::Reset { target, .. } => {
                    rust_i18n::t!("git_confirm_reset_hard", at = short(target)).into_owned()
                }
                GitAction::DeleteBranch { name, .. } => {
                    rust_i18n::t!("git_confirm_delete_branch", name = name).into_owned()
                }
                GitAction::DeleteRemoteBranch { remote, branch } => rust_i18n::t!(
                    "git_confirm_delete_remote_branch",
                    name = format!("{remote}/{branch}")
                )
                .into_owned(),
                GitAction::StashDrop(index) => {
                    rust_i18n::t!("git_confirm_drop_stash", index = index).into_owned()
                }
                GitAction::DeleteTag(name) => {
                    rust_i18n::t!("git_confirm_delete_tag", name = name).into_owned()
                }
                GitAction::RemoveRemote(name) => {
                    rust_i18n::t!("git_confirm_remove_remote", name = name).into_owned()
                }
                GitAction::Push { .. } => tr("git_confirm_force_push"),
                _ => tr("git_confirm_generic"),
            };
            self.prompt = None;
            self.confirm = Some((text, action));
            cx.notify();
            return;
        }
        self.execute(action, cx);
    }

    fn execute(&mut self, action: GitAction, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        let root = match (&action, self.root()) {
            (_, Some(root)) => root,
            (GitAction::Init, None) => self.dir.clone(),
            _ => return,
        };
        let label = action_label(&action);
        let cancel = Arc::new(AtomicBool::new(false));
        self.running = Some((label.clone(), cancel.clone(), action.is_network()));
        self.message = None;
        self.confirm = None;
        let committed = matches!(action, GitAction::Commit { .. });
        let init = matches!(action, GitAction::Init);
        Self::spawn_work(
            cx,
            move || git::run(&root, &action, &cancel),
            move |panel, result, cx| {
                panel.running = None;
                panel.message = Some(match result {
                    Ok(output) if output.trim().is_empty() => (
                        false,
                        rust_i18n::t!("git_action_done", command = label).into_owned(),
                    ),
                    Ok(output) => {
                        let lines: Vec<&str> = output.lines().collect();
                        let tail = lines[lines.len().saturating_sub(3)..].join("\n");
                        (false, tail)
                    }
                    Err(GitError::Cancelled) => (false, tr("git_cancelled")),
                    Err(error) => (true, error.to_string()),
                });
                let failed = panel.message.as_ref().is_some_and(|(failed, _)| *failed);
                let push = committed && std::mem::take(&mut panel.push_after_commit) && !failed;
                if committed && !failed {
                    panel.amend = false;
                    // Cleared on the next frame, which has the window it needs.
                    panel.clear_commit = true;
                }
                if init {
                    panel.repo = Repo::Loading;
                }
                panel.after_change(cx);
                if let Some(action) = push.then(|| panel.push_action(false)).flatten() {
                    panel.execute(action, cx);
                }
            },
        );
        cx.notify();
    }

    /// Reload after an action and let the panes' branch chips catch up.
    fn after_change(&mut self, cx: &mut Context<Self>) {
        let root = self.root().unwrap_or_else(|| self.dir.clone());
        self.reload(cx);
        let _ = self.parent.update(cx, |app, cx| {
            app.invalidate_git_probe(&root);
            app.notify_all_panes(cx);
            cx.notify();
        });
    }

    fn commit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.commit_then(false, cx);
    }

    /// Commit the staged changes, and push them afterwards when `push` is set.
    fn commit_then(&mut self, push: bool, cx: &mut Context<Self>) {
        if self.running.is_some() || self.root().is_none() {
            return;
        }
        let message = self.commit_input.read(cx).value().trim().to_owned();
        if message.is_empty() {
            self.message = Some((true, tr("git_commit_empty")));
            cx.notify();
            return;
        }
        let staged = self
            .overview
            .status
            .changes
            .iter()
            .any(|c| c.staged.is_some());
        if !staged && !self.amend {
            self.message = Some((true, tr("git_nothing_staged")));
            cx.notify();
            return;
        }
        // An amended commit that was already pushed would need a forced push.
        self.push_after_commit = push && !self.amend;
        self.execute(
            GitAction::Commit {
                message,
                amend: self.amend,
            },
            cx,
        );
    }

    fn set_amend(&mut self, amend: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = amend;
        let empty = self.commit_input.read(cx).value().trim().is_empty();
        // Amending starts from the last commit's message.
        if amend && empty {
            let Some(root) = self.root() else {
                return;
            };
            let input = self.commit_input.downgrade();
            let (sender, receiver) = flume::bounded::<String>(1);
            let _ = std::thread::Builder::new()
                .name("atlas-git".into())
                .spawn(move || {
                    let _ = sender.send(git::commit_message(&root, "HEAD").unwrap_or_default());
                });
            let handle = window.window_handle();
            cx.spawn(async move |_, cx| {
                if let Ok(text) = receiver.recv_async().await {
                    let _ = handle.update(cx, |_, window, cx| {
                        let _ = input.update(cx, |state, cx| state.set_value(text, window, cx));
                    });
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn open_prompt(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        let initial = match &prompt {
            Prompt::RenameBranch(from) => from.clone(),
            _ => String::new(),
        };
        let placeholder = match &prompt {
            Prompt::NewBranch { .. } | Prompt::RenameBranch(_) => tr("git_branch_name"),
            Prompt::NewTag { .. } => tr("git_tag_name"),
            Prompt::Stash => tr("git_stash_message"),
            Prompt::AddRemote => tr("git_remote_name"),
        };
        let extra = match &prompt {
            Prompt::NewTag { .. } => tr("git_tag_message"),
            Prompt::AddRemote => tr("git_remote_url"),
            _ => String::new(),
        };
        self.prompt_input.update(cx, |input, cx| {
            input.set_value(initial, window, cx);
            input.set_placeholder(placeholder, window, cx);
        });
        self.prompt_extra.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.set_placeholder(extra, window, cx);
        });
        self.confirm = None;
        self.prompt = Some(prompt);
        self.prompt_input
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.prompt.clone() else {
            return;
        };
        let name = self.prompt_input.read(cx).value().trim().to_owned();
        let extra = self.prompt_extra.read(cx).value().trim().to_owned();
        let needs_name = !matches!(prompt, Prompt::Stash);
        if needs_name && name.is_empty() {
            return;
        }
        let action = match prompt {
            Prompt::NewBranch { start } => GitAction::CreateBranch {
                name,
                start,
                switch: true,
            },
            Prompt::RenameBranch(from) if from == name => {
                self.prompt = None;
                cx.notify();
                return;
            }
            Prompt::RenameBranch(from) => GitAction::RenameBranch { from, to: name },
            Prompt::NewTag { target } => GitAction::CreateTag {
                name,
                target,
                message: extra,
            },
            Prompt::Stash => GitAction::StashSave {
                message: name,
                include_untracked: self.stash_untracked,
                keep_index: false,
            },
            Prompt::AddRemote if extra.is_empty() => return,
            Prompt::AddRemote => GitAction::AddRemote { name, url: extra },
        };
        self.prompt = None;
        window.focus(&self.focus, cx);
        self.execute(action, cx);
    }

    /// Stop a network action still running (fetch, pull, push).
    pub(crate) fn cancel(&mut self) {
        if let Some((_, cancel, _)) = &self.running {
            cancel.store(true, Ordering::Release);
        }
    }

    /// Escape inside the panel: closes the prompt or confirmation first.
    /// False when the panel itself should close (the App does that).
    pub(crate) fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.prompt.take().is_some() || self.confirm.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
            return true;
        }
        false
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.root();
        let _ = self.parent.update(cx, |app, cx| {
            if let Some(root) = &root {
                app.invalidate_git_probe(root);
                app.notify_all_panes(cx);
            }
            app.git_panel = None;
            window.focus(&app.focus_handle, cx);
            cx.notify();
        });
    }

    /// Keys for the open panel (Escape is handled by the App); true when consumed.
    pub(crate) fn handle_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match event.keystroke.key.as_str() {
            "f5" => self.reload(cx),
            _ => return false,
        }
        true
    }

    fn default_remote(&self) -> Option<String> {
        let remotes = &self.overview.remotes;
        remotes
            .iter()
            .find(|r| r.name == "origin")
            .or_else(|| remotes.first())
            .map(|r| r.name.clone())
    }

    fn push_action(&self, force_with_lease: bool) -> Option<GitAction> {
        let head = &self.overview.status.head;
        let branch = head.branch.clone()?;
        Some(if head.upstream.is_some() {
            GitAction::Push {
                remote: None,
                branch: None,
                set_upstream: false,
                force_with_lease,
            }
        } else {
            GitAction::Push {
                remote: Some(self.default_remote()?),
                branch: Some(branch),
                set_upstream: true,
                force_with_lease,
            }
        })
    }

    fn change_rows(&self) -> Vec<ChangeRow> {
        let changes = &self.overview.status.changes;
        let mut rows = Vec::new();
        let conflicts: Vec<usize> = (0..changes.len())
            .filter(|&i| changes[i].conflicted())
            .collect();
        let staged: Vec<usize> = (0..changes.len())
            .filter(|&i| changes[i].staged.is_some() && !changes[i].conflicted())
            .collect();
        let unstaged: Vec<usize> = (0..changes.len())
            .filter(|&i| changes[i].unstaged.is_some() && !changes[i].conflicted())
            .collect();
        if !conflicts.is_empty() {
            self.push_changes(&mut rows, "git_conflicts", Section::Conflicts, &conflicts);
        }
        self.push_changes(&mut rows, "git_staged", Section::Staged, &staged);
        self.push_changes(&mut rows, "git_unstaged", Section::Unstaged, &unstaged);
        rows
    }

    fn push_changes(
        &self,
        rows: &mut Vec<ChangeRow>,
        key: &'static str,
        section: Section,
        list: &[usize],
    ) {
        rows.push(ChangeRow::Header(key, list.len()));
        let staged = section == Section::Staged;
        if !self.view.change_tree {
            rows.extend(list.iter().map(|&index| ChangeRow::File {
                staged,
                index,
                depth: None,
            }));
            return;
        }
        let changes = &self.overview.status.changes;
        let items: Vec<(usize, &str)> = list
            .iter()
            .map(|&i| (i, changes[i].path.as_str()))
            .collect();
        for row in rows::tree(section.prefix(), &items, &self.collapsed, false, true) {
            rows.push(match row {
                TreeRow::Folder {
                    key,
                    label,
                    depth,
                    items,
                    collapsed,
                } => ChangeRow::Folder {
                    section,
                    key,
                    label,
                    depth,
                    items: Arc::new(items),
                    collapsed,
                },
                TreeRow::Leaf { index, depth, .. } => ChangeRow::File {
                    staged,
                    index,
                    depth: Some(depth),
                },
            });
        }
    }

    fn branch_rows(&self) -> Vec<BranchRow> {
        let branches = &self.overview.branches;
        let terms = &self.branch_terms;
        let pick = |remote: bool| -> Vec<(usize, &str)> {
            branches
                .iter()
                .enumerate()
                .filter(|(_, b)| {
                    b.remote == remote && rows::matches_all(&b.name.to_lowercase(), terms)
                })
                .map(|(i, b)| (i, b.name.as_str()))
                .collect()
        };
        let (local, remote) = (pick(false), pick(true));
        let mut out = vec![BranchRow::Header(format!(
            "{} · {}",
            tr("git_local_branches"),
            local.len()
        ))];
        self.push_branches(&mut out, "L:", &local);
        if !remote.is_empty() {
            out.push(BranchRow::Header(format!(
                "{} · {}",
                tr("git_remote_branches"),
                remote.len()
            )));
            self.push_branches(&mut out, "R:", &remote);
        }
        out
    }

    fn push_branches(&self, out: &mut Vec<BranchRow>, prefix: &str, items: &[(usize, &str)]) {
        if !self.view.branch_tree {
            out.extend(items.iter().map(|&(index, _)| BranchRow::Branch {
                index,
                label: None,
                depth: 0,
            }));
            return;
        }
        // While filtering, every folder with a match is open.
        let filtering = !self.branch_terms.is_empty();
        for row in rows::tree(prefix, items, &self.collapsed, filtering, false) {
            out.push(match row {
                TreeRow::Folder {
                    key,
                    label,
                    depth,
                    items,
                    collapsed,
                } => BranchRow::Folder {
                    key,
                    label,
                    depth,
                    count: items.len(),
                    collapsed,
                },
                TreeRow::Leaf {
                    index,
                    label,
                    depth,
                } => BranchRow::Branch {
                    index,
                    label: Some(label),
                    depth,
                },
            });
        }
    }

    fn toggle_folder(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_owned());
        }
        cx.notify();
    }

    /// Switch a list between tree and flat view and remember the choice.
    fn set_view(&mut self, view: GitView, cx: &mut Context<Self>) {
        self.view = view;
        self.list_scroll = UniformListScrollHandle::new();
        let _ = self.parent.update(cx, |app, _| {
            let settings = &mut app.state.settings;
            if (settings.git_branch_tree, settings.git_change_tree)
                != (view.branch_tree, view.change_tree)
            {
                settings.git_branch_tree = view.branch_tree;
                settings.git_change_tree = view.change_tree;
                atlas_core::util::config::save_settings(settings);
            }
        });
        cx.notify();
    }

    fn refilter_history(&mut self) {
        let commits = &self.history.commits;
        self.history_rows = Arc::new(
            (0..commits.len())
                .filter(|&i| rows::commit_matches(&commits[i], &self.history_terms))
                .collect(),
        );
    }

    /// The graph only makes sense over the whole, unfiltered history.
    fn history_graph(&self) -> bool {
        self.history_path.is_none() && self.history_terms.is_empty()
    }

    /// Show the commits that touched `path` in the History tab.
    fn show_file_history(&mut self, path: String, cx: &mut Context<Self>) {
        self.history_path = Some(path);
        self.open_history(cx);
    }

    fn clear_file_history(&mut self, cx: &mut Context<Self>) {
        self.history_path = None;
        self.open_history(cx);
    }

    fn open_history(&mut self, cx: &mut Context<Self>) {
        self.tab = Tab::History;
        self.history_limit = HISTORY_PAGE;
        self.history = Arc::default();
        self.history_rows = Arc::default();
        self.list_scroll = UniformListScrollHandle::new();
        self.select(Selection::None, cx);
        self.load_history(cx);
        cx.notify();
    }

    fn reveal(&mut self, path: &str, cx: &mut Context<Self>) {
        let Some(root) = self.root() else {
            return;
        };
        let full = root.join(
            path.trim_end_matches('/')
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        );
        let folder = full
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or(root);
        let _ = self.parent.update(cx, |app, cx| {
            let tile = app.state.focused_tile;
            app.navigate(tile, folder);
            cx.notify();
        });
    }
}
