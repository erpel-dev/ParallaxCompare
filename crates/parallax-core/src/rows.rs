//! Rows of a side-by-side text comparison as a front end shows them: each line
//! with its syntax colours and changed characters, cut to a length a view can lay
//! out. Shared by the desktop app's web view (serialized) and the GPUI front end
//! (used directly), so both show the same rows.
//!
//! Positions (`spans`, `marks`, the cut counts and [`LineFocus::start`]) are in
//! UTF-16 units, as the web view measures text.

use serde::{Deserialize, Serialize};

use crate::highlight::{DocHighlighter, Span};
use crate::merge::BlockKind;
use crate::merge_session::{Choice, MergeSession};
use crate::text::{self, TextDoc};
use crate::textdiff::{self, Row, RowKind};

/// Longer lines are cut for display: a view cannot lay out megabyte-long rows.
pub const MAX_SHOWN_LINE: u32 = 10_000;

/// One side's line of a row.
#[derive(Debug, Clone, Serialize)]
pub struct LineView {
    /// 0-based line number.
    #[serde(rename = "n")]
    pub number: u32,
    #[serde(rename = "t")]
    pub text: String,
    /// Syntax colours.
    #[serde(rename = "s")]
    pub spans: Vec<Span>,
    /// Changed character ranges within a changed line.
    #[serde(rename = "m")]
    pub marks: Vec<(u32, u32)>,
    /// Number of UTF-16 units cut from the end of a very long line.
    #[serde(rename = "x", skip_serializing_if = "Option::is_none")]
    pub cut_end: Option<u32>,
    /// Number of UTF-16 units cut from the start, to reveal a change or search hit.
    #[serde(rename = "o", skip_serializing_if = "Option::is_none")]
    pub cut_start: Option<u32>,
    /// The file's last line, without the terminator the other file has.
    #[serde(rename = "nn", skip_serializing_if = "std::ops::Not::not")]
    pub no_newline: bool,
}

impl LineView {
    pub fn new(number: u32, line: &str, spans: Vec<Span>, marks: Vec<(u32, u32)>) -> Self {
        Self::window(number, line, spans, marks, None)
    }

    /// The line, or for a very long one the part around `focus` (a column), else
    /// around its first change.
    pub fn window(number: u32, line: &str, spans: Vec<Span>, marks: Vec<(u32, u32)>, focus: Option<u32>) -> Self {
        let total = text::utf16_len(line) as u32;
        if total <= MAX_SHOWN_LINE {
            return LineView {
                number,
                text: line.to_string(),
                spans,
                marks,
                cut_end: None,
                cut_start: None,
                no_newline: false,
            };
        }
        // A requested search column takes precedence over the first change.
        let column = focus.unwrap_or_else(|| marks.first().map_or(0, |m| m.0)).min(total);
        let from_units = if column.saturating_add(100) > MAX_SHOWN_LINE { column.saturating_sub(200) } else { 0 };
        let byte_at = |target: u32| {
            let mut units = 0u32;
            line.char_indices()
                .find(|(_, c)| {
                    let hit = units >= target;
                    units += c.len_utf16() as u32;
                    hit
                })
                .map_or(line.len(), |(i, _)| i)
        };
        let start = byte_at(from_units);
        let skipped = text::utf16_len(&line[..start]) as u32;
        let end = start + {
            let rest = &line[start..];
            let mut units = 0u32;
            rest.char_indices()
                .find(|(_, c)| {
                    units += c.len_utf16() as u32;
                    units > MAX_SHOWN_LINE
                })
                .map_or(rest.len(), |(i, _)| i)
        };
        let shown = text::utf16_len(&line[start..end]) as u32;
        let clip = |(a, b): (u32, u32)| {
            let (a, b) = (a.max(skipped) - skipped, b.saturating_sub(skipped).min(shown));
            (a < b).then_some((a, b))
        };
        LineView {
            number,
            text: line[start..end].to_string(),
            spans: spans.into_iter().filter_map(|Span(a, b, c)| clip((a, b)).map(|(a, b)| Span(a, b, c))).collect(),
            marks: marks.into_iter().filter_map(clip).collect(),
            cut_end: (total > skipped + shown).then_some(total - skipped - shown),
            cut_start: (skipped > 0).then_some(skipped),
            no_newline: false,
        }
    }
}

/// One row of the side-by-side view: the left line, the right line, or both.
#[derive(Debug, Clone, Serialize)]
pub struct RowView {
    #[serde(rename = "k")]
    pub kind: RowKind,
    #[serde(rename = "l")]
    pub left: Option<LineView>,
    #[serde(rename = "r")]
    pub right: Option<LineView>,
}

/// A search hit to keep in view on a long line: the row, the side (0 left, 1
/// right) and the column.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct LineFocus {
    pub row: u32,
    pub side: u8,
    pub start: u32,
}

