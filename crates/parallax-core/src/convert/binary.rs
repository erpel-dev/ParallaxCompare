//! Text views of binary formats: executables, firmware images and plain hex dumps.

use std::fmt::Write;

use object::{Object, ObjectSection, ObjectSymbol};

use super::MAX_HEX_BYTES;
use crate::{Error, Result};

/// Header, sections (with a CRC of their contents) and symbols of ELF, PE and Mach-O
/// files. Symbols are sorted by name so that moved code shows as changed addresses
/// on the same line rather than as removed and added lines.
pub fn executable_summary(data: &[u8]) -> Result<String> {
    let file = object::File::parse(data).map_err(|e| Error::Unsupported(format!("executable: {e}")))?;
    let mut out = String::new();
    let _ = writeln!(out, "Format:        {:?}", file.format());
    let _ = writeln!(out, "Architecture:  {:?}", file.architecture());
    let _ = writeln!(out, "Endianness:    {:?}", file.endianness());
    let _ = writeln!(out, "Kind:          {:?}", file.kind());
    let _ = writeln!(out, "Entry point:   0x{:x}", file.entry());
    if let Ok(Some(id)) = file.build_id() {
        let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
        let _ = writeln!(out, "Build id:      {hex}");
    }

    let _ = writeln!(out, "\n=== Sections ===");
    let _ = writeln!(out, "{:<22} {:>12} {:>9}  {:<8}  kind", "name", "address", "size", "crc32");
    for section in file.sections() {
        let name = section.name().unwrap_or("?");
        let crc = match section.data() {
            Ok(d) if !d.is_empty() => format!("{:08x}", crc32fast::hash(d)),
            _ => "-".into(),
        };
        let _ = writeln!(
            out,
            "{:<22} {:>12} {:>9}  {:<8}  {:?}",
            name,
            format!("0x{:x}", section.address()),
            section.size(),
            crc,
            section.kind()
        );
    }

    let mut symbols: Vec<(String, String)> = file
        .symbols()
        .chain(file.dynamic_symbols())
        .filter(|s| s.is_definition() && !s.name().unwrap_or("").is_empty())
        .map(|s| {
            let name = s.name().unwrap_or("?").to_string();
            let line = format!(
                "{:<36} {:>12} {:>7}  {:?}{}",
                name,
                format!("0x{:x}", s.address()),
                s.size(),
                s.kind(),
                if s.is_global() { "" } else { " (local)" }
            );
            (name, line)
        })
        .collect();
    symbols.sort();
    symbols.dedup();
    let _ = writeln!(out, "\n=== Symbols ({}) ===", symbols.len());
    for (_, line) in &symbols {
        let _ = writeln!(out, "{line}");
    }

    if let Ok(imports) = file.imports() {
        let mut imports: Vec<String> = imports
            .flatten()
            .map(|i| {
                let lib = String::from_utf8_lossy(i.library());
                let name = match i.name() {
                    object::read::NameOrOrdinal::Name(n) => String::from_utf8_lossy(n),
                    object::read::NameOrOrdinal::Ordinal(o) => format!("#{o}").into(),
                };
                if lib.is_empty() { name.into_owned() } else { format!("{lib}: {name}") }
            })
            .collect();
        imports.sort();
        imports.dedup();
        if !imports.is_empty() {
            let _ = writeln!(out, "\n=== Imports ({}) ===", imports.len());
            for i in imports {
                let _ = writeln!(out, "{i}");
            }
        }
    }
    Ok(out)
}

/// A contiguous run of bytes at an address.
struct Segment {
    start: u64,
    bytes: Vec<u8>,
}

fn hex_byte(s: &[u8], i: usize) -> Option<u8> {
    let digit = |c: u8| (c as char).to_digit(16);
    Some((digit(*s.get(i)?)? * 16 + digit(*s.get(i + 1)?)?) as u8)
}

fn hex_bytes(s: &[u8]) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| hex_byte(s, i)).collect()
}

