//! Native, independently rendered disk analysis and reviewed temporary cleanup.
mod analysis;
mod appearance;
mod bubble_map_element;
#[cfg(feature = "ui-lab")]
pub mod diagnostic;
mod packing;
mod render;
mod treemap_element;

use crate::app::App;
use atlas_core::cleanup::{ScanJob, ScanMode};
use atlas_model::cleanup::{CleanupPlan, CleanupReport, DiskSnapshot, TreemapNode, TypeStat};
use bubble_map_element::{Cache, Node};
use gpui::{AppContext, ColorExt, Context, FocusHandle, WeakEntity, Window};
use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    Map,
    Files,
    Types,
    Duplicates,
    Cleanup,
    History,
}

/// What a derived view (treemap, file types) was computed for.
type DerivedKey = (PathBuf, u64, u64);

#[derive(Clone)]
pub(super) struct Row {
    path: PathBuf,
    name: String,
    size: String,
    directory: bool,
    bytes: u64,
}

/// One user-facing cleanup category and the scan result of each of its roots.
struct CatState {
    key: &'static str,
    roots: Vec<PathBuf>,
    snaps: Vec<Option<Arc<DiskSnapshot>>>,
    enabled: bool,
}
impl CatState {
    fn ready(&self) -> bool {
        self.snaps.iter().all(Option::is_some)
    }
    fn bytes(&self) -> u64 {
        self.snaps
            .iter()
            .flatten()
            .flat_map(|s| s.reclaimable.iter())
            .map(|e| e.bytes)
            .sum()
    }
    fn files(&self) -> usize {
        self.snaps
            .iter()
            .flatten()
            .map(|s| s.reclaimable.len())
            .sum()
    }
}

pub(crate) struct CleanupPanel {
    pub(crate) focus: FocusHandle,
    parent: WeakEntity<App>,
    volume: PathBuf,
    current: PathBuf,
    generation: u64,
    revision: u64,
    job: Option<ScanJob>,
    snapshot: Arc<DiskSnapshot>,
    tab: Tab,
    rows: Arc<Vec<Row>>,
    map_nodes: Arc<Vec<Node>>,
    map_cache: Rc<RefCell<Cache>>,
    selected: Option<PathBuf>,
    hovered: Option<usize>,
    checked: HashSet<PathBuf>,
    excluded: HashSet<PathBuf>,
    scroll: gpui::UniformListScrollHandle,
    cursor: usize,
    review: Option<CleanupPlan>,
    preparing: bool,
    cancel_requested: bool,
    execution: Option<Arc<AtomicBool>>,
    progress: (usize, usize),
    report: Option<CleanupReport>,
    error: Option<String>,
    list_only: bool,
    completed: VecDeque<(PathBuf, Arc<DiskSnapshot>)>,
    fast_requested: bool,
    /// Starting scale of the bubble zoom transition (<1 zooms in, >1 zooms out).
    enter: f32,
    opened: std::time::Instant,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    cats: Vec<CatState>,
    cat_jobs: Vec<ScanJob>,
    cat_pending: usize,
    plans: Vec<CleanupPlan>,
    trashing: bool,
    /// SMB share or mapped network drive: no MFT, no Recycle Bin, no temp cleanup.
    network: bool,
    /// Nested rectangles instead of bubbles in the map tab.
    treemap: bool,
    treemap_root: Option<Arc<TreemapNode>>,
    treemap_key: Option<DerivedKey>,
    treemap_pending: bool,
    treemap_revision: u64,
    treemap_cache: Rc<RefCell<treemap_element::Cache>>,
    pub(super) treemap_hovered: Option<usize>,
    /// Hovered tile: path (`None` for the "other" area), label and size.
    pub(super) treemap_hover_path: Option<(Option<PathBuf>, String, String)>,
    types: Option<Arc<Vec<TypeStat>>>,
    types_key: Option<DerivedKey>,
    types_pending: bool,
    types_scroll: gpui::UniformListScrollHandle,
    dup: analysis::Duplicates,
    /// Short confirmation shown under the content (export written, files moved).
    notice: Option<String>,
}

