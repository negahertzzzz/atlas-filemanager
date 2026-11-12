//! Cached virtual list structure. Headers never masquerade as file indices.
use crate::clamp::ClampLenient;
use crate::file_entry::{FileEntry, FileKind};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GroupBy {
    #[default]
    None,
    Date,
    Size,
    Type,
    Name,
    Created,
    Accessed,
    /// Recycle Bin only: when the item was deleted.
    Deleted,
    /// Recycle Bin only: the folder the item was deleted from.
    Origin,
}

impl GroupBy {
    /// Modes backed by timestamps share the calendar buckets.
    pub const fn is_date(self) -> bool {
        matches!(
            self,
            Self::Date | Self::Created | Self::Accessed | Self::Deleted
        )
    }

    pub const fn label_key(self) -> &'static str {
        match self {
            Self::None => "group_none",
            Self::Created => "group_created",
            Self::Accessed => "group_accessed",
            Self::Deleted => "group_deleted",
            Self::Origin => "group_origin",
            Self::Date => "group_date",
            Self::Size => "group_size",
            Self::Type => "group_type",
            Self::Name => "group_name",
        }
    }

    pub fn label(self) -> &'static str {
        self.label_key()
    }
}

/// `(created, accessed)` of one entry. The compact entry keeps only its
/// modification time, so these arrive from a background read.
pub type EntryTimes = (Option<std::time::SystemTime>, Option<std::time::SystemTime>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupRow {
    Header {
        id: String,
        label: String,
        count: usize,
        collapsed: bool,
    },
    Entry(usize),
}

/// Group headers live in the same position space as file entries so every
/// arithmetic view (rows, columns) keeps its scroll, hit and rubber geometry.
/// Real entries are `< HEADER_CELL_BASE`; headers and padding never index files.
pub const HEADER_CELL_BASE: usize = usize::MAX / 2;
pub const PAD_CELL: usize = usize::MAX;

pub const fn is_virtual_cell(cell: usize) -> bool {
    cell >= HEADER_CELL_BASE
}

/// How a view lays headers into its cell grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupFlow {
    /// One header per full-width row (Details).
    Rows,
    /// A group starts in a fresh column of `rows` cells; its header tops it (List).
    Columns { rows: usize },
    /// Row-major wrap of `cols` cells (Grid). Headers take no cell: they are bands
    /// between rows (see `RowBands`) and `GroupHeader::cell` is the group's first cell.
    Wrap { cols: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupHeader {
    /// Position in the cell list.
    pub cell: usize,
    pub id: String,
    pub label: String,
    pub count: usize,
    pub collapsed: bool,
    /// Columns covered by the group (always 1 for `GroupFlow::Rows`).
    pub span: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupCells {
    pub cells: std::sync::Arc<Vec<usize>>,
    pub headers: Vec<GroupHeader>,
}

impl GroupCells {
    pub fn header_at(&self, cell: usize) -> Option<&GroupHeader> {
        cell.checked_sub(HEADER_CELL_BASE)
            .filter(|_| cell != PAD_CELL)
            .and_then(|ordinal| self.headers.get(ordinal))
    }

    /// Cell position of a real entry, for scroll-into-view.
    pub fn position_of(&self, entry: usize) -> Option<usize> {
        self.cells.iter().position(|cell| *cell == entry)
    }
}

pub fn flow_cells(rows: &[GroupRow], flow: GroupFlow) -> GroupCells {
    let mut cells: Vec<usize> = Vec::with_capacity(rows.len() + 8);
    let mut headers: Vec<GroupHeader> = Vec::new();
    for row in rows {
        match row {
            GroupRow::Header {
                id,
                label,
                count,
                collapsed,
            } => {
                match flow {
                    GroupFlow::Columns { rows } => {
                        let rows = rows.max(2);
                        while !cells.len().is_multiple_of(rows) {
                            cells.push(PAD_CELL);
                        }
                    }
                    GroupFlow::Wrap { cols } => {
                        while !cells.len().is_multiple_of(cols.max(1)) {
                            cells.push(PAD_CELL);
                        }
                    }
                    GroupFlow::Rows => {}
                }
                if !matches!(flow, GroupFlow::Wrap { .. }) {
                    cells.push(HEADER_CELL_BASE + headers.len());
                }
                headers.push(GroupHeader {
                    cell: if matches!(flow, GroupFlow::Wrap { .. }) {
                        cells.len()
                    } else {
                        cells.len() - 1
                    },
                    id: id.clone(),
                    label: label.clone(),
                    count: *count,
                    collapsed: *collapsed,
                    span: 1,
                });
            }
            GroupRow::Entry(index) => {
                // Every column of a group keeps its top row for the header band,
                // so a wrapped group never has files under the divider.
                if let GroupFlow::Columns { rows } = flow {
                    if cells.len().is_multiple_of(rows.max(2)) {
                        cells.push(PAD_CELL);
                    }
                }
                cells.push(*index);
            }
        }
    }
    if let GroupFlow::Columns { rows } = flow {
        let rows = rows.max(2);
        let total = cells.len();
        let starts: Vec<usize> = headers.iter().map(|h| h.cell).collect();
        for (i, header) in headers.iter_mut().enumerate() {
            let end = starts.get(i + 1).copied().unwrap_or(total);
            header.span = (end - header.cell).div_ceil(rows).max(1);
        }
    }
    GroupCells {
        cells: std::sync::Arc::new(cells),
        headers,
    }
}

/// Vertical bands (group headers) stacked between the rows of a wrapped grid.
/// `rows[k]` is the grid row the k-th band sits directly above; non-decreasing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowBands {
    rows: std::sync::Arc<Vec<usize>>,
    pub band_h: f32,
}

impl RowBands {
    pub fn new(rows: Vec<usize>, band_h: f32) -> Self {
        Self {
            rows: std::sync::Arc::new(rows),
            band_h,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Extra vertical space `row` is pushed down by.
    pub fn shift(&self, row: usize) -> f32 {
        self.rows.partition_point(|band| *band <= row) as f32 * self.band_h
    }

    /// Top of `row` relative to the grid origin.
    pub fn row_top(&self, row: usize, pitch: f32) -> f32 {
        (row as f32).mul_add(pitch, self.shift(row))
    }

    /// Top of the k-th band relative to the grid origin.
    pub fn band_top(&self, k: usize, pitch: f32) -> f32 {
        self.rows
            .get(k)
            .map_or(0.0, |row| *row as f32 * pitch + k as f32 * self.band_h)
    }

    /// Total height added by every band.
    pub fn total(&self) -> f32 {
        self.rows.len() as f32 * self.band_h
    }

    /// Number of rows whose top is at or above `y` (grid-relative).
    pub fn rows_through(&self, y: f32, pitch: f32) -> usize {
        if y < 0.0 || pitch <= 0.0 {
            return 0;
        }
        let row = self.row_at(y, pitch);
        usize::from(self.row_top(row, pitch) <= y) * (row + 1)
    }

    /// Last row whose top is at or above `y` (grid-relative). Bands only push rows
    /// down, so the answer never exceeds `y / pitch`.
    pub fn row_at(&self, y: f32, pitch: f32) -> usize {
        if y <= 0.0 || pitch <= 0.0 {
            return 0;
        }
        let (mut low, mut high) = (0, (y / pitch).floor() as usize);
        while low < high {
            let mid = (low + high).div_ceil(2);
            if self.row_top(mid, pitch) <= y {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        low
    }
}

/// Moves the keyboard cursor by `delta` cells without ever landing on a header or
/// padding cell. Padding at a row/column end resolves to the nearest real cell
/// before it; headers resolve in the direction of travel.
pub fn grouped_step(cells: &[usize], current: usize, delta: isize) -> Option<usize> {
    let last = cells.len().checked_sub(1)?;
    let target = (current as isize + delta).clamp_lenient(0, last as isize) as usize;
    if !is_virtual_cell(cells[target]) {
        return Some(target);
    }
    let forward = delta > 0 && cells[target] != PAD_CELL;
    let after = || (target..=last).find(|i| !is_virtual_cell(cells[*i]));
    let before = || (0..target).rev().find(|i| !is_virtual_cell(cells[*i]));
    let found = if forward {
        after().or_else(before)
    } else {
        before().or_else(after)
    };
    found.filter(|i| *i != current)
}

/// Returns both virtual rows and keyboard/shift-selection order. Entry order
/// inside a bucket is inherited from the current sort/filter result.
pub fn build_groups(
    entries: &[FileEntry],
    visible: &[usize],
    mode: GroupBy,
    collapsed: &BTreeSet<String>,
    today: NaiveDate,
    times: Option<&[EntryTimes]>,
) -> (Vec<GroupRow>, Vec<usize>) {
    if mode == GroupBy::None {
        return (
            visible.iter().copied().map(GroupRow::Entry).collect(),
            visible.to_vec(),
        );
    }
    let mut buckets: BTreeMap<(u8, String), (String, Vec<usize>)> = BTreeMap::new();
    for &index in visible {
        let Some(entry) = entries.get(index) else {
            continue;
        };
        let stamps = times
            .and_then(|times| times.get(index))
            .copied()
            .unwrap_or_default();
        let (rank, id, label) = bucket(entry, mode, today, stamps);
        buckets
            .entry((rank, id))
            .or_insert_with(|| (label, Vec::new()))
            .1
            .push(index);
    }
    let mut rows = Vec::with_capacity(visible.len() + buckets.len());
    let mut order = Vec::with_capacity(visible.len());
    for ((_, id), (label, indices)) in buckets {
        let is_collapsed = collapsed.contains(&id);
        rows.push(GroupRow::Header {
            id,
            label,
            count: indices.len(),
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            rows.extend(indices.iter().copied().map(GroupRow::Entry));
            order.extend(indices);
        }
    }
    (rows, order)
}

/// Calendar buckets shared by every timestamp mode. `prefix` keeps the ids of
/// one mode from colliding with another's in the collapsed set.
fn date_bucket(
    time: Option<std::time::SystemTime>,
    prefix: &str,
    today: NaiveDate,
) -> (u8, String, String) {
    let id = |name: &str| format!("{prefix}:{name}");
    let Some(time) = time else {
        return (9, id("unknown"), "group_unknown".to_string());
    };
    let date = DateTime::<Local>::from(time).date_naive();
    let week = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let month = today.with_day(1).unwrap_or(today);
    let previous_month = (month - Duration::days(1)).with_day(1).unwrap_or(month);
    let year = month.with_month(1).unwrap_or(month);
    let previous_year = year.with_year(year.year() - 1).unwrap_or(year);
    // Calendar boundaries, with the more immediate bucket taking precedence when
    // a week crosses a month or year boundary. Future timestamps share Today
    // rather than creating an eleventh bucket.
    let (rank, key, label) = if date >= today {
        (0, "today", "group_today")
    } else if date == today - Duration::days(1) {
        (1, "yesterday", "group_yesterday")
    } else if date >= week {
        (2, "week", "group_week")
    } else if date >= week - Duration::days(7) {
        (3, "last-week", "group_last_week")
    } else if date >= month {
        (4, "month", "group_month")
    } else if date >= previous_month {
        (5, "last-month", "group_last_month")
    } else if date >= year {
        (6, "year", "group_year")
    } else if date >= previous_year {
        (7, "last-year", "group_last_year")
    } else {
        (8, "older", "group_older")
    };
    (rank, id(key), label.to_string())
}

fn bucket(
    entry: &FileEntry,
    mode: GroupBy,
    today: NaiveDate,
    stamps: EntryTimes,
) -> (u8, String, String) {
    let fixed = |rank, key: &str, label: String| (rank, key.to_owned(), label);
    match mode {
        GroupBy::Date => date_bucket(entry.modified, "date", today),
        GroupBy::Created => date_bucket(stamps.0, "created", today),
        GroupBy::Accessed => date_bucket(stamps.1, "accessed", today),
        GroupBy::Deleted => date_bucket(
            entry.recycle_info().map(|info| info.deleted_at),
            "deleted",
            today,
        ),
        GroupBy::Origin => match entry
            .recycle_info()
            .and_then(|info| info.original_path.parent())
        {
            Some(parent) => {
                let label = parent.display().to_string();
                (0, format!("origin:{label}"), label)
            }
            None => fixed(1, "origin:unknown", "group_unknown".to_string()),
        },
        GroupBy::Size => {
            if entry.kind() == FileKind::Dir {
                return fixed(0, "size:folders", "file_kind_folder".to_string());
            }
            let (rank, key, label) = match entry.size() {
                Some(0..=16_383) => (1, "size:tiny", "group_tiny"),
                Some(16_384..=1_048_575) => (2, "size:small", "group_small"),
                Some(1_048_576..=134_217_727) => (3, "size:medium", "group_medium"),
                Some(134_217_728..=1_073_741_823) => (4, "size:large", "group_large"),
                Some(1_073_741_824..=4_294_967_295) => (5, "size:huge", "group_huge"),
                Some(_) => (6, "size:gigantic", "group_gigantic"),
                None => (7, "size:unknown", "group_unknown"),
            };
            fixed(rank, key, label.to_string())
        }
        GroupBy::Type => {
            let label =
                crate::file_entry::file_category(&entry.kind(), entry.extension().as_deref());
            let rank = u8::from(entry.kind() != FileKind::Dir);
            (rank, format!("type:{label}"), label.to_string())
        }
        GroupBy::Name => {
            let (rank, key, label) =
                match entry.name.chars().next().map(|ch| ch.to_ascii_uppercase()) {
                    Some('0'..='9') => (0, "name:0-9", "0–9".to_string()),
                    Some('A'..='H') => (1, "name:A-H", "A–H".to_string()),
                    Some('I'..='P') => (2, "name:I-P", "I–P".to_string()),
                    Some('Q'..='Z') => (3, "name:Q-Z", "Q–Z".to_string()),
                    _ => (4, "name:other", "group_other".to_string()),
                };
            fixed(rank, key, label)
        }
        GroupBy::None => unreachable!("ungrouped list handled before bucketing"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn entry(name: &str, size: Option<u64>) -> FileEntry {
        FileEntry::from(crate::file_entry::FileEntryData {
            name: name.into(),
            path: name.into(),
            kind: FileKind::File,
            size,
            modified: None,
            _is_hidden: false,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        })
    }

    #[test]
    fn date_buckets_use_local_calendar_days_and_keep_unknown_dates() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        let mut entries = Vec::new();
        for days in [0, 1, 2, 7, 30, -1] {
            let mut entry = entry("file", None);
            entry.modified = Some(
                Local
                    .from_local_datetime(
                        &(today - Duration::days(days))
                            .and_hms_opt(12, 0, 0)
                            .unwrap(),
                    )
                    .single()
                    .unwrap()
                    .into(),
            );
            entries.push(entry);
        }
        entries.push(entry("unknown", None));
        let (rows, order) = build_groups(
            &entries,
            &[0, 1, 2, 3, 4, 5, 6],
            GroupBy::Date,
            &BTreeSet::new(),
            today,
            None,
        );
        assert_eq!(order, [0, 5, 1, 2, 3, 4, 6]);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, GroupRow::Header { .. }))
                .count(),
            6
        );
    }

    #[test]
    fn size_boundaries_and_sort_within_bucket_are_preserved() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        let entries = [
            entry("huge", Some(1_073_741_824)),
            entry("tiny-a", Some(1)),
            entry("tiny-b", Some(16_383)),
            entry("small", Some(16_384)),
        ];
        let (rows, order) = build_groups(
            &entries,
            &[0, 2, 1, 3],
            GroupBy::Size,
            &BTreeSet::new(),
            today,
            None,
        );
        assert_eq!(order, [2, 1, 3, 0]);
        assert!(matches!(&rows[0], GroupRow::Header { id, count: 2, .. } if id == "size:tiny"));
        let (rows, order) = build_groups(
            &entries,
            &[0, 2, 1, 3],
            GroupBy::Type,
            &BTreeSet::new(),
            today,
            None,
        );
        assert_eq!(order, [0, 2, 1, 3]);
        assert_eq!(rows.len(), 5);
    }

    fn dated(date: NaiveDate) -> FileEntry {
        let mut file = entry("file", None);
        file.modified = Some(
            Local
                .from_local_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
                .single()
                .unwrap()
                .into(),
        );
        file
    }

    #[test]
    fn ten_date_buckets_follow_calendar_boundaries_including_year_crossings() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
        let dates = [
            (2026, 10, 29),
            (2026, 10, 28),
            (2026, 10, 26),
            (2026, 10, 19),
            (2026, 10, 1),
            (2026, 9, 1),
            (2026, 1, 1),
            (2025, 1, 1),
            (2024, 12, 31),
        ];
        let mut files = dates
            .map(|(y, m, d)| dated(NaiveDate::from_ymd_opt(y, m, d).unwrap()))
            .to_vec();
        files.push(entry("unknown", None));
        let (rows, order) = build_groups(
            &files,
            &(0..10).collect::<Vec<_>>(),
            GroupBy::Date,
            &BTreeSet::new(),
            today,
            None,
        );
        let headers: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                GroupRow::Header { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            headers,
            [
                "date:today",
                "date:yesterday",
                "date:week",
                "date:last-week",
                "date:month",
                "date:last-month",
                "date:year",
                "date:last-year",
                "date:older",
                "date:unknown"
            ]
        );
        assert_eq!(order, (0..10).collect::<Vec<_>>());
        let january = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
        assert_eq!(
            bucket(
                &dated(NaiveDate::from_ymd_opt(2025, 12, 31).unwrap()),
                GroupBy::Date,
                january,
                Default::default()
            )
            .1,
            "date:week"
        );
        assert_eq!(
            bucket(
                &dated(NaiveDate::from_ymd_opt(2025, 12, 22).unwrap()),
                GroupBy::Date,
                january,
                Default::default()
            )
            .1,
            "date:last-week"
        );
        let march = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
        assert_eq!(
            bucket(
                &dated(NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()),
                GroupBy::Date,
                march,
                Default::default()
            )
            .1,
            "date:last-month"
        );
        assert_eq!(
            bucket(
                &dated(today + Duration::days(1)),
                GroupBy::Date,
                today,
                Default::default()
            )
            .1,
            "date:today"
        );
    }

    #[test]
    fn name_groups_have_five_balanced_ranges_with_case_insensitive_edges() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        for (name, expected) in [
            ("0file", "name:0-9"),
            ("9file", "name:0-9"),
            ("alpha", "name:A-H"),
            ("Hotel", "name:A-H"),
            ("india", "name:I-P"),
            ("Papa", "name:I-P"),
            ("queen", "name:Q-Z"),
            ("Zulu", "name:Q-Z"),
            (".hidden", "name:other"),
            ("日本語", "name:other"),
        ] {
            assert_eq!(
                bucket(&entry(name, None), GroupBy::Name, today, Default::default()).1,
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn size_ranges_cover_every_boundary_without_gaps() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        for (size, expected) in [
            (0, "tiny"),
            (16_383, "tiny"),
            (16_384, "small"),
            (1_048_575, "small"),
            (1_048_576, "medium"),
            (134_217_727, "medium"),
            (134_217_728, "large"),
            (1_073_741_823, "large"),
            (1_073_741_824, "huge"),
            (4_294_967_295, "huge"),
            (4_294_967_296, "gigantic"),
            (u64::MAX, "gigantic"),
        ] {
            assert_eq!(
                bucket(
                    &entry("file", Some(size)),
                    GroupBy::Size,
                    today,
                    Default::default()
                )
                .1,
                format!("size:{expected}")
            );
        }
        assert_eq!(
            bucket(
                &entry("unknown", None),
                GroupBy::Size,
                today,
                Default::default()
            )
            .1,
            "size:unknown"
        );
    }

    #[test]
    fn flow_cells_pad_columns_and_keep_headers_out_of_entry_space() {
        let rows = vec![
            GroupRow::Header {
                id: "a".into(),
                label: "A".into(),
                count: 1,
                collapsed: false,
            },
            GroupRow::Entry(7),
            GroupRow::Header {
                id: "b".into(),
                label: "B".into(),
                count: 4,
                collapsed: false,
            },
            GroupRow::Entry(0),
            GroupRow::Entry(1),
            GroupRow::Entry(2),
            GroupRow::Entry(3),
        ];
        let flat = flow_cells(&rows, GroupFlow::Rows);
        assert_eq!(flat.cells.len(), 7);
        assert!(is_virtual_cell(flat.cells[0]) && flat.cells[1] == 7);
        assert_eq!(flat.header_at(flat.cells[2]).unwrap().id, "b");
        let columns = flow_cells(&rows, GroupFlow::Columns { rows: 4 });
        // A: [H,7,pad,pad]  B: [H,0,1,2] [pad,3]: row 0 of every column is the band.
        assert_eq!(columns.cells[2], PAD_CELL);
        assert_eq!(columns.headers[1].cell, 4);
        assert_eq!((columns.headers[0].span, columns.headers[1].span), (1, 2));
        assert!(columns.header_at(PAD_CELL).is_none());
        assert_eq!(columns.cells[8], PAD_CELL);
        assert_eq!(columns.position_of(3), Some(9));
    }

    #[test]
    fn wrapped_grid_bands_shift_rows_and_invert_cleanly() {
        let rows = vec![
            GroupRow::Header {
                id: "a".into(),
                label: "A".into(),
                count: 3,
                collapsed: false,
            },
            GroupRow::Entry(0),
            GroupRow::Entry(1),
            GroupRow::Entry(2),
            GroupRow::Header {
                id: "b".into(),
                label: "B".into(),
                count: 2,
                collapsed: true,
            },
            GroupRow::Header {
                id: "c".into(),
                label: "C".into(),
                count: 1,
                collapsed: false,
            },
            GroupRow::Entry(3),
        ];
        let wrapped = flow_cells(&rows, GroupFlow::Wrap { cols: 2 });
        assert_eq!(wrapped.cells.as_slice(), [0, 1, 2, PAD_CELL, 3]);
        assert_eq!(
            wrapped.headers.iter().map(|h| h.cell).collect::<Vec<_>>(),
            [0, 4, 4]
        );
        let bands = RowBands::new(wrapped.headers.iter().map(|h| h.cell / 2).collect(), 10.0);
        // rows 0,1 hold group a; row 2 (group c) sits under bands b and c.
        assert_eq!(bands.row_top(0, 100.0), 10.0);
        assert_eq!(bands.row_top(1, 100.0), 110.0);
        assert_eq!(bands.row_top(2, 100.0), 230.0);
        assert_eq!(bands.band_top(1, 100.0), 210.0);
        for row in 0..3 {
            assert_eq!(bands.row_at(bands.row_top(row, 100.0) + 1.0, 100.0), row);
        }
        assert_eq!(bands.row_at(5.0, 100.0), 0);
        assert_eq!(bands.total(), 30.0);
    }

    #[test]
    fn grouped_step_skips_headers_and_row_end_padding() {
        let cells = [
            HEADER_CELL_BASE,
            0,
            1,
            PAD_CELL,
            HEADER_CELL_BASE + 1,
            2,
            3,
            4,
        ];
        assert_eq!(grouped_step(&cells, 1, 1), Some(2));
        // Padding resolves back into the row, so there is nowhere further to go.
        assert_eq!(grouped_step(&cells, 2, 1), None);
        assert_eq!(grouped_step(&cells, 2, 2), Some(5));
        assert_eq!(grouped_step(&cells, 5, -1), Some(2));
        assert_eq!(grouped_step(&cells, 7, 5), Some(7));
    }

    #[test]
    fn created_and_accessed_use_background_stamps_and_fall_back_to_unknown() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 15).unwrap();
        let entries = [entry("a", None), entry("b", None)];
        let noon = Local
            .from_local_datetime(&today.and_hms_opt(12, 0, 0).unwrap())
            .single()
            .unwrap()
            .into();
        let times = [(Some(noon), None), (None, Some(noon))];
        let ids = |mode| {
            let (rows, _) = build_groups(
                &entries,
                &[0, 1],
                mode,
                &BTreeSet::new(),
                today,
                Some(&times),
            );
            rows.iter()
                .filter_map(|row| match row {
                    GroupRow::Header { id, .. } => Some(id.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(GroupBy::Created), ["created:today", "created:unknown"]);
        assert_eq!(
            ids(GroupBy::Accessed),
            ["accessed:today", "accessed:unknown"]
        );
        let (rows, _) = build_groups(
            &entries,
            &[0, 1],
            GroupBy::Created,
            &BTreeSet::new(),
            today,
            None,
        );
        assert_eq!(
            rows.len(),
            3,
            "without stamps everything waits under Unknown"
        );
    }
}
