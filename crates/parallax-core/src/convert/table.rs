//! CSV and TSV files as a table sorted by its key column.
//!
//! Exports from databases and spreadsheets often come back in another row order,
//! with other quoting or another delimiter, so a text diff shows everything as
//! changed. The table view parses the file (quoted fields, doubled quotes,
//! line breaks inside quotes, `,` `;` tab or `|` as sniffed from the first lines;
//! encoding and BOM as for text), writes the header, then one line per row sorted
//! by the key column: the first column whose values are unique, else the first
//! column. Cells are joined by ` │ ` without padding, so a longer value changes
//! only its own row. Numbers sort as numbers when every key is one.
//!
//! The first [`MAX_ROWS`] rows are sorted and shown; the rest are counted.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt::Write as _;

use crate::Result;

/// Rows read, sorted and shown; beyond this the rows are only counted.
const MAX_ROWS: usize = 200_000;
/// Columns tried as the key column.
const KEY_CANDIDATES: usize = 5;
const SEPARATOR: &str = " │ ";

pub fn is_table(ext: &str) -> bool {
    matches!(ext, "csv" | "tsv")
}

/// Parsed table: header, rows (as their cells), rows not read, delimiter.
struct Table<'a> {
    header: Vec<Cow<'a, str>>,
    rows: Vec<Vec<Cow<'a, str>>>,
    more: usize,
    delimiter: u8,
}

/// Picks the delimiter that splits the first lines into the same, largest number of fields.
fn sniff(text: &str, ext: &str) -> u8 {
    if ext == "tsv" {
        return b'\t';
    }
    let mut best = (b',', 0usize, 0usize);
    for d in *b",;\t|" {
        let mut reader = Reader { s: text, pos: 0, delimiter: d };
        let mut cells = Vec::new();
        let mut counts = Vec::new();
        while counts.len() < 20 && reader.next_row(&mut cells) {
            counts.push(cells.len());
        }
        let Some(&first) = counts.first() else { continue };
        let consistent = counts.iter().filter(|&&c| c == first).count();
        if first > 1 && (consistent, first) > (best.1, best.2) {
            best = (d, consistent, first);
        }
    }
    best.0
}

struct Reader<'a> {
    s: &'a str,
    pos: usize,
    delimiter: u8,
}

impl<'a> Reader<'a> {
    /// Reads the next non-empty record into `cells`; false at the end.
    fn next_row(&mut self, cells: &mut Vec<Cow<'a, str>>) -> bool {
        let b = self.s.as_bytes();
        loop {
            cells.clear();
            if self.pos >= b.len() {
                return false;
            }
            // Blank lines are not rows.
            if b[self.pos] == b'\n' || b[self.pos] == b'\r' {
                self.pos += 1;
                continue;
            }
            loop {
                let (cell, end) = self.cell();
                cells.push(cell);
                match end {
                    Some(d) if d == self.delimiter => continue,
                    _ => return true,
                }
            }
        }
    }

    /// One field and the byte that ended it (delimiter, newline, or none at the end).
    fn cell(&mut self) -> (Cow<'a, str>, Option<u8>) {
        let b = self.s.as_bytes();
        let start = self.pos;
        let mut owned: Option<String> = None;
        if b.get(start) == Some(&b'"') {
            let mut i = start + 1;
            let mut piece = i;
            loop {
                match memchr::memchr(b'"', &b[i..]) {
                    None => {
                        // Unterminated quote: the rest of the file is the field.
                        let rest = &self.s[piece..];
                        self.pos = b.len();
                        return (owned.map_or(Cow::Borrowed(rest), |o| Cow::Owned(o + rest)), None);
                    }
                    Some(q) if b.get(i + q + 1) == Some(&b'"') => {
                        owned.get_or_insert_with(String::new).push_str(&self.s[piece..i + q + 1]);
                        i += q + 2;
                        piece = i;
                    }
                    Some(q) => {
                        let text = &self.s[piece..i + q];
                        let value = match owned {
                            Some(mut o) => {
                                o.push_str(text);
                                Cow::Owned(o)
                            }
                            None => Cow::Borrowed(text),
                        };
                        self.pos = i + q + 1;
                        // Anything between the closing quote and the delimiter is kept.
                        let (tail, end) = self.unquoted();
                        let value = if tail.is_empty() { value } else { Cow::Owned(value.into_owned() + tail) };
                        return (value, end);
                    }
                }
            }
        }
        let (text, end) = self.unquoted();
        (Cow::Borrowed(text), end)
    }

