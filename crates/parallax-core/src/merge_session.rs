//! A 3-way merge being resolved: base, local and remote, and the result, which
//! starts as the automatic merge and is edited by choosing a side for each conflict
//! or by typing.
//!
//! Every block of the merge keeps how many lines of the result it holds, so the
//! view can show which block a result line comes from, and choosing a side replaces
//! exactly that block's lines. Edits are splices of the result text, as in
//! [`crate::edit`]; a block an edit touches counts as edited by hand, and a conflict
//! whose markers are gone counts as resolved.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::edit::Pos;
use crate::merge::{self, Block, BlockKind, MergeOptions};
use crate::text::{self, TextDoc};

/// What a block of the result holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Choice {
    /// Merged automatically (not a conflict).
    Auto,
    /// A conflict, with markers.
    Unresolved,
    Local,
    Remote,
    /// Both, local first.
    LocalRemote,
    /// Both, remote first.
    RemoteLocal,
    Base,
    /// Changed by typing.
    Edited,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockState {
    choice: Choice,
    /// Lines of the result this block holds.
    lines: u32,
}

struct Step {
    at: usize,
    removed: String,
    inserted: String,
    before: Vec<BlockState>,
    after: Vec<BlockState>,
}

/// One row of the inputs pane: the lines of base, local and remote side by side.
/// Within a block, rows pair the n-th lines of each side.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct InputRow {
    #[serde(rename = "k")]
    pub kind: BlockKind,
    /// Index of the block.
    #[serde(rename = "i")]
    pub block: u32,
    #[serde(rename = "b")]
    pub base: Option<u32>,
    #[serde(rename = "l")]
    pub local: Option<u32>,
    #[serde(rename = "r")]
    pub remote: Option<u32>,
}

/// One row of the result pane: a line, or the header above a conflict's lines.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ResultRow {
    #[serde(rename = "i")]
    pub block: u32,
    #[serde(rename = "c")]
    pub choice: Choice,
    #[serde(rename = "k")]
    pub kind: BlockKind,
    /// The result line; `None` for a conflict's header row.
    #[serde(rename = "n")]
    pub line: Option<u32>,
}

/// A conflict and where it is in both panes.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictInfo {
    pub block: u32,
    pub choice: Choice,
    pub input_row: u32,
    pub result_row: u32,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeStats {
    /// Blocks merged without asking (changed on one side, or the same way on both).
    pub automatic: u32,
    pub conflicts: u32,
    pub unresolved: u32,
}

pub struct MergeSession {
    pub base: TextDoc,
    pub local: TextDoc,
    pub remote: TextDoc,
    result: TextDoc,
    blocks: Vec<Block>,
    state: Vec<BlockState>,
    options: MergeOptions,
    /// First input row of each block, and the total at the end.
    input_starts: Vec<u32>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    saved: u64,
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Lines with their terminators, as the merge compares them.
fn full_lines(doc: &TextDoc) -> Vec<&str> {
    merge::split_lines(&doc.text)
}

fn count_lines(text: &str) -> u32 {
    merge::split_lines(text).len() as u32
}

/// Joins parts of lines so that each part but the last ends with a terminator.
fn join(parts: &[&[&str]], newline: &str) -> String {
    let mut out = String::new();
    for part in parts.iter().filter(|p| !p.is_empty()) {
        if !out.is_empty() && !out.ends_with(['\n', '\r']) {
            out.push_str(newline);
        }
        part.iter().for_each(|l| out.push_str(l));
    }
    out
}

impl MergeSession {
    /// Merges the three texts. The result is saved like `local`: its encoding, BOM
    /// and line terminator for new lines.
    pub fn new(base: TextDoc, local: TextDoc, remote: TextDoc, options: MergeOptions) -> Self {
        let blocks = {
            let (b, l, r) = (full_lines(&base), full_lines(&local), full_lines(&remote));
            merge::merge_lines(&b, &l, &r, &options)
        };
        let mut session = MergeSession {
            result: TextDoc::empty_like(&local),
            base,
            local,
            remote,
            state: Vec::with_capacity(blocks.len()),
            input_starts: Vec::with_capacity(blocks.len() + 1),
            blocks,
            options,
            undo: Vec::new(),
            redo: Vec::new(),
            saved: 0,
        };
        let mut text = String::new();
        let mut row = 0;
        for i in 0..session.blocks.len() {
            let choice = if session.blocks[i].kind == BlockKind::Conflict { Choice::Unresolved } else { Choice::Auto };
            let content = session.content(i, choice);
            if !text.is_empty() && !text.ends_with(['\n', '\r']) && !content.is_empty() {
                text.push_str(session.local.newline);
            }
            text.push_str(&content);
            session.state.push(BlockState { choice, lines: count_lines(&content) });
            session.input_starts.push(row);
            let b = &session.blocks[i];
            row += b.base.len().max(b.local.len()).max(b.remote.len()) as u32;
        }
        session.input_starts.push(row);
        session.set_result_text(text);
        session.saved = hash(&session.result.text);
        session
    }

