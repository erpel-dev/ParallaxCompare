//! Parquet and Arrow IPC (Feather v2) data files, summarized from their footers.
//!
//! Both formats keep a description of the whole file at its end: Parquet a
//! Thrift-encoded `FileMetaData` (schema, row groups, per column chunk the
//! codec, sizes and min/max statistics), Arrow IPC a FlatBuffers `Footer` (schema
//! and the location of each record batch). Both are decoded here by hand: the
//! subset needed is small, while the `parquet` and `arrow` crates would pull in
//! a large dependency tree. Only the footer (and for Arrow the small header of
//! each record batch) is read, so a gigabyte file is summarized in milliseconds.
//!
//! There is no rows view: the data pages are compressed (snappy, zstd, lz4,
//! gzip …) and encoded (dictionary, RLE, delta), which would take most of a
//! Parquet implementation. The statistics show where values changed.

use std::fmt::Write as _;

use super::gguf::{count, escape, line, short_hash, size};
use crate::{Error, Result};

pub const PARQUET_MAGIC: &[u8] = b"PAR1";
pub const ARROW_MAGIC: &[u8] = b"ARROW1";

/// Lines of per-row-group detail before the rest is summarized.
const MAX_CHUNK_LINES: usize = 5_000;
/// Record batches listed one per line.
const MAX_BATCH_LINES: usize = 1_000;
/// Metadata values longer than this are shortened.
const LONG_VALUE: usize = 160;
/// Statistics strings longer than this are shortened.
const LONG_STAT: usize = 60;

pub fn is_parquet(data: &[u8]) -> bool {
    data.len() >= 12 && data.starts_with(PARQUET_MAGIC) && data.ends_with(PARQUET_MAGIC)
}

pub fn is_arrow(data: &[u8]) -> bool {
    data.len() >= 16 && data.starts_with(ARROW_MAGIC) && data.ends_with(ARROW_MAGIC)
}

fn damaged(what: &str) -> Error {
    Error::Unsupported(format!("{what}: the footer is truncated or damaged"))
}

/// The summary of a Parquet or Arrow IPC file.
pub fn summary(data: &[u8]) -> Result<String> {
    if is_parquet(data) {
        parquet_summary(data)
    } else if is_arrow(data) {
        arrow_summary(data)
    } else {
        Err(Error::Unsupported("not a Parquet or Arrow file".into()))
    }
}

fn metadata_value(v: &str) -> String {
    if v.chars().count() > LONG_VALUE {
        let head: String = v.chars().take(80).collect();
        format!("\"{}…\" ({} characters, crc {})", escape(&head), v.chars().count(), short_hash(v.as_bytes()))
    } else {
        format!("\"{}\"", escape(v))
    }
}

fn key_values(out: &mut String, mut kv: Vec<(String, String)>) {
    if kv.is_empty() {
        return;
    }
    kv.sort();
    let _ = writeln!(out, "\n=== Key/value metadata ===");
    for (k, v) in kv {
        let _ = writeln!(out, "{} = {}", escape(&k), metadata_value(&v));
    }
}

// ---- Thrift compact protocol ----

#[derive(Debug)]
enum T<'a> {
    Bool(bool),
    Int(i64),
    Double,
    Bin(&'a [u8]),
    List(Vec<T<'a>>),
    Struct(Vec<(i16, T<'a>)>),
    Map,
}

static EMPTY: Vec<T<'static>> = Vec::new();

impl<'a> T<'a> {
    fn field(&self, id: i16) -> Option<&T<'a>> {
        match self {
            T::Struct(fields) => fields.iter().find(|(i, _)| *i == id).map(|(_, v)| v),
            _ => None,
        }
    }

    fn int(&self, id: i16) -> Option<i64> {
        match self.field(id)? {
            T::Int(v) => Some(*v),
            _ => None,
        }
    }

    fn bool(&self, id: i16) -> Option<bool> {
        match self.field(id)? {
            T::Bool(v) => Some(*v),
            _ => None,
        }
    }

    fn bin(&self, id: i16) -> Option<&'a [u8]> {
        match self.field(id)? {
            T::Bin(v) => Some(v),
            _ => None,
        }
    }

    fn str(&self, id: i16) -> Option<String> {
        self.bin(id).map(|b| String::from_utf8_lossy(b).into_owned())
    }

    fn list(&self, id: i16) -> &[T<'a>] {
        match self.field(id) {
            Some(T::List(items)) => items,
            _ => &EMPTY,
        }
    }

    /// The field id set in a union.
    fn variant(&self) -> Option<(i16, &T<'a>)> {
        match self {
            T::Struct(fields) => fields.first().map(|(i, v)| (*i, v)),
            _ => None,
        }
    }
}

struct Thrift<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Thrift<'a> {
    fn byte(&mut self) -> Result<u8> {
        let b = *self.data.get(self.pos).ok_or_else(|| damaged("Parquet"))?;
        self.pos += 1;
        Ok(b)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..70).step_by(7) {
            let b = self.byte()?;
            v |= ((b & 0x7F) as u64) << shift.min(63);
            if b < 0x80 {
                return Ok(v);
            }
        }
        Err(damaged("Parquet"))
    }

    fn zigzag(&mut self) -> Result<i64> {
        let v = self.varint()?;
        Ok((v >> 1) as i64 ^ -((v & 1) as i64))
    }

    /// A count of items that each take at least one byte.
    fn len(&mut self, n: u64) -> Result<usize> {
        usize::try_from(n).ok().filter(|&n| n <= self.data.len() - self.pos).ok_or_else(|| damaged("Parquet"))
    }

    fn value(&mut self, kind: u8, depth: u32) -> Result<T<'a>> {
        if depth > 32 {
            return Err(damaged("Parquet"));
        }
        Ok(match kind {
            1 => T::Bool(true),
            2 => T::Bool(false),
            3 => T::Int(self.byte()? as i8 as i64),
            4..=6 => T::Int(self.zigzag()?),
            7 => {
                self.data.get(self.pos..self.pos + 8).ok_or_else(|| damaged("Parquet"))?;
                self.pos += 8;
                T::Double
            }
            8 => {
                let n = self.varint()?;
                let n = self.len(n)?;
                self.pos += n;
                T::Bin(&self.data[self.pos - n..self.pos])
            }
            9 | 10 => {
                let header = self.byte()?;
                let n = if header >> 4 == 15 { self.varint()? } else { (header >> 4) as u64 };
                let n = self.len(n)?;
                let item = header & 0x0F;
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(if item == 1 || item == 2 {
                        T::Bool(self.byte()? == 1)
                    } else {
                        self.value(item, depth + 1)?
                    });
                }
                T::List(items)
            }
            11 => {
                let n = self.varint()?;
                let n = self.len(n)?;
                let types = if n > 0 { self.byte()? } else { 0 };
                for _ in 0..n {
                    self.value(types >> 4, depth + 1)?;
                    self.value(types & 0x0F, depth + 1)?;
                }
                T::Map
            }
            12 => {
                let mut fields = Vec::new();
                let mut id = 0i16;
                loop {
                    let header = self.byte()?;
                    if header == 0 {
                        break;
                    }
                    id = match header >> 4 {
                        0 => i16::try_from(self.zigzag()?).map_err(|_| damaged("Parquet"))?,
                        delta => id.wrapping_add(delta as i16),
                    };
                    fields.push((id, self.value(header & 0x0F, depth + 1)?));
                }
                T::Struct(fields)
            }
            _ => return Err(damaged("Parquet")),
        })
    }
}