fn line_range(rows: &[Row], pick: impl Fn(&Row) -> Option<u32>) -> Option<(usize, usize)> {
    let first = rows.iter().find_map(&pick)? as usize;
    let last = rows.iter().rev().find_map(&pick)? as usize;
    Some((first, last + 1))
}

/// Rows `start..start + count` of `all_rows` (a [`textdiff::TextDiff`] of `left`
/// and `right`), with syntax colours from the two highlighters and the changed
/// characters of changed lines.
pub fn side_by_side(
    left: &TextDoc,
    right: &TextDoc,
    all_rows: &[Row],
    start: usize,
    count: usize,
    highlighters: [&mut DocHighlighter; 2],
    focus: Option<LineFocus>,
) -> Vec<RowView> {
    let end = start.saturating_add(count).min(all_rows.len());
    let rows = &all_rows[start.min(end)..end];
    let [hl_left, hl_right] = highlighters;
    let spans = |hl: &mut DocHighlighter, doc: &TextDoc, range: Option<(usize, usize)>| match range {
        Some((a, b)) => (a, hl.highlight(doc, a, b)),
        None => (0, Vec::new()),
    };
    let (left_base, left_spans) = spans(hl_left, left, line_range(rows, |r| r.left));
    let (right_base, right_spans) = spans(hl_right, right, line_range(rows, |r| r.right));
    // A last line whose terminator only the other side has.
    let no_newline = |doc: &TextDoc, other: &TextDoc, n: u32| {
        n as usize + 1 == doc.len() && !doc.ends_with_newline() && other.ends_with_newline()
    };
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let (lm, rm) = match (row.kind, row.left, row.right) {
                (RowKind::Changed, Some(l), Some(r)) => {
                    let (a, b) = textdiff::intraline(left.line(l as usize), right.line(r as usize));
                    (
                        a.into_iter().map(|r| (r.start, r.end)).collect(),
                        b.into_iter().map(|r| (r.start, r.end)).collect(),
                    )
                }
                _ => (Vec::new(), Vec::new()),
            };
            let line = |doc: &TextDoc, other: &TextDoc, n: u32, base: usize, all: &Vec<Vec<Span>>, marks, side| {
                let column = focus.filter(|f| f.row as usize == start + index && f.side == side).map(|f| f.start);
                LineView {
                    no_newline: no_newline(doc, other, n),
                    ..LineView::window(
                        n,
                        doc.line(n as usize),
                        all.get(n as usize - base).cloned().unwrap_or_default(),
                        marks,
                        column,
                    )
                }
            };
            RowView {
                kind: row.kind,
                left: row.left.map(|n| line(left, right, n, left_base, &left_spans, lm, 0)),
                right: row.right.map(|n| line(right, left, n, right_base, &right_spans, rm, 1)),
            }
        })
        .collect()
}

/// One row of a three-way merge's inputs: local, base and remote side by side.
#[derive(Debug, Clone, Serialize)]
pub struct MergeInputRow {
    #[serde(rename = "k")]
    pub kind: BlockKind,
    /// Index of the block.
    #[serde(rename = "i")]
    pub block: u32,
    #[serde(rename = "b")]
    pub base: Option<LineView>,
    #[serde(rename = "l")]
    pub local: Option<LineView>,
    #[serde(rename = "r")]
    pub remote: Option<LineView>,
}

/// One row of a merge's result: a line, or the header above a conflict's lines.
#[derive(Debug, Clone, Serialize)]
pub struct MergeResultRow {
    #[serde(rename = "k")]
    pub kind: BlockKind,
    #[serde(rename = "c")]
    pub choice: Choice,
    #[serde(rename = "i")]
    pub block: u32,
    /// A line; a conflict's header row has none.
    #[serde(rename = "l")]
    pub line: Option<LineView>,
}

/// Syntax colours for the lines `lines` of a document, as one batch: the first
/// line and the colours of each line from it.
fn spans_of(
    hl: &mut DocHighlighter,
    doc: &TextDoc,
    lines: impl Iterator<Item = u32> + Clone,
) -> (usize, Vec<Vec<Span>>) {
    let (Some(first), Some(last)) = (lines.clone().min(), lines.max()) else { return (0, Vec::new()) };
    (first as usize, hl.highlight(doc, first as usize, last as usize + 1))
}

