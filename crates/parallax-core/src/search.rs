//! Finding text in both sides of a file comparison.

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::text::TextDoc;
use crate::textdiff::Row;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOptions {
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
}

/// A match in one line: its row, the side (0 left, 1 right), and UTF-16 offsets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub row: u32,
    pub side: u8,
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub hits: Vec<Hit>,
    /// More matches exist than `hits` holds.
    pub truncated: bool,
}

/// Builds the pattern; an invalid regex is an error with a readable message.
pub fn pattern(query: &str, options: &SearchOptions) -> Result<Regex, String> {
    let mut p = if options.regex { query.to_string() } else { regex::escape(query) };
    if options.whole_word {
        p = format!(r"\b(?:{p})\b");
    }
    RegexBuilder::new(&p).case_insensitive(!options.match_case).size_limit(1 << 22).build().map_err(|e| match e {
        regex::Error::Syntax(s) => {
            let last = s.lines().last().unwrap_or(&s).trim();
            last.strip_prefix("error: ").unwrap_or(last).to_string()
        }
        e => e.to_string(),
    })
}

/// Every match in row order, left before right; empty matches are skipped.
pub fn search(rows: &[Row], docs: [&TextDoc; 2], re: &Regex, limit: usize) -> SearchResult {
    let mut hits = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        for (side, line) in [row.left, row.right].into_iter().enumerate() {
            let Some(n) = line else { continue };
            let text = docs[side].line(n as usize);
            // Byte offsets become UTF-16 offsets, counted from the previous match.
            let (mut byte, mut unit) = (0, 0u32);
            for m in re.find_iter(text) {
                if m.is_empty() {
                    continue;
                }
                if hits.len() == limit {
                    return SearchResult { hits, truncated: true };
                }
                unit += utf16(&text[byte..m.start()]);
                let start = unit;
                unit += utf16(m.as_str());
                byte = m.end();
                hits.push(Hit { row: i as u32, side: side as u8, start, end: unit });
            }
        }
    }
    SearchResult { hits, truncated: false }
}

fn utf16(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::decode;
    use crate::textdiff::{DiffOptions, diff_docs};

    fn find(a: &str, b: &str, query: &str, options: SearchOptions) -> Result<Vec<(u32, u8, u32, u32)>, String> {
        let (a, b) = (decode(a.as_bytes()), decode(b.as_bytes()));
        let diff = diff_docs(&a, &b, &DiffOptions::default());
        let re = pattern(query, &options)?;
        Ok(search(&diff.rows, [&a, &b], &re, 100).hits.into_iter().map(|h| (h.row, h.side, h.start, h.end)).collect())
    }

    #[test]
    fn finds_in_both_sides_in_row_order() {
        let (a, b) = ("one\nFoo foo\nthree\n", "one\nfoo\nthree\nfood\n");
        let hits = find(a, b, "foo", SearchOptions::default()).unwrap();
        assert_eq!(hits, [(1, 0, 0, 3), (1, 0, 4, 7), (1, 1, 0, 3), (3, 1, 0, 3)]);
        let hits = find(a, b, "foo", SearchOptions { match_case: true, ..Default::default() }).unwrap();
        assert_eq!(hits, [(1, 0, 4, 7), (1, 1, 0, 3), (3, 1, 0, 3)]);
        let hits = find(a, b, "foo", SearchOptions { whole_word: true, ..Default::default() }).unwrap();
        assert_eq!(hits.len(), 3, "{hits:?}");
    }

    #[test]
    fn offsets_are_utf16() {
        let hits = find("ä😀 x = ä\n", "", "ä", SearchOptions::default()).unwrap();
        assert_eq!(hits, [(0, 0, 0, 1), (0, 0, 8, 9)]);
        let hits = find("😀😀\n", "", "😀", SearchOptions::default()).unwrap();
        assert_eq!(hits, [(0, 0, 0, 2), (0, 0, 2, 4)]);
    }

    #[test]
    fn regex_and_errors() {
        let regex = SearchOptions { regex: true, ..Default::default() };
        let hits = find("a1 b22 c\n", "", r"\d+", regex.clone()).unwrap();
        assert_eq!(hits, [(0, 0, 1, 2), (0, 0, 4, 6)]);
        // Empty matches would never move the cursor.
        assert_eq!(find("abc\n", "", "x*", regex.clone()).unwrap(), []);
        assert_eq!(find("abc\n", "", "(", regex), Err("unclosed group".to_string()));
        // Without regex, special characters are literal.
        assert_eq!(find("a.b (c)\n", "", "(c)", SearchOptions::default()).unwrap(), [(0, 0, 4, 7)]);
    }

    #[test]
    fn stops_at_the_limit() {
        let a = decode("x\n".repeat(500).as_bytes());
        let diff = diff_docs(&a, &a, &DiffOptions::default());
        let re = pattern("x", &SearchOptions::default()).unwrap();
        let r = search(&diff.rows, [&a, &a], &re, 100);
        assert_eq!((r.hits.len(), r.truncated), (100, true));
        let r = search(&diff.rows, [&a, &a], &re, 1000);
        assert_eq!((r.hits.len(), r.truncated), (1000, false));
    }
}
