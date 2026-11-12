//! Filename and location preferences for the interactive navigation picker.
use std::path::Path;

/// Treat word separators alike without changing the displayed filesystem name.
pub fn normalized_name(name: &str, sensitive: bool) -> String {
    let mut result = String::new();
    let mut separator = false;
    for character in name.chars() {
        if character.is_whitespace() || matches!(character, '-' | '_') {
            separator = !result.is_empty();
            continue;
        }
        if separator {
            result.push(' ');
            separator = false;
        }
        if sensitive {
            result.push(character);
        } else {
            result.extend(character.to_lowercase());
        }
    }
    result
}

/// Result of matching whitespace-separated query tokens against path segments.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TokenMatch {
    /// Higher is better: segment prefix 3, word start 2, inside a word 1 per token.
    pub score: u32,
    /// Byte ranges of `path` covered by the tokens that were found.
    pub ranges: Vec<std::ops::Range<usize>>,
    /// Every token matched, each in a later position than the previous one.
    pub complete: bool,
}

/// Best occurrence of `token` in `seg[from..]`: `(byte offset in seg, score)`.
fn find_in_segment(seg: &str, from: usize, token: &str, sensitive: bool) -> Option<(usize, u32)> {
    let (hay, needle) = if sensitive {
        (seg.to_owned(), token.to_owned())
    } else {
        (seg.to_ascii_lowercase(), token.to_ascii_lowercase())
    };
    let mut best: Option<(usize, u32)> = None;
    for (rel, _) in hay[from..].match_indices(needle.as_str()) {
        let pos = from + rel;
        let score = if pos == 0 {
            3
        } else if hay[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_whitespace() || matches!(c, '-' | '_' | '.' | '(' | '['))
        {
            2
        } else {
            1
        };
        if best.is_none_or(|(_, s)| score > s) {
            best = Some((pos, score));
        }
    }
    best
}

/// Match each token of `query` to a path segment, in order ("exa pers" finds
/// `Users\Example\Personal`, "exa" in one folder and "pers" in a deeper one).
/// Tokens are literal (case-insensitive unless they contain an uppercase letter).
pub fn match_path_tokens(query: &str, path: &str) -> TokenMatch {
    let mut segments = Vec::new();
    let mut start = 0;
    for (i, c) in path.char_indices() {
        if c == '/' || c == '\\' {
            if i > start {
                segments.push((start, i));
            }
            start = i + c.len_utf8();
        }
    }
    if start < path.len() {
        segments.push((start, path.len()));
    }
    let mut result = TokenMatch {
        complete: query.split_whitespace().next().is_some(),
        ..TokenMatch::default()
    };
    let (mut first_segment, mut min_offset, mut last_segment) = (0, 0, None);
    for token in query.split_whitespace() {
        let sensitive = token.chars().any(char::is_uppercase);
        let mut best: Option<(usize, usize, u32)> = None;
        for (index, &(s, e)) in segments.iter().enumerate().skip(first_segment) {
            let from = s.max(min_offset);
            if from >= e {
                continue;
            }
            if let Some((pos, score)) = find_in_segment(&path[s..e], from - s, token, sensitive) {
                if best.is_none_or(|(_, _, b)| score > b) {
                    best = Some((index, s + pos, score));
                }
                if score == 3 {
                    break;
                }
            }
        }
        match best {
            Some((index, at, score)) => {
                result.score += score;
                result.ranges.push(at..at + token.len());
                first_segment = index;
                min_offset = at + token.len();
                last_segment = Some(index);
            }
            None => result.complete = false,
        }
    }
    if result.complete && last_segment == segments.len().checked_sub(1) {
        result.score += 2;
    }
    result
}