// ---- Parquet ----

const TYPES: [&str; 8] =
    ["BOOLEAN", "INT32", "INT64", "INT96", "FLOAT", "DOUBLE", "BYTE_ARRAY", "FIXED_LEN_BYTE_ARRAY"];
const CODECS: [&str; 8] = ["UNCOMPRESSED", "SNAPPY", "GZIP", "LZO", "BROTLI", "LZ4", "ZSTD", "LZ4_RAW"];
const ENCODINGS: [&str; 10] = [
    "PLAIN",
    "GROUP_VAR_INT",
    "PLAIN_DICTIONARY",
    "RLE",
    "BIT_PACKED",
    "DELTA_BINARY_PACKED",
    "DELTA_LENGTH_BYTE_ARRAY",
    "DELTA_BYTE_ARRAY",
    "RLE_DICTIONARY",
    "BYTE_STREAM_SPLIT",
];
const CONVERTED: [&str; 22] = [
    "UTF8",
    "MAP",
    "MAP_KEY_VALUE",
    "LIST",
    "ENUM",
    "DECIMAL",
    "DATE",
    "TIME_MILLIS",
    "TIME_MICROS",
    "TIMESTAMP_MILLIS",
    "TIMESTAMP_MICROS",
    "UINT_8",
    "UINT_16",
    "UINT_32",
    "UINT_64",
    "INT_8",
    "INT_16",
    "INT_32",
    "INT_64",
    "JSON",
    "BSON",
    "INTERVAL",
];

