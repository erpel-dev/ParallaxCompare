//! SQLite databases, read directly from the file format.
//!
//! A database is a set of B-trees in fixed-size pages: one per table and index,
//! with the schema (`sqlite_schema`) in the tree on page 1. This reader walks
//! those trees itself instead of linking the SQLite C library: the file is
//! already in memory, only the pages a view needs are touched, the code builds
//! anywhere Rust does, and a damaged or hostile file can only produce an error.
//! WAL files are not read (what is checkpointed into the database is shown).
//!
//! Two views, both independent of the page layout so that a `VACUUM` or a
//! different page size changes nothing:
//! - **Database schema**: tables with one line per column and table constraint,
//!   their indexes and triggers, then the views, each sorted by name, SQL with
//!   whitespace and comments normalized.
//! - **Database rows**: per table a header with the columns and the row count,
//!   then the rows in rowid (or primary key) order, one SQL-like tuple per line.

use std::borrow::Cow;
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use super::gguf::{count, escape, line};
use crate::{Error, Result};

pub const MAGIC: &[u8] = b"SQLite format 3\0";

/// Rows shown per table, and over all tables.
const ROWS_PER_TABLE: u64 = 1_000;
const ROWS_TOTAL: u64 = 10_000;
/// Text longer than this many characters is shortened in the rows view.
const LONG_TEXT: usize = 200;
/// Blobs up to this size are written out as hex.
const SHORT_BLOB: usize = 32;
/// B-trees deeper than this are damaged (a real one of 4 levels holds billions of rows).
const MAX_DEPTH: usize = 32;

pub fn is_sqlite(data: &[u8]) -> bool {
    data.len() >= 100 && data.starts_with(MAGIC)
}

fn damaged() -> Error {
    Error::Unsupported("SQLite: the database is truncated or damaged".into())
}

// ---- Pages and B-trees ----

#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

struct Db<'a> {
    data: &'a [u8],
    page_size: usize,
    /// Bytes of a page available for content (the rest is reserved, e.g. for encryption).
    usable: usize,
    pages: u32,
    encoding: Encoding,
}

/// A B-tree page.
struct Node<'a> {
    page: &'a [u8],
    /// Where the header starts: 100 on page 1, after the file header.
    header: usize,
    kind: u8,
    cells: usize,
    /// The right-most child of an interior page.
    right: u32,
}

impl Node<'_> {
    fn is_leaf(&self) -> bool {
        self.kind == 10 || self.kind == 13
    }

    fn cell(&self, i: usize) -> Result<usize> {
        let at = self.header + if self.is_leaf() { 8 } else { 12 } + 2 * i;
        let offset = u16::from_be_bytes(self.page.get(at..at + 2).ok_or_else(damaged)?.try_into().unwrap());
        Some(offset as usize).filter(|&o| o < self.page.len()).ok_or_else(damaged)
    }
}

fn be32(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(data.get(at..at + 4).ok_or_else(damaged)?.try_into().unwrap()))
}

/// A SQLite varint: 1 to 9 bytes, big-endian groups of 7 bits, the 9th byte whole.
fn varint(data: &[u8], at: usize) -> Result<(u64, usize)> {
    let mut v = 0u64;
    for i in 0..9 {
        let b = *data.get(at + i).ok_or_else(damaged)?;
        if i == 8 {
            return Ok(((v << 8) | b as u64, at + 9));
        }
        v = (v << 7) | (b & 0x7F) as u64;
        if b < 0x80 {
            return Ok((v, at + i + 1));
        }
    }
    unreachable!()
}

impl<'a> Db<'a> {
    fn open(data: &'a [u8]) -> Result<Db<'a>> {
        if !is_sqlite(data) {
            return Err(Error::Unsupported("not a SQLite database".into()));
        }
        let page_size = match u16::from_be_bytes([data[16], data[17]]) {
            1 => 65536,
            n if n >= 512 && n.is_power_of_two() => n as usize,
            _ => return Err(damaged()),
        };
        let usable = page_size - data[20] as usize;
        if usable < 480 {
            return Err(damaged());
        }
        let encoding = match be32(data, 56)? {
            2 => Encoding::Utf16Le,
            3 => Encoding::Utf16Be,
            _ => Encoding::Utf8,
        };
        let pages = u32::try_from(data.len() / page_size).unwrap_or(u32::MAX);
        Ok(Db { data, page_size, usable, pages, encoding })
    }

    fn page(&self, n: u32) -> Result<&'a [u8]> {
        if n == 0 || n > self.pages {
            return Err(damaged());
        }
        let start = (n as usize - 1) * self.page_size;
        Ok(&self.data[start..start + self.page_size])
    }