/// Input rows `start..start + count` of a merge, with syntax colours from the
/// highlighters of base, local and remote, and the characters each side changed
/// against the base line beside it.
pub fn merge_input_rows(
    s: &MergeSession,
    start: u32,
    count: u32,
    highlighters: [&mut DocHighlighter; 3],
) -> Vec<MergeInputRow> {
    let rows = s.input_rows(start, count);
    let [hb, hl, hr] = highlighters;
    let (bb, bs) = spans_of(hb, &s.base, rows.iter().filter_map(|r| r.base));
    let (lb, ls) = spans_of(hl, &s.local, rows.iter().filter_map(|r| r.local));
    let (rb, rs) = spans_of(hr, &s.remote, rows.iter().filter_map(|r| r.remote));
    let line = |doc: &TextDoc, n: u32, first: usize, all: &Vec<Vec<Span>>, marks: Vec<(u32, u32)>| {
        LineView::new(n, doc.line(n as usize), all.get(n as usize - first).cloned().unwrap_or_default(), marks)
    };
    // Changed characters of a changed line against the base line beside it.
    let marks =
        |kind: BlockKind, side: &TextDoc, n: Option<u32>, base: Option<u32>, changed: bool| -> Vec<(u32, u32)> {
            match (n, base) {
                (Some(n), Some(b)) if changed && kind != BlockKind::Unchanged => {
                    let (_, m) = textdiff::intraline(s.base.line(b as usize), side.line(n as usize));
                    m.into_iter().map(|r| (r.start, r.end)).collect()
                }
                _ => Vec::new(),
            }
        };
    rows.iter()
        .map(|row| {
            let local_changed = matches!(row.kind, BlockKind::Local | BlockKind::Same | BlockKind::Conflict);
            let remote_changed = matches!(row.kind, BlockKind::Remote | BlockKind::Same | BlockKind::Conflict);
            MergeInputRow {
                kind: row.kind,
                block: row.block,
                base: row.base.map(|n| line(&s.base, n, bb, &bs, Vec::new())),
                local: row
                    .local
                    .map(|n| line(&s.local, n, lb, &ls, marks(row.kind, &s.local, Some(n), row.base, local_changed))),
                remote: row.remote.map(|n| {
                    line(&s.remote, n, rb, &rs, marks(row.kind, &s.remote, Some(n), row.base, remote_changed))
                }),
            }
        })
        .collect()
}