fn name_of(names: &[&'static str], id: Option<i64>) -> String {
    match id {
        Some(i) => {
            usize::try_from(i).ok().and_then(|i| names.get(i)).map_or_else(|| format!("#{i}"), |s| s.to_string())
        }
        None => "?".into(),
    }
}

/// How a leaf column's values are interpreted, for showing statistics.
#[derive(Clone, Copy, Default)]
enum Interp {
    #[default]
    Plain,
    String,
    Unsigned,
    Decimal(u32),
    Date,
    /// Time or timestamp, with the unit as a power of ten per second.
    Time(u32, bool),
    Uuid,
}

struct Leaf {
    path: String,
    physical: Option<i64>,
    interp: Interp,
}

fn time_unit(unit: Option<&T>) -> (u32, &'static str) {
    match unit.and_then(T::variant).map(|v| v.0) {
        Some(1) => (3, "MILLIS"),
        Some(2) => (6, "MICROS"),
        Some(3) => (9, "NANOS"),
        _ => (0, "?"),
    }
}

/// The logical (or legacy converted) type of a schema element, and how its statistics read.
fn logical_type(e: &T) -> (String, Interp) {
    if let Some((id, v)) = e.field(10).and_then(T::variant) {
        let utc = |v: &T| if v.bool(1).unwrap_or(false) { "UTC" } else { "local" };
        return match id {
            1 => ("STRING".into(), Interp::String),
            2 => ("MAP".into(), Interp::Plain),
            3 => ("LIST".into(), Interp::Plain),
            4 => ("ENUM".into(), Interp::String),
            5 => {
                let (s, p) = (v.int(1).unwrap_or(0), v.int(2).unwrap_or(0));
                (format!("DECIMAL({p}, {s})"), Interp::Decimal(u32::try_from(s).unwrap_or(0)))
            }
            6 => ("DATE".into(), Interp::Date),
            7 | 8 => {
                let (digits, unit) = time_unit(v.field(2));
                let kind = if id == 7 { "TIME" } else { "TIMESTAMP" };
                (format!("{kind}({unit}, {})", utc(v)), Interp::Time(digits, id == 8))
            }
            10 => {
                let (bits, signed) = (v.int(1).unwrap_or(0), v.bool(2).unwrap_or(true));
                let interp = if signed { Interp::Plain } else { Interp::Unsigned };
                (format!("INT({bits}, {})", if signed { "signed" } else { "unsigned" }), interp)
            }
            11 => ("NULL".into(), Interp::Plain),
            12 => ("JSON".into(), Interp::String),
            13 => ("BSON".into(), Interp::Plain),
            14 => ("UUID".into(), Interp::Uuid),
            15 => ("FLOAT16".into(), Interp::Plain),
            16 => ("VARIANT".into(), Interp::Plain),
            17 => ("GEOMETRY".into(), Interp::Plain),
            18 => ("GEOGRAPHY".into(), Interp::Plain),
            n => (format!("logical type {n}"), Interp::Plain),
        };
    }
    let Some(c) = e.int(6) else { return (String::new(), Interp::Plain) };
    let name = name_of(&CONVERTED, Some(c));
    let interp = match c {
        0 | 4 | 19 => Interp::String,
        5 => Interp::Decimal(u32::try_from(e.int(7).unwrap_or(0)).unwrap_or(0)),
        6 => Interp::Date,
        7 => Interp::Time(3, false),
        8 => Interp::Time(6, false),
        9 => Interp::Time(3, true),
        10 => Interp::Time(6, true),
        11..=14 => Interp::Unsigned,
        _ => Interp::Plain,
    };
    if c == 5 {
        return (format!("DECIMAL({}, {})", e.int(8).unwrap_or(0), e.int(7).unwrap_or(0)), interp);
    }
    (name, interp)
}

/// The schema as an indented tree; collects the leaf columns in order.
fn parquet_schema(elements: &[T], out: &mut String) -> Result<Vec<Leaf>> {
    let mut leaves = Vec::new();
    // (children left, path) per open group; the root's name is not part of paths.
    let mut stack: Vec<(i64, String)> = Vec::new();
    for (i, e) in elements.iter().enumerate() {
        let children = e.int(5).unwrap_or(0);
        let name = e.str(4).unwrap_or_default();
        if i == 0 {
            stack.push((children, String::new()));
            continue;
        }
        while stack.last().is_some_and(|s| s.0 == 0) {
            stack.pop();
        }
        let Some(parent) = stack.last_mut() else { return Err(damaged("Parquet")) };
        parent.0 -= 1;
        let path = if parent.1.is_empty() { name.clone() } else { format!("{}.{name}", parent.1) };
        let depth = stack.len() - 1;
        let repetition = match e.int(3) {
            Some(0) => "required",
            Some(1) => "optional",
            Some(2) => "repeated",
            _ => "",
        };
        let (logical, interp) = logical_type(e);
        let mut text = format!("{}{}: {repetition}", "  ".repeat(depth.min(32)), escape(&name));
        if children > 0 || e.int(1).is_none() {
            text.push_str(" group");
        } else {
            let _ = write!(text, " {}", name_of(&TYPES, e.int(1)));
            if e.int(1) == Some(7) {
                let _ = write!(text, "({})", e.int(2).unwrap_or(0));
            }
        }
        if !logical.is_empty() {
            let _ = write!(text, " {logical}");
        }
        if let Some(id) = e.int(9) {
            let _ = write!(text, " (field id {id})");
        }
        let _ = writeln!(out, "{text}");
        if children > 0 {
            stack.push((children, path));
        } else {
            leaves.push(Leaf { path, physical: e.int(1), interp });
        }
    }
    Ok(leaves)
}

/// Days since 1970-01-01 as a date (Howard Hinnant's civil_from_days).
fn date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A time of day or a timestamp in units of 10^-`digits` seconds.
fn time(v: i64, digits: u32, timestamp: bool) -> String {
    let per_second = 10i64.pow(digits);
    let (secs, frac) = (v.div_euclid(per_second), v.rem_euclid(per_second));
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let mut s = format!("{:02}:{:02}:{:02}", sod / 3600, sod / 60 % 60, sod % 60);
    if frac != 0 {
        let _ = write!(s, ".{frac:0w$}", w = digits as usize);
    }
    if timestamp { format!("{}T{s}", date(days)) } else { s }
}

fn decimal(v: i128, scale: u32) -> String {
    if scale == 0 || scale > 38 {
        return v.to_string();
    }
    let digits = format!("{:0w$}", v.unsigned_abs(), w = scale as usize + 1);
    let (int, frac) = digits.split_at(digits.len() - scale as usize);
    format!("{}{int}.{frac}", if v < 0 { "-" } else { "" })
}

fn hex(b: &[u8]) -> String {
    let shown: String = b.iter().take(32).map(|b| format!("{b:02x}")).collect();
    if b.len() > 32 { format!("0x{shown}… ({} bytes)", b.len()) } else { format!("0x{shown}") }
}

/// A statistics value, decoded by the column's physical and logical type.
fn stat(b: &[u8], leaf: &Leaf) -> String {
    let int = |n: usize| -> Option<i64> {
        let bytes: [u8; 8] = match n {
            4 => {
                let v = i32::from_le_bytes(b.try_into().ok()?);
                (v as i64).to_le_bytes()
            }
            _ => b.try_into().ok()?,
        };
        Some(i64::from_le_bytes(bytes))
    };
    let shown = match (leaf.physical, leaf.interp) {
        (Some(0), _) => b.first().map(|&v| (v != 0).to_string()),
        (Some(1), Interp::Unsigned) => int(4).map(|v| (v as u32).to_string()),
        (Some(2), Interp::Unsigned) => int(8).map(|v| (v as u64).to_string()),
        (Some(1), Interp::Date) => int(4).map(date),
        (Some(1 | 2), Interp::Decimal(s)) => {
            int(if leaf.physical == Some(1) { 4 } else { 8 }).map(|v| decimal(v.into(), s))
        }
        (Some(1 | 2), Interp::Time(d, ts)) => int(if leaf.physical == Some(1) { 4 } else { 8 }).map(|v| time(v, d, ts)),
        (Some(1), _) => int(4).map(|v| v.to_string()),
        (Some(2), _) => int(8).map(|v| v.to_string()),
        (Some(4), _) => b.try_into().ok().map(|a| format!("{:?}", f32::from_le_bytes(a))),
        (Some(5), _) => b.try_into().ok().map(|a| format!("{:?}", f64::from_le_bytes(a))),
        (Some(6 | 7), Interp::String) => {
            let s = String::from_utf8_lossy(b);
            Some(if s.chars().count() > LONG_STAT {
                format!("\"{}…\"", escape(&s.chars().take(LONG_STAT).collect::<String>()))
            } else {
                format!("\"{}\"", escape(&s))
            })
        }
        (Some(6 | 7), Interp::Decimal(s)) if !b.is_empty() && b.len() <= 16 => {
            let mut v: i128 = if b[0] & 0x80 != 0 { -1 } else { 0 };
            for &x in b {
                v = (v << 8) | x as i128;
            }
            Some(decimal(v, s))
        }
        (Some(7), Interp::Uuid) if b.len() == 16 => {
            let h: String = b.iter().map(|b| format!("{b:02x}")).collect();
            Some(format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
        }
        _ => None,
    };
    shown.unwrap_or_else(|| hex(b))
}

/// Codec, sizes, encodings and statistics of a column chunk (or of a column over all row groups).
fn chunk_text(meta: &T, leaf: Option<&Leaf>) -> String {
    let mut s = name_of(&CODECS, meta.int(4));
    let (compressed, uncompressed) = (meta.int(7).unwrap_or(0), meta.int(6).unwrap_or(0));
    let _ = write!(s, ", {} ({} uncompressed)", size(compressed.max(0) as u64), size(uncompressed.max(0) as u64));
    let mut encodings: Vec<String> = meta
        .list(2)
        .iter()
        .filter_map(|e| match e {
            T::Int(i) => Some(name_of(&ENCODINGS, Some(*i))),
            _ => None,
        })
        .collect();
    encodings.sort();
    encodings.dedup();
    let _ = write!(s, ", {}", encodings.join(" "));
    if let (Some(stats), Some(leaf)) = (meta.field(12), leaf) {
        // min_value/max_value; the deprecated min/max only where their sort order was right.
        let legacy_ok = !matches!(leaf.physical, Some(6 | 7));
        let min = stats.bin(6).or_else(|| stats.bin(2).filter(|_| legacy_ok));
        let max = stats.bin(5).or_else(|| stats.bin(1).filter(|_| legacy_ok));
        if let Some(min) = min {
            let _ = write!(s, ", min {}", stat(min, leaf));
        }
        if let Some(max) = max {
            let _ = write!(s, ", max {}", stat(max, leaf));
        }
        if let Some(n) = stats.int(3) {
            let _ = write!(s, ", nulls {}", count(n.max(0) as u64));
        }
        if let Some(n) = stats.int(4) {
            let _ = write!(s, ", distinct {}", count(n.max(0) as u64));
        }
    } else {
        s.push_str(", no statistics");
    }
    s
}

fn parquet_summary(data: &[u8]) -> Result<String> {
    let len = data.len();
    let footer_len = u32::from_le_bytes(data[len - 8..len - 4].try_into().unwrap()) as usize;
    if footer_len > len - 12 {
        return Err(damaged("Parquet"));
    }
    let mut r = Thrift { data: &data[len - 8 - footer_len..len - 8], pos: 0 };
    let meta = r.value(12, 0)?;

    let mut out = String::new();
    let _ = writeln!(out, "=== Parquet file ===");
    line(&mut out, "Format version", meta.int(1).unwrap_or(0));
    if let Some(by) = meta.str(6) {
        line(&mut out, "Created by", escape(&by));
    }
    line(&mut out, "Rows", count(meta.int(3).unwrap_or(0).max(0) as u64));
    let groups = meta.list(4);
    line(&mut out, "Row groups", count(groups.len() as u64));
    let schema = meta.list(2);
    if meta.field(8).is_some() {
        let _ = writeln!(out, "! The file is encrypted; only plaintext footer fields are shown.");
    }

    let _ = writeln!(out, "\n=== Schema ===");
    let leaves = parquet_schema(schema, &mut out)?;

    let kv: Vec<(String, String)> =
        meta.list(5).iter().map(|e| (e.str(1).unwrap_or_default(), e.str(2).unwrap_or_default())).collect();
    key_values(&mut out, kv);

    // Per column over all row groups: sizes, codecs and null counts.
    if groups.len() > 1 {
        let _ = writeln!(out, "\n=== Columns ===");
        for (i, leaf) in leaves.iter().enumerate() {
            let (mut compressed, mut uncompressed, mut nulls, mut values) = (0i64, 0i64, Some(0i64), 0i64);
            let mut codecs: Vec<String> = Vec::new();
            for g in groups {
                let Some(m) = g.list(1).get(i).and_then(|c| c.field(3)) else { continue };
                compressed += m.int(7).unwrap_or(0);
                uncompressed += m.int(6).unwrap_or(0);
                values += m.int(5).unwrap_or(0);
                nulls = nulls.zip(m.field(12).and_then(|s| s.int(3))).map(|(a, b)| a + b);
                let codec = name_of(&CODECS, m.int(4));
                if !codecs.contains(&codec) {
                    codecs.push(codec);
                }
            }
            let nulls = nulls.map_or_else(|| "nulls ?".into(), |n| format!("nulls {}", count(n.max(0) as u64)));
            let _ = writeln!(
                out,
                "{}: {}, {} ({} uncompressed), {} values, {nulls}",
                escape(&leaf.path),
                codecs.join("/"),
                size(compressed.max(0) as u64),
                size(uncompressed.max(0) as u64),
                count(values.max(0) as u64),
            );
        }
    }

    let mut lines = 0;
    for (n, g) in groups.iter().enumerate() {
        if lines >= MAX_CHUNK_LINES {
            let _ = writeln!(out, "\n… {} more row groups not shown", count((groups.len() - n) as u64));
            break;
        }
        let _ = writeln!(out, "\n=== Row group {} ===", n + 1);
        line(&mut out, "Rows", count(g.int(3).unwrap_or(0).max(0) as u64));
        let total = g.int(2).unwrap_or(0).max(0) as u64;
        match g.int(6) {
            Some(c) => line(&mut out, "Size", format!("{} ({} uncompressed)", size(c.max(0) as u64), size(total))),
            None => line(&mut out, "Size", format!("{} uncompressed", size(total))),
        }
        let sorting: Vec<String> = g
            .list(4)
            .iter()
            .map(|s| {
                let col = s.int(1).and_then(|c| leaves.get(usize::try_from(c).ok()?)).map_or("?", |l| &l.path);
                let dir = if s.bool(2).unwrap_or(false) { " DESC" } else { "" };
                format!("{}{dir}", escape(col))
            })
            .collect();
        if !sorting.is_empty() {
            line(&mut out, "Sorted by", sorting.join(", "));
        }
        for (i, chunk) in g.list(1).iter().enumerate() {
            lines += 1;
            let leaf = leaves.get(i);
            let name = leaf.map_or_else(|| format!("column {i}"), |l| escape(&l.path));
            let Some(m) = chunk.field(3) else {
                let _ = writeln!(
                    out,
                    "{name}: (no metadata{})",
                    chunk.str(1).map(|p| format!(", in {p}")).unwrap_or_default()
                );
                continue;
            };
            let file = chunk.str(1).map(|p| format!(", in file {}", escape(&p))).unwrap_or_default();
            let _ = writeln!(out, "{name}: {}{file}", chunk_text(m, leaf));
        }
    }
    Ok(out)
}

// ---- FlatBuffers (Arrow IPC) ----

#[derive(Clone, Copy)]
struct Fb<'a> {
    buf: &'a [u8],
    /// Where the table starts, and its vtable.
    pos: usize,
    vtable: usize,
    vtable_len: usize,
}

fn fb_damaged() -> Error {
    damaged("Arrow")
}

fn le_u32(buf: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(buf.get(at..at + 4).ok_or_else(fb_damaged)?.try_into().unwrap()))
}

