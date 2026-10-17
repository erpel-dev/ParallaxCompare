//! Line diff with aligned rows for a side-by-side view, plus intra-line highlights.

use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::ops::Range;

use imara_diff::{Algorithm, Diff, InternedInput, Interner};
use serde::{Deserialize, Serialize};

use crate::text::{TextDoc, utf16_len};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum WhitespaceMode {
    /// Every whitespace difference counts.
    #[default]
    Exact,
    /// Leading and trailing whitespace is ignored, runs of inner whitespace count as one.
    Normalize,
    /// All whitespace is ignored.
    IgnoreAll,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiffOptions {
    pub whitespace: WhitespaceMode,
    pub ignore_case: bool,
    /// Histogram can give more readable results for moved code, but it slows down
    /// sharply on files with many repeated lines (A2L, generated XML); Myers, the
    /// algorithm Git uses by default, does not.
    pub histogram: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions { whitespace: WhitespaceMode::Exact, ignore_case: false, histogram: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RowKind {
    Equal,
    /// Equal under the active rules (whitespace, case) but not byte-identical.
    Minor,
    Changed,
    /// Only on the left side; the right shows a filler.
    Removed,
    /// Only on the right side; the left shows a filler.
    Added,
}

/// One aligned row of the side-by-side view. Line numbers are 0-based.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Row {
    #[serde(rename = "l")]
    pub left: Option<u32>,
    #[serde(rename = "r")]
    pub right: Option<u32>,
    #[serde(rename = "k")]
    pub kind: RowKind,
}

/// A block of consecutive non-equal rows.
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStats {
    pub changed: u32,
    pub added: u32,
    pub removed: u32,
    pub minor: u32,
}

pub struct TextDiff {
    pub rows: Vec<Row>,
    pub sections: Vec<Section>,
    pub stats: DiffStats,
}

/// Interned line key: the normalized text, plus the raw text for slider heuristics.
#[derive(Eq)]
struct Key<'a>(Cow<'a, str>);

impl PartialEq for Key<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Hash for Key<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}

impl AsRef<[u8]> for Key<'_> {
    fn as_ref(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

fn normalize_line<'a>(line: &'a str, opts: &DiffOptions) -> Cow<'a, str> {
    let mut out: Cow<'a, str> = match opts.whitespace {
        WhitespaceMode::Exact => Cow::Borrowed(line),
        WhitespaceMode::Normalize => {
            let trimmed = line.trim();
            if trimmed.contains(|c: char| c.is_whitespace()) {
                Cow::Owned(trimmed.split_whitespace().collect::<Vec<_>>().join(" "))
            } else {
                Cow::Borrowed(trimmed)
            }
        }
        WhitespaceMode::IgnoreAll => {
            if line.contains(|c: char| c.is_whitespace()) {
                Cow::Owned(line.chars().filter(|c| !c.is_whitespace()).collect())
            } else {
                Cow::Borrowed(line)
            }
        }
    };
    if opts.ignore_case && out.chars().any(|c| c.is_uppercase()) {
        out = Cow::Owned(out.to_lowercase());
    }
    out
}

pub fn diff_docs(left: &TextDoc, right: &TextDoc, opts: &DiffOptions) -> TextDiff {
    let mut interner = Interner::new(left.len() + right.len());
    let before = (0..left.len()).map(|i| interner.intern(Key(normalize_line(left.line(i), opts)))).collect();
    let after = (0..right.len()).map(|i| interner.intern(Key(normalize_line(right.line(i), opts)))).collect();
    let input = InternedInput { before, after, interner };
    let algorithm = if opts.histogram { Algorithm::Histogram } else { Algorithm::Myers };
    let mut diff = Diff::compute(algorithm, &input);
    diff.postprocess_lines(&input);

    let mut rows = Vec::with_capacity(left.len().max(right.len()));
    let mut stats = DiffStats::default();
    let (mut l, mut r) = (0u32, 0u32);
    let push_equal = |rows: &mut Vec<Row>, stats: &mut DiffStats, until_l: u32, l: &mut u32, r: &mut u32| {
        while *l < until_l {
            let kind = if left.line(*l as usize) == right.line(*r as usize) {
                RowKind::Equal
            } else {
                stats.minor += 1;
                RowKind::Minor
            };
            rows.push(Row { left: Some(*l), right: Some(*r), kind });
            *l += 1;
            *r += 1;
        }
    };
    for hunk in diff.hunks() {
        push_equal(&mut rows, &mut stats, hunk.before.start, &mut l, &mut r);
        let (n, m) = (hunk.before.len() as u32, hunk.after.len() as u32);
        let paired = n.min(m);
        for i in 0..paired {
            rows.push(Row { left: Some(l + i), right: Some(r + i), kind: RowKind::Changed });
        }
        for i in paired..n {
            rows.push(Row { left: Some(l + i), right: None, kind: RowKind::Removed });
        }
        for i in paired..m {
            rows.push(Row { left: None, right: Some(r + i), kind: RowKind::Added });
        }
        stats.changed += paired;
        stats.removed += n - paired;
        stats.added += m - paired;
        l += n;
        r += m;
    }
    push_equal(&mut rows, &mut stats, left.len() as u32, &mut l, &mut r);

    let mut sections = Vec::new();
    let mut open: Option<u32> = None;
    for (i, row) in rows.iter().enumerate() {
        let differs = matches!(row.kind, RowKind::Changed | RowKind::Removed | RowKind::Added);
        match (differs, open) {
            (true, None) => open = Some(i as u32),
            (false, Some(start)) => {
                sections.push(Section { start, end: i as u32 });
                open = None;
            }
            _ => {}
        }
    }
    if let Some(start) = open {
        sections.push(Section { start, end: rows.len() as u32 });
    }
    TextDiff { rows, sections, stats }
}