    fn node(&self, n: u32) -> Result<Node<'a>> {
        let page = self.page(n)?;
        let header = if n == 1 { 100 } else { 0 };
        let kind = page[header];
        if !matches!(kind, 2 | 5 | 10 | 13) {
            return Err(damaged());
        }
        let cells = u16::from_be_bytes([page[header + 3], page[header + 4]]) as usize;
        let right = if kind == 2 || kind == 5 { be32(page, header + 8)? } else { 0 };
        Ok(Node { page, header, kind, cells, right })
    }

    /// The payload of a cell starting at `at` (after its size and rowid), with
    /// the part that spilled into overflow pages appended.
    fn payload(&self, page: &'a [u8], at: usize, size: u64, index: bool) -> Result<Cow<'a, [u8]>> {
        if size > self.data.len() as u64 {
            return Err(damaged());
        }
        let size = size as usize;
        let u = self.usable;
        let max_local = if index { (u - 12) * 64 / 255 - 23 } else { u - 35 };
        if size <= max_local {
            return page.get(at..at + size).map(Cow::Borrowed).ok_or_else(damaged);
        }
        let min_local = (u - 12) * 32 / 255 - 23;
        let k = min_local + (size - min_local) % (u - 4);
        let local = if k <= max_local { k } else { min_local };
        let mut out = Vec::with_capacity(size);
        out.extend_from_slice(page.get(at..at + local).ok_or_else(damaged)?);
        let mut next = be32(page, at + local)?;
        let mut hops = 0;
        while out.len() < size {
            hops += 1;
            if hops > self.pages {
                return Err(damaged());
            }
            let p = self.page(next)?;
            let n = (size - out.len()).min(u - 4);
            out.extend_from_slice(p.get(4..4 + n).ok_or_else(damaged)?);
            next = be32(p, 0)?;
        }
        Ok(Cow::Owned(out))
    }

    /// The entries of the B-tree at `root`, in key order.
    fn cursor(&self, root: u32, index: bool) -> Cursor<'_, 'a> {
        Cursor { db: self, stack: Vec::new(), root: Some(root), index, visited: 0 }
    }

    /// Entries of a B-tree, counted from the page headers without reading cells.
    fn count(&self, root: u32, index: bool) -> Result<u64> {
        let mut stack = vec![(root, 0usize)];
        let (mut total, mut visited) = (0u64, 0u32);
        while let Some((n, depth)) = stack.pop() {
            visited += 1;
            if visited > self.pages || depth > MAX_DEPTH {
                return Err(damaged());
            }
            let node = self.node(n)?;
            if node.kind == 2 || node.kind == 10 {
                if !index {
                    return Err(damaged());
                }
            } else if index {
                return Err(damaged());
            }
            if node.is_leaf() {
                total += node.cells as u64;
                continue;
            }
            if index {
                total += node.cells as u64; // Interior cells of an index hold entries too.
            }
            for i in 0..node.cells {
                stack.push((be32(node.page, node.cell(i)?)?, depth + 1));
            }
            stack.push((node.right, depth + 1));
        }
        Ok(total)
    }
}

/// Walks a B-tree in order. Table trees yield `(rowid, record)`, index trees
/// (also used for WITHOUT ROWID tables) `(0, record)`.
struct Cursor<'d, 'a> {
    db: &'d Db<'a>,
    /// Pages from the root down, with the next step in each: on an interior page,
    /// step `2i` descends left of cell `i` and step `2i + 1` yields cell `i` (index trees).
    stack: Vec<(Node<'a>, usize)>,
    root: Option<u32>,
    index: bool,
    visited: u32,
}

impl<'a> Cursor<'_, 'a> {
    fn descend(&mut self, n: u32) -> Result<()> {
        self.visited += 1;
        if self.visited > self.db.pages || self.stack.len() >= MAX_DEPTH {
            return Err(damaged());
        }
        let node = self.db.node(n)?;
        if (node.kind == 2 || node.kind == 10) != self.index {
            return Err(damaged());
        }
        self.stack.push((node, 0));
        Ok(())
    }

    fn step(&mut self) -> Result<Option<(i64, Cow<'a, [u8]>)>> {
        if let Some(root) = self.root.take() {
            self.descend(root)?;
        }
        loop {
            let Some((node, step)) = self.stack.last_mut() else { return Ok(None) };
            let (page, i) = (node.page, *step);
            if node.is_leaf() {
                if i >= node.cells {
                    self.stack.pop();
                    continue;
                }
                *step += 1;
                let at = node.cell(i)?;
                let (size, at) = varint(page, at)?;
                if self.index {
                    return Ok(Some((0, self.db.payload(page, at, size, true)?)));
                }
                let (rowid, at) = varint(page, at)?;
                return Ok(Some((rowid as i64, self.db.payload(page, at, size, false)?)));
            }
            if i > 2 * node.cells {
                self.stack.pop();
                continue;
            }
            *step += 1;
            let cell = i / 2;
            if i % 2 == 0 {
                let child = if cell == node.cells { node.right } else { be32(page, node.cell(cell)?)? };
                self.descend(child)?;
            } else if self.index {
                let at = node.cell(cell)? + 4;
                let (size, at) = varint(page, at)?;
                return Ok(Some((0, self.db.payload(page, at, size, true)?)));
            }
        }
    }
}

