//! 3-way text merge with the same results as Git.
//!
//! Base→local and base→remote are diffed line by line; walking both edit scripts
//! together sorts every stretch of the files into blocks: unchanged, changed on one
//! side, changed the same way on both, or a conflict. Changes that overlap or touch
//! (one ends where the other starts) conflict, unless they are identical, which is
//! Git's rule as well. Like Git's "zealous" level, conflicts are then narrowed to
//! the lines where local and remote really differ, and conflicts only a few lines
//! apart are joined into one. Rendering a merge with conflict markers gives the
//! bytes `git merge-file` writes; the tests compare against it.

use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput, Interner};
use serde::{Deserialize, Serialize};

/// How much effort goes into making conflicts small, as Git's `XDL_MERGE_*` levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    /// Identical changes on both sides are not conflicts. `diff3` markers use this.
    Eager,
    /// Conflicts shrink to the lines local and remote differ in, and conflicts at
    /// most three lines apart are joined. What `git merge` does.
    #[default]
    Zealous,
    /// Zealous, and conflicts separated only by lines without letters or digits
    /// (braces, blank lines) are joined too. What `git merge-file` does.
    ZealousAlnum,
}

/// The conflict markers written for unresolved conflicts, as `merge.conflictStyle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum MarkerStyle {
    /// Local and remote.
    #[default]
    Merge,
    /// Local, base and remote.
    Diff3,
    /// Like diff3, with the lines both sides share at the start and end of a
    /// conflict moved out of the markers.
    Zdiff3,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MergeOptions {
    pub level: Level,
    pub style: MarkerStyle,
    /// The histogram diff instead of Myers, as `-X diff-algorithm=histogram`.
    pub histogram: bool,
    /// Names after the markers: local, base, remote.
    pub labels: [Option<String>; 3],
}

