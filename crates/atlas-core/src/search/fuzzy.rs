//! One-shot fuzzy ranking over an in-memory list (the command/ext palettes).
//!
//! Unlike `NucleoPool` (a streaming index for thousands of paths), this scores a
//! small, already-materialized list per keystroke: parse the query once, score
//! every haystack, sort by score descending. Smart-case and Unicode
//! normalization match nucleo's defaults.

pub use nucleo::pattern::Pattern;
use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Matcher, Utf32Str};

pub struct FuzzyScorer {
    matcher: Matcher,
    /// Scratch buffer for `Utf32Str` conversion, reused across calls.
    buf: Vec<char>,
}

impl FuzzyScorer {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    /// Rank `haystacks` against `query`: indices of the matches, best first.
    /// An empty (or whitespace) query keeps every item in its original order.
    pub fn rank<'a>(
        &mut self,
        query: &str,
        haystacks: impl IntoIterator<Item = &'a str>,
    ) -> Vec<usize> {
        if query.trim().is_empty() {
            return haystacks.into_iter().enumerate().map(|(i, _)| i).collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        let mut scored: Vec<(usize, u32)> = haystacks
            .into_iter()
            .enumerate()
            .filter_map(|(i, hay)| {
                let hay = Utf32Str::new(hay, &mut self.buf);
                pattern.score(hay, &mut self.matcher).map(|s| (i, s))
            })
            .collect();
        scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.into_iter().map(|(i, _)| i).collect()
    }

    /// Score every haystack against `query` (one pattern parse for the batch).
    /// `None` entries didn't match; an empty query yields all-`None`.
    pub fn scores<'a>(
        &mut self,
        query: &str,
        haystacks: impl IntoIterator<Item = &'a str>,
    ) -> Vec<Option<u32>> {
        if query.trim().is_empty() {
            return haystacks.into_iter().map(|_| None).collect();
        }
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
        haystacks
            .into_iter()
            .map(|hay| {
                let hay = Utf32Str::new(hay, &mut self.buf);
                pattern.score(hay, &mut self.matcher)
            })
            .collect()
    }

    /// Parse a query once for batch scoring across multiple attributes.
    pub fn parse_pattern(query: &str) -> Option<Pattern> {
        if query.trim().is_empty() {
            return None;
        }
        Some(Pattern::parse(
            query,
            CaseMatching::Smart,
            Normalization::Smart,
        ))
    }

    /// Score a single haystack against a pre-compiled pattern without re-parsing.
    pub fn score_pattern(&mut self, pattern: &Pattern, hay: &str) -> Option<u32> {
        let hay = Utf32Str::new(hay, &mut self.buf);
        pattern.score(hay, &mut self.matcher)
    }

    /// Highlight ranges for a single haystack against a pre-compiled pattern.
    pub fn match_ranges_pattern(
        &mut self,
        pattern: &Pattern,
        hay: &str,
    ) -> Option<Vec<std::ops::Range<usize>>> {
        let utf32 = Utf32Str::new(hay, &mut self.buf);
        let mut indices: Vec<u32> = Vec::new();
        pattern.indices(utf32, &mut self.matcher, &mut indices)?;
        indices.sort_unstable();
        indices.dedup();
        // Map char indices to byte offsets, merging contiguous chars.
        let offsets: Vec<usize> = hay.char_indices().map(|(b, _)| b).collect();
        let byte_end = |ci: usize| offsets.get(ci + 1).copied().unwrap_or(hay.len());
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
        for &ci in &indices {
            let ci = ci as usize;
            let (start, end) = (*offsets.get(ci)?, byte_end(ci));
            match ranges.last_mut() {
                Some(last) if last.end == start => last.end = end,
                _ => ranges.push(start..end),
            }
        }
        Some(ranges)
    }

    /// Byte ranges of the characters in `hay` matched by `query` (for match
    /// highlighting), contiguous runs merged. `None` when there is no match
    /// or the query is empty.
    pub fn match_ranges(&mut self, query: &str, hay: &str) -> Option<Vec<std::ops::Range<usize>>> {
        let pattern = Self::parse_pattern(query)?;
        self.match_ranges_pattern(&pattern, hay)
    }
}

impl Default for FuzzyScorer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::FuzzyScorer;

    #[test]
    fn empty_query_keeps_order() {
        let mut scorer = FuzzyScorer::new();
        let items = ["beta", "alpha", "gamma"];
        assert_eq!(scorer.rank("", items), vec![0, 1, 2]);
        assert_eq!(scorer.rank("   ", items), vec![0, 1, 2]);
    }

    #[test]
    fn ranks_and_filters() {
        let mut scorer = FuzzyScorer::new();
        let items = [
            "Open Settings",
            "New Tab",
            "Toggle Sidebar",
            "Split Vertical",
        ];
        let ranked = scorer.rank("set", items);
        // Only "Open Settings" matches all of s-e-t contiguously best; others may
        // fuzzy-match but must rank below it.
        assert_eq!(ranked.first(), Some(&0));
        assert!(!ranked.contains(&1)); // "New Tab" has no s/e/t in order

        let ranked = scorer.rank("zzz", items);
        assert!(ranked.is_empty());
    }

    #[test]
    fn smart_case_is_insensitive_for_lowercase_query() {
        let mut scorer = FuzzyScorer::new();
        let ranked = scorer.rank("newtab", ["New Tab"]);
        assert_eq!(ranked, vec![0]);
    }

    #[test]
    fn match_ranges_merges_contiguous_chars() {
        let mut scorer = FuzzyScorer::new();
        let ranges = scorer.match_ranges("tab", "New Tab").unwrap();
        assert_eq!(ranges, vec![4..7]); // "Tab" contiguous
        assert!(scorer.match_ranges("", "New Tab").is_none());
        assert!(scorer.match_ranges("zzz", "New Tab").is_none());
    }
}
