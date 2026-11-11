//! Lane layout for the commit graph. Commits arrive newest first (topological
//! or date order, children before parents); each lane remembers the commit it
//! waits for, and every row records the lines entering and leaving its dot.
use atlas_model::git::{Commit, GraphLine, GraphRow, GraphSpan};

/// Lanes past this many are drawn on the last one: a wide octopus history
/// stays readable instead of pushing the subjects off-screen.
const MAX_LANES: usize = 24;

fn clamp(lane: usize) -> u16 {
    u16::try_from(lane.min(MAX_LANES - 1)).unwrap_or(0)
}

fn free_lane(lanes: &mut Vec<Option<String>>, except: usize) -> usize {
    if let Some(i) = lanes
        .iter()
        .enumerate()
        .position(|(i, lane)| lane.is_none() && i != except)
    {
        return i;
    }
    lanes.push(None);
    lanes.len() - 1
}

pub(crate) fn layout(commits: &[Commit]) -> Vec<GraphRow> {
    let mut lanes: Vec<Option<String>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());
    for commit in commits {
        let incoming: Vec<usize> = lanes
            .iter()
            .enumerate()
            .filter(|(_, want)| want.as_deref() == Some(commit.id.as_str()))
            .map(|(i, _)| i)
            .collect();
        let lane = match incoming.first() {
            Some(&lane) => lane,
            None => free_lane(&mut lanes, usize::MAX),
        };
        let width_before = lanes.len();
        let waiting_before: Vec<bool> = lanes.iter().map(Option::is_some).collect();
        let mut lines: Vec<GraphLine> = incoming
            .iter()
            .map(|&i| GraphLine {
                from: clamp(i),
                to: clamp(lane),
                span: GraphSpan::Top,
                color: clamp(i),
            })
            .collect();
        for &i in &incoming {
            lanes[i] = None;
        }
        // First parent continues the commit's lane; the others branch off.
        let mut parents = commit.parents.iter();
        if let Some(first) = parents.next() {
            lanes[lane] = Some(first.clone());
            lines.push(GraphLine {
                from: clamp(lane),
                to: clamp(lane),
                span: GraphSpan::Bottom,
                color: clamp(lane),
            });
        }
        for parent in parents {
            let target = match lanes
                .iter()
                .position(|want| want.as_deref() == Some(parent.as_str()))
            {
                Some(existing) => existing,
                None => {
                    let slot = free_lane(&mut lanes, lane);
                    lanes[slot] = Some(parent.clone());
                    slot
                }
            };
            lines.push(GraphLine {
                from: clamp(lane),
                to: clamp(target),
                span: GraphSpan::Bottom,
                color: clamp(target),
            });
        }
        // Lanes that only pass by this row.
        for (i, waiting) in waiting_before.iter().enumerate() {
            if *waiting && i != lane && !incoming.contains(&i) {
                lines.push(GraphLine {
                    from: clamp(i),
                    to: clamp(i),
                    span: GraphSpan::Full,
                    color: clamp(i),
                });
            }
        }
        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        let width = width_before.max(lanes.len()).max(lane + 1).min(MAX_LANES);
        rows.push(GraphRow {
            lane: clamp(lane),
            lines,
            width: u16::try_from(width).unwrap_or(1),
        });
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.into(),
            parents: parents.iter().map(|p| (*p).to_owned()).collect(),
            author: String::new(),
            email: String::new(),
            time: 0,
            refs: Vec::new(),
            subject: String::new(),
        }
    }

    #[test]
    fn linear_history_stays_on_one_lane() {
        let rows = layout(&[commit("c", &["b"]), commit("b", &["a"]), commit("a", &[])]);
        assert!(rows.iter().all(|r| r.lane == 0 && r.width == 1));
        assert!(!rows[0].lines.iter().any(|l| l.span == GraphSpan::Top));
        assert!(rows[1].lines.contains(&GraphLine {
            from: 0,
            to: 0,
            span: GraphSpan::Top,
            color: 0
        }));
        assert!(!rows[2].lines.iter().any(|l| l.span == GraphSpan::Bottom));
    }

    #[test]
    fn merge_branches_out_and_joins_back() {
        // m merges f into b; f and b both come from a.
        let rows = layout(&[
            commit("m", &["b", "f"]),
            commit("f", &["a"]),
            commit("b", &["a"]),
            commit("a", &[]),
        ]);
        assert_eq!(rows[0].lane, 0);
        assert!(rows[0].lines.contains(&GraphLine {
            from: 0,
            to: 1,
            span: GraphSpan::Bottom,
            color: 1
        }));
        // f sits on the second lane while b's lane passes by.
        assert_eq!(rows[1].lane, 1);
        assert!(rows[1].lines.contains(&GraphLine {
            from: 0,
            to: 0,
            span: GraphSpan::Full,
            color: 0
        }));
        assert_eq!(rows[2].lane, 0);
        // a collects both lanes.
        assert_eq!(rows[3].lane, 0);
        assert!(rows[3].lines.contains(&GraphLine {
            from: 1,
            to: 0,
            span: GraphSpan::Top,
            color: 1
        }));
        assert_eq!(rows[3].width, 2);
    }

    #[test]
    fn unrelated_heads_get_their_own_lanes() {
        let rows = layout(&[
            commit("x", &["y"]),
            commit("p", &["q"]),
            commit("y", &[]),
            commit("q", &[]),
        ]);
        assert_eq!(rows[0].lane, 0);
        assert_eq!(rows[1].lane, 1);
        assert_eq!(rows[2].lane, 0);
        assert_eq!(rows[3].lane, 1);
        assert!(rows[2].lines.contains(&GraphLine {
            from: 1,
            to: 1,
            span: GraphSpan::Full,
            color: 1
        }));
    }
}