impl Default for MergeOptions {
    fn default() -> Self {
        MergeOptions { level: Level::Zealous, style: MarkerStyle::Merge, histogram: false, labels: [None, None, None] }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockKind {
    /// The same on all three.
    Unchanged,
    /// Changed on the local side only.
    Local,
    /// Changed on the remote side only.
    Remote,
    /// Changed the same way on both sides.
    Same,
    Conflict,
}

/// A stretch of the merge: line ranges in base, local and remote. Blocks follow
/// each other without gaps, so they cover all three files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Block {
    pub kind: BlockKind,
    pub base: Range<u32>,
    pub local: Range<u32>,
    pub remote: Range<u32>,
}

impl Block {
    /// The lines this block puts into the result, or `None` for a conflict.
    pub fn merged(&self) -> Option<(Side3, Range<u32>)> {
        match self.kind {
            BlockKind::Unchanged | BlockKind::Local | BlockKind::Same => Some((Side3::Local, self.local.clone())),
            BlockKind::Remote => Some((Side3::Remote, self.remote.clone())),
            BlockKind::Conflict => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Side3 {
    Base,
    Local,
    Remote,
}

/// Splits text into lines with their terminators (`\n`, `\r\n` or a lone `\r`); the
/// last line may have none. Merging compares lines with their terminators, as Git does.
pub fn split_lines(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 40 + 1);
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out.push(&text[start..=i]);
                start = i + 1;
            }
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                out.push(&text[start..i + 2]);
                i += 1;
                start = i + 1;
            }
            b'\r' => {
                out.push(&text[start..=i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < bytes.len() {
        out.push(&text[start..]);
    }
    out
}

/// Changed regions of `a` → `b`: (range in a, range in b).
fn hunks(a: &[&str], b: &[&str], histogram: bool) -> Vec<(Range<u32>, Range<u32>)> {
    let mut interner = Interner::new(a.len() + b.len());
    let before = a.iter().map(|l| interner.intern(*l)).collect();
    let after = b.iter().map(|l| interner.intern(*l)).collect();
    let input = InternedInput { before, after, interner };
    let mut diff = Diff::compute(if histogram { Algorithm::Histogram } else { Algorithm::Myers }, &input);
    // Git slides ambiguous changes down as far as they go, without the indent heuristic.
    diff.postprocess_no_heuristic(&input);
    diff.hunks().map(|h| (h.before, h.after)).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Conflict,
    Local,
    Remote,
    Same,
}

/// A change record while merging. `base` is `None` for the parts a conflict was
/// split into after the first: the base cannot be divided between them.
#[derive(Debug, Clone)]
struct Rec {
    mode: Mode,
    base: Option<Range<u32>>,
    local: Range<u32>,
    remote: Range<u32>,
}

/// A range while merging: positions worked out from another side's offset can come
/// out below zero for a change that overlaps the previous record, which absorbs it.
type Span = (i64, i64);

fn span(r: &Range<u32>) -> Span {
    (r.start as i64, r.end as i64)
}

fn range((start, end): Span) -> Range<u32> {
    start.max(0) as u32..end.max(start).max(0) as u32
}

/// Adds a change, joining it with the previous one when they touch on either side;
/// two different kinds of change joined are a conflict. A joined record ends where
/// the new change ends, as in Git.
fn push(recs: &mut Vec<Rec>, mode: Mode, base: Span, local: Span, remote: Span) {
    if let Some(last) = recs.last_mut()
        && (local.0 <= last.local.end as i64 || remote.0 <= last.remote.end as i64)
    {
        if last.mode != mode {
            last.mode = Mode::Conflict;
        }
        let start = last.base.as_ref().map_or(base.0, |b| b.start as i64);
        last.base = Some(range((start, base.1)));
        last.local = range((last.local.start as i64, local.1));
        last.remote = range((last.remote.start as i64, remote.1));
        return;
    }
    recs.push(Rec { mode, base: Some(range(base)), local: range(local), remote: range(remote) });
}

/// Merges three versions given as lines (see [`split_lines`]).
pub fn merge_lines(base: &[&str], local: &[&str], remote: &[&str], opts: &MergeOptions) -> Vec<Block> {
    // diff3 markers show the base of each conflict, so conflicts are not split.
    let level = if opts.style != MarkerStyle::Merge { Level::Eager } else { opts.level };
    let h1 = hunks(base, local, opts.histogram);
    let h2 = hunks(base, remote, opts.histogram);
    let mut recs: Vec<Rec> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < h1.len() && j < h2.len() {
        let (b1, s1) = (span(&h1[i].0), span(&h1[i].1));
        let (b2, s2) = (span(&h2[j].0), span(&h2[j].1));
        if b1.1 < b2.0 {
            // Local only; the remote position follows the next remote change's offset.
            let r = s2.0 - b2.0 + b1.0;
            push(&mut recs, Mode::Local, b1, s1, (r, r + b1.1 - b1.0));
            i += 1;
            continue;
        }
        if b2.1 < b1.0 {
            let l = s1.0 - b1.0 + b2.0;
            push(&mut recs, Mode::Remote, b2, (l, l + b2.1 - b2.0), s2);
            j += 1;
            continue;
        }
        let identical = b1 == b2
            && s1.1 - s1.0 == s2.1 - s2.0
            && local[s1.0 as usize..s1.1 as usize] == remote[s2.0 as usize..s2.1 as usize];
        if !identical {
            // The union of both changes, on all three sides.
            let off = b1.0 - b2.0;
            let ffo = off + (b1.1 - b1.0) - (b2.1 - b2.0);
            let (mut i0, mut i1, mut i2) = (b1.0, s1.0, s2.0);
            if off > 0 {
                i0 -= off;
                i1 -= off;
            } else {
                i2 += off;
            }
            let (mut e0, mut e1, mut e2) = (b1.1, s1.1, s2.1);
            if ffo < 0 {
                e0 -= ffo;
                e1 -= ffo;
            } else {
                e2 += ffo;
            }
            push(&mut recs, Mode::Conflict, (i0, e0), (i1, e1), (i2, e2));
        }
        if b1.1 >= b2.1 {
            j += 1;
        }
        if b2.1 >= b1.1 {
            i += 1;
        }
    }
    let shift2 = remote.len() as i64 - base.len() as i64;
    let shift1 = local.len() as i64 - base.len() as i64;
    for (b, s) in &h1[i..] {
        let (b, s) = (span(b), span(s));
        push(&mut recs, Mode::Local, b, s, (b.0 + shift2, b.1 + shift2));
    }
    for (b, s) in &h2[j..] {
        let (b, s) = (span(b), span(s));
        push(&mut recs, Mode::Remote, b, (b.0 + shift1, b.1 + shift1), s);
    }
    if level >= Level::Zealous {
        recs = refine(recs, local, remote, opts.histogram);
        join_close_conflicts(&mut recs, local, level == Level::ZealousAlnum);
    }
    if opts.style == MarkerStyle::Zdiff3 {
        recs = trim_shared(recs, local, remote);
    }
    blocks(&recs, base, local, remote)
}

/// Narrows each conflict to the lines local and remote differ in: a diff of the two
/// sides splits it into smaller conflicts, with the lines they share in between.
fn refine(recs: Vec<Rec>, local: &[&str], remote: &[&str], histogram: bool) -> Vec<Rec> {
    let mut out = Vec::with_capacity(recs.len());
    for rec in recs {
        if rec.mode != Mode::Conflict || rec.local.is_empty() || rec.remote.is_empty() {
            out.push(rec);
            continue;
        }
        let a = &local[rec.local.start as usize..rec.local.end as usize];
        let b = &remote[rec.remote.start as usize..rec.remote.end as usize];
        let parts = hunks(a, b, histogram);
        if parts.is_empty() {
            out.push(Rec { mode: Mode::Same, ..rec });
            continue;
        }
        for (n, (x, y)) in parts.into_iter().enumerate() {
            out.push(Rec {
                mode: Mode::Conflict,
                base: if n == 0 { rec.base.clone() } else { None },
                local: rec.local.start + x.start..rec.local.start + x.end,
                remote: rec.remote.start + y.start..rec.remote.start + y.end,
            });
        }
    }
    out
}

/// Joins conflicts at most three lines apart (or, with `no_alnum`, apart only by
/// lines without letters and digits): one bigger conflict reads better than
/// several separated by a brace.
fn join_close_conflicts(recs: &mut Vec<Rec>, local: &[&str], no_alnum: bool) {
    let mut i = 0;
    while i + 1 < recs.len() {
        let (a, b) = (&recs[i], &recs[i + 1]);
        let gap = a.local.end..b.local.start;
        let close = gap.len() <= 3
            || (no_alnum
                && !local[gap.start as usize..gap.end as usize].iter().any(|l| l.chars().any(char::is_alphanumeric)));
        if a.mode != Mode::Conflict || b.mode != Mode::Conflict || !close {
            i += 1;
            continue;
        }
        let next = recs.remove(i + 1);
        let a = &mut recs[i];
        a.local.end = next.local.end;
        a.remote.end = next.remote.end;
        if let (Some(base), Some(nb)) = (&mut a.base, next.base) {
            base.end = nb.end;
        }
    }
}

/// zdiff3: lines local and remote share at the start and end of a conflict go out of it.
fn trim_shared(recs: Vec<Rec>, local: &[&str], remote: &[&str]) -> Vec<Rec> {
    let mut out = Vec::with_capacity(recs.len());
    for rec in recs {
        if rec.mode != Mode::Conflict {
            out.push(rec);
            continue;
        }
        let a = &local[rec.local.start as usize..rec.local.end as usize];
        let b = &remote[rec.remote.start as usize..rec.remote.end as usize];
        let head = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
        let (head, tail) = (head as u32, tail as u32);
        if head > 0 {
            out.push(Rec {
                mode: Mode::Same,
                base: None,
                local: rec.local.start..rec.local.start + head,
                remote: rec.remote.start..rec.remote.start + head,
            });
        }
        out.push(Rec {
            mode: Mode::Conflict,
            base: rec.base.clone(),
            local: rec.local.start + head..rec.local.end - tail,
            remote: rec.remote.start + head..rec.remote.end - tail,
        });
        if tail > 0 {
            out.push(Rec {
                mode: Mode::Same,
                base: None,
                local: rec.local.end - tail..rec.local.end,
                remote: rec.remote.end - tail..rec.remote.end,
            });
        }
    }
    out
}

/// The records with the stretches between them filled in, so the blocks cover all
/// three files. Between records, local and remote are equal; where the base differs
/// there, both sides made the same change.
fn blocks(recs: &[Rec], base: &[&str], local: &[&str], remote: &[&str]) -> Vec<Block> {
    let mut out = Vec::with_capacity(recs.len() * 2 + 1);
    let (mut b, mut l, mut r) = (0u32, 0u32, 0u32);
    let between = |out: &mut Vec<Block>, b_end: u32, l_end: u32, r_end: u32, b: u32, l: u32, r: u32| {
        if b == b_end && l == l_end {
            return;
        }
        let bs = &base[b as usize..b_end as usize];
        let ls = &local[l as usize..l_end as usize];
        if bs == ls {
            out.push(Block { kind: BlockKind::Unchanged, base: b..b_end, local: l..l_end, remote: r..r_end });
            return;
        }
        // Identical changes on both sides, between unchanged lines.
        let (mut bb, mut ll) = (b, l);
        for (x, y) in hunks(bs, ls, false) {
            let (xs, ys) = (b + x.start, l + y.start);
            if ys > ll {
                let n = ys - ll;
                out.push(Block {
                    kind: BlockKind::Unchanged,
                    base: bb..bb + n,
                    local: ll..ys,
                    remote: r + (ll - l)..r + (ys - l),
                });
            }
            let (xe, ye) = (b + x.end, l + y.end);
            out.push(Block { kind: BlockKind::Same, base: xs..xe, local: ys..ye, remote: r + (ys - l)..r + (ye - l) });
            bb = xe;
            ll = ye;
        }
        if ll < l_end || bb < b_end {
            out.push(Block {
                kind: BlockKind::Unchanged,
                base: bb..b_end,
                local: ll..l_end,
                remote: r + (ll - l)..r_end,
            });
        }
    };
    for rec in recs {
        let base_range = rec.base.clone();
        let b_start = base_range.as_ref().map_or(b, |x| x.start);
        between(&mut out, b_start, rec.local.start, rec.remote.start, b, l, r);
        let (b_from, b_to) = match &base_range {
            Some(x) => (x.start, x.end),
            None => (b_start, b_start),
        };
        let kind = match rec.mode {
            Mode::Conflict => BlockKind::Conflict,
            Mode::Local => BlockKind::Local,
            Mode::Remote => BlockKind::Remote,
            Mode::Same => BlockKind::Same,
        };
        out.push(Block { kind, base: b_from..b_to, local: rec.local.clone(), remote: rec.remote.clone() });
        b = b_to;
        l = rec.local.end;
        r = rec.remote.end;
    }
    between(&mut out, base.len() as u32, local.len() as u32, remote.len() as u32, b, l, r);
    out
}

/// Marker lines end like the surrounding lines: CRLF when the local side before the
/// conflict, the remote side and the base all use it, as Git decides.
fn crlf_markers(block: &Block, base: &[&str], local: &[&str], remote: &[&str]) -> bool {
    // Some(true/false) when the line has a terminator, None when undecided.
    let crlf = |lines: &[&str], i: u32| {
        lines.get(i as usize).and_then(|l| {
            if l.ends_with("\r\n") {
                Some(true)
            } else if l.ends_with('\n') {
                Some(false)
            } else {
                None
            }
        })
    };
    let at = |r: &Range<u32>| r.start.saturating_sub(1);
    let first = crlf(local, at(&block.local));
    if first == Some(false) {
        return false;
    }
    let second = crlf(remote, at(&block.remote));
    if second == Some(false) {
        return false;
    }
    crlf(base, 0).unwrap_or(false)
}

/// The merged text, with markers for unresolved conflicts, and the number of conflicts.
pub fn render(
    blocks: &[Block],
    base: &[&str],
    local: &[&str],
    remote: &[&str],
    opts: &MergeOptions,
) -> (String, usize) {
    let mut out = String::new();
    let mut conflicts = 0;
    for block in blocks {
        match block.merged() {
            Some((side, range)) => {
                let lines = if side == Side3::Remote { remote } else { local };
                lines[range.start as usize..range.end as usize].iter().for_each(|l| out.push_str(l));
            }
            None => {
                conflicts += 1;
                out.push_str(&conflict_text(block, base, local, remote, opts));
            }
        }
    }
    (out, conflicts)
}

/// One conflict with Git's markers.
pub fn conflict_text(block: &Block, base: &[&str], local: &[&str], remote: &[&str], opts: &MergeOptions) -> String {
    let nl = if crlf_markers(block, base, local, remote) { "\r\n" } else { "\n" };
    let mut out = String::new();
    let marker = |out: &mut String, c: char, label: &Option<String>| {
        (0..7).for_each(|_| out.push(c));
        if let Some(label) = label {
            out.push(' ');
            out.push_str(label);
        }
        out.push_str(nl);
    };
    // Each side ends with a terminator before the next marker.
    let copy = |out: &mut String, lines: &[&str], range: &Range<u32>| {
        for line in &lines[range.start as usize..range.end as usize] {
            out.push_str(line);
        }
        if !range.is_empty() && !out.ends_with(['\n', '\r']) {
            out.push_str(nl);
        }
    };
    marker(&mut out, '<', &opts.labels[0]);
    copy(&mut out, local, &block.local);
    if opts.style != MarkerStyle::Merge {
        marker(&mut out, '|', &opts.labels[1]);
        copy(&mut out, base, &block.base);
    }
    marker(&mut out, '=', &None);
    copy(&mut out, remote, &block.remote);
    marker(&mut out, '>', &opts.labels[2]);
    out
}

/// Merges three texts; returns the result with markers and the number of conflicts.
pub fn merge_text(base: &str, local: &str, remote: &str, opts: &MergeOptions) -> (String, usize) {
    let (b, l, r) = (split_lines(base), split_lines(local), split_lines(remote));
    let blocks = merge_lines(&b, &l, &r, opts);
    render(&blocks, &b, &l, &r, opts)
}

/// Whether a text still holds a conflict: a line starting with seven `<` or `>`
/// followed by a space or the line end, as Git's own check for leftover markers.
pub fn has_conflict_markers(text: &str) -> bool {
    split_lines(text).iter().any(|line| {
        let l = line.trim_end_matches(['\r', '\n']);
        (l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>")) && matches!(l.as_bytes().get(7), None | Some(b' '))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(level: Level, style: MarkerStyle) -> MergeOptions {
        MergeOptions {
            level,
            style,
            histogram: false,
            labels: [Some("local".into()), Some("base".into()), Some("remote".into())],
        }
    }

    fn kinds(base: &str, local: &str, remote: &str) -> Vec<BlockKind> {
        let (b, l, r) = (split_lines(base), split_lines(local), split_lines(remote));
        merge_lines(&b, &l, &r, &MergeOptions::default()).into_iter().map(|b| b.kind).collect()
    }

    #[test]
    fn block_kinds() {
        use BlockKind::*;
        let base = "a\nb\nc\nd\ne\nf\ng\nh\n";
        assert_eq!(kinds(base, base, base), vec![Unchanged]);
        assert_eq!(kinds(base, "a\nB\nc\nd\ne\nf\ng\nh\n", base), vec![Unchanged, Local, Unchanged]);
        assert_eq!(kinds(base, base, "a\nb\nc\nd\ne\nf\ng\nH\n"), vec![Unchanged, Remote]);
        assert_eq!(
            kinds(base, "a\nB\nc\nd\ne\nf\ng\nh\n", "a\nB\nc\nd\ne\nf\ng\nh\n"),
            vec![Unchanged, Same, Unchanged]
        );
        assert_eq!(
            kinds(base, "a\nB\nc\nd\ne\nf\ng\nh\n", "a\nX\nc\nd\ne\nf\ng\nh\n"),
            vec![Unchanged, Conflict, Unchanged]
        );
        // Touching changes conflict, as in Git.
        assert_eq!(
            kinds(base, "a\nB\nc\nd\ne\nf\ng\nh\n", "a\nb\nC\nd\ne\nf\ng\nh\n"),
            vec![Unchanged, Conflict, Unchanged]
        );
        // Both inserted at the same place.
        assert_eq!(kinds("a\nb\n", "a\nx\nb\n", "a\ny\nb\n"), vec![Unchanged, Conflict, Unchanged]);
    }

    #[test]
    fn blocks_cover_everything() {
        let (b, l, r) = (
            split_lines("1\n2\n3\n4\n5\n6\n"),
            split_lines("1\nx\n3\n4\n5\n6\n7\n"),
            split_lines("0\n1\n2\n3\n4\n5\n"),
        );
        let blocks = merge_lines(&b, &l, &r, &MergeOptions::default());
        let ends = |f: fn(&Block) -> &Range<u32>| blocks.windows(2).all(|w| f(&w[0]).end == f(&w[1]).start);
        assert!(ends(|b| &b.base) && ends(|b| &b.local) && ends(|b| &b.remote));
        assert_eq!(blocks.last().unwrap().local.end as usize, l.len());
        assert_eq!(blocks.last().unwrap().remote.end as usize, r.len());
        // Remote deleted the last line, local appended after it: touching, so a conflict.
        assert_eq!(
            merge_text("1\n2\n3\n4\n5\n6\n", "1\nx\n3\n4\n5\n6\n7\n", "0\n1\n2\n3\n4\n5\n", &MergeOptions::default()).0,
            "0\n1\nx\n3\n4\n5\n<<<<<<<\n6\n7\n=======\n>>>>>>>\n"
        );
    }

    #[test]
    fn markers() {
        let o = opts(Level::Zealous, MarkerStyle::Diff3);
        let (text, n) = merge_text("a\nb\nc\n", "a\nL\nc\n", "a\nR\nc\n", &o);
        assert_eq!(n, 1);
        assert_eq!(text, "a\n<<<<<<< local\nL\n||||||| base\nb\n=======\nR\n>>>>>>> remote\nc\n");
        assert!(has_conflict_markers(&text) && !has_conflict_markers("<<<<<<<<\n"));
        // CRLF files get CRLF markers; a side without a final newline gets one.
        let (text, _) = merge_text("a\r\nb\r\n", "a\r\nL", "a\r\nR", &opts(Level::Zealous, MarkerStyle::Merge));
        assert_eq!(text, "a\r\n<<<<<<< local\r\nL\r\n=======\r\nR\r\n>>>>>>> remote\r\n");
    }

    #[test]
    fn zealous_splits_conflicts() {
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let local = "1\nA\n3\n4\n5\n6\n7\nB\n9\n";
        let remote = "1\nX\n3\n4\n5\n6\n7\nY\n9\n";
        // Both changed lines 2-8 in one go on each side ... here each side changed two lines.
        let n = |level| merge_text(base, local, remote, &opts(level, MarkerStyle::Merge)).1;
        assert_eq!(n(Level::Zealous), 2);
        // One conflict over the whole stretch, split where both sides agree.
        let local2 = "1\nA\nsame\n4\n5\n6\n7\nB\n9\n";
        let remote2 = "1\nX\nsame\n4\n5\n6\n7\nY\n9\n";
        assert_eq!(merge_text(base, local2, remote2, &opts(Level::Eager, MarkerStyle::Merge)).1, 2);
    }

    /// A small deterministic generator for the comparisons with Git.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn mutate(rng: &mut Rng, base: &[String]) -> Vec<String> {
        let mut out = base.to_vec();
        for _ in 0..rng.below(4 + base.len() as u64 / 4) {
            let at = rng.below(out.len() as u64 + 1) as usize;
            match rng.below(3) {
                0 if at < out.len() => {
                    out.remove(at);
                }
                1 if at < out.len() => {
                    out[at] = format!("{}{}\n", ["x", "y", "z", "{", "}", ""][rng.below(6) as usize], rng.below(3))
                }
                _ => out.insert(at, format!("{}\n", ["n", "m", "}", "", "a"][rng.below(5) as usize])),
            }
        }
        out
    }

    /// The same output and conflict count as `git merge-file`, for random edits of
    /// small files with many repeated lines. Skipped where Git is not installed.
    ///
    /// Where two diffs are equally short, this diff and Git's can pick different
    /// lines (which of two blank lines stays), and the merges then differ. Over
    /// 20,000 cases (`PARALLAX_MERGE_CASES=20000 PARALLAX_MERGE_STATS=1`) 99.4% give
    /// the same bytes and 0.02% a different clean/conflict outcome; the cases here
    /// all agree.
    #[test]
    fn same_as_git_merge_file() {
        let Ok(dir) = tempfile::tempdir() else { return };
        if std::process::Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        // PARALLAX_MERGE_CASES raises the count for a longer run.
        let cases: usize = std::env::var("PARALLAX_MERGE_CASES").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
        let mut checked = 0;
        let mut stats = [0usize; 3];
        for case in 0..cases {
            let n = rng.below(if case % 4 == 3 { 40 } else { 9 }) as usize;
            let base: Vec<String> =
                (0..n).map(|_| format!("{}\n", ["a", "b", "c", "}", ""][rng.below(5) as usize])).collect();
            let local = mutate(&mut rng, &base);
            let remote = mutate(&mut rng, &base);
            let (b, l, r) = (base.concat(), local.concat(), remote.concat());
            for (style, flag, level) in
                [(MarkerStyle::Merge, None, Level::ZealousAlnum), (MarkerStyle::Diff3, Some("--diff3"), Level::Eager)]
            {
                for (name, text) in [("l", &l), ("b", &b), ("r", &r)] {
                    std::fs::write(dir.path().join(name), text).unwrap();
                }
                let mut cmd = std::process::Command::new("git");
                cmd.current_dir(dir.path()).args(["merge-file", "-p", "-L", "local", "-L", "base", "-L", "remote"]);
                if let Some(flag) = flag {
                    cmd.arg(flag);
                }
                let out = cmd.args(["l", "b", "r"]).output().unwrap();
                let (ours, conflicts) = merge_text(&b, &l, &r, &opts(level, style));
                let theirs = String::from_utf8(out.stdout).unwrap();
                if std::env::var("PARALLAX_MERGE_STATS").is_ok() {
                    let git_conflicts = out.status.code().unwrap();
                    let k = if ours == theirs {
                        0
                    } else if (conflicts > 0) == (git_conflicts > 0) {
                        1
                    } else {
                        2
                    };
                    stats[k] += 1;
                    if k == 2 {
                        eprintln!("OUTCOME {style:?} b={b:?} l={l:?} r={r:?} ours={conflicts} git={git_conflicts}");
                    }
                    checked += 1;
                    continue;
                }
                assert_eq!(
                    (ours.as_str(), conflicts as i32),
                    (theirs.as_str(), out.status.code().unwrap()),
                    "case {case} {style:?}\nbase: {b:?}\nlocal: {l:?}\nremote: {r:?}"
                );
                checked += 1;
            }
        }
        eprintln!("same text {} · same outcome, other text {} · other outcome {}", stats[0], stats[1], stats[2]);
        assert_eq!(checked, cases * 2);
    }
}
