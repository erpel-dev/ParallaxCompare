//! MDF measurement files (ASAM MDF 4.x `.mf4`, and MDF 3.x `.mdf` / `.dat`).
//!
//! An MDF file is a tree of linked blocks: a header (`HD`) with the measurement
//! start time, comments and file history (`FH`); data groups (`DG`) holding
//! channel groups (`CG`, one record layout each, with a cycle count) holding
//! channels (`CN`, with name, unit, data type, bit position, conversion `CC` and
//! source `SI`); and attachments (`AT`). The samples themselves are in data blocks
//! that can be gigabytes.
//!
//! The "Measurement summary" view follows the links of the block tree only, never
//! the data blocks, so a multi-gigabyte recording opens instantly. It shows the
//! header and its metadata, then each channel group with its channels sorted by
//! name (the master channel first), one fact per line, conversions summarized
//! (formula or table entries) and attachments. There is no ≈ similar rule: two
//! recordings with the same channels still differ in their samples, which this
//! view does not read.

use std::collections::HashSet;
use std::fmt::Write as _;

use crate::{Error, Result};

/// Channels shown at most (over all groups).
const MAX_CHANNELS: usize = 100_000;
/// Groups, channels per group, attachments or history entries followed at most.
const MAX_CHAIN: usize = 1_000_000;
/// Table conversion entries shown per channel.
const MAX_TABLE: usize = 64;
/// Longest text or XML block read.
const MAX_TEXT: usize = 1 << 20;

/// Whether the data starts with an MDF identification block.
pub fn is_mdf(data: &[u8]) -> bool {
    data.len() >= 64 && (data.starts_with(b"MDF     ") || data.starts_with(b"UnFinMF "))
}

/// The "Measurement summary" view.
pub fn summary(data: &[u8]) -> Result<String> {
    if !is_mdf(data) {
        return Err(Error::Unsupported("not an MDF file".into()));
    }
    let version = u16le(data, 28).unwrap_or(0);
    let mut out = String::new();
    let version_text = String::from_utf8_lossy(&data[8..16]).trim().trim_end_matches('\0').to_string();
    line(&mut out, 0, "Version", if version_text.is_empty() { version.to_string() } else { version_text });
    let program = String::from_utf8_lossy(&data[16..24]).trim().trim_end_matches('\0').to_string();
    if !program.is_empty() {
        line(&mut out, 0, "Program", program);
    }
    if data.starts_with(b"UnFinMF ") {
        let _ = writeln!(out, "! The file is not finalized: the writer did not finish it");
    }
    if version >= 400 {
        Mdf4 { data, channels: 0 }.render(&mut out)?;
    } else if version >= 200 {
        if u16le(data, 24) == Some(1) {
            return Err(Error::Unsupported("big-endian MDF 3 files are not supported".into()));
        }
        Mdf3 { data, channels: 0 }.render(&mut out)?;
    } else {
        return Err(Error::Unsupported(format!("MDF version {version} is not supported")));
    }
    Ok(out)
}

fn damaged() -> Error {
    Error::Unsupported("MDF: the block structure is damaged".into())
}

fn bytes_at(data: &[u8], at: usize, n: usize) -> Option<&[u8]> {
    data.get(at..at.checked_add(n)?)
}

