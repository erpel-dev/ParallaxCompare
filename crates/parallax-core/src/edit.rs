//! Editing the two sides of a text compare: free edits, copying blocks from one
//! side to the other, undo and redo.
//!
//! Every edit is one splice of a document's text, so the undo history holds only
//! the replaced text, and line terminators that are not edited keep their bytes.
//! The diff is recomputed after each edit.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::text::TextDoc;
use crate::textdiff::{self, DiffOptions, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

impl Side {
    fn index(self) -> usize {
        self as usize
    }

    pub fn other(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// A position as the web view counts: line and UTF-16 column. A column past the
/// end of the line means the end of the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

/// Where an edit happened: highlighting is stale from this line on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edited {
    pub side: Side,
    pub line: usize,
}

struct Splice {
    side: Side,
    at: usize,
    removed: String,
    inserted: String,
}

pub struct EditSession {
    docs: [TextDoc; 2],
    diff: TextDiff,
    options: DiffOptions,
    undo: Vec<Splice>,
    redo: Vec<Splice>,
    /// Hashes of the texts as last loaded or saved, to tell unsaved changes.
    saved: [u64; 2],
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Converts any line terminator in typed or pasted text to the document's.
fn with_newlines(text: &str, newline: &str) -> String {
    if !text.contains(['\n', '\r']) {
        return text.to_string();
    }
    text.replace("\r\n", "\n").replace('\r', "\n").replace('\n', newline)
}

impl EditSession {
    pub fn new(left: TextDoc, right: TextDoc, options: DiffOptions) -> Self {
        let diff = textdiff::diff_docs(&left, &right, &options);
        let saved = [hash(&left.text), hash(&right.text)];
        EditSession { docs: [left, right], diff, options, undo: Vec::new(), redo: Vec::new(), saved }
    }

    pub fn doc(&self, side: Side) -> &TextDoc {
        &self.docs[side.index()]
    }

    pub fn diff(&self) -> &TextDiff {
        &self.diff
    }

    /// Diffs again with other whitespace and case rules, keeping the edits.
    pub fn set_options(&mut self, options: DiffOptions) {
        self.options = options;
        self.rediff();
    }

    fn rediff(&mut self) {
        self.diff = textdiff::diff_docs(&self.docs[0], &self.docs[1], &self.options);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Whether the side differs from what was loaded or last saved.
    pub fn is_dirty(&self, side: Side) -> bool {
        hash(&self.doc(side).text) != self.saved[side.index()]
    }

    pub fn mark_saved(&mut self, side: Side) {
        self.saved[side.index()] = hash(&self.doc(side).text);
    }

    /// Replaces the text between two positions; `\n` in `text` starts new lines.
    pub fn replace(&mut self, side: Side, from: Pos, to: Pos, text: &str) -> Edited {
        let doc = self.doc(side);
        let a = doc.offset(from.line as usize, from.col);
        let b = doc.offset(to.line as usize, to.col).max(a);
        let text = with_newlines(text, doc.newline);
        self.apply(side, a..b, text)
    }

    /// Inserts a line where the row shows a filler on `side`.
    pub fn insert_line(&mut self, side: Side, row: u32, text: &str) -> Edited {
        let line = self.line_at_row(side, row);
        let doc = self.doc(side);
        let nl = doc.newline;
        let text = with_newlines(text, nl);
        let at = doc.line_start(line);
        let text = if line >= doc.len() && !doc.is_empty() && !doc.ends_with_newline() {
            // After a last line without terminator: it gets one, the new line none.
            format!("{nl}{text}")
        } else {
            format!("{text}{nl}")
        };
        self.apply(side, at..at, text)
    }

    /// The lines of `side` shown in `rows`.
    fn lines_in(&self, side: Side, rows: Range<usize>) -> Option<Range<usize>> {
        let mut lines = self.diff.rows[rows].iter().filter_map(|r| pick(r, side)).map(|l| l as usize);
        let first = lines.next()?;
        Some(first..lines.next_back().unwrap_or(first) + 1)
    }

    /// Makes `rows` of the `to` side equal to the other side: the target's lines in
    /// those rows are replaced by the source's. Line terminators follow the target.
    pub fn copy_rows(&mut self, rows: Range<u32>, to: Side) -> Option<Edited> {
        let from = to.other();
        let n = self.diff.rows.len();
        let rows = (rows.start as usize).min(n)..(rows.end as usize).min(n);
        let source = self.lines_in(from, rows.clone());
        let target = self.lines_in(to, rows.clone());
        if source.is_none() && target.is_none() {
            return None;
        }
        let (src, dst) = (self.doc(from), self.doc(to));
        let nl = dst.newline;
        let mut text = String::new();
        if let Some(source) = &source {
            for line in source.clone() {
                text.push_str(src.line(line));
                text.push_str(nl);
            }
            // The source file's last line keeps its missing terminator.
            if source.end >= src.len() && !src.ends_with_newline() {
                text.truncate(text.len() - nl.len());
            }
        }
        let terminate = |text: &mut String| {
            if !text.is_empty() && !text.ends_with(nl) {
                text.push_str(nl);
            }
        };
        // Nothing of the source follows: the target's end follows the source's end.
        let source_ends = self.lines_in(from, rows.end..n).is_none() && !src.ends_with_newline() && !src.is_empty();
        let range = match target {
            Some(t) if t.end >= dst.len() && text.is_empty() && source_ends && t.start > 0 => {
                dst.lines[t.start - 1].end..dst.text.len()
            }
            // Replacing up to the end: the end follows the source block.
            Some(t) if t.end >= dst.len() => dst.line_start(t.start)..dst.text.len(),
            Some(t) => {
                terminate(&mut text);
                dst.line_start(t.start)..dst.line_start(t.end)
            }
            None => {
                let line = self.line_at_row(to, rows.start as u32);
                if line < dst.len() {
                    terminate(&mut text);
                } else if !dst.is_empty() && !dst.ends_with_newline() {
                    text.insert_str(0, nl);
                }
                let at = dst.line_start(line);
                at..at
            }
        };
        Some(self.apply(to, range, text))
    }

    /// Writes every line break of `side` as `newline` (`"\n"`, `"\r\n"` or `"\r"`).
    pub fn set_line_endings(&mut self, side: Side, newline: &str) -> Option<Edited> {
        let doc = self.doc(side);
        let text = doc.text.replace("\r\n", "\n").replace('\r', "\n");
        let text = if newline == "\n" { text } else { text.replace('\n', newline) };
        if text == doc.text {
            return None;
        }
        let len = doc.text.len();
        Some(self.apply(side, 0..len, text))
    }

    /// The text of the lines of `side` shown in `rows`, each with a `\n`.
    pub fn rows_text(&self, side: Side, rows: Range<u32>) -> String {
        let n = self.diff.rows.len();
        let Some(lines) = self.lines_in(side, (rows.start as usize).min(n)..(rows.end as usize).min(n)) else {
            return String::new();
        };
        let doc = self.doc(side);
        lines.map(|l| format!("{}\n", doc.line(l))).collect()
    }

    /// Replaces the lines of `side` shown in `rows` with `text`, or with `replace`
    /// false inserts it before the line at the first row. Empty text deletes the
    /// lines. Whether the file ends with a line break stays as it was.
    pub fn replace_rows(&mut self, side: Side, rows: Range<u32>, text: &str, replace: bool) -> Option<Edited> {
        let n = self.diff.rows.len();
        let rows = (rows.start as usize).min(n)..(rows.end as usize).min(n);
        let target = if replace { self.lines_in(side, rows.clone()) } else { None };
        let doc = self.doc(side);
        let nl = doc.newline;
        let mut text = with_newlines(text, nl);
        let terminate = |text: &mut String| {
            if !text.is_empty() && !text.ends_with(nl) {
                text.push_str(nl);
            }
        };
        let range = match target {
            // The last lines go from a file without a final line break: the line
            // before them becomes the last, also without one.
            Some(t) if t.end >= doc.len() && text.is_empty() && t.start > 0 && !doc.ends_with_newline() => {
                doc.lines[t.start - 1].end..doc.text.len()
            }
            Some(t) if t.end >= doc.len() => {
                if doc.ends_with_newline() {
                    terminate(&mut text);
                } else if let Some(t) = text.strip_suffix(nl) {
                    text = t.to_string();
                }
                doc.line_start(t.start)..doc.text.len()
            }
            Some(t) => {
                terminate(&mut text);
                doc.line_start(t.start)..doc.line_start(t.end)
            }
            None if text.is_empty() => return None,
            None => {
                let line = self.line_at_row(side, rows.start as u32);
                if line < doc.len() || doc.ends_with_newline() {
                    terminate(&mut text);
                } else if !doc.is_empty() {
                    text.insert_str(0, nl);
                    if let Some(t) = text.strip_suffix(nl) {
                        text = t.to_string();
                    }
                }
                let at = doc.line_start(line);
                at..at
            }
        };
        if range.is_empty() && text.is_empty() {
            return None;
        }
        Some(self.apply(side, range, text))
    }

    fn apply(&mut self, side: Side, range: Range<usize>, inserted: String) -> Edited {
        let at = range.start;
        let removed = self.docs[side.index()].splice(range, &inserted);
        self.undo.push(Splice { side, at, removed, inserted });
        self.redo.clear();
        self.finish(side, at)
    }

    fn finish(&mut self, side: Side, at: usize) -> Edited {
        self.rediff();
        let doc = self.doc(side);
        let line = doc.lines.partition_point(|r| r.end < at);
        Edited { side, line }
    }

    pub fn undo(&mut self) -> Option<Edited> {
        let s = self.undo.pop()?;
        self.docs[s.side.index()].splice(s.at..s.at + s.inserted.len(), &s.removed);
        let edited = self.finish(s.side, s.at);
        self.redo.push(s);
        Some(edited)
    }

    pub fn redo(&mut self) -> Option<Edited> {
        let s = self.redo.pop()?;
        self.docs[s.side.index()].splice(s.at..s.at + s.removed.len(), &s.inserted);
        let edited = self.finish(s.side, s.at);
        self.undo.push(s);
        Some(edited)
    }

    /// The line of `side` shown at `row`, or for a filler, the line that follows it.
    pub fn line_at_row(&self, side: Side, row: u32) -> usize {
        self.diff.rows[(row as usize).min(self.diff.rows.len())..]
            .iter()
            .find_map(|r| pick(r, side))
            .map_or(self.doc(side).len(), |l| l as usize)
    }

    /// The row showing `line` of `side`; past the end, the last row.
    pub fn row_of_line(&self, side: Side, line: usize) -> u32 {
        let rows = &self.diff.rows;
        rows.iter()
            .position(|r| pick(r, side).is_some_and(|l| l as usize >= line))
            .unwrap_or(rows.len().saturating_sub(1)) as u32
    }
}

fn pick(row: &textdiff::Row, side: Side) -> Option<u32> {
    match side {
        Side::Left => row.left,
        Side::Right => row.right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::decode;

    fn session(a: &str, b: &str) -> EditSession {
        EditSession::new(decode(a.as_bytes()), decode(b.as_bytes()), DiffOptions::default())
    }

    fn text(s: &EditSession, side: Side) -> &str {
        &s.doc(side).text
    }

    #[test]
    fn copying_all_sections_makes_the_files_equal() {
        for (a, b) in [
            ("a\nb\nc\nd\n", "a\nB\nc\nx\ny\nd\n"),
            ("one\ntwo", "zero\none\ntwo\nthree\n"),
            ("", "x\ny"),
            ("x\ny\n", ""),
            ("same\nleft only", "same"),
        ] {
            let mut s = session(a, b);
            while let Some(section) = s.diff().sections.first().cloned() {
                s.copy_rows(section.start..section.end, Side::Right).unwrap();
            }
            assert_eq!(text(&s, Side::Right), a, "{a:?} -> {b:?}");
            // Undo restores the original, redo the copies.
            while s.undo().is_some() {}
            assert_eq!(text(&s, Side::Right), b);
            assert!(!s.is_dirty(Side::Right));
            while s.redo().is_some() {}
            assert_eq!(text(&s, Side::Right), a);
            assert!(s.is_dirty(Side::Right));
        }
    }

    #[test]
    fn copying_sections_of_random_files() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..500 {
            let mut file = || {
                let lines: Vec<&str> = (0..next(7)).map(|_| ["a", "b", "c", "d", ""][next(5) as usize]).collect();
                let mut text = lines.join("\n");
                if !lines.is_empty() && next(2) == 0 {
                    text.push('\n');
                }
                text
            };
            let (a, b) = (file(), file());
            let mut s = session(&a, &b);
            while let Some(section) = s.diff().sections.first().cloned() {
                s.copy_rows(section.start..section.end, Side::Right).unwrap();
            }
            assert_eq!(text(&s, Side::Right), a, "{a:?} <- {b:?}");
            while s.undo().is_some() {}
            assert_eq!(text(&s, Side::Right), b);
        }
    }

    #[test]
    fn copied_lines_take_the_target_line_endings() {
        let mut s = session("a\nnew\nb\n", "a\r\nb\r\n");
        let section = s.diff().sections[0].clone();
        s.copy_rows(section.start..section.end, Side::Right);
        assert_eq!(text(&s, Side::Right), "a\r\nnew\r\nb\r\n");
    }

    #[test]
    fn typing_splitting_and_joining_lines() {
        let mut s = session("x\n", "hello world\r\nend\r\n");
        let pos = |line, col| Pos { line, col };
        // Enter in the middle of a line.
        s.replace(Side::Right, pos(0, 0), pos(0, u32::MAX), "hello\n world");
        assert_eq!(text(&s, Side::Right), "hello\r\n world\r\nend\r\n");
        // Backspace at the start of line 1 joins it to line 0.
        s.replace(Side::Right, pos(0, u32::MAX), pos(1, 0), "");
        assert_eq!(text(&s, Side::Right), "hello world\r\nend\r\n");
        // A line typed into a filler row.
        let row = s.diff().rows.len() as u32;
        s.insert_line(Side::Left, row, "typed");
        assert_eq!(text(&s, Side::Left), "x\ntyped\n");
        assert_eq!(s.row_of_line(Side::Left, 1), 1);
    }

    #[test]
    fn inserting_after_a_last_line_without_terminator() {
        let mut s = session("a", "a");
        s.insert_line(Side::Left, 1, "b");
        assert_eq!(text(&s, Side::Left), "a\nb");
        let mut s = session("a\nb", "a");
        let section = s.diff().sections[0].clone();
        s.copy_rows(section.start..section.end, Side::Right);
        assert_eq!(text(&s, Side::Right), "a\nb");
    }

    #[test]
    fn rows_and_lines() {
        let s = session("a\nb\nc\n", "a\nc\n");
        // Rows: a=a, b|filler, c=c.
        assert_eq!(s.line_at_row(Side::Right, 1), 1);
        assert_eq!(s.row_of_line(Side::Left, 2), 2);
        assert_eq!(s.row_of_line(Side::Right, 1), 2);
        assert_eq!(s.row_of_line(Side::Right, 9), 2);
    }

    /// Deleting, replacing and inserting lines, against a model of the file as
    /// a list of lines and whether it ends with a line break.
    #[test]
    fn replacing_rows_of_random_files() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n.max(1)
        };
        let render = |lines: &[String], final_nl: bool| {
            let mut t = lines.join("\n");
            if final_nl && !lines.is_empty() {
                t.push('\n');
            }
            t
        };
        for _ in 0..2000 {
            let mut lines: Vec<String> = (0..next(6)).map(|i| format!("l{i}")).collect();
            let final_nl = next(2) == 0;
            let start = render(&lines, final_nl);
            // The same text on both sides: row i shows line i.
            let mut s = session(&start, &start);
            let n = lines.len() as u64;
            let a = next(n + 1) as usize;
            let b = (a + next(3) as usize).min(lines.len());
            let paste: Vec<String> = (0..next(3)).map(|i| format!("p{i}")).collect();
            let clip = paste.iter().map(|l| format!("{l}\n")).collect::<String>();
            let clip = if next(2) == 0 { clip.trim_end_matches('\n').to_string() } else { clip };
            let replace = next(2) == 0;
            let edited = s.replace_rows(Side::Left, a as u32..b as u32, &clip, replace);
            // The model: replace lines a..b (or insert before a).
            let mut want_final = final_nl;
            if replace && a < b {
                lines.splice(a..b, paste.clone());
            } else {
                for (k, l) in paste.iter().enumerate() {
                    lines.insert(a + k, l.clone());
                }
            }
            if lines.is_empty() {
                want_final = false;
            } else if start.is_empty() {
                want_final = clip.ends_with('\n') || paste.len() > 1;
            }
            let want = render(&lines, want_final);
            if edited.is_none() {
                assert_eq!(text(&s, Side::Left), start);
                assert!(paste.is_empty() && (!replace || a == b), "{start:?} {a}..{b} {clip:?} {replace}");
                continue;
            }
            if start.is_empty() {
                // An empty file takes the pasted text as it is.
                assert_eq!(text(&s, Side::Left).trim_end_matches('\n'), want.trim_end_matches('\n'));
            } else {
                assert_eq!(text(&s, Side::Left), want, "{start:?} rows {a}..{b} {clip:?} replace {replace}");
            }
            s.undo();
            assert_eq!(text(&s, Side::Left), start);
        }
    }

    #[test]
    fn converting_line_endings() {
        let mut s = session("a\r\nb\r\nc", "a\nb\nc");
        assert!(s.set_line_endings(Side::Right, "\n").is_none(), "already LF");
        s.set_line_endings(Side::Left, "\n").unwrap();
        assert_eq!(text(&s, Side::Left), "a\nb\nc");
        assert_eq!(s.doc(Side::Left).newline, "\n", "new lines follow");
        assert!(s.diff().sections.is_empty());
        s.undo();
        assert_eq!(text(&s, Side::Left), "a\r\nb\r\nc");
        s.set_line_endings(Side::Right, "\r\n").unwrap();
        assert_eq!(text(&s, Side::Right), "a\r\nb\r\nc");
    }

    #[test]
    fn copying_rows_as_text() {
        let s = session("a\nb\nc", "a\nb\nc");
        assert_eq!(s.rows_text(Side::Left, 1..3), "b\nc\n");
        assert_eq!(s.rows_text(Side::Right, 3..9), "");
    }
}