pub fn filename_match_quality(name: &str, query: &str) -> u8 {
    if query.is_empty() {
        return 0;
    }
    let sensitive = query.chars().any(char::is_uppercase);
    let name = normalized_name(name, sensitive);
    let query = normalized_name(query, sensitive);
    if query.is_empty() {
        return 0;
    }
    if name == query {
        4
    } else if name.starts_with(&query) {
        3
    } else if name.contains(&query) {
        2
    } else {
        1
    }
}

/// Prefer user folders over dependency trees, caches, and hidden histories.
/// An explicitly requested component remains searchable at its usual rank.
pub fn component_noise(name: &str, query: &str) -> u8 {
    let query = query.to_lowercase();
    let name = name.to_lowercase();
    if query
        .split(['/', '\\'])
        .any(|part| !part.is_empty() && part == name)
    {
        return 0;
    }
    // CLI histories encode an absolute path as a directory name. It is a
    // history bucket, not the original project. Preserve deliberate searches
    // for the encoded name, but omit these aliases from ordinary navigation.
    let encoded_drive = name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name
            .get(1..)
            .is_some_and(|rest| rest.starts_with("--users-") || rest.starts_with("--home-"));
    if encoded_drive
        && !(query.len() >= 3 && query.as_bytes().get(1..3) == Some(b"--") && name.contains(&query))
    {
        return 3;
    }
    if name.starts_with('.') && name != "." && name != ".." {
        2
    } else if matches!(
        name.as_str(),
        "appdata"
            | "node_modules"
            | "target"
            | "__pycache__"
            | "$recycle.bin"
            | "system volume information"
    ) {
        1
    } else {
        0
    }
}

pub fn path_noise(path: &Path, query: &str) -> u8 {
    path.components()
        .map(|part| component_noise(&part.as_os_str().to_string_lossy(), query))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_word_separators_match_without_erasing_punctuation() {
        for name in ["My Project", "my-project", "my_project"] {
            assert_eq!(filename_match_quality(name, "my-project"), 4);
        }
        assert_eq!(
            filename_match_quality("My Project heartbeats", "my-project"),
            3
        );
        assert_eq!(filename_match_quality("Cargo.toml", "Cargotoml"), 1);
    }

    #[test]
    fn tokens_match_successive_path_segments() {
        let good = match_path_tokens("exa pers", r"C:\Users\Example\Personal");
        assert!(good.complete);
        assert_eq!(good.ranges, vec![9..12, 17..21]);
        // Wrong order, or a token that only exists earlier, does not complete.
        assert!(!match_path_tokens("pers exa", r"C:\Users\Example\Personal").complete);
        assert!(!match_path_tokens("exa zzz", r"C:\Users\Example\Personal").complete);
        // Segment prefixes outrank matches inside a word.
        let prefix = match_path_tokens("exa pers", r"C:\Users\Example\Personal");
        let inside = match_path_tokens("exa pers", r"C:\Users\Hexagon\Superstore");
        assert!(inside.complete && prefix.score > inside.score);
        // Two tokens may share one segment, in order.
        assert!(match_path_tokens("foo bar", "x/foo-bar").complete);
        assert!(!match_path_tokens("bar foo", "x/foo-bar").complete);
    }

    #[test]
    fn hidden_ancestors_and_dependency_trees_are_deprioritized() {
        assert_eq!(
            path_noise(Path::new("home/projects/My Project"), "my-project"),
            0
        );
        assert_eq!(
            path_noise(Path::new("home/.claude/projects/my-project"), "my-project"),
            2
        );
        assert_eq!(
            path_noise(Path::new("home/AppData/Local/my-project"), "my-project"),
            1
        );
        assert_eq!(
            path_noise(
                Path::new("home/project/node_modules/my-project"),
                "my-project"
            ),
            1
        );
        assert_eq!(path_noise(Path::new("home/.claude/projects"), ".claude"), 0);
        assert_eq!(component_noise("C--Users-user-my-project", "my-project"), 3);
        assert_eq!(component_noise("C--Users-user-my-project", "C--Users"), 0);
    }
}