impl CleanupPanel {
    pub(crate) fn new(volume: PathBuf, parent: WeakEntity<App>, cx: &mut Context<Self>) -> Self {
        let network = atlas_core::fs::network::is_network_path(&volume);
        let mut panel = Self {
            focus: cx.focus_handle(),
            parent,
            current: volume.clone(),
            volume,
            generation: 0,
            revision: 0,
            job: None,
            snapshot: Arc::new(DiskSnapshot::default()),
            tab: Tab::Map,
            rows: Arc::default(),
            map_nodes: Arc::default(),
            map_cache: Rc::default(),
            selected: None,
            hovered: None,
            checked: HashSet::new(),
            excluded: HashSet::new(),
            scroll: gpui::UniformListScrollHandle::new(),
            cursor: 0,
            review: None,
            preparing: false,
            cancel_requested: false,
            execution: None,
            progress: (0, 0),
            report: None,
            error: None,
            list_only: false,
            completed: VecDeque::new(),
            // NTFS volumes read the MFT by default; a refused UAC falls back to the parallel walk.
            // Network volumes have no readable MFT: they always use the walk.
            fast_requested: cfg!(windows) && !network,
            enter: 0.85,
            opened: std::time::Instant::now(),
            back: Vec::new(),
            forward: Vec::new(),
            cats: Vec::new(),
            cat_jobs: Vec::new(),
            cat_pending: 0,
            plans: Vec::new(),
            trashing: false,
            network,
            treemap: false,
            treemap_root: None,
            treemap_key: None,
            treemap_pending: false,
            treemap_revision: 0,
            treemap_cache: Rc::default(),
            treemap_hovered: None,
            treemap_hover_path: None,
            types: None,
            types_key: None,
            types_pending: false,
            types_scroll: gpui::UniformListScrollHandle::new(),
            dup: analysis::Duplicates::default(),
            notice: None,
        };
        panel.scan(cx);
        panel
    }
    fn scan(&mut self, cx: &mut Context<Self>) {
        // An explicit rescan must read the disk again.
        atlas_core::cleanup::invalidate_tree();
        self.completed.clear();
        self.scan_cached(false, cx);
    }
    fn scan_cached(&mut self, reuse: bool, cx: &mut Context<Self>) {
        if self.execution.is_some() || self.preparing {
            return;
        }
        if self.tab == Tab::Cleanup && self.discover(cx) {
            return;
        }
        self.generation += 1;
        self.cancel_requested = false;
        self.checked.clear();
        self.review = None;
        self.plans.clear();
        self.error = None;
        self.selected = None;
        self.hovered = None;
        self.cursor = 0;
        let fast = std::mem::take(&mut self.fast_requested);
        let mode = if self.tab == Tab::Cleanup {
            ScanMode::Temporary
        } else if fast {
            ScanMode::FastFolder
        } else {
            ScanMode::Folder
        };
        let root = if mode == ScanMode::Temporary {
            self.volume.clone()
        } else {
            self.current.clone()
        };
        if reuse && mode == ScanMode::Folder {
            // Navigation inside a finished scan is a lookup in the in-memory tree.
            if let Some(snapshot) = atlas_core::cleanup::cached_snapshot(&root, self.generation) {
                self.snapshot = Arc::new(snapshot);
                self.job = None;
                self.rebuild();
                cx.notify();
                return;
            }
            if let Some((_, snapshot)) = self.completed.iter().find(|(path, _)| path == &root) {
                let mut snapshot = (**snapshot).clone();
                snapshot.generation = self.generation;
                self.snapshot = Arc::new(snapshot);
                self.job = None;
                self.rebuild();
                cx.notify();
                return;
            }
        }
        self.snapshot = Arc::new(DiskSnapshot {
            generation: self.generation,
            root: root.clone(),
            ..Default::default()
        });
        let job = ScanJob::start(root, self.generation, mode);
        let receiver = job.receiver.clone();
        self.job = Some(job);
        self.rebuild();
        cx.spawn(async move |this, cx| {
            while let Ok(snapshot) = receiver.recv_async().await {
                let terminal = snapshot.status.terminal();
                if this
                    .update(cx, |panel, cx| {
                        if snapshot.generation != panel.generation {
                            return;
                        }
                        panel.snapshot = Arc::new(snapshot);
                        if terminal {
                            panel.cancel_requested = false;
                            if mode != ScanMode::Temporary
                                && matches!(
                                    panel.snapshot.status,
                                    atlas_model::cleanup::ScanStatus::Complete
                                        | atlas_model::cleanup::ScanStatus::Partial
                                )
                            {
                                let root = panel.snapshot.root.clone();
                                panel.completed.retain(|(path, _)| path != &root);
                                panel.completed.push_back((root, panel.snapshot.clone()));
                                if panel.completed.len() > 8 {
                                    panel.completed.pop_front();
                                }
                            }
                        }
                        panel.rebuild();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                if terminal {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }
    /// Scan every cleanup category root on this volume. False if none applies.
    fn discover(&mut self, cx: &mut Context<Self>) -> bool {
        let cats: Vec<CatState> = atlas_core::cleanup::categories()
            .into_iter()
            .filter_map(|c| {
                let roots: Vec<_> = c
                    .roots
                    .into_iter()
                    .filter(|r| r.starts_with(&self.volume))
                    .collect();
                (!roots.is_empty()).then(|| CatState {
                    key: c.key,
                    snaps: vec![None; roots.len()],
                    roots,
                    enabled: true,
                })
            })
            .collect();
        if cats.is_empty() {
            self.cats.clear();
            return false;
        }
        self.generation += 1;
        let generation = self.generation;
        self.cancel_requested = false;
        self.review = None;
        self.plans.clear();
        self.error = None;
        self.selected = None;
        self.job = None;
        self.cat_jobs.clear();
        self.cat_pending = cats.iter().map(|c| c.roots.len()).sum();
        self.snapshot = Arc::new(DiskSnapshot {
            generation,
            root: self.volume.clone(),
            ..Default::default()
        });
        self.rebuild();
        for (ci, cat) in cats.iter().enumerate() {
            for (ri, root) in cat.roots.iter().enumerate() {
                let job = ScanJob::start(root.clone(), generation, ScanMode::Category);
                let receiver = job.receiver.clone();
                self.cat_jobs.push(job);
                cx.spawn(async move |this, cx| {
                    while let Ok(snapshot) = receiver.recv_async().await {
                        let terminal = snapshot.status.terminal();
                        if this
                            .update(cx, |panel, cx| {
                                if generation != panel.generation || !terminal {
                                    return;
                                }
                                panel.cats[ci].snaps[ri] = Some(Arc::new(snapshot));
                                panel.cat_pending = panel.cat_pending.saturating_sub(1);
                                if panel.cat_pending == 0 {
                                    let files =
                                        panel.cats.iter().map(CatState::files).sum::<usize>();
                                    panel.snapshot = Arc::new(DiskSnapshot {
                                        generation,
                                        root: panel.volume.clone(),
                                        status: atlas_model::cleanup::ScanStatus::Complete,
                                        entry_count: files as u64,
                                        ..Default::default()
                                    });
                                }
                                cx.notify();
                            })
                            .is_err()
                        {
                            break;
                        }
                        if terminal {
                            break;
                        }
                    }
                })
                .detach();
            }
        }
        self.cats = cats;
        cx.notify();
        true
    }
    /// Prepare one reviewed plan per selected category root, then ask for one confirmation.
    fn clean(&mut self, cx: &mut Context<Self>) {
        if self.execution.is_some() || self.preparing || self.cat_pending > 0 {
            return;
        }
        let jobs: Vec<_> = self
            .cats
            .iter()
            .filter(|c| c.enabled)
            .flat_map(|c| c.roots.iter().zip(&c.snaps))
            .filter_map(|(root, snap)| {
                let paths: Vec<_> = snap
                    .as_ref()?
                    .reclaimable
                    .iter()
                    .map(|e| e.path.clone())
                    .collect();
                (!paths.is_empty()).then(|| (root.clone(), paths))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let volume = self.volume.clone();
        self.preparing = true;
        self.error = None;
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    jobs.into_iter()
                        .filter_map(|(root, paths)| {
                            let candidates = atlas_core::cleanup::inspect_candidates(&paths);
                            (!candidates.is_empty()).then(|| {
                                atlas_core::cleanup::prepare_plan_in(
                                    volume.clone(),
                                    root,
                                    candidates,
                                )
                            })
                        })
                        .collect::<std::io::Result<Vec<_>>>()
                })
                .await;
            let _ = this.update(cx, |panel, cx| {
                panel.preparing = false;
                match outcome {
                    Ok(mut plans) if !plans.is_empty() => {
                        panel.review = Some(plans.remove(0));
                        panel.plans = plans;
                    }
                    Ok(_) => panel.error = Some("cleanup_unavailable".into()),
                    Err(e) => panel.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn trash_selected(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        if self.execution.is_some() || self.preparing || self.trashing {
            return;
        }
        self.trashing = true;
        self.error = None;
        cx.spawn(async move |this, cx| {
            let target = path.clone();
            let result = cx
                .background_executor()
                .spawn(async move { atlas_core::cleanup::move_to_trash(&target) })
                .await;
            let _ = this.update(cx, |panel, cx| {
                panel.trashing = false;
                match result {
                    Ok(()) => {
                        panel.selected = None;
                        panel.completed.clear();
                        panel.enter = 0.92;
                        let current = panel.current.clone();
                        if let Some(s) =
                            atlas_core::cleanup::cached_snapshot(&current, panel.generation)
                        {
                            panel.snapshot = Arc::new(s);
                        }
                        panel.rebuild();
                    }
                    Err(e) => panel.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn rebuild(&mut self) {
        // Incomplete sizes must not change visual topology on every progress
        // event. Only counters change until the scan has a terminal snapshot.
        if !self.snapshot.status.terminal() {
            self.rows = Arc::default();
            if !self.map_nodes.is_empty() {
                self.map_nodes = Arc::default();
                self.revision += 1;
            }
            return;
        }
        let source = if self.tab == Tab::Files {
            &self.snapshot.large_files
        } else {
            &self.snapshot.entries
        };
        let mut rows = if matches!(self.tab, Tab::Types | Tab::Duplicates) {
            Vec::new()
        } else if self.tab == Tab::Cleanup {
            self.snapshot
                .candidates
                .iter()
                .filter(|c| !self.excluded.contains(&c.path))
                .map(|c| Row {
                    path: c.path.clone(),
                    name: c
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    size: atlas_core::util::display::format_size(c.identity.bytes),
                    directory: false,
                    bytes: c.identity.bytes,
                })
                .collect::<Vec<_>>()
        } else {
            source
                .iter()
                .map(|e| Row {
                    path: e.path.clone(),
                    name: e
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    size: atlas_core::util::display::format_size(e.bytes),
                    directory: e.directory,
                    bytes: e.bytes,
                })
                .collect()
        };
        if self.tab == Tab::Cleanup {
            rows.sort_by(|a, b| a.path.cmp(&b.path));
        }
        self.rows = Arc::new(rows);
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        if !self.snapshot.status.terminal() {
            if !self.map_nodes.is_empty() {
                self.map_nodes = Arc::default();
                self.revision += 1;
            }
        } else {
            let shown = self
                .snapshot
                .entries
                .iter()
                .filter(|e| {
                    e.bytes > 0 && e.bytes as f64 / self.snapshot.bytes.max(1) as f64 >= 0.003
                })
                .take(31);
            let mut nodes: Vec<_> = shown
                .map(|e| Node {
                    path: Some(e.path.clone()),
                    label: e
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    size: atlas_core::util::display::format_size(e.bytes),
                    bytes: e.bytes,
                    directory: e.directory,
                })
                .collect();
            let represented = nodes
                .iter()
                .fold(0u64, |total, n| total.saturating_add(n.bytes));
            let other = self.snapshot.bytes.saturating_sub(represented);
            if other > 0 {
                nodes.push(Node {
                    path: None,
                    label: tr("cleanup_other"),
                    size: atlas_core::util::display::format_size(other),
                    bytes: other,
                    directory: false,
                });
            }
            nodes.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
            if *self.map_nodes != nodes {
                self.map_nodes = Arc::new(nodes);
                self.revision += 1;
            }
        }
    }
    fn switch(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.execution.is_some() || self.preparing || self.tab == tab {
            return;
        }
        let old = self.tab;
        self.tab = tab;
        self.review = None;
        self.plans.clear();
        self.error = None;
        self.notice = None;
        self.cursor = 0;
        self.selected = None;
        if tab != Tab::History
            && (old == Tab::Cleanup || tab == Tab::Cleanup || old == Tab::History)
        {
            self.scan_cached(true, cx);
        } else {
            self.rebuild();
            cx.notify();
        }
    }
    fn navigate(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if path.starts_with(&self.volume) && self.execution.is_none() {
            self.enter = if self.current.starts_with(&path) {
                1.25
            } else {
                0.6
            };
            if path != self.current {
                self.back.push(std::mem::replace(&mut self.current, path));
                self.forward.clear();
            }
            self.tab = Tab::Map;
            self.scan_cached(true, cx);
        }
    }
    /// Move the selection to the nearest bubble in a direction (screen axes).
    fn arrow(&mut self, dx: f32, dy: f32) {
        let cache = self.map_cache.borrow();
        let with_path = |i: usize| self.map_nodes.get(i).is_some_and(|n| n.path.is_some());
        let current = self
            .selected
            .as_ref()
            .and_then(|s| {
                self.map_nodes
                    .iter()
                    .position(|n| n.path.as_ref() == Some(s))
            })
            .and_then(|i| cache.circles.iter().find(|c| c.index == i))
            .copied();
        let target = match current {
            None => cache.circles.iter().find(|c| with_path(c.index)).copied(),
            Some(from) => cache
                .circles
                .iter()
                .filter(|c| c.index != from.index && with_path(c.index))
                .filter_map(|c| {
                    let (vx, vy) = (c.x - from.x, c.y - from.y);
                    let along = vx * dx + vy * dy;
                    let across = (vx * dy - vy * dx).abs();
                    // Must lie in the half-plane of travel; prefer straight, near targets.
                    (along > 1.0).then_some((along + 2.0 * across, *c))
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, c)| c),
        };
        if let Some(circle) = target {
            let path = self.map_nodes[circle.index].path.clone();
            if let Some(path) = &path {
                if let Some(row) = self.rows.iter().position(|r| &r.path == path) {
                    self.cursor = row;
                }
            }
            drop(cache);
            self.selected = path;
        }
    }
    /// Alt+Left/Right and the mouse side buttons walk the visited folders.
    fn go_history(&mut self, backwards: bool, cx: &mut Context<Self>) {
        if self.execution.is_some() || self.preparing {
            return;
        }
        if self.review.is_some() {
            // Back leaves the confirmation first, like Escape.
            if backwards {
                self.review = None;
                self.plans.clear();
                cx.notify();
            }
            return;
        }
        let target = if backwards {
            self.back.pop()
        } else {
            self.forward.pop()
        };
        let Some(target) = target else {
            if backwards && self.tab != Tab::Map {
                self.switch(Tab::Map, cx);
            }
            return;
        };
        let from = std::mem::replace(&mut self.current, target);
        if backwards {
            self.forward.push(from);
        } else {
            self.back.push(from);
        }
        self.enter = if backwards { 1.25 } else { 0.6 };
        self.tab = Tab::Map;
        self.scan_cached(true, cx);
    }
    fn up(&mut self, cx: &mut Context<Self>) {
        if self.current != self.volume {
            if let Some(parent) = self.current.parent() {
                self.navigate(parent.to_owned(), cx);
            }
        }
    }
    fn map_click(&mut self, path: Option<PathBuf>, clicks: usize, cx: &mut Context<Self>) {
        if let Some(path) = path {
            let directory = self
                .snapshot
                .entries
                .iter()
                .any(|e| e.path == path && e.directory);
            self.selected = Some(path.clone());
            if clicks >= 2 && directory {
                self.navigate(path, cx);
            }
        } else {
            self.list_only = true;
        }
        cx.notify();
    }
    fn toggle(&mut self, path: PathBuf) {
        if self.execution.is_some()
            || self.preparing
            || self.review.is_some()
            || self.excluded.contains(&path)
        {
            return;
        }
        if !self.checked.remove(&path) {
            self.checked.insert(path);
        }
    }
    fn prepare(&mut self, cx: &mut Context<Self>) {
        if self.checked.is_empty()
            || !self.snapshot.status.terminal()
            || self.execution.is_some()
            || self.preparing
        {
            return;
        }
        let candidates = self
            .snapshot
            .candidates
            .iter()
            .filter(|c| self.checked.contains(&c.path) && !self.excluded.contains(&c.path))
            .cloned()
            .collect();
        let volume = self.volume.clone();
        self.preparing = true;
        self.error = None;
        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { atlas_core::cleanup::prepare_plan(volume, candidates) })
                .await;
            let _ = this.update(cx, |panel, cx| {
                panel.preparing = false;
                match outcome {
                    Ok(plan) => panel.review = Some(plan),
                    Err(e) => panel.error = Some(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn review_selection(&mut self, cx: &mut Context<Self>) {
        // This explicit review action can select all visible temporary files;
        // entering the tab itself never preselects anything or starts deletion.
        if self.checked.is_empty() {
            self.checked = self.rows.iter().map(|r| r.path.clone()).collect();
        }
        self.prepare(cx);
    }
    fn execute(&mut self, cx: &mut Context<Self>) {
        if !cfg!(feature = "windows-cleanup") {
            self.error = Some(tr("cleanup_execution_pending"));
            cx.notify();
            return;
        }
        let Some(first) = self.review.take() else {
            return;
        };
        if self.execution.is_some() {
            return;
        }
        let mut plans = vec![first];
        plans.append(&mut self.plans);
        let total: usize = plans.iter().map(|p| p.candidates.len()).sum();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel_requested = false;
        self.execution = Some(cancel.clone());
        self.error = None;
        self.report = None;
        self.progress = (0, total);
        // A single latest-value progress channel, plus the non-coalesced task result.
        let (sender, receiver) = flume::bounded(1);
        let work = cx.background_executor().spawn(async move {
            let mut merged: Option<CleanupReport> = None;
            let mut base = 0usize;
            for plan in &plans {
                if merged.is_some() && cancel.load(Ordering::Acquire) {
                    break;
                }
                let sender = sender.clone();
                let report = atlas_core::cleanup::execute(plan, &cancel, |done, _| {
                    let _ = sender.try_send((base + done, total));
                })?;
                base += plan.candidates.len();
                merged = Some(match merged {
                    None => report,
                    Some(mut all) => {
                        all.results.extend(report.results);
                        all.cancelled |= report.cancelled;
                        all.free_after = report.free_after;
                        all.error = all.error.or(report.error);
                        all
                    }
                });
            }
            merged.ok_or_else(|| std::io::Error::other("cleanup_scope_changed"))
        });
        cx.spawn(async move |this, cx| {
            while let Ok(progress) = receiver.recv_async().await {
                if this
                    .update(cx, |panel, cx| {
                        panel.progress = progress;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |panel, cx| {
                panel.execution = None;
                panel.cancel_requested = false;
                panel.checked.clear();
                panel.tab = Tab::History;
                match result {
                    Ok(report) => panel.report = Some(report),
                    Err(e) => panel.error = Some(e.to_string()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    /// Escape handling that does not touch `App`: true when the panel may be dismissed now.
    pub(crate) fn escape(&mut self, cx: &mut Context<Self>) -> bool {
        if self.review.is_some() {
            self.review = None;
            self.plans.clear();
            cx.notify();
            return false;
        }
        if self.execution.is_some() {
            self.cancel_requested = true;
            if let Some(cancel) = &self.execution {
                cancel.store(true, Ordering::Release);
            }
            cx.notify();
            return false;
        }
        if self.preparing {
            return false;
        }
        if let Some(job) = &self.job {
            job.cancel();
        }
        true
    }
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Do not hide a destructive operation before its real terminal result.
        if self.execution.is_some() {
            self.cancel_requested = true;
            if let Some(cancel) = &self.execution {
                cancel.store(true, Ordering::Release);
            }
            cx.notify();
            return;
        }
        if self.preparing {
            return;
        }
        if let Some(job) = &self.job {
            job.cancel();
        }
        let _ = self.parent.update(cx, |app, cx| {
            app.cleanup_panel = None;
            window.focus(&app.focus_handle, cx);
            cx.notify();
        });
    }
}
impl Drop for CleanupPanel {
    fn drop(&mut self) {
        atlas_core::cleanup::release_analysis_cache();
        if let Some(cancel) = &self.execution {
            cancel.store(true, Ordering::Release);
        }
    }
}
fn tr(key: &str) -> String {
    rust_i18n::t!(key).into_owned()
}
fn user_error(error: &str) -> String {
    if error.starts_with("cleanup_") {
        tr(error)
    } else {
        error.to_owned()
    }
}
fn muted(cx: &gpui::App) -> gpui::Hsla {
    use gpui_component::ActiveTheme;
    let theme = cx.theme();
    if theme.is_dark() {
        theme.foreground.opacity(0.70)
    } else {
        theme.muted_foreground
    }
}

impl App {
    pub(crate) fn open_disk_cleanup(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !atlas_core::cleanup::is_analysis_root(&path) {
            return;
        }
        if let Some(panel) = &self.cleanup_panel {
            // Keep an active execution visible; the volume cannot be switched under it.
            if panel.read(cx).volume == path || panel.read(cx).execution.is_some() {
                cx.notify();
                return;
            }
        }
        let parent = cx.entity().downgrade();
        self.cleanup_panel = Some(cx.new(|cx| CleanupPanel::new(path, parent, cx)));
        cx.notify();
    }
    pub(crate) fn render_disk_cleanup(&mut self, _cx: &mut Context<Self>) -> gpui::AnyElement {
        use gpui::prelude::*;
        use gpui_component::ActiveTheme;
        let panel = self.cleanup_panel.as_ref().unwrap().clone();
        let back = panel.clone();
        let forward = panel.clone();
        gpui::deferred(
            gpui::div()
                .absolute()
                .inset_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_3()
                .bg(_cx.theme().overlay)
                .occlude()
                .on_mouse_down(
                    gpui::MouseButton::Navigate(gpui::NavigationDirection::Back),
                    move |_, _, cx| {
                        back.update(cx, |p, cx| p.go_history(true, cx));
                        cx.stop_propagation();
                    },
                )
                .on_mouse_down(
                    gpui::MouseButton::Navigate(gpui::NavigationDirection::Forward),
                    move |_, _, cx| {
                        forward.update(cx, |p, cx| p.go_history(false, cx));
                        cx.stop_propagation();
                    },
                )
                .child(panel),
        )
        .into_any_element()
    }
}
