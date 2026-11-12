#![allow(dead_code)] // reason: retained API surface not exercised by every GPUI flow yet
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Per-file verdict for the current previews, used to flag rows in the UI and to
/// block an unsafe apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameIssue {
    /// Safe to apply (may be a no-op if the name is unchanged).
    Ok,
    /// The new name is empty, `.`/`..`, or contains a path separator.
    Invalid,
    /// Two outputs land on the same path, or the target already exists on disk and is
    /// not itself one of the files being renamed.
    Collision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameMode {
    MultiCursor,
    Find,
    Prefix,
    Suffix,
    Number,
}

/// Whether a proposed new name is a usable filename on Linux/macOS/Windows. We
/// reject path separators (which would move the file) and the `.`/`..` specials.
pub fn name_is_valid(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\\')
}

impl std::fmt::Display for RenameMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MultiCursor => write!(f, "Multi-Cursor"),
            Self::Find => write!(f, "Find & Replace"),
            Self::Prefix => write!(f, "Add Prefix"),
            Self::Suffix => write!(f, "Add Suffix"),
            Self::Number => write!(f, "Number Files"),
        }
    }
}

pub struct BatchRenameState {
    pub files: Vec<PathBuf>,
    pub mode: RenameMode,
    pub pattern: String,
    pub replacement: String,
    pub previews: Vec<String>,    // new filenames
    pub cursor_pos: usize,        // Character offset for the synchronized cursor
    pub multi_lines: Vec<String>, // Current edited filenames for each file
    /// When set, the shared caret sits `tail` chars before the end of every line
    /// instead of `cursor_pos` chars after the start. Chosen automatically so the
    /// caret always lands on the same logical spot even when lengths differ.
    pub anchor_end: bool,
    pub tail: usize,
    /// Verdict per file for the current names (refreshed with the previews).
    pub row_issues: Vec<RenameIssue>,
    /// Line edited by a real text input; the others mirror its edits.
    pub primary: usize,
    /// The primary input has everything before its caret selected (the stem on
    /// rename start); mirrors show the same span, each up to its own caret.
    pub select_to_caret: bool,
}

/// What a detected span of a file name represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Number,
    Date,
}

/// A number or date inside a name, in char offsets of the whole line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

fn digits(chars: &[char], at: usize, n: usize) -> Option<u32> {
    let slice = chars.get(at..at + n)?;
    slice.iter().all(char::is_ascii_digit).then(|| {
        slice
            .iter()
            .fold(0, |acc, c| acc * 10 + c.to_digit(10).unwrap_or(0))
    })
}

fn valid_date(y: u32, m: u32, d: u32) -> bool {
    (1900..=2100).contains(&y) && (1..=12).contains(&m) && (1..=31).contains(&d)
}

/// A calendar date starting at `at`: `YYYY-MM-DD`, `DD-MM-YYYY` (separators `-_.`)
/// or compact `YYYYMMDD`. Returns (length, year, month, day).
fn date_at(chars: &[char], at: usize) -> Option<(usize, u32, u32, u32)> {
    let sep = |i: usize| chars.get(i).is_some_and(|c| matches!(c, '-' | '_' | '.'));
    let bounded = |len: usize| {
        !(at > 0 && chars[at - 1].is_ascii_digit())
            && !chars.get(at + len).is_some_and(char::is_ascii_digit)
    };
    if let (Some(y), true, Some(m), true, Some(d)) = (
        digits(chars, at, 4),
        sep(at + 4),
        digits(chars, at + 5, 2),
        sep(at + 7),
        digits(chars, at + 8, 2),
    ) {
        if valid_date(y, m, d) && bounded(10) {
            return Some((10, y, m, d));
        }
    }
    if let (Some(d), true, Some(m), true, Some(y)) = (
        digits(chars, at, 2),
        sep(at + 2),
        digits(chars, at + 3, 2),
        sep(at + 5),
        digits(chars, at + 6, 4),
    ) {
        if valid_date(y, m, d) && bounded(10) {
            return Some((10, y, m, d));
        }
    }
    if let (Some(y), Some(m), Some(d)) = (
        digits(chars, at, 4),
        digits(chars, at + 4, 2),
        digits(chars, at + 6, 2),
    ) {
        if valid_date(y, m, d) && bounded(8) {
            return Some((8, y, m, d));
        }
    }
    None
}