fn u16le(data: &[u8], at: usize) -> Option<u16> {
    bytes_at(data, at, 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32le(data: &[u8], at: usize) -> Option<u32> {
    bytes_at(data, at, 4).map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")))
}

fn u64le(data: &[u8], at: usize) -> Option<u64> {
    bytes_at(data, at, 8).map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
}

fn f64le(data: &[u8], at: usize) -> Option<f64> {
    u64le(data, at).map(f64::from_bits)
}

fn line(out: &mut String, indent: usize, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:indent$}{:<18} {value}", "", format!("{label}:"));
}

/// A number as written by a person: `0.1`, `-40`, `1e-9`.
fn num(v: f64) -> String {
    if v == 0.0 { "0".into() } else { format!("{v}") }
}

fn escape(s: &str) -> String {
    s.replace('\r', "").replace('\n', "\\n").replace('\t', "\\t")
}

/// Text up to the first NUL, as UTF-8 (lossy).
fn c_text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

/// UTC date and time from nanoseconds since 1970.
fn utc(ns: u64) -> String {
    let secs = ns / 1_000_000_000;
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let ms = (ns / 1_000_000) % 1000;
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{ms:03} UTC", rest / 3600, rest / 60 % 60, rest % 60)
}

/// The leaf elements of an MDF 4 XML comment: `TX` text and `name: value` pairs
/// (`<e name="…">` properties by their name).
fn xml_fields(xml: &str) -> Vec<(String, String)> {
    use super::office::{Ev, attr, walk_xml};
    let mut out = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut text = String::new();
    let _ = walk_xml(xml, |ev| match ev {
        Ev::Start(tag, e) => {
            stack.push(attr(e, "name").unwrap_or_else(|| tag.to_string()));
            text.clear();
        }
        Ev::Text(t) => text.push_str(t),
        Ev::End(_) => {
            let name = stack.pop().unwrap_or_default();
            let value = text.trim();
            if !value.is_empty() && out.len() < 1000 {
                out.push((name, escape(value)));
            }
            text.clear();
        }
    });
    out
}

/// A comment or unit: plain text, or the fields of an XML block.
fn show_text(out: &mut String, indent: usize, label: &str, text: &str) {
    let t = text.trim();
    if t.is_empty() {
        return;
    }
    if !t.starts_with('<') {
        line(out, indent, label, escape(t));
        return;
    }
    for (name, value) in xml_fields(t) {
        if name == "TX" {
            line(out, indent, label, value);
        } else {
            line(out, indent, &name, value);
        }
    }
}

// ---- MDF 4 ----

struct Block<'a> {
    id: [u8; 4],
    links: Vec<u64>,
    data: &'a [u8],
}

impl Block<'_> {
    fn link(&self, i: usize) -> u64 {
        self.links.get(i).copied().unwrap_or(0)
    }
}

struct Mdf4<'a> {
    data: &'a [u8],
    channels: usize,
}