impl<'a> Iterator for Cursor<'_, 'a> {
    type Item = Result<(i64, Cow<'a, [u8]>)>;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.step().transpose();
        if matches!(item, Some(Err(_))) {
            self.stack.clear();
        }
        item
    }
}

// ---- Records ----

#[derive(Debug, Clone, PartialEq)]
enum Value<'p> {
    Null,
    Int(i64),
    Real(f64),
    Text(Cow<'p, str>),
    Blob(&'p [u8]),
}

fn decode_text(bytes: &[u8], encoding: Encoding) -> Cow<'_, str> {
    let units = |f: fn([u8; 2]) -> u16| {
        let u: Vec<u16> = bytes.chunks_exact(2).map(|c| f([c[0], c[1]])).collect();
        Cow::Owned(String::from_utf16_lossy(&u))
    };
    match encoding {
        Encoding::Utf8 => String::from_utf8_lossy(bytes),
        Encoding::Utf16Le => units(u16::from_le_bytes),
        Encoding::Utf16Be => units(u16::from_be_bytes),
    }
}

/// The values of a record: a header of serial types, then the values.
fn record(payload: &[u8], encoding: Encoding) -> Result<Vec<Value<'_>>> {
    let (header_size, mut at) = varint(payload, 0)?;
    let header_end = usize::try_from(header_size).ok().filter(|&h| h <= payload.len()).ok_or_else(damaged)?;
    let mut body = header_end;
    let mut values = Vec::new();
    while at < header_end {
        let (serial, next) = varint(payload, at)?;
        at = next;
        let int = |n: usize, body: usize| -> Result<i64> {
            let b = payload.get(body..body + n).ok_or_else(damaged)?;
            let mut v = if b[0] & 0x80 != 0 { -1i64 } else { 0 };
            for &x in b {
                v = (v << 8) | x as i64;
            }
            Ok(v)
        };
        let (value, len) = match serial {
            0 => (Value::Null, 0),
            1..=4 => (Value::Int(int(serial as usize, body)?), serial as usize),
            5 => (Value::Int(int(6, body)?), 6),
            6 => (Value::Int(int(8, body)?), 8),
            7 => (Value::Real(f64::from_bits(int(8, body)? as u64)), 8),
            8 => (Value::Int(0), 0),
            9 => (Value::Int(1), 0),
            10 | 11 => return Err(damaged()),
            n => {
                let len = usize::try_from((n - 12) / 2).map_err(|_| damaged())?;
                let bytes = payload.get(body..body.checked_add(len).ok_or_else(damaged)?).ok_or_else(damaged)?;
                if n % 2 == 0 { (Value::Blob(bytes), len) } else { (Value::Text(decode_text(bytes, encoding)), len) }
            }
        };
        body += len;
        values.push(value);
    }
    Ok(values)
}

fn sha(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

fn quote(s: &str) -> String {
    format!("'{}'", escape(s).replace('\'', "''"))
}

fn show(value: &Value) -> String {
    match value {
        Value::Null => "NULL".into(),
        Value::Int(v) => v.to_string(),
        Value::Real(v) => format!("{v:?}"),
        Value::Text(s) if s.chars().count() > LONG_TEXT => {
            let head: String = s.chars().take(LONG_TEXT / 2).collect();
            format!("{}… ({} characters, sha256:{})", quote(&head), s.chars().count(), sha(s.as_bytes()))
        }
        Value::Text(s) => quote(s),
        Value::Blob(b) if b.len() <= SHORT_BLOB => {
            format!("x'{}'", b.iter().map(|b| format!("{b:02x}")).collect::<String>())
        }
        Value::Blob(b) => format!("<blob {} bytes sha256:{}>", b.len(), sha(b)),
    }
}

// ---- SQL text ----

#[derive(Debug, Clone, Copy, PartialEq)]
enum Tok {
    Word,
    /// `"x"`, `[x]` or `` `x` ``.
    Quoted,
    Str,
    Number,
    Punct,
}

/// SQL tokens, each with whether whitespace or a comment came before it.
fn tokens(sql: &str) -> Vec<(Tok, &str, bool)> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut space) = (0, false);
    // Index of the closing delimiter of a quoted token starting at `i`, doubled quotes allowed.
    let closing = |i: usize, close: u8| {
        let mut j = i + 1;
        while j < b.len() {
            if b[j] == close {
                if close != b']' && b.get(j + 1) == Some(&close) {
                    j += 2;
                    continue;
                }
                return j + 1;
            }
            j += 1;
        }
        b.len()
    };
    while i < b.len() {
        let c = b[i];
        let (kind, end) = match c {
            c if c.is_ascii_whitespace() => {
                (space, i) = (true, i + 1);
                continue;
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                i = sql[i..].find('\n').map_or(b.len(), |n| i + n);
                space = true;
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..].find("*/").map_or(b.len(), |n| i + n + 4);
                space = true;
                continue;
            }
            b'\'' => (Tok::Str, closing(i, b'\'')),
            b'"' => (Tok::Quoted, closing(i, b'"')),
            b'`' => (Tok::Quoted, closing(i, b'`')),
            b'[' => (Tok::Quoted, closing(i, b']')),
            b'x' | b'X' if b.get(i + 1) == Some(&b'\'') => (Tok::Str, closing(i + 1, b'\'')),
            b'0'..=b'9' | b'.' if c != b'.' || b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let mut j = i + 1;
                while j < b.len()
                    && (b[j].is_ascii_alphanumeric()
                        || b[j] == b'.'
                        || ((b[j] == b'+' || b[j] == b'-') && matches!(b[j - 1], b'e' | b'E')))
                {
                    j += 1;
                }
                (Tok::Number, j)
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => {
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'$' || b[j] >= 0x80) {
                    j += 1;
                }
                (Tok::Word, j)
            }
            _ => (Tok::Punct, i + sql[i..].chars().next().map_or(1, char::len_utf8)),
        };
        out.push((kind, &sql[i..end], space));
        (i, space) = (end, false);
    }
    out
}

