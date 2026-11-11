//! Repository probes behind the pane header's branch chip: which folders sit
//! in a Git repository, on which branch, with how many changes. Probed off
//! the UI thread and cached briefly, so browsing never waits on git.
use crate::app::App;
use atlas_model::app_state::BgResult;
use atlas_model::git::RepoHead;
use atlas_model::layout_tree::{Tile, TileId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a probe stays fresh: a branch switched outside Atlas shows up
/// after at most this long, or right away after the Git panel changes it.
const FRESH_FOR: Duration = Duration::from_secs(15);

struct Probe {
    head: Option<RepoHead>,
    at: Instant,
    pending: bool,
    stale: bool,
}

#[derive(Default)]
pub(crate) struct GitProbes {
    entries: RefCell<HashMap<PathBuf, Probe>>,
}

impl App {
    /// Repository state of the folder `pane` shows, when Git integration is
    /// on. Starts a background probe when the cached one is missing or old;
    /// until it answers, the previous state (if any) is returned.
    pub(crate) fn pane_git_head(&self, pane: TileId) -> Option<RepoHead> {
        if !self.state.settings.git_integration {
            return None;
        }
        let td = self.tab_data_for(pane)?;
        let atlas_model::location::Location::Directory(dir) = &td.location else {
            return None;
        };
        let mut entries = self.git_probes.entries.borrow_mut();
        let due = entries
            .get(dir)
            .is_none_or(|p| !p.pending && (p.stale || p.at.elapsed() > FRESH_FOR));
        if due {
            let head = entries.get(dir).and_then(|p| p.head.clone());
            entries.insert(
                dir.clone(),
                Probe {
                    head,
                    at: Instant::now(),
                    pending: true,
                    stale: false,
                },
            );
            let tx = self.tx.clone();
            let dir = dir.clone();
            let _ = std::thread::Builder::new()
                .name("atlas-git-probe".into())
                .spawn(move || {
                    let head = atlas_core::git::find_repo(&dir)
                        .and_then(|root| atlas_core::git::head(&root).ok());
                    let _ = tx.send(BgResult::GitProbe { dir, head });
                });
        }
        entries.get(dir).and_then(|p| p.head.clone())
    }

    /// Store a finished probe; returns the panes whose chip changed.
    pub(crate) fn apply_git_probe(&mut self, dir: PathBuf, head: Option<RepoHead>) -> Vec<TileId> {
        let changed = {
            let mut entries = self.git_probes.entries.borrow_mut();
            let before = entries.get(&dir).and_then(|p| p.head.clone());
            let changed = before != head;
            entries.insert(
                dir.clone(),
                Probe {
                    head,
                    at: Instant::now(),
                    pending: false,
                    stale: false,
                },
            );
            changed
        };
        if !changed {
            return Vec::new();
        }
        self.state
            .pane_tree
            .tiles
            .iter()
            .filter_map(|(id, tile)| match tile {
                Tile::Pane(pane) if pane.location.fs_dir() == Some(dir.as_path()) => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Re-probe folders inside `root` on their next paint, e.g. after the
    /// Git panel committed or switched branches.
    pub(crate) fn invalidate_git_probe(&self, root: &Path) {
        for (dir, probe) in self.git_probes.entries.borrow_mut().iter_mut() {
            if dir.starts_with(root) {
                probe.stale = true;
            }
        }
    }
}
