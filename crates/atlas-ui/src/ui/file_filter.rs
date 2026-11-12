//! One projection for Details rendering, keyboard movement and selection.
use crate::{
    app::App,
    ui::{size_filter_menu::SizeFilter, views::details::entry_type_label},
};
use atlas_model::{
    file_entry::{FileEntry, FileKind},
    pane::TabId,
    settings::ViewMode,
    tab::VisibleCounts,
};
use std::{collections::HashSet, sync::Arc};

pub(crate) struct FileFilterProjection {
    entries: Arc<[FileEntry]>,
    base: Arc<Vec<usize>>,
    types: HashSet<String>,
    sizes: HashSet<SizeFilter>,
    visible: Arc<Vec<usize>>,
    counts: VisibleCounts,
}

impl FileFilterProjection {
    fn matches(
        &self,
        entries: &Arc<[FileEntry]>,
        base: &Arc<Vec<usize>>,
        types: &HashSet<String>,
        sizes: &HashSet<SizeFilter>,
    ) -> bool {
        Arc::ptr_eq(&self.entries, entries)
            && Arc::ptr_eq(&self.base, base)
            && self.types == *types
            && self.sizes == *sizes
    }

    fn new(
        entries: Arc<[FileEntry]>,
        base: Arc<Vec<usize>>,
        types: HashSet<String>,
        sizes: HashSet<SizeFilter>,
    ) -> Self {
        let visible: Arc<Vec<usize>> = Arc::new(
            base.iter()
                .copied()
                .filter(|index| {
                    entries.get(*index).is_some_and(|entry| {
                        (types.is_empty() || types.contains(&entry_type_label(entry)))
                            && (sizes.is_empty()
                                || sizes.iter().any(|filter| filter.matches(entry)))
                    })
                })
                .collect(),
        );
        let mut counts = VisibleCounts::default();
        for entry in entries.iter() {
            if entry.kind() == FileKind::Dir {
                counts.folders_total += 1;
            } else {
                counts.files_total += 1;
            }
        }
        for entry in visible.iter().map(|i| &entries[*i]) {
            if entry.kind() == FileKind::Dir {
                counts.folders_shown += 1;
            } else {
                counts.files_shown += 1;
            }
            counts.bytes_shown += u128::from(entry.size().unwrap_or(0));
        }
        Self {
            entries,
            base,
            types,
            sizes,
            visible,
            counts,
        }
    }
}

impl App {
    pub(crate) fn flat_visible_indices(&mut self, pane: TabId) -> Arc<Vec<usize>> {
        let details = self.tab_data_for(pane).is_some_and(|tab| {
            tab.view_mode == ViewMode::Details && tab.group_by == atlas_model::tab::GroupBy::None
        });
        let Some(state) = self.state.tab_states.get_mut(&pane) else {
            return Arc::default();
        };
        let base = state.visible_indices_shared();
        let types = self
            .type_filters
            .get(&pane)
            .cloned()
            .filter(|_| details)
            .unwrap_or_default();
        let sizes = self
            .size_filters
            .get(&pane)
            .cloned()
            .filter(|_| details)
            .unwrap_or_default();
        if types.is_empty() && sizes.is_empty() {
            if self.file_filter_projections.remove(&pane).is_some() {
                state.selection.retain_visible(&base);
            }
            return base;
        }
        let cached = self
            .file_filter_projections
            .get(&pane)
            .is_some_and(|cache| cache.matches(&state.entries, &base, &types, &sizes));
        if !cached {
            let projection = FileFilterProjection::new(state.entries.clone(), base, types, sizes);
            state.selection.retain_visible(&projection.visible);
            self.file_filter_projections.insert(pane, projection);
        }
        self.file_filter_projections[&pane].visible.clone()
    }