    fn unquoted(&mut self) -> (&'a str, Option<u8>) {
        let b = self.s.as_bytes();
        let start = self.pos;
        let end = memchr::memchr3(self.delimiter, b'\n', b'\r', &b[start..]).map_or(b.len(), |i| start + i);
        let text = &self.s[start..end];
        let stop = b.get(end).copied();
        self.pos = match stop {
            Some(b'\r') if b.get(end + 1) == Some(&b'\n') => end + 2,
            Some(_) => end + 1,
            None => end,
        };
        (text, stop)
    }
}

fn parse<'a>(text: &'a str, ext: &str) -> Table<'a> {
    let delimiter = sniff(text, ext);
    let mut reader = Reader { s: text, pos: 0, delimiter };
    let mut header = Vec::new();
    reader.next_row(&mut header);
    let mut rows = Vec::new();
    let mut cells = Vec::new();
    while rows.len() < MAX_ROWS && reader.next_row(&mut cells) {
        rows.push(std::mem::take(&mut cells));
    }
    let mut more = 0;
    while reader.next_row(&mut cells) {
        more += 1;
    }
    Table { header, rows, more, delimiter }
}

/// A cell on one line: line breaks inside quotes become `\n`.
fn cell_text(cell: &str, out: &mut String) {
    if cell.contains(['\n', '\r']) {
        out.push_str(&cell.replace("\r\n", "\\n").replace('\n', "\\n").replace('\r', "\\r"));
    } else {
        out.push_str(cell);
    }
}

fn row_text(cells: &[Cow<str>]) -> String {
    let mut out = String::with_capacity(cells.iter().map(|c| c.len() + 3).sum());
    for (i, c) in cells.iter().enumerate() {
        if i > 0 {
            out.push_str(SEPARATOR);
        }
        cell_text(c, &mut out);
    }
    out
}

/// The first of the leading columns whose values are all present and unique.
fn key_column(table: &Table) -> Option<usize> {
    let columns = table.header.len().min(KEY_CANDIDATES);
    (0..columns).find(|&c| {
        let mut seen = HashSet::with_capacity(table.rows.len());
        table.rows.iter().all(|r| r.get(c).is_some_and(|v| !v.is_empty() && seen.insert(v.as_ref())))
    })
}

fn number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E')) {
        return None;
    }
    t.parse().ok()
}

/// The rows sorted by the key column (numerically when every key is a number), then by the whole row.
fn sorted_rows(table: &Table, key: usize) -> Vec<String> {
    let keys: Vec<&str> = table.rows.iter().map(|r| r.get(key).map_or("", |c| c.as_ref())).collect();
    let numbers: Option<Vec<f64>> = keys.iter().map(|k| number(k)).collect();
    let mut order: Vec<(usize, String)> = table.rows.iter().map(|r| row_text(r)).enumerate().collect();
    order.sort_by(|(a, ra), (b, rb)| {
        let by_key = match &numbers {
            Some(n) => n[*a].total_cmp(&n[*b]),
            None => keys[*a].cmp(keys[*b]),
        };
        if by_key == Ordering::Equal { ra.cmp(rb) } else { by_key }
    });
    order.into_iter().map(|(_, r)| r).collect()
}

fn delimiter_name(d: u8) -> &'static str {
    match d {
        b',' => "comma",
        b';' => "semicolon",
        b'\t' => "tab",
        _ => "vertical bar",
    }
}

/// The header and the rows sorted by the key column.
pub fn sorted(ext: &str, data: &[u8]) -> Result<String> {
    let text = crate::text::decode(data).text;
    let table = parse(&text, ext);
    let key = key_column(&table);
    let rows = sorted_rows(&table, key.unwrap_or(0));
    let mut out = String::with_capacity(rows.iter().map(|r| r.len() + 1).sum::<usize>() + 300);
    let line = |out: &mut String, label: &str, value: &dyn std::fmt::Display| {
        let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
    };
    line(&mut out, "Delimiter", &delimiter_name(table.delimiter));
    line(&mut out, "Columns", &table.header.len());
    line(&mut out, "Rows", &(table.rows.len() + table.more));
    let column_name = |c: usize| table.header.get(c).map_or(String::new(), |h| h.to_string());
    match key {
        Some(c) => line(&mut out, "Sorted by", &format!("{} (column {}, unique)", column_name(c), c + 1)),
        None if !table.rows.is_empty() => {
            line(&mut out, "Sorted by", &format!("{} (column 1, not unique)", column_name(0)))
        }
        None => {}
    }
    let _ = writeln!(out, "\n=== {} ===", row_text(&table.header));
    for r in &rows {
        out.push_str(r);
        out.push('\n');
    }
    if table.more > 0 {
        let _ = writeln!(out, "… {} more rows, not shown (only the first {MAX_ROWS} are sorted)", table.more);
    }
    Ok(out)
}