/// Result rows `start..start + count` of a merge, with syntax colours, except
/// between conflict markers where the colouring goes astray.
pub fn merge_result_rows(s: &MergeSession, start: u32, count: u32, hl: &mut DocHighlighter) -> Vec<MergeResultRow> {
    let rows = s.result_rows(start, count);
    let doc = s.result();
    let (first, all) = spans_of(hl, doc, rows.iter().filter_map(|r| r.line));
    rows.iter()
        .map(|row| MergeResultRow {
            kind: row.kind,
            choice: row.choice,
            block: row.block,
            line: row.line.map(|n| {
                let markers = row.choice == Choice::Unresolved
                    || (row.choice == Choice::Syntax && s.syntax_left(row.block as usize) > 0);
                let line = doc.line(n as usize);
                let marker_line = ["<<<<<<<", "|||||||", "=======", ">>>>>>>"].iter().any(|m| line.starts_with(m));
                let colours = if markers || marker_line {
                    Vec::new()
                } else {
                    all.get(n as usize - first).cloned().unwrap_or_default()
                };
                LineView::new(n, line, colours, Vec::new())
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{EditSession, Side};
    use crate::highlight::{ThemeKind, find_syntax};
    use crate::textdiff::DiffOptions;

    #[test]
    fn distant_search_hits_override_the_first_difference_without_unbounding_rows() {
        let line = format!("{}left{}needle", "a".repeat(300_000), "z".repeat(300_000));
        let marks = vec![(300_000, 300_004)];
        let initial = LineView::new(0, &line, Vec::new(), marks.clone());
        assert!(initial.text.contains("left"));
        assert!(!initial.text.contains("needle"));
        let found = LineView::window(0, &line, vec![Span(600_004, 600_010, 0x123456)], marks, Some(600_004));
        assert!(found.text.contains("needle"));
        assert!(text::utf16_len(&found.text) <= MAX_SHOWN_LINE as usize);
        assert_eq!(found.cut_start, Some(599_804));
        assert!(found.cut_end.is_none());
        assert_eq!(found.spans.iter().map(|s| (s.0, s.1, s.2)).collect::<Vec<_>>(), [(200, 206, 0x123456)]);
        assert!(found.marks.is_empty());
        let beginning = LineView::window(0, &line, Vec::new(), Vec::new(), Some(0));
        assert!(beginning.cut_start.is_none());
        assert!(beginning.cut_end.is_some());
    }

    #[test]
    fn search_windows_keep_utf16_boundaries_and_clip_marks() {
        let line = format!("{}needle{}", "😀".repeat(20_000), "😀".repeat(20_000));
        let found = LineView::window(3, &line, Vec::new(), vec![(40_000, 40_006)], Some(40_000));
        assert_eq!(found.cut_start, Some(39_800));
        assert_eq!(found.marks, [(200, 206)]);
        assert_eq!(
            String::from_utf16(&found.text.encode_utf16().skip(200).take(6).collect::<Vec<_>>()).unwrap(),
            "needle"
        );
        assert!(text::utf16_len(&found.text) <= MAX_SHOWN_LINE as usize);
        assert_eq!(
            found.cut_start.unwrap() + text::utf16_len(&found.text) as u32 + found.cut_end.unwrap(),
            text::utf16_len(&line) as u32
        );
        let ordinary = LineView::window(0, "a😀b", Vec::new(), Vec::new(), Some(3));
        assert_eq!(ordinary.text, "a😀b");
        assert!(ordinary.cut_start.is_none() && ordinary.cut_end.is_none());
    }

    #[test]
    fn rows_carry_kinds_changed_characters_colours_and_missing_newlines() {
        let left = text::decode(b"fn a() {}\nlet x = 1;\nsame\n");
        let right = text::decode(b"fn a() {}\nlet x = 2;\nsame\nadded");
        let edit = EditSession::new(left, right, DiffOptions::default());
        let rust = find_syntax("Rust");
        let (mut hl, mut hr) =
            (DocHighlighter::new(rust, ThemeKind::Light), DocHighlighter::new(rust, ThemeKind::Light));
        let (l, r) = (edit.doc(Side::Left), edit.doc(Side::Right));
        let rows = side_by_side(l, r, &edit.diff().rows, 0, 100, [&mut hl, &mut hr], None);
        let kinds: Vec<_> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [RowKind::Equal, RowKind::Changed, RowKind::Equal, RowKind::Added]);
        let changed = &rows[1];
        assert_eq!(changed.left.as_ref().unwrap().marks, [(8, 9)]);
        assert_eq!(changed.right.as_ref().unwrap().marks, [(8, 9)]);
        assert!(!rows[0].left.as_ref().unwrap().spans.is_empty(), "Rust is highlighted");
        let added = &rows[3];
        assert!(added.left.is_none());
        assert_eq!(added.right.as_ref().unwrap().text, "added");
        // Rows past the end are not invented; a window in the middle keeps line numbers.
        let window = side_by_side(l, r, &edit.diff().rows, 1, 2, [&mut hl, &mut hr], None);
        assert_eq!(window.iter().map(|r| r.left.as_ref().map(|l| l.number)).collect::<Vec<_>>(), [Some(1), Some(2)]);
        assert!(side_by_side(l, r, &edit.diff().rows, 10, 5, [&mut hl, &mut hr], None).is_empty());
    }

    #[test]
    fn merge_rows_mark_changes_against_the_base_and_leave_markers_plain() {
        use crate::merge::MergeOptions;
        let doc = |t: &str| text::decode(t.as_bytes());
        let s = MergeSession::new(
            doc("fn a() {}\nlet x = 1;\nend\n"),
            doc("fn a() {}\nlet x = 2;\nend\n"),
            doc("fn a() {}\nlet x = 3;\nend\n"),
            MergeOptions::default(),
        );
        let rust = find_syntax("Rust");
        let mut hls: [DocHighlighter; 4] = std::array::from_fn(|_| DocHighlighter::new(rust, ThemeKind::Light));
        let [hb, hl, hr, hres] = &mut hls;
        let inputs = merge_input_rows(&s, 0, 100, [hb, hl, hr]);
        let conflict = inputs.iter().find(|r| r.kind == BlockKind::Conflict).expect("a conflict row");
        assert_eq!(conflict.local.as_ref().unwrap().marks, [(8, 9)]);
        assert_eq!(conflict.remote.as_ref().unwrap().marks, [(8, 9)]);
        assert!(conflict.base.as_ref().unwrap().marks.is_empty());
        assert!(!inputs[0].local.as_ref().unwrap().spans.is_empty(), "Rust is highlighted");
        let result = merge_result_rows(&s, 0, 100, hres);
        assert!(result.iter().any(|r| r.line.is_none()), "a conflict has a header row");
        let marker = result.iter().filter_map(|r| r.line.as_ref()).find(|l| l.text.starts_with("<<<<<<<")).unwrap();
        assert!(marker.spans.is_empty());
        let json = serde_json::to_value(&result[0]).unwrap();
        assert!(
            json.get("k").is_some() && json.get("c").is_some() && json.get("i").is_some() && json.get("l").is_some()
        );
    }

    #[test]
    fn serialized_shape_is_what_the_web_view_reads() {
        let row = RowView {
            kind: RowKind::Changed,
            left: Some(LineView::new(4, "abc", vec![Span(0, 1, 0xff0000)], vec![(1, 2)])),
            right: None,
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"k": "changed", "l": {"n": 4, "t": "abc", "s": [[0, 1, 0xff0000]], "m": [[1, 2]]}, "r": null})
        );
    }
}