fn le_i64(buf: &[u8], at: usize) -> Result<i64> {
    Ok(i64::from_le_bytes(buf.get(at..at + 8).ok_or_else(fb_damaged)?.try_into().unwrap()))
}

impl<'a> Fb<'a> {
    fn root(buf: &'a [u8]) -> Result<Fb<'a>> {
        Fb::at(buf, le_u32(buf, 0)? as usize)
    }

    fn at(buf: &'a [u8], pos: usize) -> Result<Fb<'a>> {
        let soffset = le_u32(buf, pos)? as i32 as i64;
        let vtable = usize::try_from(pos as i64 - soffset).map_err(|_| fb_damaged())?;
        let vtable_len = u16::from_le_bytes(buf.get(vtable..vtable + 2).ok_or_else(fb_damaged)?.try_into().unwrap());
        if vtable + vtable_len as usize > buf.len() {
            return Err(fb_damaged());
        }
        Ok(Fb { buf, pos, vtable, vtable_len: vtable_len as usize })
    }

    /// Where field `id` is stored, if present.
    fn field(&self, id: usize) -> Option<usize> {
        let o = 4 + 2 * id;
        if o + 2 > self.vtable_len {
            return None;
        }
        let off = u16::from_le_bytes([self.buf[self.vtable + o], self.buf[self.vtable + o + 1]]);
        (off != 0).then_some(self.pos + off as usize)
    }

    fn bytes<const N: usize>(&self, id: usize) -> Option<[u8; N]> {
        let p = self.field(id)?;
        self.buf.get(p..p + N)?.try_into().ok()
    }

    fn u8(&self, id: usize) -> u8 {
        self.bytes::<1>(id).map_or(0, |b| b[0])
    }

    fn i16(&self, id: usize, default: i16) -> i16 {
        self.bytes(id).map_or(default, i16::from_le_bytes)
    }

    fn i32(&self, id: usize, default: i32) -> i32 {
        self.bytes(id).map_or(default, i32::from_le_bytes)
    }

    fn i64(&self, id: usize) -> i64 {
        self.bytes(id).map_or(0, i64::from_le_bytes)
    }

    fn bool(&self, id: usize) -> bool {
        self.u8(id) != 0
    }

    /// The target of an offset field.
    fn indirect(&self, id: usize) -> Result<Option<usize>> {
        let Some(p) = self.field(id) else { return Ok(None) };
        Ok(Some(p + le_u32(self.buf, p)? as usize))
    }

    fn table(&self, id: usize) -> Result<Option<Fb<'a>>> {
        self.indirect(id)?.map(|p| Fb::at(self.buf, p)).transpose()
    }

    fn string(&self, id: usize) -> Result<Option<String>> {
        let Some(p) = self.indirect(id)? else { return Ok(None) };
        let n = le_u32(self.buf, p)? as usize;
        let b = self.buf.get(p + 4..p + 4 + n).ok_or_else(fb_damaged)?;
        Ok(Some(String::from_utf8_lossy(b).into_owned()))
    }

    /// A vector of `elem`-byte items: where the first is, and how many.
    fn vector(&self, id: usize, elem: usize) -> Result<(usize, usize)> {
        let Some(p) = self.indirect(id)? else { return Ok((0, 0)) };
        let n = le_u32(self.buf, p)? as usize;
        if n.saturating_mul(elem) > self.buf.len().saturating_sub(p + 4) {
            return Err(fb_damaged());
        }
        Ok((p + 4, n))
    }

    fn tables(&self, id: usize) -> Result<Vec<Fb<'a>>> {
        let (start, n) = self.vector(id, 4)?;
        (0..n).map(|i| start + 4 * i).map(|p| Fb::at(self.buf, p + le_u32(self.buf, p)? as usize)).collect()
    }