/// Detect dates and numbers in the stem of `line` (the extension is never a token).
pub fn detect_tokens(line: &str) -> Vec<Token> {
    let chars: Vec<char> = line.chars().collect();
    let stem_end = match chars.iter().rposition(|c| *c == '.') {
        Some(dot) if dot > 0 => dot,
        _ => chars.len(),
    };
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < stem_end {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        if let Some((len, ..)) = date_at(&chars, i).filter(|(len, ..)| i + len <= stem_end) {
            tokens.push(Token {
                start: i,
                end: i + len,
                kind: TokenKind::Date,
            });
            i += len;
            continue;
        }
        let start = i;
        while i < stem_end && chars[i].is_ascii_digit() {
            i += 1;
        }
        tokens.push(Token {
            start,
            end: i,
            kind: TokenKind::Number,
        });
    }
    tokens
}

fn common_prefix_len(lines: &[String]) -> usize {
    let Some(first) = lines.first() else { return 0 };
    let mut n = first.chars().count();
    for line in &lines[1..] {
        n = n.min(
            first
                .chars()
                .zip(line.chars())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    n
}

fn common_suffix_len(lines: &[String]) -> usize {
    let Some(first) = lines.first() else { return 0 };
    let mut n = first.chars().count();
    for line in &lines[1..] {
        n = n.min(
            first
                .chars()
                .rev()
                .zip(line.chars().rev())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    n
}

fn char_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn prev_word_boundary(s: &str, char_pos: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    if len == 0 || char_pos == 0 {
        return 0;
    }
    let mut idx = char_pos.min(len);
    // 1. Skip non-alphanumeric chars
    while idx > 0 && !chars[idx - 1].is_alphanumeric() {
        idx -= 1;
    }
    // 2. Skip alphanumeric chars
    while idx > 0 && chars[idx - 1].is_alphanumeric() {
        idx -= 1;
    }
    idx
}

fn next_word_boundary(s: &str, char_pos: usize) -> usize {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    if len == 0 || char_pos >= len {
        return len;
    }
    let mut idx = char_pos;
    if chars[idx].is_alphanumeric() {
        while idx < len && chars[idx].is_alphanumeric() {
            idx += 1;
        }
        while idx < len && !chars[idx].is_alphanumeric() {
            idx += 1;
        }
    } else {
        while idx < len && !chars[idx].is_alphanumeric() {
            idx += 1;
        }
        while idx < len && chars[idx].is_alphanumeric() {
            idx += 1;
        }
    }
    idx
}

impl BatchRenameState {
    pub fn new(files: Vec<PathBuf>) -> Self {
        let mut s = Self {
            files,
            mode: RenameMode::MultiCursor,
            pattern: String::new(),
            replacement: String::new(),
            previews: Vec::new(),
            cursor_pos: 0,
            multi_lines: Vec::new(),
            anchor_end: false,
            tail: 0,
            row_issues: Vec::new(),
            primary: 0,
            select_to_caret: false,
        };
        s.init_multicursor();
        s.update_previews();
        s
    }

    pub fn set_mode(&mut self, mode: RenameMode) {
        if mode == RenameMode::MultiCursor {
            self.init_multicursor();
        }
        self.mode = mode;
        self.update_previews();
    }

    pub fn init_multicursor(&mut self) {
        self.multi_lines = self
            .files
            .iter()
            .map(|path| {
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            })
            .collect();
        self.align_cursor_to_stem_end();
    }

    pub fn update_previews(&mut self) {
        self.compute_previews();
        self.row_issues = self.issues();
    }

    fn compute_previews(&mut self) {
        if self.mode == RenameMode::MultiCursor {
            self.previews = self.multi_lines.clone();
            return;
        }

        self.previews = self
            .files
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let ext = path
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                let name = format!("{stem}{ext}");

                match &self.mode {
                    RenameMode::MultiCursor => name,
                    RenameMode::Find => {
                        if self.pattern.is_empty() {
                            name
                        } else {
                            name.replace(&self.pattern, &self.replacement)
                        }
                    }
                    RenameMode::Prefix => format!("{}{}", self.pattern, name),
                    RenameMode::Suffix => format!("{}{}{}", stem, self.pattern, ext),
                    RenameMode::Number => {
                        format!("{:03}{}{}{}", i + 1, self.replacement, stem, ext)
                    }
                }
            })
            .collect();
    }

    pub fn insert_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.insert_str(c.encode_utf8(&mut buf));
    }

    /// Caret column for `line`, honouring the current anchor.
    pub fn pos_in(&self, line: &str) -> usize {
        let n = line.chars().count();
        if self.anchor_end {
            n.saturating_sub(self.tail)
        } else {
            self.cursor_pos.min(n)
        }
    }

    fn first_len(&self) -> usize {
        self.multi_lines
            .get(self.primary)
            .map_or(0, |l| l.chars().count())
    }

    /// Place the shared caret at `pos` (a column of the first line) and pick the
    /// anchor that keeps it on the same logical spot in every other line.
    pub fn set_cursor(&mut self, pos: usize) {
        let first_len = self.first_len();
        let pos = pos.min(first_len);
        self.cursor_pos = pos;
        self.anchor_end = false;
        self.tail = 0;
        if pos > common_prefix_len(&self.multi_lines)
            && first_len - pos <= common_suffix_len(&self.multi_lines)
        {
            self.anchor_end = true;
            self.tail = first_len - pos;
        }
    }

    pub fn insert_str(&mut self, s: &str) {
        let str_chars = s.chars().count();
        if str_chars == 0 {
            return;
        }
        let target = self.cursor_pos + str_chars;
        let (anchor_end, tail, cursor) = (self.anchor_end, self.tail, self.cursor_pos);
        for line in &mut self.multi_lines {
            let n = line.chars().count();
            let pos = if anchor_end {
                n.saturating_sub(tail)
            } else {
                cursor.min(n)
            };
            line.insert_str(char_to_byte_index(line, pos), s);
        }
        self.set_cursor(target);
        self.update_previews();
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let target = self.cursor_pos - 1;
        let (anchor_end, tail, cursor) = (self.anchor_end, self.tail, self.cursor_pos);
        for line in &mut self.multi_lines {
            let n = line.chars().count();
            let pos = if anchor_end {
                n.saturating_sub(tail)
            } else {
                cursor.min(n)
            };
            if pos > 0 {
                let start = char_to_byte_index(line, pos - 1);
                let end = char_to_byte_index(line, pos);
                line.replace_range(start..end, "");
            }
        }
        self.set_cursor(target);
        self.update_previews();
    }

    pub fn delete_forward(&mut self) {
        let target = self.cursor_pos;
        let (anchor_end, tail, cursor) = (self.anchor_end, self.tail, self.cursor_pos);
        for line in &mut self.multi_lines {
            let n = line.chars().count();
            let pos = if anchor_end {
                n.saturating_sub(tail)
            } else {
                cursor.min(n)
            };
            if pos < n {
                let start = char_to_byte_index(line, pos);
                let end = char_to_byte_index(line, pos + 1);
                line.replace_range(start..end, "");
            }
        }
        self.set_cursor(target);
        self.update_previews();
    }

    pub fn move_cursor_left(&mut self) {
        self.set_cursor(self.cursor_pos.saturating_sub(1));
    }

    pub fn move_cursor_right(&mut self) {
        self.set_cursor(self.cursor_pos + 1);
    }

    pub fn move_cursor_home(&mut self) {
        self.set_cursor(0);
    }

    pub fn move_cursor_end(&mut self) {
        self.set_cursor(self.first_len());
    }

    pub fn move_cursor_word_left(&mut self) {
        if let Some(reference) = self.multi_lines.first() {
            let to = prev_word_boundary(reference, self.cursor_pos);
            self.set_cursor(to);
        } else {
            self.cursor_pos = 0;
        }
    }

    pub fn move_cursor_word_right(&mut self) {
        if let Some(reference) = self.multi_lines.first() {
            let to = next_word_boundary(reference, self.cursor_pos);
            self.set_cursor(to);
        }
    }

    pub fn insert_counter(&mut self, padding: usize) {
        let target = self.cursor_pos + format!("{:0width$}", 1, width = padding).chars().count();
        let (anchor_end, tail, cursor) = (self.anchor_end, self.tail, self.cursor_pos);
        for (i, line) in self.multi_lines.iter_mut().enumerate() {
            let num = format!("{:0width$}", i + 1, width = padding);
            let n = line.chars().count();
            let pos = if anchor_end {
                n.saturating_sub(tail)
            } else {
                cursor.min(n)
            };
            line.insert_str(char_to_byte_index(line, pos), &num);
        }
        self.set_cursor(target);
        self.update_previews();
    }

    /// Map a column of the primary line (`old_len` chars) onto another line.
    fn map_col(col: usize, old_len: usize, line_len: usize, prefix: usize, suffix: usize) -> usize {
        if col <= prefix {
            col.min(line_len)
        } else if old_len - col <= suffix {
            line_len.saturating_sub(old_len - col)
        } else {
            col.min(line_len)
        }
    }

    /// The real input holding the primary line changed to `new_text` with its caret
    /// at `caret_col`: replay the same edit on every other line, anchored to the
    /// shared prefix/suffix, and move the shared caret. Returns true on any change.
    /// Record whether the primary input selects `0..caret`; true when that changed.
    pub fn set_select_to_caret(&mut self, on: bool) -> bool {
        let changed = self.select_to_caret != on;
        self.select_to_caret = on;
        changed
    }

    pub fn apply_primary(&mut self, new_text: &str, caret_col: usize) -> bool {
        let Some(old) = self.multi_lines.get(self.primary).cloned() else {
            return false;
        };
        let moved = self.cursor_pos != caret_col;
        if old == new_text {
            if moved {
                self.set_cursor(caret_col);
            }
            return moved;
        }
        let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new_text.chars().collect());
        let p = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
        let max_s = o.len().min(n.len()) - p;
        let s = o
            .iter()
            .rev()
            .zip(n.iter().rev())
            .take_while(|(a, b)| a == b)
            .count()
            .min(max_s);
        let (del_end, ins): (usize, String) = (o.len() - s, n[p..n.len() - s].iter().collect());
        let prefix = common_prefix_len(&self.multi_lines);
        let suffix = common_suffix_len(&self.multi_lines);
        let primary = self.primary;
        for (i, line) in self.multi_lines.iter_mut().enumerate() {
            if i == primary {
                *line = new_text.to_string();
                continue;
            }
            let len = line.chars().count();
            let a = Self::map_col(p, o.len(), len, prefix, suffix);
            let b = Self::map_col(del_end, o.len(), len, prefix, suffix).max(a);
            let (ba, bb) = (char_to_byte_index(line, a), char_to_byte_index(line, b));
            line.replace_range(ba..bb, &ins);
        }
        self.set_cursor(caret_col);
        self.update_previews();
        true
    }

    /// Tokens of the reference (first) line.
    pub fn tokens(&self) -> Vec<Token> {
        self.multi_lines
            .get(self.primary)
            .map(|l| detect_tokens(l))
            .unwrap_or_default()
    }

    /// Jump the shared caret to the next/previous structural boundary of the first
    /// line: token edges, the end of the stem, the end of the name.
    pub fn jump_boundary(&mut self, forward: bool) {
        let Some(first) = self.multi_lines.get(self.primary) else {
            return;
        };
        let len = first.chars().count();
        let stem_end = first
            .chars()
            .collect::<Vec<_>>()
            .iter()
            .rposition(|c| *c == '.')
            .filter(|dot| *dot > 0)
            .unwrap_or(len);
        let mut stops: Vec<usize> = vec![0, stem_end, len];
        for t in detect_tokens(first) {
            stops.extend([t.start, t.end]);
        }
        stops.sort_unstable();
        stops.dedup();
        let at = self.cursor_pos;
        let to = if forward {
            stops.iter().copied().find(|s| *s > at).unwrap_or(stops[0])
        } else {
            stops.iter().rev().copied().find(|s| *s < at).unwrap_or(len)
        };
        self.set_cursor(to);
    }

    /// Replace the first numeric token of every line by a running counter,
    /// keeping zero padding wide enough for the largest number.
    pub fn renumber(&mut self, start: usize) -> bool {
        let numbers = |l: &str| -> Vec<Token> {
            detect_tokens(l)
                .into_iter()
                .filter(|t| t.kind == TokenKind::Number)
                .collect()
        };
        if !self
            .multi_lines
            .first()
            .is_some_and(|l| !numbers(l).is_empty())
        {
            return false;
        }
        let count = self.multi_lines.len();
        let width = (start + count.saturating_sub(1)).to_string().len().max(
            self.multi_lines
                .iter()
                .filter_map(|l| numbers(l).first().map(|t| t.end - t.start))
                .max()
                .unwrap_or(1),
        );
        for (i, line) in self.multi_lines.iter_mut().enumerate() {
            if let Some(t) = numbers(line).first().copied() {
                let a = char_to_byte_index(line, t.start);
                let b = char_to_byte_index(line, t.end);
                line.replace_range(a..b, &format!("{:0width$}", start + i, width = width));
            }
        }
        self.set_cursor(self.cursor_pos);
        self.update_previews();
        true
    }

    /// Rewrite every detected date as ISO `YYYY-MM-DD`.
    pub fn normalize_dates(&mut self) -> bool {
        let mut changed = false;
        for line in &mut self.multi_lines {
            let chars: Vec<char> = line.chars().collect();
            let mut out = String::new();
            let mut i = 0;
            let dates: Vec<_> = detect_tokens(line)
                .into_iter()
                .filter(|t| t.kind == TokenKind::Date)
                .collect();
            for t in dates {
                let Some((_, y, m, d)) = date_at(&chars, t.start) else {
                    continue;
                };
                out.extend(&chars[i..t.start]);
                out.push_str(&format!("{y:04}-{m:02}-{d:02}"));
                i = t.end;
            }
            out.extend(&chars[i..]);
            if out != *line {
                *line = out;
                changed = true;
            }
        }
        self.set_cursor(self.cursor_pos);
        self.update_previews();
        changed
    }

    pub fn align_cursor_to_stem_end(&mut self) {
        if let Some(first) = self.multi_lines.first() {
            let stem_len = std::path::Path::new(first)
                .file_stem()
                .map(|s| s.to_string_lossy().chars().count())
                .unwrap_or_else(|| first.chars().count());
            self.set_cursor(stem_len);
        } else {
            self.cursor_pos = 0;
        }
    }

    pub fn align_cursor_to_start(&mut self) {
        self.set_cursor(0);
    }

    /// Resolve the absolute target path for preview `i` (its parent dir + new name).
    fn target(&self, i: usize) -> Option<PathBuf> {
        let parent = self.files.get(i)?.parent()?;
        Some(parent.join(self.previews.get(i)?))
    }

    /// Classify every file: invalid name, colliding target, or ok. Aligned with
    /// `previews`. Collisions cover both duplicate outputs and pre-existing files on
    /// disk that aren't part of this batch.
    pub fn issues(&self) -> Vec<RenameIssue> {
        let sources: HashSet<&PathBuf> = self.files.iter().collect();
        // How many outputs map to each target path (to catch duplicate destinations).
        let mut target_counts: HashMap<PathBuf, usize> = HashMap::new();
        for i in 0..self.files.len() {
            if let Some(t) = self.target(i) {
                *target_counts.entry(t).or_insert(0) += 1;
            }
        }

        (0..self.files.len())
            .map(|i| {
                let name = self.previews.get(i).map(String::as_str).unwrap_or_default();
                if !name_is_valid(name) {
                    return RenameIssue::Invalid;
                }
                let Some(target) = self.target(i) else {
                    return RenameIssue::Invalid;
                };
                // Unchanged name is always fine (no-op).
                if Some(&target) == self.files.get(i) {
                    return RenameIssue::Ok;
                }
                if target_counts.get(&target).copied().unwrap_or(0) > 1 {
                    return RenameIssue::Collision;
                }
                // A pre-existing file at the target that isn't being renamed away.
                if target.exists() && !sources.contains(&target) {
                    return RenameIssue::Collision;
                }
                RenameIssue::Ok
            })
            .collect()
    }

    /// True when at least one file has a blocking problem (invalid name or collision),
    /// so the UI can refuse to apply rather than clobber data.
    pub fn has_blocking_issues(&self) -> bool {
        self.issues()
            .iter()
            .any(|v| matches!(v, RenameIssue::Invalid | RenameIssue::Collision))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multicursor_init() {
        let files = vec![
            PathBuf::from("/test/photo_1.png"),
            PathBuf::from("/test/photo_2.png"),
        ];
        let state = BatchRenameState::new(files);
        assert_eq!(state.mode, RenameMode::MultiCursor);
        assert_eq!(
            state.multi_lines,
            vec!["photo_1.png".to_string(), "photo_2.png".to_string()]
        );
        assert_eq!(state.cursor_pos, 7); // "photo_1" has 7 characters
        assert_eq!(state.previews, state.multi_lines);
    }

    #[test]
    fn test_multicursor_typing() {
        let files = vec![
            PathBuf::from("/test/doc_a.txt"),
            PathBuf::from("/test/doc_b.txt"),
        ];
        let mut state = BatchRenameState::new(files);
        assert_eq!(state.cursor_pos, 5); // "doc_a" length is 5

        state.insert_str("_v2");
        assert_eq!(
            state.multi_lines,
            vec!["doc_a_v2.txt".to_string(), "doc_b_v2.txt".to_string()]
        );
        assert_eq!(state.cursor_pos, 8);
        assert_eq!(state.previews, state.multi_lines);

        state.insert_char('!');
        assert_eq!(
            state.multi_lines,
            vec!["doc_a_v2!.txt".to_string(), "doc_b_v2!.txt".to_string()]
        );
        assert_eq!(state.cursor_pos, 9);
    }

    #[test]
    fn test_multicursor_backspace_and_delete() {
        let files = vec![
            PathBuf::from("/test/cat.png"),
            PathBuf::from("/test/dog.png"),
        ];
        let mut state = BatchRenameState::new(files);
        assert_eq!(state.cursor_pos, 3); // "cat" len is 3

        state.backspace();
        assert_eq!(
            state.multi_lines,
            vec!["ca.png".to_string(), "do.png".to_string()]
        );
        assert_eq!(state.cursor_pos, 2);

        // Delete forward deletes '.'
        state.delete_forward();
        assert_eq!(
            state.multi_lines,
            vec!["capng".to_string(), "dopng".to_string()]
        );
        assert_eq!(state.cursor_pos, 2);
    }

    #[test]
    fn test_multicursor_navigation() {
        let files = vec![
            PathBuf::from("/test/my_vacation_photo.jpg"),
            PathBuf::from("/test/my_vacation_other.jpg"),
        ];
        let mut state = BatchRenameState::new(files);
        // stem is "my_vacation_photo" (len 17)
        assert_eq!(state.cursor_pos, 17);

        state.move_cursor_home();
        assert_eq!(state.cursor_pos, 0);

        state.move_cursor_right();
        assert_eq!(state.cursor_pos, 1);

        state.move_cursor_left();
        assert_eq!(state.cursor_pos, 0);

        // Word navigation forward:
        // "my" -> skips alphanumeric, skips delimiter '_' -> pos 3
        state.move_cursor_word_right();
        assert_eq!(state.cursor_pos, 3);

        // "vacation" -> skips alphanumeric, skips delimiter '_' -> pos 12
        state.move_cursor_word_right();
        assert_eq!(state.cursor_pos, 12);

        // Word navigation backward:
        // skips delimiter '_', skips "vacation" -> pos 3
        state.move_cursor_word_left();
        assert_eq!(state.cursor_pos, 3);

        state.move_cursor_end();
        assert_eq!(state.cursor_pos, "my_vacation_photo.jpg".len());
    }

    #[test]
    fn test_multicursor_numbering() {
        let files = vec![
            PathBuf::from("/test/item.png"),
            PathBuf::from("/test/item.png"),
            PathBuf::from("/test/item.png"),
        ];
        let mut state = BatchRenameState::new(files);
        assert_eq!(state.cursor_pos, 4); // "item" len 4

        state.insert_str("_");
        state.insert_counter(2);
        assert_eq!(
            state.multi_lines,
            vec![
                "item_01.png".to_string(),
                "item_02.png".to_string(),
                "item_03.png".to_string(),
            ]
        );
        assert_eq!(state.cursor_pos, 7); // 4 + 1 + 2 = 7
    }

    #[test]
    fn test_multicursor_stem_and_start_align() {
        let files = vec![
            PathBuf::from("/test/readme.md"),
            PathBuf::from("/test/notes.md"),
        ];
        let mut state = BatchRenameState::new(files);
        assert_eq!(state.cursor_pos, 6);

        state.align_cursor_to_start();
        assert_eq!(state.cursor_pos, 0);

        state.align_cursor_to_stem_end();
        assert_eq!(state.cursor_pos, 6);
    }

    #[test]
    fn test_multicursor_collision_and_validation() {
        let files = vec![PathBuf::from("/test/a.txt"), PathBuf::from("/test/b.txt")];
        let mut state = BatchRenameState::new(files);
        // Initially a.txt and b.txt are unchanged (no-op, Ok)
        assert_eq!(state.issues(), vec![RenameIssue::Ok, RenameIssue::Ok]);
        assert!(!state.has_blocking_issues());

        // Replace both stems with "same"
        state.align_cursor_to_start();
        state.insert_str("same");
        // multi_lines: ["samea.txt", "sameb.txt"] -> no collision
        assert!(!state.has_blocking_issues());

        // Make both lines identical
        state.multi_lines = vec!["same.txt".to_string(), "same.txt".to_string()];
        state.update_previews();
        assert_eq!(
            state.issues(),
            vec![RenameIssue::Collision, RenameIssue::Collision]
        );
        assert!(state.has_blocking_issues());

        // Test invalid filename
        state.multi_lines = vec!["invalid/slash.txt".to_string(), "valid.txt".to_string()];
        state.update_previews();
        assert_eq!(state.issues(), vec![RenameIssue::Invalid, RenameIssue::Ok]);
        assert!(state.has_blocking_issues());
    }

    fn state(names: &[&str]) -> BatchRenameState {
        BatchRenameState::new(
            names
                .iter()
                .map(|n| PathBuf::from(format!("/t/{n}")))
                .collect(),
        )
    }

    #[test]
    fn detects_numbers_and_dates_but_not_extension_digits() {
        let t = detect_tokens("IMG_2024-03-05_007.mp4");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].kind, TokenKind::Date);
        assert_eq!((t[0].start, t[0].end), (4, 14));
        assert_eq!(t[1].kind, TokenKind::Number);
        assert!(detect_tokens("clip.mp4").is_empty());
        assert_eq!(detect_tokens("20240305.png")[0].kind, TokenKind::Date);
    }

    #[test]
    fn caret_in_shared_prefix_edits_every_name_even_with_different_numbers() {
        let mut s = state(&["IMG_1.png", "IMG_22.png"]);
        s.set_cursor(0);
        s.insert_str("Trip_");
        assert_eq!(s.multi_lines, vec!["Trip_IMG_1.png", "Trip_IMG_22.png"]);
        // After the varying number the caret anchors to the end of the name.
        s.set_cursor(s.multi_lines[0].chars().count() - 4);
        s.insert_str("_x");
        assert_eq!(s.multi_lines, vec!["Trip_IMG_1_x.png", "Trip_IMG_22_x.png"]);
    }

    #[test]
    fn renumber_and_normalize_dates() {
        let mut s = state(&["a_7.txt", "a_3.txt", "a_9.txt"]);
        assert!(s.renumber(1));
        assert_eq!(s.multi_lines, vec!["a_1.txt", "a_2.txt", "a_3.txt"]);
        let mut d = state(&["05-03-2024 x.txt"]);
        assert!(d.normalize_dates());
        assert_eq!(d.multi_lines, vec!["2024-03-05 x.txt"]);
    }

    #[test]
    fn primary_edits_replay_on_the_other_lines() {
        let mut s = state(&["IMG_1.png", "IMG_22.png", "IMG_333.png"]);
        s.primary = 1;
        // Type "Trip_" at the start of the primary's input.
        assert!(s.apply_primary("Trip_IMG_22.png", 5));
        assert_eq!(
            s.multi_lines,
            vec!["Trip_IMG_1.png", "Trip_IMG_22.png", "Trip_IMG_333.png"]
        );
        // Replace the stem's tail: selection of "22" -> "x" lands on each name's own number.
        let mut t = state(&["a_1.txt", "a_22.txt"]);
        t.primary = 0;
        assert!(t.apply_primary("a_x.txt", 3));
        assert_eq!(t.multi_lines, vec!["a_x.txt", "a_x.txt"]);
    }
}