/// Decodes Intel HEX or Motorola S-record text into data records.
fn parse_records(data: &[u8], notes: &mut Vec<String>) -> Vec<Segment> {
    let mut records = Vec::new();
    let mut base: u64 = 0;
    for (n, raw) in data.split(|&b| b == b'\n').enumerate() {
        let line = raw.trim_ascii();
        if line.is_empty() {
            continue;
        }
        let line_no = n + 1;
        match line[0] {
            b':' => {
                let Some(bytes) = hex_bytes(&line[1..]) else {
                    notes.push(format!("line {line_no}: not a valid Intel HEX record"));
                    continue;
                };
                if bytes.len() < 5 || bytes.len() != bytes[0] as usize + 5 {
                    notes.push(format!("line {line_no}: record length mismatch"));
                    continue;
                }
                if bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b)) != 0 {
                    notes.push(format!("line {line_no}: checksum error"));
                }
                let addr = u16::from_be_bytes([bytes[1], bytes[2]]) as u64;
                let payload = &bytes[4..bytes.len() - 1];
                match bytes[3] {
                    0x00 => records.push(Segment { start: base + addr, bytes: payload.to_vec() }),
                    0x02 if payload.len() == 2 => base = (u16::from_be_bytes([payload[0], payload[1]]) as u64) << 4,
                    0x04 if payload.len() == 2 => base = (u16::from_be_bytes([payload[0], payload[1]]) as u64) << 16,
                    0x03 | 0x05 => {
                        let hex: String = payload.iter().map(|b| format!("{b:02X}")).collect();
                        notes.push(format!("start address: {hex}"));
                    }
                    _ => {}
                }
            }
            b'S' if line.len() >= 4 => {
                let kind = line[1];
                let Some(bytes) = hex_bytes(&line[2..]) else {
                    notes.push(format!("line {line_no}: not a valid S-record"));
                    continue;
                };
                if bytes.is_empty() || bytes.len() != bytes[0] as usize + 1 {
                    notes.push(format!("line {line_no}: record length mismatch"));
                    continue;
                }
                if bytes[..bytes.len() - 1].iter().fold(0u8, |a, b| a.wrapping_add(*b)) != !bytes[bytes.len() - 1] {
                    notes.push(format!("line {line_no}: checksum error"));
                }
                let addr_len = match kind {
                    b'1' | b'9' => 2,
                    b'2' | b'8' => 3,
                    b'3' | b'7' => 4,
                    b'0' => {
                        let text = String::from_utf8_lossy(&bytes[3..bytes.len() - 1]).into_owned();
                        notes.push(format!("header: {}", text.trim_end_matches('\0')));
                        continue;
                    }
                    _ => continue,
                };
                if bytes.len() < addr_len + 2 {
                    continue;
                }
                let addr = bytes[1..1 + addr_len].iter().fold(0u64, |a, b| a << 8 | *b as u64);
                match kind {
                    b'1'..=b'3' => {
                        records.push(Segment { start: addr, bytes: bytes[1 + addr_len..bytes.len() - 1].to_vec() })
                    }
                    _ => notes.push(format!("start address: 0x{addr:08X}")),
                }
            }
            _ => notes.push(format!("line {line_no}: unknown record")),
        }
    }
    records
}

/// Merges records into contiguous segments; later records win on overlap.
fn merge(mut records: Vec<Segment>, notes: &mut Vec<String>) -> Vec<Segment> {
    // A stable sort keeps file order for equal addresses, so "later wins" holds.
    records.sort_by_key(|r| r.start);
    let mut out: Vec<Segment> = Vec::new();
    for rec in records {
        if let Some(last) = out.last_mut() {
            let end = last.start + last.bytes.len() as u64;
            if rec.start <= end {
                let offset = (rec.start - last.start) as usize;
                if offset < last.bytes.len() {
                    notes.push(format!("overlapping data at 0x{:08X}", rec.start));
                }
                let needed = offset + rec.bytes.len();
                if last.bytes.len() < needed {
                    last.bytes.resize(needed, 0xFF);
                }
                last.bytes[offset..needed].copy_from_slice(&rec.bytes);
                continue;
            }
        }
        out.push(rec);
    }
    out
}