    fn set_result_text(&mut self, text: String) {
        let len = self.result.text.len();
        self.result.splice(0..len, &text);
    }

    pub fn result(&self) -> &TextDoc {
        &self.result
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    pub fn choice(&self, block: usize) -> Choice {
        self.state[block].choice
    }

    pub fn options(&self) -> &MergeOptions {
        &self.options
    }

    /// The text a block holds for a choice. Unresolved conflicts get Git's markers.
    fn content(&self, i: usize, choice: Choice) -> String {
        let block = &self.blocks[i];
        let (b, l, r) = (full_lines(&self.base), full_lines(&self.local), full_lines(&self.remote));
        fn part<'a>(lines: &[&'a str], range: &Range<u32>) -> Vec<&'a str> {
            lines[range.start as usize..range.end as usize].to_vec()
        }
        let (bp, lp, rp) = (part(&b, &block.base), part(&l, &block.local), part(&r, &block.remote));
        let nl = self.local.newline;
        match choice {
            Choice::Auto => match block.merged() {
                Some((merge::Side3::Remote, _)) => join(&[&rp], nl),
                _ => join(&[&lp], nl),
            },
            Choice::Unresolved => merge::conflict_text(block, &b, &l, &r, &self.options),
            Choice::Local => join(&[&lp], nl),
            Choice::Remote => join(&[&rp], nl),
            Choice::LocalRemote => join(&[&lp, &rp], nl),
            Choice::RemoteLocal => join(&[&rp, &lp], nl),
            Choice::Base => join(&[&bp], nl),
            Choice::Edited => self.block_text(i),
        }
    }

    /// The result lines of each block.
    fn result_ranges(&self) -> Vec<Range<u32>> {
        let mut at = 0;
        self.state
            .iter()
            .map(|s| {
                let r = at..at + s.lines;
                at += s.lines;
                r
            })
            .collect()
    }

    fn byte_range(&self, lines: &Range<u32>) -> Range<usize> {
        self.result.line_start(lines.start as usize)..self.result.line_start(lines.end as usize)
    }

    fn block_text(&self, i: usize) -> String {
        let range = self.result_ranges()[i].clone();
        self.result.text[self.byte_range(&range)].to_string()
    }

    pub fn stats(&self) -> MergeStats {
        let mut s = MergeStats::default();
        for (block, state) in self.blocks.iter().zip(&self.state) {
            match block.kind {
                BlockKind::Conflict => {
                    s.conflicts += 1;
                    if state.choice == Choice::Unresolved {
                        s.unresolved += 1;
                    }
                }
                BlockKind::Local | BlockKind::Remote | BlockKind::Same => s.automatic += 1,
                BlockKind::Unchanged => {}
            }
        }
        s
    }

    /// Whether the result can be saved as merged: no unresolved conflict, and no
    /// conflict markers typed or pasted anywhere.
    pub fn is_clean(&self) -> bool {
        self.stats().unresolved == 0 && !merge::has_conflict_markers(&self.result.text)
    }

    pub fn is_dirty(&self) -> bool {
        hash(&self.result.text) != self.saved
    }

    pub fn mark_saved(&mut self) {
        self.saved = hash(&self.result.text);
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Puts a choice into a conflict (or any block). Returns the first result line changed.
    pub fn resolve(&mut self, block: usize, choice: Choice) -> Option<usize> {
        if block >= self.blocks.len() || choice == Choice::Edited {
            return None;
        }
        let choice = if choice == Choice::Auto && self.blocks[block].kind == BlockKind::Conflict {
            Choice::Unresolved
        } else {
            choice
        };
        let lines = self.result_ranges()[block].clone();
        let mut content = self.content(block, choice);
        // A block before the end ends with a terminator, or it would join the next line;
        // after a last line without one, it starts on a line of its own.
        let count = count_lines(&content);
        let bytes = self.byte_range(&lines);
        if bytes.end < self.result.text.len() && !content.is_empty() && !content.ends_with(['\n', '\r']) {
            content.push_str(self.local.newline);
        }
        if bytes.start > 0 && !content.is_empty() && !self.result.text[..bytes.start].ends_with(['\n', '\r']) {
            content.insert_str(0, self.local.newline);
        }
        let before = self.state.clone();
        self.state[block] = BlockState { choice, lines: count };
        let after = self.state.clone();
        let range = self.byte_range(&lines);
        self.splice(range, content, before, after);
        Some(lines.start as usize)
    }

    /// Replaces the text between two positions of the result; `\n` starts new lines.
    /// Returns the first line changed.
    pub fn replace(&mut self, from: Pos, to: Pos, text: &str) -> usize {
        let a = self.result.offset(from.line as usize, from.col);
        let b = self.result.offset(to.line as usize, to.col).max(a);
        let text = if text.contains(['\n', '\r']) {
            text.replace("\r\n", "\n").replace('\r', "\n").replace('\n', self.result.newline)
        } else {
            text.to_string()
        };
        let old_total = count_lines(&self.result.text);
        let first = from.line.min(to.line).min(old_total);
        let last = to.line.max(from.line).min(old_total.saturating_sub(1)).max(first);
        // Lines first..=last are rewritten into `rewritten` lines; the block that
        // holds `first` takes them, and the others lose what they had in that span.
        let span = first..(last + 1).min(old_total).max(first);
        let mut probe = self.result.text.clone();
        probe.replace_range(a..b, &text);
        let rewritten = (count_lines(&probe) as i64 - (old_total as i64 - span.len() as i64)).max(0) as u32;
        let before = self.state.clone();
        let ranges = self.result_ranges();
        let holder = ranges
            .iter()
            .position(|r| r.contains(&first))
            .or_else(|| ranges.iter().rposition(|r| r.start <= first))
            .unwrap_or(0);
        let mut after = before.clone();
        for (i, r) in ranges.iter().enumerate() {
            let overlap = r.end.min(span.end).saturating_sub(r.start.max(span.start));
            if overlap > 0 || i == holder {
                after[i].choice = Choice::Edited;
            }
            after[i].lines -= overlap;
        }
        if let Some(s) = after.get_mut(holder) {
            s.lines += rewritten;
        }
        self.splice(a..b, text, before, after);
        // A conflict that still has its markers is still unresolved.
        let ranges = self.result_ranges();
        for (i, r) in ranges.iter().enumerate() {
            if self.state[i].choice == Choice::Edited
                && self.blocks[i].kind == BlockKind::Conflict
                && merge::has_conflict_markers(&self.result.text[self.byte_range(r)])
            {
                self.state[i].choice = Choice::Unresolved;
                if let Some(step) = self.undo.last_mut() {
                    step.after = self.state.clone();
                }
            }
        }
        first as usize
    }

    /// Replaces the whole result, e.g. with the file Git wrote. Every block then
    /// counts as edited; conflicts with markers left in the text stay unresolved
    /// for saving through [`MergeSession::is_clean`].
    pub fn replace_all(&mut self, text: &str) {
        let before = self.state.clone();
        let mut after: Vec<BlockState> =
            before.iter().map(|_| BlockState { choice: Choice::Edited, lines: 0 }).collect();
        if let Some(first) = after.first_mut() {
            first.lines = count_lines(text);
        }
        let len = self.result.text.len();
        self.splice(0..len, text.to_string(), before, after);
    }

    fn splice(&mut self, range: Range<usize>, inserted: String, before: Vec<BlockState>, after: Vec<BlockState>) {
        let at = range.start;
        let removed = self.result.splice(range, &inserted);
        self.state = after.clone();
        self.undo.push(Step { at, removed, inserted, before, after });
        self.redo.clear();
    }

    /// Undoes the last choice or edit; returns the first line changed.
    pub fn undo(&mut self) -> Option<usize> {
        let step = self.undo.pop()?;
        self.result.splice(step.at..step.at + step.inserted.len(), &step.removed);
        self.state = step.before.clone();
        let line = self.line_of(step.at);
        self.redo.push(step);
        Some(line)
    }

    pub fn redo(&mut self) -> Option<usize> {
        let step = self.redo.pop()?;
        self.result.splice(step.at..step.at + step.removed.len(), &step.inserted);
        self.state = step.after.clone();
        let line = self.line_of(step.at);
        self.undo.push(step);
        Some(line)
    }

    fn line_of(&self, byte: usize) -> usize {
        self.result.lines.partition_point(|r| r.end < byte)
    }

    // ---- rows for the view ----

    pub fn input_row_count(&self) -> u32 {
        *self.input_starts.last().unwrap_or(&0)
    }

    pub fn input_rows(&self, start: u32, count: u32) -> Vec<InputRow> {
        let end = (start + count).min(self.input_row_count());
        let mut out = Vec::with_capacity((end.saturating_sub(start)) as usize);
        let mut i = self.input_starts.partition_point(|&s| s <= start).saturating_sub(1);
        let mut row = start;
        while row < end && i < self.blocks.len() {
            let block = &self.blocks[i];
            let offset = row - self.input_starts[i];
            let height = self.input_starts[i + 1] - self.input_starts[i];
            if offset >= height {
                i += 1;
                continue;
            }
            let at = |r: &Range<u32>| (offset < r.len() as u32).then_some(r.start + offset);
            out.push(InputRow {
                kind: block.kind,
                block: i as u32,
                base: at(&block.base),
                local: at(&block.local),
                remote: at(&block.remote),
            });
            row += 1;
        }
        out
    }

    /// Result rows: each conflict has a header row above its lines.
    fn result_layout(&self) -> Vec<(u32, Range<u32>, bool)> {
        self.result_ranges()
            .into_iter()
            .enumerate()
            .map(|(i, r)| (i as u32, r, self.blocks[i].kind == BlockKind::Conflict))
            .collect()
    }

    pub fn result_row_count(&self) -> u32 {
        self.result.len() as u32 + self.stats().conflicts
    }

    pub fn result_rows(&self, start: u32, count: u32) -> Vec<ResultRow> {
        let end = start.saturating_add(count);
        let mut out = Vec::new();
        let mut row = 0u32;
        for (i, lines, header) in self.result_layout() {
            let height = lines.len() as u32 + header as u32;
            if row + height <= start {
                row += height;
                continue;
            }
            if row >= end {
                break;
            }
            let (kind, choice) = (self.blocks[i as usize].kind, self.state[i as usize].choice);
            if header {
                if row >= start {
                    out.push(ResultRow { block: i, choice, kind, line: None });
                }
                row += 1;
            }
            for n in lines {
                if row >= start && row < end {
                    out.push(ResultRow { block: i, choice, kind, line: Some(n) });
                }
                row += 1;
            }
        }
        out
    }

    /// The conflicts with their rows in both panes.
    pub fn conflicts(&self) -> Vec<ConflictInfo> {
        let mut out = Vec::new();
        let mut row = 0u32;
        for (i, lines, header) in self.result_layout() {
            if header {
                out.push(ConflictInfo {
                    block: i,
                    choice: self.state[i as usize].choice,
                    input_row: self.input_starts[i as usize],
                    result_row: row,
                });
            }
            row += lines.len() as u32 + header as u32;
        }
        out
    }

    /// The result row of a result line, and the first result row of a block.
    pub fn result_row_of_block(&self, block: usize) -> u32 {
        let mut row = 0u32;
        for (i, lines, header) in self.result_layout() {
            if i as usize == block {
                return row;
            }
            row += lines.len() as u32 + header as u32;
        }
        row
    }

    pub fn input_row_of_block(&self, block: usize) -> u32 {
        self.input_starts.get(block).copied().unwrap_or(0)
    }

    /// The text with Git's markers style for the result as saved now.
    pub fn result_text(&self) -> &str {
        &self.result.text
    }
}