impl<'a> Mdf4<'a> {
    /// The block at `at`: its id, links and (at most `max_data` bytes of) data.
    fn block(&self, at: u64, max_data: usize) -> Option<Block<'a>> {
        let at = usize::try_from(at).ok()?;
        let head = bytes_at(self.data, at, 24)?;
        if &head[..2] != b"##" {
            return None;
        }
        let length = usize::try_from(u64le(head, 8)?).ok()?;
        let count = usize::try_from(u64le(head, 16)?).ok()?;
        let links_end = 24usize.checked_add(count.checked_mul(8)?)?;
        if links_end > length || at.checked_add(links_end)? > self.data.len() {
            return None;
        }
        let links = (0..count).map(|i| u64le(self.data, at + 24 + i * 8).unwrap_or(0)).collect();
        let data_len = (length - links_end).min(max_data).min(self.data.len() - (at + links_end));
        Some(Block {
            id: head[..4].try_into().ok()?,
            links,
            data: &self.data[at + links_end..at + links_end + data_len],
        })
    }

    /// A chain of blocks linked by their first link.
    fn chain(&self, first: u64, max_data: usize) -> Vec<Block<'a>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut at = first;
        while at != 0 && seen.insert(at) && out.len() < MAX_CHAIN {
            let Some(b) = self.block(at, max_data) else { break };
            at = b.link(0);
            out.push(b);
        }
        out
    }

    /// The text of a TX or MD block.
    fn text(&self, at: u64) -> Option<String> {
        if at == 0 {
            return None;
        }
        let b = self.block(at, MAX_TEXT)?;
        matches!(&b.id, b"##TX" | b"##MD").then(|| c_text(b.data))
    }

    fn render(&mut self, out: &mut String) -> Result<()> {
        let hd = self.block(64, 64).filter(|b| &b.id == b"##HD").ok_or_else(damaged)?;
        if let Some(ns) = u64le(hd.data, 0) {
            line(out, 0, "Start time", utc(ns));
            let flags = hd.data.get(12).copied().unwrap_or(0);
            if flags & 2 != 0 {
                let tz = u16le(hd.data, 8).map_or(0, |v| v as i16);
                let dst = u16le(hd.data, 10).map_or(0, |v| v as i16);
                line(out, 0, "Time zone", format!("UTC{:+} min, DST {:+} min", tz, dst));
            }
        }
        if let Some(c) = self.text(hd.link(5)) {
            show_text(out, 0, "Comment", &c);
        }
        let history = self.chain(hd.link(1), 32);
        if !history.is_empty() {
            let _ = writeln!(out, "\n=== File history ({}) ===", history.len());
            // The entries' times are when a tool wrote the file, so they are left out.
            for (i, fh) in history.iter().enumerate() {
                let _ = writeln!(out, "Entry {}", i + 1);
                if let Some(c) = self.text(fh.link(1)) {
                    show_text(out, 2, "Comment", &c);
                }
            }
        }
        let groups = self.chain(hd.link(0), 16);
        let cg_count: usize = groups.iter().map(|dg| self.chain(dg.link(1), 0).len()).sum();
        line(out, 0, "Data groups", groups.len());
        line(out, 0, "Channel groups", cg_count);
        for (i, dg) in groups.iter().enumerate() {
            let record_id_size = dg.data.first().copied().unwrap_or(0);
            for (j, cg) in self.chain(dg.link(1), 32).iter().enumerate() {
                self.channel_group(out, i + 1, j + 1, record_id_size, dg, cg);
            }
        }
        let attachments = self.chain(hd.link(3), 40);
        if !attachments.is_empty() {
            let _ = writeln!(out, "\n=== Attachments ({}) ===", attachments.len());
            for at in &attachments {
                let name = self.text(at.link(1)).unwrap_or_default();
                let _ = writeln!(out, "\n--- {} ---", if name.is_empty() { "(no file name)" } else { &name });
                if let Some(m) = self.text(at.link(2)) {
                    line(out, 0, "MIME type", m);
                }
                let flags = u16le(at.data, 0).unwrap_or(0);
                let embedded = flags & 1 != 0;
                line(
                    out,
                    0,
                    "Stored",
                    match (embedded, flags & 2 != 0) {
                        (true, true) => "embedded, compressed",
                        (true, false) => "embedded",
                        _ => "external file",
                    },
                );
                if let Some(size) = u64le(at.data, 24).filter(|_| embedded) {
                    line(out, 0, "Size", format!("{size} bytes"));
                }
                if flags & 4 != 0
                    && let Some(md5) = bytes_at(at.data, 8, 16)
                {
                    line(out, 0, "MD5", md5.iter().map(|b| format!("{b:02x}")).collect::<String>());
                }
                if let Some(c) = self.text(at.link(3)) {
                    show_text(out, 0, "Comment", &c);
                }
            }
        }
        Ok(())
    }

    fn channel_group(
        &mut self,
        out: &mut String,
        dg: usize,
        cg_index: usize,
        record_id_size: u8,
        dg_block: &Block,
        cg: &Block,
    ) {
        let name = self.text(cg.link(2)).unwrap_or_default();
        let title = if name.is_empty() { String::new() } else { format!(": {name}") };
        let _ = writeln!(out, "\n=== Group {dg}.{cg_index}{title} ===");
        let flags = u16le(cg.data, 16).unwrap_or(0);
        if flags & 1 != 0 {
            line(out, 0, "Kind", "variable length signal data");
        }
        line(out, 0, "Cycles", u64le(cg.data, 8).unwrap_or(0));
        let data_bytes = u32le(cg.data, 24).unwrap_or(0);
        let inval_bytes = u32le(cg.data, 28).unwrap_or(0);
        line(
            out,
            0,
            "Record size",
            format!(
                "{} bytes{}",
                data_bytes,
                if inval_bytes > 0 { format!(" + {inval_bytes} invalidation") } else { String::new() }
            ),
        );
        if record_id_size > 0 {
            line(out, 0, "Record id", format!("{} ({record_id_size} bytes)", u64le(cg.data, 0).unwrap_or(0)));
        }
        if let Some(s) = self.source(cg.link(3)) {
            line(out, 0, "Source", s);
        }
        if let Some(c) = self.text(cg.link(5)) {
            show_text(out, 0, "Comment", &c);
        }
        if let Some(c) = self.text(dg_block.link(3)) {
            show_text(out, 0, "Group comment", &c);
        }
        let channels = self.chain(cg.link(1), 72);
        line(out, 0, "Channels", channels.len());
        let names: Vec<String> = channels.iter().map(|c| self.text(c.link(2)).unwrap_or_default()).collect();
        let mut order: Vec<usize> = (0..channels.len()).collect();
        let is_master = |c: &Block| matches!(c.data.first(), Some(2 | 3));
        order.sort_by(|&a, &b| (!is_master(&channels[a]), &names[a]).cmp(&(!is_master(&channels[b]), &names[b])));
        for i in order {
            if self.channels >= MAX_CHANNELS {
                let _ = writeln!(out, "! More channels are not shown");
                return;
            }
            self.channels += 1;
            self.channel(out, 0, &names[i], &channels[i], 0);
        }
    }

    fn channel(&mut self, out: &mut String, indent: usize, name: &str, cn: &Block, depth: usize) {
        let d = cn.data;
        let _ = writeln!(out, "{:indent$}Channel {}", "", if name.is_empty() { "(no name)" } else { name });
        let ind = indent + 2;
        let kind = match d.first() {
            Some(1) => Some("variable length signal data"),
            Some(2) => Some("master"),
            Some(3) => Some("virtual master"),
            Some(4) => Some("synchronization"),
            Some(5) => Some("maximum length data"),
            Some(6) => Some("virtual data"),
            Some(7) => Some("variable length signal (VLSC)"),
            _ => None,
        };
        let sync = match d.get(1) {
            Some(1) => " (time)",
            Some(2) => " (angle)",
            Some(3) => " (distance)",
            Some(4) => " (index)",
            _ => "",
        };
        if let Some(k) = kind {
            line(out, ind, "Kind", format!("{k}{sync}"));
        }
        let data_type = match d.get(2) {
            Some(0) => "unsigned integer, little-endian",
            Some(1) => "unsigned integer, big-endian",
            Some(2) => "signed integer, little-endian",
            Some(3) => "signed integer, big-endian",
            Some(4) => "float, little-endian",
            Some(5) => "float, big-endian",
            Some(6) => "string, Latin-1",
            Some(7) => "string, UTF-8",
            Some(8) => "string, UTF-16 little-endian",
            Some(9) => "string, UTF-16 big-endian",
            Some(10) => "byte array",
            Some(11) => "MIME sample",
            Some(12) => "MIME stream",
            Some(13) => "CANopen date",
            Some(14) => "CANopen time",
            Some(15) => "complex, little-endian",
            Some(16) => "complex, big-endian",
            _ => "?",
        };
        line(out, ind, "Data type", data_type);
        let bits = u32le(d, 8).unwrap_or(0);
        line(
            out,
            ind,
            "Position",
            format!("byte {}, bit {}, {bits} bits", u32le(d, 4).unwrap_or(0), d.get(3).copied().unwrap_or(0)),
        );
        let unit = self.text(cn.link(6)).or_else(|| self.block(cn.link(4), 0).and_then(|cc| self.text(cc.link(1))));
        if let Some(u) = unit.filter(|u| !u.trim().is_empty()) {
            show_text(out, ind, "Unit", &u);
        }
        let flags = u32le(d, 12).unwrap_or(0);
        if flags & 0x08 != 0 {
            line(
                out,
                ind,
                "Value range",
                format!("{} .. {}", num(f64le(d, 24).unwrap_or(0.0)), num(f64le(d, 32).unwrap_or(0.0))),
            );
        }
        if flags & 0x10 != 0 {
            line(
                out,
                ind,
                "Limits",
                format!("{} .. {}", num(f64le(d, 40).unwrap_or(0.0)), num(f64le(d, 48).unwrap_or(0.0))),
            );
        }
        if flags & 0x40 != 0 {
            line(out, ind, "Discrete", "yes");
        }
        if flags & 0x02 != 0 {
            line(out, ind, "Invalidation bit", u32le(d, 16).unwrap_or(0));
        }
        if cn.link(4) != 0 {
            self.conversion(out, ind, cn.link(4), 0);
        }
        if let Some(s) = self.source(cn.link(3)) {
            line(out, ind, "Source", s);
        }
        if let Some(c) = self.text(cn.link(7)) {
            show_text(out, ind, "Comment", &c);
        }
        // Structure members (a CN chain) or an array (CA).
        if cn.link(1) != 0
            && let Some(comp) = self.block(cn.link(1), 0)
        {
            if &comp.id == b"##CA" {
                line(out, ind, "Composition", "array");
            } else if &comp.id == b"##CN" && depth < 8 {
                let members = self.chain(cn.link(1), 72);
                line(out, ind, "Members", members.len());
                for m in &members {
                    let n = self.text(m.link(2)).unwrap_or_default();
                    self.channel(out, ind, &n, m, depth + 1);
                }
            }
        }
    }

    /// `name (path), ECU, CAN bus`
    fn source(&self, at: u64) -> Option<String> {
        let si = self.block(at, 8).filter(|b| &b.id == b"##SI")?;
        let name = self.text(si.link(0)).unwrap_or_default();
        let path = self.text(si.link(1)).unwrap_or_default();
        let kind = match si.data.first() {
            Some(1) => "ECU",
            Some(2) => "bus",
            Some(3) => "I/O",
            Some(4) => "tool",
            Some(5) => "user",
            _ => "other",
        };
        let bus = match si.data.get(1) {
            Some(2) => ", CAN",
            Some(3) => ", LIN",
            Some(4) => ", MOST",
            Some(5) => ", FlexRay",
            Some(6) => ", K-Line",
            Some(7) => ", Ethernet",
            Some(8) => ", USB",
            _ => "",
        };
        let path = if path.is_empty() || path == name { String::new() } else { format!(" ({path})") };
        Some(format!("{name}{path}, {kind}{bus}"))
    }

    fn conversion(&self, out: &mut String, ind: usize, at: u64, depth: usize) {
        let Some(cc) = self.block(at, 24 + 8 * 4096).filter(|b| &b.id == b"##CC") else { return };
        let d = cc.data;
        let val_count = usize::from(u16le(d, 6).unwrap_or(0));
        let vals: Vec<f64> = (0..val_count).map_while(|i| f64le(d, 24 + 8 * i)).collect();
        let refs: Vec<u64> = cc.links.get(4..).unwrap_or_default().to_vec();
        let v = |i: usize| vals.get(i).copied().map(num).unwrap_or_else(|| "?".into());
        let ref_text = |i: usize| -> String {
            match refs.get(i).copied().filter(|&r| r != 0) {
                None => "(none)".into(),
                Some(r) => match self.block(r, MAX_TEXT) {
                    Some(b) if &b.id == b"##TX" || &b.id == b"##MD" => format!("\"{}\"", escape(&c_text(b.data))),
                    Some(b) if &b.id == b"##CC" => "(conversion)".into(),
                    _ => "?".into(),
                },
            }
        };
        let name = self.text(cc.link(0)).filter(|n| !n.is_empty()).map(|n| format!(" \"{n}\"")).unwrap_or_default();
        let kind = d.first().copied().unwrap_or(0);
        let summary = match kind {
            0 => "identity".to_string(),
            1 => format!("linear: {} * x + {}", v(1), v(0)),
            2 => format!("rational: ({} x² + {} x + {}) / ({} x² + {} x + {})", v(0), v(1), v(2), v(3), v(4), v(5)),
            3 => format!("formula: {}", ref_text(0).trim_matches('"')),
            4 => format!("table with interpolation, {} entries", vals.len() / 2),
            5 => format!("table without interpolation, {} entries", vals.len() / 2),
            6 => format!("value range to value, {} ranges", vals.len().saturating_sub(1) / 3),
            7 => format!("value to text, {} entries", vals.len()),
            8 => format!("value range to text, {} ranges", vals.len() / 2),
            9 => format!("text to value, {} entries", refs.len()),
            10 => format!("text to text, {} entries", refs.len() / 2),
            11 => format!("bit field to text, {} entries", vals.len()),
            k => format!("type {k}"),
        };
        line(out, ind, "Conversion", format!("{summary}{name}"));
        let ind = ind + 2;
        let mut shown = 0usize;
        let mut entry = |out: &mut String, text: String| {
            if shown < MAX_TABLE {
                let _ = writeln!(out, "{:ind$}{text}", "");
            } else if shown == MAX_TABLE {
                let _ = writeln!(out, "{:ind$}…", "");
            }
            shown += 1;
        };
        match kind {
            4 | 5 => {
                for p in vals.chunks(2).filter(|p| p.len() == 2) {
                    entry(out, format!("{} -> {}", num(p[0]), num(p[1])));
                }
            }
            6 => {
                for t in vals.chunks(3).filter(|t| t.len() == 3) {
                    entry(out, format!("{} .. {} -> {}", num(t[0]), num(t[1]), num(t[2])));
                }
                if vals.len() % 3 == 1 {
                    entry(out, format!("default -> {}", v(vals.len() - 1)));
                }
            }
            7 | 11 => {
                for (i, x) in vals.iter().enumerate() {
                    entry(out, format!("{} = {}", num(*x), ref_text(i)));
                }
                if refs.len() > vals.len() {
                    entry(out, format!("default = {}", ref_text(vals.len())));
                }
            }
            8 => {
                for (i, p) in vals.chunks(2).filter(|p| p.len() == 2).enumerate() {
                    entry(out, format!("{} .. {} = {}", num(p[0]), num(p[1]), ref_text(i)));
                }
                if refs.len() > vals.len() / 2 {
                    entry(out, format!("default = {}", ref_text(vals.len() / 2)));
                }
            }
            9 => {
                for (i, x) in vals.iter().enumerate().take(refs.len()) {
                    entry(out, format!("{} -> {}", ref_text(i), num(*x)));
                }
            }
            10 => {
                for i in 0..refs.len() / 2 {
                    entry(out, format!("{} -> {}", ref_text(2 * i), ref_text(2 * i + 1)));
                }
            }
            _ => {}
        }
        // Text tables may fall back to a nested conversion for the default.
        if depth < 2
            && matches!(kind, 7 | 8)
            && let Some(&r) = refs.last()
            && self.block(r, 0).is_some_and(|b| &b.id == b"##CC")
        {
            self.conversion(out, ind, r, depth + 1);
        }
    }
}