/// Lines longer than this are highlighted as a whole instead of per token.
const MAX_INTRALINE_LEN: usize = 4000;

/// Character ranges (UTF-16 offsets) that differ between two paired lines.
/// Lines are split into words, whitespace runs and single punctuation characters,
/// which gives more readable results than a character diff.
pub fn intraline(left: &str, right: &str) -> (Vec<Range<u32>>, Vec<Range<u32>>) {
    if left.len() > MAX_INTRALINE_LEN || right.len() > MAX_INTRALINE_LEN {
        let whole = |s: &str| {
            let len = utf16_len(s) as u32;
            std::iter::once(0..len).filter(|r| !r.is_empty()).collect()
        };
        return (whole(left), whole(right));
    }
    let a = tokenize(left);
    let b = tokenize(right);
    let ops = similar::capture_diff_slices(similar::Algorithm::Myers, &a, &b);
    let offsets = |tokens: &[&str]| {
        let mut out = Vec::with_capacity(tokens.len() + 1);
        let mut pos = 0u32;
        out.push(0);
        for t in tokens {
            pos += utf16_len(t) as u32;
            out.push(pos);
        }
        out
    };
    let (oa, ob) = (offsets(&a), offsets(&b));
    let mut left_ranges: Vec<Range<u32>> = Vec::new();
    let mut right_ranges: Vec<Range<u32>> = Vec::new();
    let add = |ranges: &mut Vec<Range<u32>>, range: Range<u32>| {
        if range.is_empty() {
            return;
        }
        match ranges.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => ranges.push(range),
        }
    };
    for op in ops {
        match op {
            similar::DiffOp::Equal { .. } => {}
            similar::DiffOp::Delete { old_index, old_len, .. } => {
                add(&mut left_ranges, oa[old_index]..oa[old_index + old_len]);
            }
            similar::DiffOp::Insert { new_index, new_len, .. } => {
                add(&mut right_ranges, ob[new_index]..ob[new_index + new_len]);
            }
            similar::DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                add(&mut left_ranges, oa[old_index]..oa[old_index + old_len]);
                add(&mut right_ranges, ob[new_index]..ob[new_index + new_len]);
            }
        }
    }
    (left_ranges, right_ranges)
}

fn tokenize(s: &str) -> Vec<&str> {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Word,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut prev: Option<Class> = None;
    for (i, c) in s.char_indices() {
        let cls = class(c);
        if let Some(p) = prev
            && (p != cls || cls == Class::Other)
        {
            tokens.push(&s[start..i]);
            start = i;
        }
        prev = Some(cls);
    }
    if start < s.len() {
        tokens.push(&s[start..]);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::decode;

    fn kinds(diff: &TextDiff) -> Vec<RowKind> {
        diff.rows.iter().map(|r| r.kind).collect()
    }

    #[test]
    fn aligned_rows() {
        let a = decode(b"a\nb\nc\nd\n");
        let b = decode(b"a\nB\nc\nx\ny\nd\n");
        let diff = diff_docs(&a, &b, &DiffOptions::default());
        use RowKind::*;
        assert_eq!(kinds(&diff), vec![Equal, Changed, Equal, Added, Added, Equal]);
        assert_eq!(diff.sections.len(), 2);
        assert_eq!((diff.stats.changed, diff.stats.added), (1, 2));
    }

    #[test]
    fn whitespace_rules() {
        let a = decode(b"fn  main() {\n");
        let b = decode(b"fn main() {   \n");
        let exact = diff_docs(&a, &b, &DiffOptions::default());
        assert_eq!(kinds(&exact), vec![RowKind::Changed]);
        let opts = DiffOptions { whitespace: WhitespaceMode::Normalize, ..Default::default() };
        let relaxed = diff_docs(&a, &b, &opts);
        assert_eq!(kinds(&relaxed), vec![RowKind::Minor]);
    }

    #[test]
    fn intraline_tokens() {
        let (l, r) = intraline("let total = 50;", "let total = 100;");
        assert_eq!(l, vec![12..14]);
        assert_eq!(r, vec![12..15]);
        let (l, r) = intraline("items: CartItem[])", "items: CartItem[], d?: Discount)");
        assert!(l.is_empty());
        assert_eq!(r, vec![17..31]);
    }
}