/// Decodes the three inputs of a merge.
pub fn decode3(base: &[u8], local: &[u8], remote: &[u8]) -> (TextDoc, TextDoc, TextDoc) {
    (text::decode(base), text::decode(local), text::decode(remote))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(base: &str, local: &str, remote: &str) -> MergeSession {
        let (b, l, r) = decode3(base.as_bytes(), local.as_bytes(), remote.as_bytes());
        let options =
            MergeOptions { labels: [Some("L".into()), Some("B".into()), Some("R".into())], ..Default::default() };
        MergeSession::new(b, l, r, options)
    }

    const BASE: &str = "a\nb\nc\nd\ne\nf\ng\n";
    const LOCAL: &str = "a\nb\nC\nd\ne\nF1\ng\n";
    const REMOTE: &str = "A\nb\nc\nd\ne\nF2\ng\n";

    fn conflict(s: &MergeSession) -> usize {
        s.conflicts()[0].block as usize
    }

    #[test]
    fn starts_as_the_automatic_merge() {
        let s = session(BASE, LOCAL, REMOTE);
        assert_eq!(s.result_text(), "A\nb\nC\nd\ne\n<<<<<<< L\nF1\n=======\nF2\n>>>>>>> R\ng\n");
        assert_eq!(s.stats().conflicts, 1);
        assert_eq!(s.stats().unresolved, 1);
        assert!(!s.is_clean() && !s.is_dirty());
        // Two blocks changed on one side each.
        assert_eq!(s.stats().automatic, 2);
    }

    #[test]
    fn choices_and_undo() {
        let mut s = session(BASE, LOCAL, REMOTE);
        let c = conflict(&s);
        for (choice, text) in [
            (Choice::Local, "F1\n"),
            (Choice::Remote, "F2\n"),
            (Choice::LocalRemote, "F1\nF2\n"),
            (Choice::RemoteLocal, "F2\nF1\n"),
            (Choice::Base, "f\n"),
        ] {
            s.resolve(c, choice);
            assert_eq!(s.result_text(), format!("A\nb\nC\nd\ne\n{text}g\n"), "{choice:?}");
            assert!(s.is_clean());
        }
        s.resolve(c, Choice::Auto);
        assert!(!s.is_clean());
        for _ in 0..6 {
            s.undo();
        }
        assert_eq!(s.choice(c), Choice::Unresolved);
        s.redo();
        assert_eq!(s.choice(c), Choice::Local);
        assert!(s.is_dirty());
    }

    #[test]
    fn typing_resolves_and_keeps_blocks_in_place() {
        let mut s = session(BASE, LOCAL, REMOTE);
        let c = conflict(&s);
        s.resolve(c, Choice::LocalRemote);
        // Line 5 is "F1", 6 is "F2": type into F1.
        s.replace(Pos { line: 5, col: 2 }, Pos { line: 5, col: 2 }, "x\ny");
        assert_eq!(s.result_text(), "A\nb\nC\nd\ne\nF1x\nyF2\ng\n".replace("yF2", "y\nF2").as_str());
        assert_eq!(s.choice(c), Choice::Edited);
        // The rows stay consistent: one header plus the lines of the conflict.
        let rows = s.result_rows(0, 100);
        assert_eq!(rows.len() as u32, s.result_row_count());
        assert_eq!(rows.iter().filter(|r| r.line.is_none()).count(), 1);
        // Editing inside an unresolved conflict keeps it unresolved while markers remain.
        let mut s = session(BASE, LOCAL, REMOTE);
        s.replace(Pos { line: 6, col: 0 }, Pos { line: 6, col: 2 }, "F1 and F2");
        assert_eq!(s.choice(conflict(&s)), Choice::Unresolved);
        assert!(!s.is_clean());
    }

    #[test]
    fn input_rows_align_blocks() {
        let s = session("a\nb\n", "a\nx\ny\nb\n", "a\nb\nz\n");
        let rows = s.input_rows(0, 100);
        assert_eq!(rows.len() as u32, s.input_row_count());
        // a, then x/y inserted locally (two rows, base and remote empty), b, z.
        assert_eq!(
            rows.iter().map(|r| (r.base, r.local, r.remote)).collect::<Vec<_>>(),
            vec![
                (Some(0), Some(0), Some(0)),
                (None, Some(1), None),
                (None, Some(2), None),
                (Some(1), Some(3), Some(1)),
                (None, None, Some(2)),
            ]
        );
        assert_eq!(s.input_rows(3, 1)[0].local, Some(3));
    }

    #[test]
    fn crlf_and_missing_newlines() {
        let mut s = session("a\r\nb", "a\r\nL", "a\r\nR");
        let c = conflict(&s);
        s.resolve(c, Choice::LocalRemote);
        assert_eq!(s.result_text(), "a\r\nL\r\nR");
    }

    /// Random choices, edits, undo and redo keep the block line counts equal to the
    /// result's lines, and undoing everything gives back the automatic merge.
    #[test]
    fn random_operations_stay_consistent() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n.max(1)
        };
        for _ in 0..300 {
            let mut s = session(BASE, LOCAL, REMOTE);
            let start = s.result_text().to_string();
            let mut log = Vec::new();
            for _ in 0..8 {
                let lines = s.result().len() as u64;
                match next(4) {
                    0 => {
                        let choices = [Choice::Local, Choice::Remote, Choice::LocalRemote, Choice::Base, Choice::Auto];
                        let (b, c) = (next(s.blocks().len() as u64) as usize, choices[next(5) as usize]);
                        log.push(format!("resolve {b} {c:?}"));
                        s.resolve(b, c);
                    }
                    1 => {
                        let (a, b) = (next(lines + 1) as u32, next(lines + 1) as u32);
                        let texts = ["", "x", "y\n", "\nz\n", "<<<<<<< q\n"];
                        let (from, to) =
                            (Pos { line: a.min(b), col: next(3) as u32 }, Pos { line: a.max(b), col: next(3) as u32 });
                        let t = texts[next(5) as usize];
                        log.push(format!("replace {from:?} {to:?} {t:?} in {:?}", s.result_text()));
                        s.replace(from, to, t);
                    }
                    2 => {
                        log.push("undo".into());
                        s.undo();
                    }
                    _ => {
                        log.push("redo".into());
                        s.redo();
                    }
                }
                let total: u32 = s.state.iter().map(|b| b.lines).sum();
                assert_eq!(
                    total as usize,
                    merge::split_lines(s.result_text()).len(),
                    "{:?}\n{}",
                    s.result_text(),
                    log.join("\n")
                );
                assert_eq!(s.result_rows(0, u32::MAX).len() as u32, s.result_row_count());
            }
            while s.undo().is_some() {}
            assert_eq!(s.result_text(), start);
        }
    }
}