/// Joins tokens with single spaces where the source had whitespace, none inside
/// parentheses and one after each comma.
fn join(toks: &[(Tok, &str, bool)]) -> String {
    let mut out = String::new();
    let mut prev = "";
    for &(_, t, space) in toks {
        let want_space = !out.is_empty() && (space || prev == ",") && prev != "(" && t != ")" && t != ",";
        if want_space {
            out.push(' ');
        }
        out.push_str(t);
        prev = t;
    }
    out
}

/// SQL with comments removed and whitespace normalized.
fn normalize(sql: &str) -> String {
    let mut toks = tokens(sql);
    while toks.last().is_some_and(|t| t.1 == ";") {
        toks.pop();
    }
    join(&toks)
}

fn unquote(tok: (Tok, &str, bool)) -> String {
    match tok {
        (Tok::Quoted, t, _) if t.len() >= 2 => {
            let close = t.as_bytes()[0];
            let inner = &t[1..t.len() - 1];
            match close {
                b'"' => inner.replace("\"\"", "\""),
                b'`' => inner.replace("``", "`"),
                _ => inner.to_string(),
            }
        }
        (_, t, _) => t.to_string(),
    }
}

fn is_word(tok: Option<&(Tok, &str, bool)>, word: &str) -> bool {
    tok.is_some_and(|t| t.0 == Tok::Word && t.1.eq_ignore_ascii_case(word))
}

#[derive(Debug, Default)]
struct Column {
    name: String,
    /// The declared type, e.g. `VARCHAR(20)`.
    type_name: String,
    /// Type and constraints, normalized.
    definition: String,
    primary_key: bool,
    /// `INTEGER PRIMARY KEY` (not DESC): the column is the rowid.
    rowid_alias: bool,
    /// Generated and not stored: absent from records.
    virtual_generated: bool,
    /// The value of rows written before the column was added.
    default: Option<String>,
}

#[derive(Debug, Default)]
struct TableDef {
    columns: Vec<Column>,
    constraints: Vec<String>,
    /// What follows the column list: `WITHOUT ROWID`, `STRICT`.
    options: String,
    without_rowid: bool,
    /// Columns of the primary key, in key order (WITHOUT ROWID tables store them first).
    primary_key: Vec<usize>,
}

const CONSTRAINT_WORDS: &[&str] =
    &["CONSTRAINT", "PRIMARY", "NOT", "NULL", "UNIQUE", "CHECK", "DEFAULT", "COLLATE", "REFERENCES", "GENERATED", "AS"];