/// Intel HEX / S-record as a memory map: one line per 16-byte aligned address, so
/// files with different record lengths or record order still line up.
pub fn firmware_map(data: &[u8]) -> Result<String> {
    let mut notes = Vec::new();
    let segments = merge(parse_records(data, &mut notes), &mut notes);
    let total: usize = segments.iter().map(|s| s.bytes.len()).sum();
    if total > MAX_HEX_BYTES * 4 {
        return Err(Error::Unsupported(format!("{total} bytes of firmware data is too large to show")));
    }
    let mut out = String::new();
    let _ = writeln!(out, "{} bytes in {} segment(s)", total, segments.len());
    for note in &notes {
        let _ = writeln!(out, "! {note}");
    }
    for seg in &segments {
        let end = seg.start + seg.bytes.len() as u64;
        let _ = writeln!(out, "\n--- 0x{:08X} .. 0x{:08X} ({} bytes) ---", seg.start, end - 1, seg.bytes.len());
        let mut row = seg.start & !0xF;
        while row < end {
            let cells: Vec<Option<u8>> = (row..row + 16)
                .map(|a| (a >= seg.start && a < end).then(|| seg.bytes[(a - seg.start) as usize]))
                .collect();
            write_row(&mut out, row, &cells);
            row += 16;
        }
    }
    Ok(out)
}

fn write_row(out: &mut String, addr: u64, cells: &[Option<u8>]) {
    let _ = write!(out, "{addr:08X}  ");
    for (i, c) in cells.iter().enumerate() {
        match c {
            Some(b) => {
                let _ = write!(out, "{b:02X} ");
            }
            None => out.push_str("   "),
        }
        if i == 7 {
            out.push(' ');
        }
    }
    out.push_str(" |");
    for c in cells {
        out.push(match c {
            Some(b) if b.is_ascii_graphic() || *b == b' ' => *b as char,
            Some(_) => '.',
            None => ' ',
        });
    }
    out.push_str("|\n");
}

pub fn hex_dump(data: &[u8]) -> Result<String> {
    if data.len() > MAX_HEX_BYTES {
        return Err(Error::Unsupported(format!(
            "the file is {} MB; hex dumps are limited to {} MB",
            data.len() / 1_048_576,
            MAX_HEX_BYTES / 1_048_576
        )));
    }
    let mut out = String::with_capacity(data.len() / 16 * 78 + 80);
    for (i, chunk) in data.chunks(16).enumerate() {
        let mut cells: Vec<Option<u8>> = chunk.iter().copied().map(Some).collect();
        cells.resize(16, None);
        write_row(&mut out, i as u64 * 16, &cells);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intel_hex_and_srec_give_the_same_map() {
        let ihex = b":0400000001020304F2\n:020004000506EF\n:020000040001F9\n:010000009967\n:00000001FF\n";
        let map = firmware_map(ihex).unwrap();
        assert!(map.contains("00000000  01 02 03 04 05 06"), "{map}");
        assert!(map.contains("--- 0x00010000"), "{map}");
        assert!(map.contains("! line 4: checksum error"), "{map}");

        let srec = b"S00600004844521B\nS1090000010203040506E1\nS9030000FC\n";
        let map = firmware_map(srec).unwrap();
        assert!(map.contains("00000000  01 02 03 04 05 06"), "{map}");
        assert!(map.contains("header: HDR"), "{map}");
        assert!(!map.contains("checksum"), "{map}");
    }

    #[test]
    fn hex_rows() {
        let dump = hex_dump(b"ABC\x00").unwrap();
        assert!(dump.starts_with("00000000  41 42 43 00    "), "{dump}");
        assert!(dump.ends_with(" |ABC.            |\n"), "{dump}");
    }
}