    /// Headers + entries for a grouped pane in the view's own cell flow.
    pub(crate) fn grouped_cells(
        &mut self,
        pane: TabId,
        flow: atlas_model::grouping::GroupFlow,
    ) -> Option<Arc<atlas_model::grouping::GroupCells>> {
        let state = self.state.tab_states.get_mut(&pane)?;
        (state.group_by != atlas_model::tab::GroupBy::None)
            .then(|| state.grouped_cells_shared(flow))
    }

    /// Position space of the pane's DG view: grouped Details/List get their
    /// header cells, everything else the plain visible order.
    pub(crate) fn view_cells(&mut self, pane: TabId) -> Arc<Vec<usize>> {
        let flow = match self.tab_data_for(pane).map(|tab| tab.view_mode.clone()) {
            Some(ViewMode::Details) => Some(atlas_model::grouping::GroupFlow::Rows),
            Some(ViewMode::Grid) => Some(atlas_model::grouping::GroupFlow::Wrap {
                cols: self
                    .grid_hit
                    .get(&pane)
                    .map_or(1, |model| model.cols.max(1)),
            }),
            Some(ViewMode::List) => Some(atlas_model::grouping::GroupFlow::Columns {
                rows: self.list_hit.get(&pane).map_or(2, |model| model.major_rows),
            }),
            _ => None,
        };
        match flow.and_then(|flow| self.grouped_cells(pane, flow)) {
            Some(groups) => groups.cells.clone(),
            None => self.flat_visible_indices(pane),
        }
    }

    /// Cell position of the `visible_pos`-th file of the entry-only order.
    pub(crate) fn cell_position(&mut self, pane: TabId, visible_pos: usize) -> usize {
        let Some(entry) = self.flat_visible_indices(pane).get(visible_pos).copied() else {
            return visible_pos;
        };
        self.view_cells(pane)
            .iter()
            .position(|cell| *cell == entry)
            .unwrap_or(visible_pos)
    }

    pub(crate) fn flat_visible_counts(&mut self, pane: TabId) -> VisibleCounts {
        self.flat_visible_indices(pane);
        self.file_filter_projections
            .get(&pane)
            .map(|projection| projection.counts)
            .unwrap_or_else(|| {
                self.state
                    .tab_states
                    .get_mut(&pane)
                    .map(|state| state.counts())
                    .unwrap_or_default()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_model::file_entry::{FileEntryData, FileKind};
    fn file(name: &str, size: u64) -> FileEntry {
        FileEntryData {
            name: name.into(),
            path: name.into(),
            kind: FileKind::File,
            size: Some(size),
            modified: None,
            _is_hidden: false,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        }
        .into()
    }
    #[test]
    fn size_and_type_filters_intersect_without_reordering_and_invalidate_on_refresh() {
        let entries: Arc<[FileEntry]> = vec![
            file("z.txt", 50),
            file("hidden.txt", 5),
            file("b.txt", 0),
            file("a.png", 50),
        ]
        .into();
        let base = Arc::new(vec![3, 0, 2]);
        let types: HashSet<_> = [entry_type_label(&entries[0])].into();
        let sizes: HashSet<_> = [SizeFilter::Tiny].into();
        let projection =
            FileFilterProjection::new(entries.clone(), base.clone(), types.clone(), sizes.clone());
        assert_eq!(*projection.visible, vec![0]);
        assert_eq!(projection.counts.bytes_shown, 50);
        assert_eq!(projection.counts.files_shown, 1);
        assert_eq!(projection.counts.files_total, 4);
        assert!(projection.matches(&entries, &base, &types, &sizes));
        assert!(!projection.matches(&entries, &Arc::new(vec![0, 3, 2]), &types, &sizes));
        assert!(!projection.matches(&entries, &base, &types, &HashSet::new()));
        let changed: Arc<[FileEntry]> = vec![file("z.txt", 0)].into();
        assert!(!projection.matches(&changed, &base, &types, &sizes));
        let sizes_only = FileFilterProjection::new(entries, base, HashSet::new(), sizes);
        assert_eq!(*sizes_only.visible, vec![3, 0]);
    }
}