/// Parses `CREATE TABLE name (columns, constraints) options`.
fn parse_table(sql: &str) -> Option<TableDef> {
    let toks = tokens(sql);
    let open = toks.iter().position(|t| t.1 == "(")?;
    if !toks[..open].iter().any(|t| t.0 == Tok::Word && t.1.eq_ignore_ascii_case("TABLE")) {
        return None;
    }
    // Split the column list at commas outside parentheses.
    let mut parts: Vec<&[(Tok, &str, bool)]> = Vec::new();
    let (mut depth, mut start, mut close) = (0, open + 1, None);
    for (i, t) in toks.iter().enumerate().skip(open) {
        match t.1 {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    parts.push(&toks[start..i]);
                    close = Some(i);
                    break;
                }
            }
            "," if depth == 1 => {
                parts.push(&toks[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    let close = close?;
    let mut def = TableDef { options: join(&toks[close + 1..]), ..Default::default() };
    def.options = def.options.trim_end_matches(';').trim().to_string();
    def.without_rowid =
        toks[close + 1..].windows(2).any(|w| is_word(w.first(), "WITHOUT") && is_word(w.get(1), "ROWID"));
    let mut table_pk: Vec<String> = Vec::new();
    for part in parts {
        let first = part.first()?;
        let is_constraint = first.0 == Tok::Word
            && ["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"].iter().any(|w| first.1.eq_ignore_ascii_case(w));
        if is_constraint {
            if let Some(p) = part.windows(2).position(|w| is_word(w.first(), "PRIMARY") && is_word(w.get(1), "KEY")) {
                // PRIMARY KEY (a, b COLLATE x DESC): the first word of each item.
                let mut depth = 0;
                let mut expect = false;
                for t in &part[p + 2..] {
                    match t.1 {
                        "(" => {
                            depth += 1;
                            expect = depth == 1;
                        }
                        ")" => depth -= 1,
                        "," if depth == 1 => expect = true,
                        _ if expect => {
                            table_pk.push(unquote(*t).to_ascii_lowercase());
                            expect = false;
                        }
                        _ => {}
                    }
                }
            }
            def.constraints.push(join(part));
            continue;
        }
        let rest = &part[1..];
        let type_end = rest
            .iter()
            .position(|t| t.0 == Tok::Word && CONSTRAINT_WORDS.iter().any(|w| t.1.eq_ignore_ascii_case(w)))
            .unwrap_or(rest.len());
        let type_name = join(&rest[..type_end]);
        let pk = rest.windows(2).position(|w| is_word(w.first(), "PRIMARY") && is_word(w.get(1), "KEY"));
        let generated = rest.windows(2).any(|w| is_word(w.first(), "AS") && w[1].1 == "(");
        let default = rest.iter().position(|t| is_word(Some(t), "DEFAULT")).and_then(|i| {
            let value = &rest[i + 1..];
            match value {
                [(_, "(", _), inner @ .., (_, ")", _)] if inner.len() <= 2 => Some(join(inner)),
                [(_, s @ ("-" | "+"), _), (Tok::Number, n, _), ..] => Some(format!("{s}{n}")),
                [t, ..] => Some(t.1.to_string()),
                [] => None,
            }
        });
        def.columns.push(Column {
            name: unquote(*first),
            definition: join(rest),
            primary_key: pk.is_some(),
            type_name: type_name.clone(),
            rowid_alias: pk.is_some()
                && type_name.eq_ignore_ascii_case("INTEGER")
                && !pk.is_some_and(|p| is_word(rest.get(p + 2), "DESC")),
            virtual_generated: generated && !rest.iter().any(|t| is_word(Some(t), "STORED")),
            default,
        });
    }
    if !table_pk.is_empty() {
        for name in &table_pk {
            let i = def.columns.iter().position(|c| c.name.to_ascii_lowercase() == *name)?;
            def.columns[i].primary_key = true;
            def.primary_key.push(i);
        }
        if let [i] = def.primary_key[..] {
            // PRIMARY KEY (id) on an INTEGER column makes it the rowid too.
            let c = &mut def.columns[i];
            c.rowid_alias = c.type_name.eq_ignore_ascii_case("INTEGER");
        }
    } else {
        def.primary_key = def.columns.iter().enumerate().filter(|(_, c)| c.primary_key).map(|(i, _)| i).collect();
    }
    if def.without_rowid {
        def.columns.iter_mut().for_each(|c| c.rowid_alias = false);
    }
    Some(def)
}

/// The value of a `DEFAULT` literal.
fn default_value(literal: &str) -> Value<'static> {
    let t = literal.trim();
    if let Ok(v) = t.parse::<i64>() {
        return Value::Int(v);
    }
    if let Ok(v) = t.parse::<f64>() {
        return Value::Real(v);
    }
    if t.eq_ignore_ascii_case("TRUE") {
        return Value::Int(1);
    }
    if t.eq_ignore_ascii_case("FALSE") {
        return Value::Int(0);
    }
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        return Value::Text(Cow::Owned(t[1..t.len() - 1].replace("''", "'")));
    }
    Value::Null
}

// ---- Schema ----

struct Object {
    kind: String,
    name: String,
    table: String,
    root: u32,
    sql: Option<String>,
}

struct Schema<'a> {
    db: Db<'a>,
    objects: Vec<Object>,
}

impl<'a> Schema<'a> {
    fn read(data: &'a [u8]) -> Result<Schema<'a>> {
        let db = Db::open(data)?;
        let mut objects = Vec::new();
        for entry in db.cursor(1, false) {
            let (_, payload) = entry?;
            let values = record(&payload, db.encoding)?;
            let text = |i: usize| match values.get(i) {
                Some(Value::Text(s)) => Some(s.to_string()),
                _ => None,
            };
            let root = match values.get(3) {
                Some(Value::Int(n)) => u32::try_from(*n).unwrap_or(0),
                _ => 0,
            };
            objects.push(Object {
                kind: text(0).unwrap_or_default(),
                name: text(1).unwrap_or_default(),
                table: text(2).unwrap_or_default(),
                root,
                sql: text(4),
            });
        }
        objects.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Schema { db, objects })
    }

    fn header_u32(&self, at: usize) -> u32 {
        be32(self.db.data, at).unwrap_or(0)
    }

    /// Tables with rows: not virtual, with a B-tree.
    fn tables(&self) -> impl Iterator<Item = (&Object, Option<TableDef>)> {
        self.objects.iter().filter(|o| o.kind == "table" && o.root > 0).map(|o| {
            let def = if o.name == "sqlite_schema" || o.name == "sqlite_master" {
                None
            } else {
                o.sql.as_deref().and_then(parse_table)
            };
            (o, def)
        })
    }
}