// ---- MDF 3 ----

struct Mdf3<'a> {
    data: &'a [u8],
    channels: usize,
}

impl<'a> Mdf3<'a> {
    /// The block at `at` with the given 2-letter id: its bytes after the 4-byte header.
    fn block(&self, at: u32, id: &[u8; 2]) -> Option<&'a [u8]> {
        let at = at as usize;
        let head = bytes_at(self.data, at, 4)?;
        if &head[..2] != id {
            return None;
        }
        let size = usize::from(u16le(head, 2)?).max(4);
        let end = at.checked_add(size)?.min(self.data.len());
        Some(&self.data[at..end])
    }

    fn chain(&self, first: u32, id: &[u8; 2]) -> Vec<&'a [u8]> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut at = first;
        while at != 0 && seen.insert(at) && out.len() < MAX_CHAIN {
            let Some(b) = self.block(at, id) else { break };
            at = u32le(b, 4).unwrap_or(0);
            out.push(b);
        }
        out
    }

    fn text(&self, at: u32) -> Option<String> {
        if at == 0 {
            return None;
        }
        let at_usize = at as usize;
        let head = bytes_at(self.data, at_usize, 4)?;
        if &head[..2] != b"TX" {
            return None;
        }
        let size = usize::from(u16le(head, 2)?);
        let body = bytes_at(self.data, at_usize + 4, size.saturating_sub(4).min(MAX_TEXT))?;
        Some(c_text(body))
    }

    fn render(&mut self, out: &mut String) -> Result<()> {
        let hd = self.block(64, b"HD").ok_or_else(damaged)?;
        let field = |at: usize, n: usize| bytes_at(hd, at, n).map(c_text).unwrap_or_default();
        let (date, time) = (field(18, 10), field(28, 8));
        if !date.is_empty() {
            line(out, 0, "Start time", format!("{date} {time}"));
        }
        for (label, at) in [("Author", 36), ("Organization", 68), ("Project", 100), ("Subject", 132)] {
            let v = field(at, 32);
            if !v.is_empty() {
                line(out, 0, label, v);
            }
        }
        if let Some(c) = self.text(u32le(hd, 8).unwrap_or(0)) {
            show_text(out, 0, "Comment", &c);
        }
        let groups = self.chain(u32le(hd, 4).unwrap_or(0), b"DG");
        let cg_count: usize = groups.iter().map(|dg| self.chain(u32le(dg, 8).unwrap_or(0), b"CG").len()).sum();
        line(out, 0, "Data groups", groups.len());
        line(out, 0, "Channel groups", cg_count);
        for (i, dg) in groups.iter().enumerate() {
            for (j, cg) in self.chain(u32le(dg, 8).unwrap_or(0), b"CG").iter().enumerate() {
                let _ = writeln!(out, "\n=== Group {}.{} ===", i + 1, j + 1);
                line(out, 0, "Cycles", u32le(cg, 22).unwrap_or(0));
                line(out, 0, "Record size", format!("{} bytes", u16le(cg, 20).unwrap_or(0)));
                if let Some(c) = self.text(u32le(cg, 12).unwrap_or(0)) {
                    show_text(out, 0, "Comment", &c);
                }
                let channels = self.chain(u32le(cg, 8).unwrap_or(0), b"CN");
                line(out, 0, "Channels", channels.len());
                let mut named: Vec<(bool, String, &[u8])> = channels
                    .iter()
                    .map(|cn| {
                        let long = self.text(u32le(cn, 218).unwrap_or(0)).filter(|s| !s.is_empty());
                        let name = long.unwrap_or_else(|| bytes_at(cn, 26, 32).map(c_text).unwrap_or_default());
                        (u16le(cn, 24) != Some(1), name, *cn)
                    })
                    .collect();
                named.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
                for (data_channel, name, cn) in named {
                    if self.channels >= MAX_CHANNELS {
                        let _ = writeln!(out, "! More channels are not shown");
                        return Ok(());
                    }
                    self.channels += 1;
                    self.channel(out, &name, cn, !data_channel);
                }
            }
        }
        Ok(())
    }

    fn channel(&self, out: &mut String, name: &str, cn: &[u8], master: bool) {
        let _ = writeln!(out, "Channel {}", if name.is_empty() { "(no name)" } else { name });
        if master {
            line(out, 2, "Kind", "master (time)");
        }
        let data_type = match u16le(cn, 190) {
            Some(0) => "unsigned integer",
            Some(1) => "signed integer",
            Some(2) => "float",
            Some(3) => "double",
            Some(7) => "string",
            Some(8) => "byte array",
            Some(9) => "unsigned integer, big-endian",
            Some(10) => "signed integer, big-endian",
            Some(11) => "float, big-endian",
            Some(12) => "double, big-endian",
            Some(13) => "unsigned integer, little-endian",
            Some(14) => "signed integer, little-endian",
            Some(15) => "float, little-endian",
            Some(16) => "double, little-endian",
            _ => "?",
        };
        line(out, 2, "Data type", data_type);
        let start = u32::from(u16le(cn, 186).unwrap_or(0)) + 8 * u32::from(u16le(cn, 226).unwrap_or(0));
        line(
            out,
            2,
            "Position",
            format!("byte {}, bit {}, {} bits", start / 8, start % 8, u16le(cn, 188).unwrap_or(0)),
        );
        let description = bytes_at(cn, 58, 128).map(c_text).unwrap_or_default();
        if !description.is_empty() {
            line(out, 2, "Description", escape(&description));
        }
        if u16le(cn, 192).unwrap_or(0) != 0 {
            line(
                out,
                2,
                "Value range",
                format!("{} .. {}", num(f64le(cn, 194).unwrap_or(0.0)), num(f64le(cn, 202).unwrap_or(0.0))),
            );
        }
        if let Some(cc) = self.block(u32le(cn, 8).unwrap_or(0), b"CC") {
            let unit = bytes_at(cc, 22, 20).map(c_text).unwrap_or_default();
            if !unit.is_empty() {
                line(out, 2, "Unit", unit);
            }
            let p = |i: usize| f64le(cc, 46 + 8 * i).map(num).unwrap_or_else(|| "?".into());
            let n = usize::from(u16le(cc, 44).unwrap_or(0));
            let summary = match u16le(cc, 42).unwrap_or(65535) {
                0 => format!("linear: {} * x + {}", p(1), p(0)),
                1 => format!("table with interpolation, {n} entries"),
                2 => format!("table without interpolation, {n} entries"),
                6 => format!("polynomial: {} {} {} {} {} {}", p(0), p(1), p(2), p(3), p(4), p(5)),
                7 => "exponential".into(),
                8 => "logarithmic".into(),
                9 => format!("rational: ({} x² + {} x + {}) / ({} x² + {} x + {})", p(0), p(1), p(2), p(3), p(4), p(5)),
                10 => format!("formula: {}", bytes_at(cc, 46, 256).map(c_text).unwrap_or_default()),
                11 => format!("value to text, {n} entries"),
                12 => format!("value range to text, {n} entries"),
                132 => "date".into(),
                133 => "time".into(),
                65535 => "identity".into(),
                k => format!("type {k}"),
            };
            line(out, 2, "Conversion", summary);
            if u16le(cc, 42) == Some(11) {
                for i in 0..n.min(MAX_TABLE) {
                    let at = 46 + 40 * i;
                    if let (Some(v), Some(t)) = (f64le(cc, at), bytes_at(cc, at + 8, 32)) {
                        let _ = writeln!(out, "    {} = \"{}\"", num(v), escape(&c_text(t)));
                    }
                }
            }
        }
        if let Some(ce) = self.block(u32le(cn, 12).unwrap_or(0), b"CE") {
            match u16le(ce, 4) {
                Some(2) => line(
                    out,
                    2,
                    "Source",
                    format!(
                        "ECU {}, module {}",
                        bytes_at(ce, 92, 32).map(c_text).unwrap_or_default(),
                        u16le(ce, 6).unwrap_or(0)
                    ),
                ),
                Some(19) => line(
                    out,
                    2,
                    "Source",
                    format!(
                        "CAN message {} (id 0x{:X}), channel {}",
                        bytes_at(ce, 14, 36).map(c_text).unwrap_or_default(),
                        u32le(ce, 6).unwrap_or(0),
                        u32le(ce, 10).unwrap_or(0)
                    ),
                ),
                _ => {}
            }
        }
        if let Some(c) = self.text(u32le(cn, 20).unwrap_or(0)) {
            show_text(out, 2, "Comment", &c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mdf/", $name))
        };
    }
    const V1: &[u8] = fixture!("drive-v1.mf4");
    const V2: &[u8] = fixture!("drive-v2.mf4");
    const V3: &[u8] = fixture!("drive-v1.mdf");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn mdf4_summary() {
        let s = summary(V1).unwrap();
        for expected in [
            "Version:           4.10",
            "Start time:        2026-10-01 08:30:00.000 UTC",
            "Comment:           Test drive on the proving ground",
            "Data groups:       2",
            "=== Group 1.1: Engine ===",
            "Cycles:            100",
            "Channel time\n  Kind:              master (time)\n",
            "=== File history (2) ===\nEntry 1\n  Comment:           Added new embedded attachment from calibration.ini\n  tool_id:           asammdf\n",
            "Channel EngineSpeed\n  Data type:         unsigned integer, little-endian\n  Position:          byte 8, bit 0, 16 bits\n  Unit:              rpm\n",
            "Conversion:        linear: 0.25 * x + 0",
            "Conversion:        value to text, 3 entries\n    0 = \"Neutral\"\n    1 = \"First\"\n    2 = \"Second\"\n",
            "=== Attachments (1) ===",
        ] {
            assert!(s.contains(expected), "{expected}\n---\n{s}");
        }
    }

    #[test]
    fn mdf3_summary() {
        let s = summary(V3).unwrap();
        for expected in
            ["Version:           3.30", "Channel EngineSpeed\n", "Unit:              rpm", "Data groups:       2"]
        {
            assert!(s.contains(expected), "{expected}\n---\n{s}");
        }
    }

    #[test]
    fn an_added_channel_and_a_changed_unit() {
        let (a, b) = (summary(V1).unwrap(), summary(V2).unwrap());
        let removed = lines_only_in(&a, &b);
        assert!(removed.contains(&"  Unit:              degC"), "{removed:?}");
        let added = lines_only_in(&b, &a);
        assert!(added.contains(&"Channel OilPressure"), "{added:?}");
        assert!(added.contains(&"  Unit:              K"), "{added:?}");
    }

    #[test]
    fn garbage_never_panics() {
        assert!(!is_mdf(b"MDF     4.10"));
        assert!(summary(b"hello").is_err());
        for data in [V1, V3] {
            for end in (64..data.len()).step_by(53) {
                let _ = summary(&data[..end]);
            }
            let mut bytes = data.to_vec();
            for i in (64..bytes.len()).step_by(11) {
                bytes[i] = bytes[i].wrapping_add(0x5A);
                let _ = summary(&bytes[..bytes.len().min(i + 4096)]);
            }
            let _ = summary(&bytes);
        }
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc(0), "1970-01-01 00:00:00.000 UTC");
        assert_eq!(utc(951_782_400_500_000_000), "2000-02-29 00:00:00.500 UTC");
    }
}
