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
    /// Lines are matched by their place in a JSON-like tree (the names of the
    /// objects around them and their own name) instead of by their text, so
    /// members on the same level pair up by name: a renamed container is one
    /// block removed and one added, and a changed value is one changed line.
    /// Set by views whose text is such a tree (ECUC values), not by the user.
    #[serde(skip)]
    pub keyed: bool,
}

impl Default for DiffOptions {
    fn default() -> Self {
        DiffOptions { whitespace: WhitespaceMode::Exact, ignore_case: false, histogram: false, keyed: false }
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
    let (before, after) = if opts.keyed {
        let mut keys =
            |doc: &TextDoc| tree_keys(doc).into_iter().map(|k| interner.intern(Key(Cow::Owned(k)))).collect();
        (keys(left), keys(right))
    } else {
        let before = (0..left.len()).map(|i| interner.intern(Key(normalize_line(left.line(i), opts)))).collect();
        let after = (0..right.len()).map(|i| interner.intern(Key(normalize_line(right.line(i), opts)))).collect();
        (before, after)
    };
    let input = InternedInput { before, after, interner };
    let algorithm = if opts.histogram { Algorithm::Histogram } else { Algorithm::Myers };
    let mut diff = Diff::compute(algorithm, &input);
    diff.postprocess_lines(&input);

    let mut rows = Vec::with_capacity(left.len().max(right.len()));
    let mut stats = DiffStats::default();
    let (mut l, mut r) = (0u32, 0u32);
    let push_equal = |rows: &mut Vec<Row>, stats: &mut DiffStats, until_l: u32, l: &mut u32, r: &mut u32| {
        while *l < until_l {
            let (a, b) = (left.line(*l as usize), right.line(*r as usize));
            let kind = if a == b {
                RowKind::Equal
            } else if !opts.keyed || same_member(a, b, opts) {
                stats.minor += 1;
                RowKind::Minor
            } else {
                // Keyed: the same member with another value.
                stats.changed += 1;
                RowKind::Changed
            };
            rows.push(Row { left: Some(*l), right: Some(*r), kind });
            *l += 1;
            *r += 1;
        }
    };
    for hunk in diff.hunks() {
        push_equal(&mut rows, &mut stats, hunk.before.start, &mut l, &mut r);
        let (n, m) = (hunk.before.len() as u32, hunk.after.len() as u32);
        // Keyed lines pair only by their key: what is left is on one side only.
        let paired = if opts.keyed { 0 } else { n.min(m) };
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
    // A final newline on one side only is a difference too, or it could not be copied.
    if let Some(last) = rows.last_mut()
        && matches!(last.kind, RowKind::Equal | RowKind::Minor)
        && left.ends_with_newline() != right.ends_with_newline()
    {
        if last.kind == RowKind::Minor {
            stats.minor -= 1;
        }
        last.kind = RowKind::Changed;
        stats.changed += 1;
    }

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

/// A key per line of a JSON-like tree, as written by the ECUC values view: the
/// path of object names down to the line, then the line's own member name.
/// `"Can": {`, its members and its closing brace get `Can{`, `Can/…` and `Can}`;
/// an array element is keyed by its value. Lines that do not parse keep their
/// text, under their path, so hand-edited text still diffs sensibly. A key
/// that repeats gets a counter.
pub fn tree_keys(doc: &TextDoc) -> Vec<String> {
    // Open objects and arrays: their name and whether it is an array.
    let mut stack: Vec<(String, bool)> = Vec::new();
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut keys = Vec::with_capacity(doc.len());
    for i in 0..doc.len() {
        let line = doc.line(i).trim();
        let t = line.strip_suffix(',').unwrap_or(line).trim_end();
        let path = stack.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join("/");
        let in_array = stack.last().is_some_and(|(_, array)| *array);
        let key = if t == "}" || t == "]" {
            stack.pop();
            format!("{path}{t}")
        } else if t == "{" || t == "[" {
            stack.push((String::new(), t == "["));
            format!("{path}/{t}")
        } else if in_array {
            format!("{path}/[{t}]")
        } else if let Some((name, value)) = member(t) {
            match value {
                "{" | "[" => {
                    stack.push((name.to_string(), value == "["));
                    format!("{path}/{name}{value}")
                }
                _ => format!("{path}/{name}"),
            }
        } else {
            format!("{path}/?{t}")
        };
        let n = seen.entry(key.clone()).or_insert(0);
        *n += 1;
        keys.push(if *n == 1 { key } else { format!("{key}#{n}") });
    }
    keys
}

/// Two lines of the same member that differ only under the rules or in the
/// trailing comma, which a tree view writes after all but the last member.
fn same_member(a: &str, b: &str, opts: &DiffOptions) -> bool {
    let (a, b) = (normalize_line(a, opts), normalize_line(b, opts));
    let bare = |s: &str| s.trim_end().strip_suffix(',').unwrap_or(s.trim_end()).to_string();
    a == b || bare(&a) == bare(&b)
}

/// `"name": value` as the name (without its quotes) and the value text.
fn member(t: &str) -> Option<(&str, &str)> {
    let rest = t.strip_prefix('"')?;
    let mut escaped = false;
    let end = rest.char_indices().find_map(|(i, c)| {
        let close = c == '"' && !escaped;
        escaped = c == '\\' && !escaped;
        close.then_some(i)
    })?;
    let value = rest[end + 1..].trim_start().strip_prefix(':')?.trim();
    Some((&rest[..end], value))
}

/// Lines longer than this are highlighted as a whole instead of per token.
const MAX_INTRALINE_LEN: usize = 4000;

/// Character ranges (UTF-16 offsets) that differ between two paired lines.
/// Lines are split into words, whitespace runs and single punctuation characters,
/// which gives more readable results than a character diff.
pub fn intraline(left: &str, right: &str) -> (Vec<Range<u32>>, Vec<Range<u32>>) {
    if left.len() > MAX_INTRALINE_LEN || right.len() > MAX_INTRALINE_LEN {
        // Long lines (minified code): what both share at the start and the end is
        // equal; only the part between is compared, or marked as a whole.
        let prefix = left
            .char_indices()
            .zip(right.chars())
            .find(|((_, a), b)| a != b)
            .map_or(left.len().min(right.len()), |((i, _), _)| i);
        let (l, r) = (&left[prefix..], &right[prefix..]);
        let mut suffix = 0;
        for (a, b) in l.chars().rev().zip(r.chars().rev()) {
            if a != b {
                break;
            }
            suffix += a.len_utf8();
        }
        let (l, r) = (&l[..l.len() - suffix], &r[..r.len() - suffix]);
        let offset = utf16_len(&left[..prefix]) as u32;
        let shift = |v: Vec<Range<u32>>| v.into_iter().map(|x| x.start + offset..x.end + offset).collect::<Vec<_>>();
        if l.len() <= MAX_INTRALINE_LEN && r.len() <= MAX_INTRALINE_LEN {
            let (a, b) = token_marks(l, r);
            return (shift(a), shift(b));
        }
        let whole = |s: &str| std::iter::once(0..utf16_len(s) as u32).filter(|r| !r.is_empty()).collect();
        return (shift(whole(l)), shift(whole(r)));
    }
    let (left_ranges, right_ranges) = token_marks(left, right);
    // Lines with little in common: the row's colour says they differ, and marks
    // over most of both would only add noise.
    if mostly(left, &left_ranges) && mostly(right, &right_ranges) {
        return (Vec::new(), Vec::new());
    }
    (left_ranges, right_ranges)
}

/// The differing ranges of two lines by their tokens, phrases joined.
fn token_marks(left: &str, right: &str) -> (Vec<Range<u32>>, Vec<Range<u32>>) {
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
    (calm(left, left_ranges), calm(right, right_ranges))
}

/// Joins marks that only whitespace or up to two other characters separate, so a
/// changed phrase is one mark instead of one per word.
fn calm(line: &str, ranges: Vec<Range<u32>>) -> Vec<Range<u32>> {
    if ranges.len() < 2 {
        return ranges;
    }
    let units: Vec<u16> = line.encode_utf16().collect();
    let space = |u: &u16| *u == b' ' as u16 || *u == b'\t' as u16;
    let mut out: Vec<Range<u32>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match out.last_mut() {
            Some(last) => {
                let gap = &units[last.end as usize..range.start as usize];
                if gap.iter().all(space) || gap.iter().filter(|u| !space(u)).count() <= 2 && gap.len() <= 4 {
                    last.end = range.end;
                } else {
                    out.push(range);
                }
            }
            None => out.push(range),
        }
    }
    out
}

/// Whether the marks cover more than two thirds of the line's text.
fn mostly(line: &str, ranges: &[Range<u32>]) -> bool {
    let text = utf16_len(line.trim()) as u32;
    let marked: u32 = ranges.iter().map(|r| r.end - r.start).sum();
    text > 0 && marked * 3 > text * 2
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

    /// Tree views (ECUC values) pair members by name, never by text: a renamed
    /// container is removed and added as a whole, not one changed line with
    /// the contents of another container shown as equal.
    #[test]
    fn keyed_rows_match_members_by_name() {
        let left = decode(
            r#"{
  "Can": {
    "Ctrl_Body": {
      "Id": 0,
      "BaudRate": 500
    },
    "Ctrl_Chassis": {
      "Id": 1,
      "BaudRate": 500
    },
    "Ctrl_Powertrain": {
      "Id": 2,
      "BaudRate": 500,
      "Wakeups": [
        "→ EcuM/Can1",
        "→ EcuM/Can2"
      ]
    }
  }
}
"#
            .as_bytes(),
        );
        let right = decode(
            r#"{
  "Can": {
    "Ctrl_Body": {
      "Id": 0,
      "BaudRate": 500
    },
    "Ctrl_Comfort": {
      "Id": 1,
      "BaudRate": 500
    },
    "Ctrl_Powertrain": {
      "Id": 2,
      "BaudRate": 250,
      "Wakeups": [
        "→ EcuM/Can2",
        "→ EcuM/Can3"
      ]
    }
  }
}
"#
            .as_bytes(),
        );
        use RowKind::*;
        // By text, Chassis and Comfort pair up as one changed line.
        let plain = diff_docs(&left, &right, &DiffOptions::default());
        assert!(plain.rows.iter().any(|r| r.kind == Changed && r.left == Some(6) && r.right == Some(6)));
        let keyed = diff_docs(&left, &right, &DiffOptions { keyed: true, ..Default::default() });
        let at =
            |l: Option<u32>, r: Option<u32>| keyed.rows.iter().find(|x| x.left == l && x.right == r).map(|x| x.kind);
        // Ctrl_Chassis (lines 6..9) only on the left, Ctrl_Comfort only on the right.
        for line in 6..=9 {
            assert_eq!(at(Some(line), None), Some(Removed), "left line {line}");
            assert_eq!(at(None, Some(line)), Some(Added), "right line {line}");
        }
        // The same member with another value is one changed row.
        assert_eq!(at(Some(12), Some(12)), Some(Changed));
        // Array elements are matched by value: Can1 removed, Can2 kept (only its
        // comma differs), Can3 added.
        assert_eq!(at(Some(14), None), Some(Removed));
        assert_eq!(at(Some(15), Some(14)), Some(Minor));
        assert_eq!(at(None, Some(15)), Some(Added));
        assert_eq!((keyed.stats.changed, keyed.stats.removed, keyed.stats.added), (1, 5, 5));
    }