/// The database schema: settings from the header, tables with their columns,
/// indexes and triggers, then views.
pub fn schema(data: &[u8]) -> Result<String> {
    let s = Schema::read(data)?;
    let mut out = String::new();
    let _ = writeln!(out, "=== Database ===");
    line(&mut out, "Page size", s.db.page_size);
    let encoding = match s.db.encoding {
        Encoding::Utf8 => "UTF-8",
        Encoding::Utf16Le => "UTF-16le",
        Encoding::Utf16Be => "UTF-16be",
    };
    line(&mut out, "Text encoding", encoding);
    line(&mut out, "User version", s.header_u32(60) as i32);
    if s.header_u32(68) != 0 {
        line(&mut out, "Application id", format!("0x{:08x}", s.header_u32(68)));
    }
    if s.header_u32(52) != 0 {
        line(&mut out, "Auto-vacuum", if s.header_u32(64) != 0 { "incremental" } else { "full" });
    }
    for kind in ["table", "index", "view", "trigger"] {
        let n = s.objects.iter().filter(|o| o.kind == kind && o.sql.is_some()).count();
        let label = match kind {
            "table" => "Tables",
            "index" => "Indexes",
            "view" => "Views",
            _ => "Triggers",
        };
        line(&mut out, label, n);
    }

    for table in s.objects.iter().filter(|o| o.kind == "table") {
        let sql = table.sql.as_deref().unwrap_or("");
        let virtual_table = table.root == 0;
        let _ = writeln!(out, "\n=== {} {} ===", if virtual_table { "Virtual table" } else { "Table" }, table.name);
        match parse_table(sql).filter(|_| !virtual_table) {
            Some(def) => {
                for c in &def.columns {
                    let _ = writeln!(out, "{} {}", c.name, c.definition);
                }
                for c in &def.constraints {
                    let _ = writeln!(out, "{c}");
                }
                if !def.options.is_empty() {
                    line(&mut out, "Options", &def.options);
                }
            }
            None => {
                let _ = writeln!(out, "{}", normalize(sql));
            }
        }
        for o in s.objects.iter().filter(|o| o.table == table.name && o.sql.is_some()) {
            match o.kind.as_str() {
                "index" => line(&mut out, "Index", normalize(o.sql.as_deref().unwrap_or(""))),
                "trigger" => line(&mut out, "Trigger", normalize(o.sql.as_deref().unwrap_or(""))),
                _ => {}
            }
        }
    }
    for view in s.objects.iter().filter(|o| o.kind == "view") {
        let _ = writeln!(out, "\n=== View {} ===", view.name);
        let _ = writeln!(out, "{}", normalize(view.sql.as_deref().unwrap_or("")));
        for o in s.objects.iter().filter(|o| o.kind == "trigger" && o.table == view.name) {
            line(&mut out, "Trigger", normalize(o.sql.as_deref().unwrap_or("")));
        }
    }
    Ok(out)
}

/// How the fields of a record map to the declared columns.
struct Layout {
    names: Vec<String>,
    /// Per shown column: the record field holding it, or `None` for the rowid.
    fields: Vec<Option<usize>>,
    defaults: Vec<Value<'static>>,
    /// Columns with REAL affinity, whose integral values SQLite stores as integers.
    real: Vec<bool>,
    index: bool,
}

/// Whether a declared type gives REAL affinity (the rules of section 3.1 of the SQLite datatype docs).
fn real_affinity(type_name: &str) -> bool {
    let t = type_name.to_ascii_uppercase();
    let has = |s: &str| t.contains(s);
    !has("INT")
        && !has("CHAR")
        && !has("CLOB")
        && !has("TEXT")
        && !has("BLOB")
        && (has("REAL") || has("FLOA") || has("DOUB"))
}

impl Layout {
    fn new(def: Option<&TableDef>) -> Layout {
        let Some(def) = def else {
            // sqlite_schema, or a table whose SQL was not understood: fields as stored.
            return Layout {
                names: Vec::new(),
                fields: Vec::new(),
                defaults: Vec::new(),
                real: Vec::new(),
                index: false,
            };
        };
        let stored: Vec<usize> = (0..def.columns.len()).filter(|&i| !def.columns[i].virtual_generated).collect();
        let mut order: Vec<usize> = Vec::new();
        if def.without_rowid {
            // The key columns come first, then the others in declared order.
            order.extend(&def.primary_key);
            order.extend(stored.iter().filter(|i| !def.primary_key.contains(i)));
        } else {
            order = stored.clone();
        }
        let mut layout = Layout {
            names: Vec::new(),
            fields: Vec::new(),
            defaults: Vec::new(),
            real: Vec::new(),
            index: def.without_rowid,
        };
        for &i in &stored {
            let c = &def.columns[i];
            layout.names.push(c.name.clone());
            layout.fields.push(if c.rowid_alias { None } else { order.iter().position(|&o| o == i) });
            layout.defaults.push(c.default.as_deref().map_or(Value::Null, default_value));
            layout.real.push(real_affinity(&c.type_name));
        }
        layout
    }