    fn key_values(&self, id: usize) -> Result<Vec<(String, String)>> {
        self.tables(id)?
            .iter()
            .map(|kv| Ok((kv.string(0)?.unwrap_or_default(), kv.string(1)?.unwrap_or_default())))
            .collect()
    }
}

const UNITS: [&str; 4] = ["s", "ms", "us", "ns"];

fn unit(u: i16) -> &'static str {
    UNITS.get(u as usize).copied().unwrap_or("?")
}

/// The type of an Arrow field, written the way pyarrow prints it.
fn arrow_type(field: &Fb) -> Result<String> {
    let t = field.table(3)?;
    let get = |id: usize, default: i32| t.map_or(default, |t| t.i32(id, default));
    let get16 = |id: usize, default: i16| t.map_or(default, |t| t.i16(id, default));
    Ok(match field.u8(2) {
        1 => "null".into(),
        2 => format!("{}int{}", if t.is_some_and(|t| t.bool(1)) { "" } else { "u" }, get(0, 0)),
        3 => ["float16", "float32", "float64"].get(get16(0, 0) as usize).unwrap_or(&"float?").to_string(),
        4 => "binary".into(),
        5 => "string".into(),
        6 => "bool".into(),
        7 => format!("decimal{}({}, {})", get(2, 128), get(0, 0), get(1, 0)),
        8 => {
            if get16(0, 1) == 0 {
                "date32[day]".into()
            } else {
                "date64[ms]".into()
            }
        }
        9 => format!("time{}[{}]", get(1, 32), unit(get16(0, 1))),
        10 => {
            let tz = t.map(|t| t.string(1)).transpose()?.flatten();
            format!("timestamp[{}{}]", unit(get16(0, 0)), tz.map(|z| format!(", tz={z}")).unwrap_or_default())
        }
        11 => ["month_interval", "day_time_interval", "month_day_nano_interval"]
            .get(get16(0, 0) as usize)
            .unwrap_or(&"interval")
            .to_string(),
        12 => "list".into(),
        13 => "struct".into(),
        14 => format!("{}_union", if get16(0, 0) == 0 { "sparse" } else { "dense" }),
        15 => format!("fixed_size_binary[{}]", get(0, 0)),
        16 => format!("fixed_size_list[{}]", get(0, 0)),
        17 => {
            if t.is_some_and(|t| t.bool(0)) {
                "map (keys sorted)".into()
            } else {
                "map".into()
            }
        }
        18 => format!("duration[{}]", unit(get16(0, 1))),
        19 => "large_binary".into(),
        20 => "large_string".into(),
        21 => "large_list".into(),
        22 => "run_end_encoded".into(),
        23 => "binary_view".into(),
        24 => "string_view".into(),
        25 => "list_view".into(),
        26 => "large_list_view".into(),
        n => format!("type {n}"),
    })
}