    /// A removed last member leaves the one before without its comma: the same
    /// member, similar, not a difference.
    #[test]
    fn keyed_rows_ignore_trailing_commas() {
        let left = decode(b"{\n  \"A\": {\n    \"x\": 1\n  },\n  \"B\": {\n    \"x\": 2\n  }\n}\n");
        let right = decode(b"{\n  \"A\": {\n    \"x\": 1\n  }\n}\n");
        let diff = diff_docs(&left, &right, &DiffOptions { keyed: true, ..Default::default() });
        use RowKind::*;
        assert_eq!(kinds(&diff), vec![Equal, Equal, Equal, Minor, Removed, Removed, Removed, Equal]);
        assert_eq!(diff.stats.changed, 0);
    }

    #[test]
    fn tree_keys_follow_the_names() {
        let doc = decode(
            b"{\n  \"Can\": {\n    \"say \\\"hi\\\"\": 1,\n    \"L\": [\n      2\n    ],\n    odd line\n  }\n}\n",
        );
        assert_eq!(
            tree_keys(&doc),
            ["/{", "/Can{", "/Can/say \\\"hi\\\"", "/Can/L[", "/Can/L/[2]", "/Can/L]", "/Can/?odd line", "/Can}", "}"]
        );
    }

    #[test]
    fn missing_final_newline_is_a_change() {
        let diff = diff_docs(&decode(b"a\nb"), &decode(b"a\nb\n"), &DiffOptions::default());
        assert_eq!(kinds(&diff), vec![RowKind::Equal, RowKind::Changed]);
        assert_eq!(diff.sections.len(), 1);
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

    /// Minified code: only the changed part of a long line is marked, also far into it.
    #[test]
    fn intraline_long_lines() {
        let a = format!("{}var a=1;{}", "var x=0;".repeat(5000), "var z=9;".repeat(10));
        let b = format!("{}var a=2;{}", "var x=0;".repeat(5000), "var z=9;".repeat(10));
        let (l, r) = intraline(&a, &b);
        assert_eq!(l, vec![40006..40007]);
        assert_eq!(r, vec![40006..40007]);
        // Long changed middles are marked as a whole, with their offset.
        let c = format!("{}{}", "é".repeat(10), "x".repeat(9000));
        let d = format!("{}{}", "é".repeat(10), "y".repeat(9000));
        let (l, r) = intraline(&c, &d);
        let whole = 10..9010;
        assert_eq!((l, r), (vec![whole.clone()], vec![whole]));
    }

    #[test]
    fn intraline_tokens() {
        let (l, r) = intraline("let total = 50;", "let total = 100;");
        assert_eq!(l, vec![12..14]);
        assert_eq!(r, vec![12..15]);
        let (l, r) = intraline("items: CartItem[])", "items: CartItem[], d?: Discount)");
        assert!(l.is_empty());
        assert_eq!(r, vec![17..31]);
        // A changed phrase is one mark, not one per word.
        let (l, r) = intraline("/// Values that are lists of ids", "/// Values of the fields kept as lists of ids");
        assert_eq!((l.len(), r.len()), (1, 1));
        // Lines with little in common are not marked at all.
        let (l, r) = intraline("let parts = value.split(',').collect();", "if listy {");
        assert!(l.is_empty() && r.is_empty());
    }
}