/// Whether two tables have the same header and rows: the reason when they do,
/// in the same order or another. `None` when they differ, or are too large to check.
pub fn same_rows(ext: &str, left: &[u8], right: &[u8]) -> Option<&'static str> {
    let (lt, rt) = (crate::text::decode(left).text, crate::text::decode(right).text);
    let (l, r) = (parse(&lt, ext), parse(&rt, ext));
    if l.more > 0 || r.more > 0 || l.rows.len() != r.rows.len() || l.header != r.header {
        return None;
    }
    if l.rows == r.rows {
        return Some("same table, only quoting, delimiter or line endings differ");
    }
    let mut a: Vec<&Vec<Cow<str>>> = l.rows.iter().collect();
    let mut b: Vec<&Vec<Cow<str>>> = r.rows.iter().collect();
    a.sort_unstable();
    b.sort_unstable();
    (a == b).then_some("same rows, different order")
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/table/", $name))
        };
    }
    const PRODUCTS: &[u8] = fixture!("products.csv");

    fn rows(s: &str) -> Vec<&str> {
        s.split(" ===\n").nth(1).unwrap().lines().collect()
    }

    #[test]
    fn parses_quotes_and_sorts_by_key() {
        let t = sorted("csv", b"id,name,note\n10,b,\"x, \"\"y\"\"\"\n9,a,\"two\nlines\"\n\n11,c,\n").unwrap();
        assert_eq!(
            t,
            "Delimiter:         comma\nColumns:           3\nRows:              3\nSorted by:         id (column 1, unique)\n\n=== id │ name │ note ===\n9 │ a │ two\\nlines\n10 │ b │ x, \"y\"\n11 │ c │ \n"
        );
        // Semicolons, CRLF and a BOM; the first column repeats, the second is unique.
        let t = sorted("csv", b"\xEF\xBB\xBFgroup;sku;price\r\nb;A-2;1,5\r\na;A-1;2\r\nb;A-0;3\r\n").unwrap();
        assert!(t.contains("Delimiter:         semicolon\n"), "{t}");
        assert!(t.contains("Sorted by:         sku (column 2, unique)\n"), "{t}");
        assert_eq!(rows(&t), ["b │ A-0 │ 3", "a │ A-1 │ 2", "b │ A-2 │ 1,5"]);
        assert!(sorted("tsv", b"a\tb\n2\tx\n1\ty\n").unwrap().ends_with("=== a │ b ===\n1 │ y\n2 │ x\n"));
    }

    #[test]
    fn exports_in_another_order() {
        let (a, b) = (fixture!("products-shuffled.csv"), fixture!("products-edited.csv"));
        assert_eq!(same_rows("csv", PRODUCTS, a), Some("same rows, different order"));
        assert_eq!(sorted("csv", PRODUCTS).unwrap(), sorted("csv", a).unwrap());
        assert_eq!(
            same_rows("csv", PRODUCTS, fixture!("products-semicolon.csv")),
            Some("same table, only quoting, delimiter or line endings differ")
        );
        assert_eq!(same_rows("csv", PRODUCTS, b), None);
        let (x, y) = (sorted("csv", PRODUCTS).unwrap(), sorted("csv", b).unwrap());
        let changed: Vec<&str> = rows(&x).into_iter().filter(|l| !rows(&y).contains(l)).collect();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert!(changed[0].starts_with("1017 │ "), "{changed:?}");
    }

    #[test]
    fn caps_and_damaged_input() {
        let mut big = String::from("n,v\n");
        for i in 0..MAX_ROWS + 5 {
            let _ = writeln!(big, "{},x", MAX_ROWS + 5 - i);
        }
        let t = sorted("csv", big.as_bytes()).unwrap();
        assert!(
            t.ends_with("… 5 more rows, not shown (only the first 200000 are sorted)\n"),
            "{}",
            &t[t.len() - 200..]
        );
        assert!(t.contains("\n6 │ x\n") && !t.contains("\n5 │ x\n"));
        assert_eq!(same_rows("csv", big.as_bytes(), big.as_bytes()), None);
        for n in [0, 1, 5, PRODUCTS.len() / 2] {
            let _ = sorted("csv", &PRODUCTS[..n]).unwrap();
        }
        let _ = sorted("csv", b"\"unterminated,\n1,2").unwrap();
        let _ = sorted("csv", b"\xff\xfe\x00garbage").unwrap();
        assert!(sorted("csv", b"").unwrap().contains("Rows:              0\n"));
    }
}
