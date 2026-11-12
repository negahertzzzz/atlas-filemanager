use atlas_model::file_entry::FileEntry;
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

const SEARCH_LIMIT: usize = 500;

pub async fn search_names_recursive(
    root: PathBuf,
    query: String,
    show_hidden: bool,
) -> anyhow::Result<Vec<FileEntry>> {
    let needle = query.to_lowercase();
    let needle2 = needle.clone();
    let root_for_walk = root.clone();
    let paths = tokio::task::spawn_blocking(move || {
        collect_matching_paths(&root_for_walk, &needle2, show_hidden)
    })
    .await?;

    let mut entries = Vec::with_capacity(paths.len());
    for path in paths {
        if let Some(mut entry) = crate::fs::list::entry_for_path(&path).await {
            if let Ok(relative) = entry.path().strip_prefix(&root) {
                let label = relative.to_string_lossy().into_owned();
                if !label.is_empty() {
                    entry.set_display_name(label.into());
                }
            }
            entries.push(entry);
        }
    }

    entries.sort_by(|a, b| {
        let a_score = fuzzy_score(&needle, &a.name_lower()).unwrap_or_default();
        let b_score = fuzzy_score(&needle, &b.name_lower()).unwrap_or_default();
        b_score.cmp(&a_score).then_with(|| a.path().cmp(&b.path()))
    });
    Ok(entries)
}

fn collect_matching_paths(root: &Path, needle: &str, show_hidden: bool) -> Vec<PathBuf> {
    if needle.trim().is_empty() {
        return Vec::new();
    }

    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!show_hidden)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .parents(true)
        .filter_entry(|entry| {
            !crate::fs::scan_policy::is_scan_excluded(entry.path())
                && !crate::fs::preview::is_cloud_placeholder(entry.path())
        })
        .follow_links(false);

    let mut scored = Vec::<(PathBuf, i64)>::new();
    for result in builder.build() {
        let entry = match result {
            Ok(entry) => entry,
            Err(err) => {
                tracing::debug!(?err, "skipping unreadable search entry");
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        let Some(name) = entry.file_name().to_str() else {
            continue;
        };
        let lower = name.to_lowercase();
        let Some(score) = fuzzy_score(needle, &lower) else {
            continue;
        };
        scored.push((entry.path().to_path_buf(), score));
        if scored.len() >= SEARCH_LIMIT * 4 {
            break;
        }
    }

    scored.sort_by(|(ap, ascore), (bp, bscore)| {
        bscore
            .cmp(ascore)
            .then_with(|| ap.components().count().cmp(&bp.components().count()))
            .then_with(|| ap.cmp(bp))
    });
    scored.truncate(SEARCH_LIMIT);
    scored.into_iter().map(|(path, _)| path).collect()
}

fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }

    let mut score = 0_i64;
    let mut last_match: Option<usize> = None;
    let mut search_from = 0;

    for q in query.chars().filter(|c| !c.is_whitespace()) {
        let hay = &candidate[search_from..];
        let offset = hay.find(q)?;
        let pos = search_from + offset;
        if pos == 0 {
            score += 24;
        }
        if candidate[..pos].ends_with(['-', '_', '.', ' ', '/', '\\']) {
            score += 12;
        }
        if let Some(prev) = last_match {
            let gap = pos.saturating_sub(prev + 1);
            score += 20_i64.saturating_sub(i64::try_from(gap).unwrap_or(20));
        }
        score += 4;
        last_match = Some(pos);
        search_from = pos + q.len_utf8();
    }

    if candidate.starts_with(query) {
        score += 60;
    } else if candidate.contains(query) {
        score += 30;
    }

    score -= i64::try_from(candidate.len()).unwrap_or(i64::MAX / 2) / 8;
    Some(score)
}