    /// The row's values in declared order.
    fn row<'p>(&'p self, rowid: i64, values: Vec<Value<'p>>) -> Vec<Value<'p>> {
        if self.names.is_empty() {
            return values;
        }
        self.fields
            .iter()
            .zip(&self.defaults)
            .zip(&self.real)
            .map(|((field, default), &real)| match field {
                None => Value::Int(rowid),
                Some(f) => match values.get(*f).cloned().unwrap_or_else(|| default.clone()) {
                    Value::Int(v) if real => Value::Real(v as f64),
                    v => v,
                },
            })
            .collect()
    }
}

/// Every table's rows in key order, up to a limit per table and in all.
pub fn rows(data: &[u8]) -> Result<String> {
    let s = Schema::read(data)?;
    let mut out = String::new();
    let mut budget = ROWS_TOTAL;
    let mut tables = 0;
    for (table, def) in s.tables() {
        tables += 1;
        let layout = Layout::new(def.as_ref());
        let total = s.db.count(table.root, layout.index)?;
        let rows = if total == 1 { "row" } else { "rows" };
        let _ =
            writeln!(out, "{}=== {} ({} {rows}) ===", if out.is_empty() { "" } else { "\n" }, table.name, count(total));
        if !layout.names.is_empty() {
            let _ = writeln!(out, "({})", layout.names.join(", "));
        }
        let limit = ROWS_PER_TABLE.min(budget);
        let mut shown = 0u64;
        for entry in s.db.cursor(table.root, layout.index).take(limit as usize) {
            let (rowid, payload) = entry?;
            let values = record(&payload, s.db.encoding)?;
            let row = layout.row(rowid, values);
            let _ = writeln!(out, "({})", row.iter().map(show).collect::<Vec<_>>().join(", "));
            shown += 1;
        }
        budget -= shown;
        if total > shown {
            let _ = writeln!(out, "… {} more rows not shown", count(total - shown));
        }
    }
    if tables == 0 {
        out.push_str("(no tables)\n");
    }
    Ok(out)
}