/// Writes the fields as an indented tree; returns their names in depth-first order.
fn arrow_fields(fields: &[Fb], depth: usize, prefix: &str, out: &mut String, names: &mut Vec<String>) -> Result<()> {
    if depth > 32 {
        return Err(fb_damaged());
    }
    for f in fields {
        let name = f.string(0)?.unwrap_or_default();
        let mut text = format!("{}{}: {}", "  ".repeat(depth), escape(&name), arrow_type(f)?);
        if let Some(dict) = f.table(4)? {
            let index = match dict.table(1)? {
                Some(i) => format!("{}int{}", if i.bool(1) { "" } else { "u" }, i.i32(0, 32)),
                None => "int32".into(),
            };
            let _ =
                write!(text, ", dictionary-encoded ({index} indices{})", if dict.bool(2) { ", ordered" } else { "" });
        }
        text.push_str(if f.bool(1) { ", nullable" } else { ", not null" });
        let _ = writeln!(out, "{text}");
        for (k, v) in f.key_values(6)? {
            let _ = writeln!(out, "{}  {} = {}", "  ".repeat(depth), escape(&k), metadata_value(&v));
        }
        let path = if prefix.is_empty() { name } else { format!("{prefix}.{name}") };
        names.push(path.clone());
        arrow_fields(&f.tables(5)?, depth + 1, &path, out, names)?;
    }
    Ok(())
}