/// Whether two databases hold the same schema and the same rows (with the same
/// rowids), however their pages are laid out. Stops at the first difference.
/// `None` when either is not a readable database.
pub fn same_content(left: &[u8], right: &[u8]) -> Option<bool> {
    let (a, b) = (Schema::read(left).ok()?, Schema::read(right).ok()?);
    for at in [60, 68] {
        if a.header_u32(at) != b.header_u32(at) {
            return Some(false);
        }
    }
    let key = |s: &Schema| -> Vec<(String, String, String)> {
        s.objects
            .iter()
            .map(|o| (o.kind.clone(), o.name.clone(), o.sql.as_deref().map(normalize).unwrap_or_default()))
            .collect()
    };
    if key(&a) != key(&b) {
        return Some(false);
    }
    let same_encoding = a.db.encoding == b.db.encoding;
    for ((ta, def), (tb, _)) in a.tables().zip(b.tables()) {
        let layout = Layout::new(def.as_ref());
        let (mut ca, mut cb) = (a.db.cursor(ta.root, layout.index), b.db.cursor(tb.root, layout.index));
        loop {
            match (ca.next(), cb.next()) {
                (None, None) => break,
                (Some(Ok((ra, pa))), Some(Ok((rb, pb)))) => {
                    if ra != rb {
                        return Some(false);
                    }
                    if same_encoding && pa == pb {
                        continue; // The usual case: the record was copied as it was.
                    }
                    let (va, vb) = (record(&pa, a.db.encoding).ok()?, record(&pb, b.db.encoding).ok()?);
                    if layout.row(ra, va) != layout.row(rb, vb) {
                        return Some(false);
                    }
                }
                (Some(Err(_)), _) | (_, Some(Err(_))) => return None,
                _ => return Some(false),
            }
        }
    }
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sqlite/", $name))
        };
    }
    const V1: &[u8] = fixture!("shop-v1.sqlite");
    const V2: &[u8] = fixture!("shop-v2.sqlite");
    const VACUUM: &[u8] = fixture!("shop-v1-vacuum.db");
    const UTF16: &[u8] = fixture!("shop-v1-utf16.db");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn schema_view() {
        let s = schema(V1).unwrap();
        println!("{s}");
        for expected in [
            "Text encoding:     UTF-8",
            "User version:      7",
            "=== Table customers ===\nid INTEGER PRIMARY KEY\nname TEXT NOT NULL\nemail TEXT UNIQUE\nscore REAL DEFAULT 0.0\navatar BLOB\nname_upper TEXT GENERATED ALWAYS AS (upper(name)) VIRTUAL\nIndex:             CREATE INDEX idx_customers_name ON customers(name COLLATE NOCASE)\n",
            "total NUMERIC(10, 2)\nnote TEXT\nCHECK (total >= 0)\nIndex:             CREATE INDEX idx_orders_customer ON orders (customer_id)\nTrigger:           CREATE TRIGGER orders_log AFTER INSERT ON orders BEGIN INSERT INTO log (message, at) VALUES ('order ' || new.id, 0); END\n",
            "PRIMARY KEY (scope, key)\nOptions:           WITHOUT ROWID\n",
            "=== View big_orders ===\nCREATE VIEW big_orders AS SELECT c.name, o.total FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.total > 100\n",
            "=== Table sqlite_sequence ===",
        ] {
            assert!(s.contains(expected), "{expected}\n---\n{s}");
        }
        assert!(!s.contains("rowid alias"), "comments are removed");
    }

    #[test]
    fn rows_view() {
        let r = rows(V1).unwrap();
        println!("{r}");
        for expected in [
            "=== customers (5 rows) ===\n(id, name, email, score, avatar)\n(1, 'Alice', 'alice@example.com', 4.5, x'010203')\n(2, 'Bob', NULL, NULL, NULL)\n",
            "(3, 'Zoë Ünïcode 日本語', 'zoe@example.com', 3.25, <blob 1024 bytes sha256:",
            "(4, 'Dave', 'dave@example.com', 0.0, x'')",
            "(5, 'Line\\nbreak ''quoted''', NULL, -1.5, NULL)",
            "=== orders (40 rows) ===",
            "(7, 3, 87.5, 'xxxxx",
            "… (9000 characters, sha256:",
            "(10, 1, 125, NULL)",
            "(11, 2, 137.5, 'note 11')",
            "=== settings (4 rows) ===\n(key, scope, value)\n('ratio', 'global', 0.75)\n('timeout', 'global', 30)\n('lang', 'user', 'de')\n('theme', 'user', 'dark')\n",
            "=== log (40 rows) ===",
            "=== sqlite_sequence (1 row) ===\n(name, seq)\n('orders', 40)",
        ] {
            assert!(r.contains(expected), "{expected}\n---\n{r}");
        }
    }

    #[test]
    fn edits_show_as_changed_lines_only() {
        let (r1, r2) = (rows(V1).unwrap(), rows(V2).unwrap());
        assert_eq!(
            lines_only_in(&r1, &r2),
            [
                "(id, name, email, score, avatar)",
                "(1, 'Alice', 'alice@example.com', 4.5, x'010203')",
                "(2, 'Bob', NULL, NULL, NULL)",
                "(3, 'Zoë Ünïcode 日本語', 'zoe@example.com', 3.25, <blob 1024 bytes sha256:785b0751fc2c>)",
                "(4, 'Dave', 'dave@example.com', 0.0, x'')",
                "(5, 'Line\\nbreak ''quoted''', NULL, -1.5, NULL)",
                "=== orders (40 rows) ===",
                "(20, 1, 250, NULL)",
                "('theme', 'user', 'dark')",
            ]
        );
        assert!(r2.contains("(1, 'Alice', 'alice@example.com', 4.5, x'010203', 0)"), "added column: default");
        assert!(r2.contains("(2, 'Bob', 'bob@example.com', NULL, NULL, 0)"), "{r2}");
        let (s1, s2) = (schema(V1).unwrap(), schema(V2).unwrap());
        assert_eq!(lines_only_in(&s2, &s1), ["vip INTEGER DEFAULT 0"]);
    }

    #[test]
    fn page_layout_and_encoding_do_not_matter() {
        assert_eq!(rows(V1).unwrap(), rows(VACUUM).unwrap());
        let (s1, sv) = (schema(V1).unwrap(), schema(VACUUM).unwrap());
        assert_eq!(lines_only_in(&sv, &s1), ["Page size:         1024"]);
        assert_eq!(rows(V1).unwrap(), rows(UTF16).unwrap());
        assert_eq!(same_content(V1, VACUUM), Some(true));
        assert_eq!(same_content(V1, UTF16), Some(true));
        assert_eq!(same_content(V1, V2), Some(false));
        assert_eq!(same_content(V1, b"SQLite format 3\0"), None);
    }

    #[test]
    fn damaged_files_are_errors() {
        for cut in [0, 50, 100, 1000, 4096, 5000, 20_000, V1.len() - 1] {
            let _ = schema(&V1[..cut]);
            let _ = rows(&V1[..cut]);
        }
        // Garbage pages: every page overwritten in turn with a pattern.
        for page in 0..V1.len() / 4096 {
            for pattern in [0x00u8, 0xFF, 0x0D, 0x05] {
                let mut bad = V1.to_vec();
                let start = page * 4096 + if page == 0 { 100 } else { 0 };
                bad[start..(page + 1) * 4096].iter_mut().enumerate().for_each(|(i, b)| *b = pattern ^ (i as u8 & 1));
                let _ = schema(&bad);
                let _ = rows(&bad);
                let _ = same_content(V1, &bad);
            }
        }
        assert!(schema(b"SQLite format 3\0").is_err());
    }
}