struct Batch {
    rows: i64,
    body: i64,
    /// Per field (depth-first) its null count.
    nulls: Vec<i64>,
    codec: Option<i8>,
}

/// The header of the record batch message at `offset`.
fn record_batch(data: &[u8], offset: i64, body: i64) -> Result<Batch> {
    let mut at = usize::try_from(offset).map_err(|_| fb_damaged())?;
    let mut len = le_u32(data, at)?;
    if len == 0xFFFF_FFFF {
        at += 4;
        len = le_u32(data, at)?;
    }
    let buf = data.get(at + 4..at + 4 + len as usize).ok_or_else(fb_damaged)?;
    let message = Fb::root(buf)?;
    if message.u8(1) != 3 {
        return Err(fb_damaged());
    }
    let batch = message.table(2)?.ok_or_else(fb_damaged)?;
    let (start, n) = batch.vector(1, 16)?;
    let nulls = (0..n).map(|i| le_i64(buf, start + 16 * i + 8)).collect::<Result<_>>()?;
    let codec = batch.table(3)?.map(|c| c.u8(0) as i8);
    Ok(Batch { rows: batch.i64(0), body, nulls, codec })
}

fn arrow_summary(data: &[u8]) -> Result<String> {
    let len = data.len();
    let footer_len = le_u32(data, len - 10)? as usize;
    if footer_len > len - 16 {
        return Err(fb_damaged());
    }
    let footer = Fb::root(&data[len - 10 - footer_len..len - 10])?;
    let schema = footer.table(1)?.ok_or_else(fb_damaged)?;
    let block = |id| -> Result<Vec<(i64, i64)>> {
        let (start, n) = footer.vector(id, 24)?;
        (0..n).map(|i| Ok((le_i64(footer.buf, start + 24 * i)?, le_i64(footer.buf, start + 24 * i + 16)?))).collect()
    };
    let (dictionaries, batches) = (block(2)?, block(3)?);
    let batches: Vec<Batch> =
        batches.iter().map(|&(offset, body)| record_batch(data, offset, body)).collect::<Result<_>>()?;

    let mut out = String::new();
    let _ = writeln!(out, "=== Arrow IPC file ===");
    let version = footer.i16(0, 0);
    line(&mut out, "Format version", format!("V{}", version + 1));
    line(&mut out, "Rows", count(batches.iter().map(|b| b.rows.max(0) as u64).sum()));
    line(&mut out, "Record batches", count(batches.len() as u64));
    if !dictionaries.is_empty() {
        line(&mut out, "Dictionary batches", count(dictionaries.len() as u64));
    }
    let mut codecs: Vec<&str> = batches
        .iter()
        .map(|b| match b.codec {
            None => "none",
            Some(0) => "LZ4_FRAME",
            Some(1) => "ZSTD",
            Some(_) => "?",
        })
        .collect();
    codecs.sort();
    codecs.dedup();
    if !codecs.is_empty() {
        line(&mut out, "Compression", codecs.join(", "));
    }
    if schema.i16(0, 0) != 0 {
        line(&mut out, "Endianness", "big");
    }

    let _ = writeln!(out, "\n=== Schema ===");
    let mut names = Vec::new();
    arrow_fields(&schema.tables(1)?, 0, "", &mut out, &mut names)?;
    key_values(&mut out, schema.key_values(2)?);

    if !batches.is_empty() {
        let _ = writeln!(out, "\n=== Null counts ===");
        for (i, name) in names.iter().enumerate() {
            let nulls: i64 = batches.iter().map(|b| b.nulls.get(i).copied().unwrap_or(0).max(0)).sum();
            let _ = writeln!(out, "{}: {}", escape(name), count(nulls as u64));
        }
        let _ = writeln!(out, "\n=== Record batches ===");
        for (i, b) in batches.iter().enumerate().take(MAX_BATCH_LINES) {
            let _ =
                writeln!(out, "Batch {}: {} rows, {}", i + 1, count(b.rows.max(0) as u64), size(b.body.max(0) as u64));
        }
        if batches.len() > MAX_BATCH_LINES {
            let _ =
                writeln!(out, "… {} more record batches not shown", count((batches.len() - MAX_BATCH_LINES) as u64));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/parquet/", $name))
        };
    }
    const V1: &[u8] = fixture!("sales-v1.parquet");
    const V2: &[u8] = fixture!("sales-v2.parquet");
    const ZSTD: &[u8] = fixture!("sales-v1-zstd.parquet");
    const GROUPS: &[u8] = fixture!("sales-v1-groups.parquet");
    const FEATHER: &[u8] = fixture!("sales-v1.feather");
    const ARROW_V2: &[u8] = fixture!("sales-v2.arrow");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn parquet_summary_view() {
        let s = summary(V1).unwrap();
        println!("{s}");
        for expected in [
            "Rows:              5\n",
            "Row groups:        1\n",
            "=== Schema ===\nid: optional INT64\nproduct: optional BYTE_ARRAY STRING\nprice: optional FIXED_LEN_BYTE_ARRAY(4) DECIMAL(9, 2)\nqty: optional INT32\nratio: optional DOUBLE\nsold: optional INT32 DATE\nat: optional INT64 TIMESTAMP(MICROS, UTC)\nok: optional BOOLEAN\ntags: optional group LIST\n  list: repeated group\n    element: optional BYTE_ARRAY STRING\ndims: optional group\n  w: optional INT32 INT(16, signed)\n  h: optional INT32 INT(16, signed)\nnote: optional BYTE_ARRAY STRING\n",
            "source = \"regenerate.py\"",
            "ARROW:schema = \"",
            "id: SNAPPY, 122 B (128 B uncompressed), PLAIN RLE RLE_DICTIONARY, min 1, max 5, nulls 0\n",
            ", min 1, max 5, nulls 0",
            "product: SNAPPY, ",
            ", min \"Birne\", max \"cherry\", nulls 1",
            ", min 0.99, max 12.50, nulls 1",
            ", min 2024-01-01, max 2024-01-05, nulls 0",
            ", min 2024-01-01T12:00:00, max 2024-01-01T12:00:04, nulls 0",
            ", min false, max true, nulls 1",
            "tags.list.element: SNAPPY",
            "dims.w: SNAPPY",
        ] {
            assert!(s.contains(expected), "{expected}\n---\n{s}");
        }
    }

    #[test]
    fn parquet_changes_show_as_changed_lines() {
        let (s1, s2) = (summary(V1).unwrap(), summary(V2).unwrap());
        let added = lines_only_in(&s2, &s1);
        assert!(added.iter().any(|l| l.starts_with("region: optional BYTE_ARRAY STRING")), "{added:?}");
        assert!(added.iter().any(|l| l.starts_with("qty: ") && l.contains("max 99")), "{added:?}");
        assert!(!added.iter().any(|l| l.starts_with("id: ") || l.starts_with("product:")), "{added:?}");
        let zstd = summary(ZSTD).unwrap();
        assert!(zstd.contains("id: ZSTD, "), "{zstd}");
        assert_eq!(
            s1.lines().filter(|l| l.contains("SNAPPY")).count(),
            zstd.lines().filter(|l| l.contains("ZSTD")).count()
        );
    }

    #[test]
    fn row_groups() {
        let s = summary(GROUPS).unwrap();
        assert!(s.contains("Row groups:        3\n"), "{s}");
        assert!(s.contains("=== Row group 3 ===\nRows:              1\n"), "{s}");
        assert!(s.contains("note: UNCOMPRESSED, ") && s.contains(", no statistics"), "{s}");
        assert!(s.contains(", min 1, max 2, nulls 0"), "{s}");
    }

    #[test]
    fn arrow_summary_view() {
        let s = summary(FEATHER).unwrap();
        println!("{s}");
        for expected in [
            "Format version:    V5",
            "Rows:              5\nRecord batches:    1\nCompression:       LZ4_FRAME\n",
            "id: int64, nullable\nproduct: string, nullable\nprice: decimal128(9, 2), nullable\nqty: int32, nullable\nratio: float64, nullable\nsold: date32[day], nullable\nat: timestamp[us, tz=UTC], nullable\nok: bool, nullable\ntags: list, nullable\n  item: string, nullable\ndims: struct, nullable\n  w: int16, nullable\n  h: int16, nullable\nnote: string, nullable\n",
            "=== Null counts ===\nid: 0\nproduct: 1\nprice: 1\n",
            "tags.item: 0",
            "Batch 1: 5 rows, ",
        ] {
            assert!(s.contains(expected), "{expected}\n---\n{s}");
        }
        let s2 = summary(ARROW_V2).unwrap();
        assert!(s2.contains("Record batches:    2\nDictionary batches: 1\nCompression:       none\n"), "{s2}");
        assert!(s2.contains("region: string, dictionary-encoded (int32 indices), nullable"), "{s2}");
    }

    #[test]
    fn damaged_files_are_errors() {
        for data in [V1, FEATHER, ARROW_V2, GROUPS] {
            let magic = &data[..4];
            for cut in 0..data.len() {
                // Keep the trailing magic so detection still says yes.
                let mut bad = data[..cut].to_vec();
                bad.extend_from_slice(&data[data.len() - 10..]);
                let _ = summary(&bad);
                let mut flipped = data.to_vec();
                flipped[cut] ^= 0x55;
                let _ = summary(&flipped);
            }
            assert!(is_parquet(data) || is_arrow(data));
            assert!(!is_parquet(magic));
        }
        assert!(!is_parquet(b"PAR1 just text PAR1x"));
    }
}
