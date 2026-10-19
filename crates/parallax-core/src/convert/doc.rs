//! Legacy Word documents (.doc, Word 97–2003, [MS-DOC]) read without LibreOffice.
//!
//! The text of all stories (main text, footnotes, headers, comments, endnotes,
//! text boxes) is one character stream, assembled from the piece table; the FIB
//! says how many characters each story has. Paragraph properties are read only
//! as far as needed to tell table cells from rows. The output follows the .docx
//! text of `office::ooxml_text`, so a .doc and the same document saved as .docx
//! read the same.

use std::io::{Cursor, Read};

use crate::{Error, Result};

/// Streams larger than this are not read.
const MAX_STREAM: u64 = 512 * 1024 * 1024;

pub fn text(data: &[u8]) -> Result<String> {
    // cfb validates its input, but a hostile file should never take the app down.
    std::panic::catch_unwind(|| read(data)).unwrap_or_else(|_| Err(damaged()))
}

fn damaged() -> Error {
    Error::Unsupported("the .doc file is damaged".into())
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn stream(file: &mut cfb::CompoundFile<Cursor<&[u8]>>, path: &str) -> Result<Vec<u8>> {
    let mut s = file.open_stream(path).map_err(|_| Error::Unsupported(format!("the .doc file has no {path} stream")))?;
    if s.len() > MAX_STREAM {
        return Err(Error::Unsupported("the .doc file is too large".into()));
    }
    let mut buf = Vec::with_capacity(s.len() as usize);
    s.read_to_end(&mut buf).map_err(|_| damaged())?;
    Ok(buf)
}

/// The parts of the File Information Block the reader needs.
struct Fib {
    table1: bool,
    /// Character counts of the stories in stream order: main text, footnotes,
    /// headers, macros (unused), comments, endnotes, text boxes, header text boxes.
    ccp: [usize; 8],
    /// FibRgFcLcb97 as (offset, size) pairs into the table stream.
    fc_lcb: Vec<(usize, usize)>,
}

impl Fib {
    fn parse(word: &[u8]) -> Result<Fib> {
        if u16_at(word, 0) != Some(0xA5EC) {
            return Err(Error::Unsupported("not a Word document".into()));
        }
        let nfib = u16_at(word, 2).ok_or_else(damaged)?;
        if nfib < 0x00C1 {
            return Err(Error::Unsupported("Word 6/95 documents are not read natively".into()));
        }
        let flags = u16_at(word, 0x0A).ok_or_else(damaged)?;
        if flags & 0x0100 != 0 {
            return Err(Error::Unsupported("the .doc file is encrypted".into()));
        }
        let csw = usize::from(u16_at(word, 32).ok_or_else(damaged)?);
        let lw_at = 34 + csw * 2;
        let cslw = usize::from(u16_at(word, lw_at).ok_or_else(damaged)?);
        let lw = |i: usize| -> Result<usize> {
            if i >= cslw {
                return Ok(0);
            }
            let v = u32_at(word, lw_at + 2 + i * 4).ok_or_else(damaged)? as i32;
            usize::try_from(v).map_err(|_| damaged())
        };
        let mut ccp = [0; 8];
        for (n, slot) in ccp.iter_mut().enumerate() {
            *slot = lw(3 + n)?;
        }
        ccp[3] = 0;
        let fc_at = lw_at + 2 + cslw * 4;
        let cb = usize::from(u16_at(word, fc_at).ok_or_else(damaged)?);
        let fc_lcb = (0..cb)
            .map(|i| {
                let at = fc_at + 2 + i * 8;
                Some((u32_at(word, at)? as usize, u32_at(word, at + 4)? as usize))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(damaged)?;
        Ok(Fib { table1: flags & 0x0200 != 0, ccp, fc_lcb })
    }

    /// The bytes of FibRgFcLcb97 entry `i` in the table stream; empty if absent.
    fn part<'a>(&self, table: &'a [u8], i: usize) -> &'a [u8] {
        match self.fc_lcb.get(i) {
            Some(&(fc, lcb)) if lcb > 0 => table.get(fc..fc.saturating_add(lcb)).unwrap_or_default(),
            _ => &[],
        }
    }
}

// Indexes into FibRgFcLcb97.
const PLCF_HDD: usize = 11;
const PLCF_BTE_PAPX: usize = 13;
const CLX: usize = 33;

/// A run of the piece table: characters from `cp` on are stored at byte `fc`.
struct Piece {
    cp: usize,
    fc: usize,
    compressed: bool,
}

/// Reads the piece table and returns the whole character stream with its pieces.
fn characters(word: &[u8], clx: &[u8]) -> Result<(Vec<u16>, Vec<Piece>)> {
    let mut at = 0;
    // Prc entries (formatting of the pieces) come first; only the piece table matters.
    while clx.get(at) == Some(&0x01) {
        at += 3 + usize::from(u16_at(clx, at + 1).ok_or_else(damaged)?);
    }
    if clx.get(at) != Some(&0x02) {
        return Err(damaged());
    }
    let lcb = u32_at(clx, at + 1).ok_or_else(damaged)? as usize;
    let plc = clx.get(at + 5..(at + 5).saturating_add(lcb)).ok_or_else(damaged)?;
    if lcb < 4 || (lcb - 4) % 12 != 0 {
        return Err(damaged());
    }
    let n = (lcb - 4) / 12;
    let (mut units, mut pieces) = (Vec::new(), Vec::with_capacity(n));
    for i in 0..n {
        let (start, end) = (u32_at(plc, i * 4).ok_or_else(damaged)?, u32_at(plc, i * 4 + 4).ok_or_else(damaged)?);
        if start as usize != units.len() || end < start {
            return Err(damaged());
        }
        let len = (end - start) as usize;
        let raw = u32_at(plc, (n + 1) * 4 + i * 8 + 2).ok_or_else(damaged)?;
        let compressed = raw & 0x4000_0000 != 0;
        let fc = (raw & 0x3FFF_FFFF) as usize;
        if compressed {
            let fc = fc / 2;
            let bytes = word.get(fc..fc + len).ok_or_else(damaged)?;
            let (text, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes);
            units.extend(text.encode_utf16());
            pieces.push(Piece { cp: start as usize, fc, compressed });
        } else {
            let bytes = word.get(fc..fc + len * 2).ok_or_else(damaged)?;
            units.extend(bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)));
            pieces.push(Piece { cp: start as usize, fc, compressed });
        }
    }
    Ok((units, pieces))
}

/// What the paragraph properties say about a paragraph's place in a table.
#[derive(Clone, Copy, Default)]
struct Para {
    in_table: bool,
    /// The paragraph is the end of a table row.
    row_end: bool,
    /// The paragraph mark ends a cell of a nested table.
    inner_cell: bool,
    inner_row_end: bool,
}

/// Paragraph properties by file position: (start, end, properties), sorted.
struct Paragraphs(Vec<(usize, usize, Para)>);

impl Paragraphs {
    /// Reads the PAPX FKPs listed in the PlcBtePapx; broken pages are skipped.
    fn read(word: &[u8], bte: &[u8]) -> Paragraphs {
        let mut runs = Vec::new();
        let n = bte.len().saturating_sub(4) / 8;
        for i in 0..n {
            let Some(pn) = u32_at(bte, (n + 1) * 4 + i * 4) else { break };
            let at = (pn & 0x3F_FFFF) as usize * 512;
            let Some(page) = word.get(at..at + 512) else { continue };
            let crun = usize::from(page[511]);
            for r in 0..crun {
                let (Some(start), Some(end)) = (u32_at(page, r * 4), u32_at(page, r * 4 + 4)) else { break };
                let offset = page.get((crun + 1) * 4 + r * 13).map_or(0, |b| usize::from(*b) * 2);
                let props = if offset == 0 { Para::default() } else { papx(page, offset).unwrap_or_default() };
                runs.push((start as usize, end as usize, props));
            }
        }
        runs.sort_by_key(|r| r.0);
        Paragraphs(runs)
    }

    fn at(&self, fc: usize) -> Option<Para> {
        let i = self.0.partition_point(|r| r.0 <= fc).checked_sub(1)?;
        let (_, end, props) = self.0[i];
        (fc < end).then_some(props)
    }
}

/// Parses a PapxInFkp at `offset` of an FKP page.
fn papx(page: &[u8], offset: usize) -> Option<Para> {
    let cb = usize::from(*page.get(offset)?);
    let (start, len) = if cb == 0 { (offset + 2, usize::from(*page.get(offset + 1)?) * 2) } else { (offset + 1, cb * 2 - 1) };
    // The style index comes before the properties.
    let grpprl = page.get(start + 2..start + len)?;
    let mut para = Para::default();
    let mut at = 0;
    while let Some(sprm) = u16_at(grpprl, at) {
        let rest = &grpprl[at + 2..];
        let len = match sprm >> 13 {
            0 | 1 => 1,
            2 | 4 | 5 => 2,
            3 => 4,
            7 => 3,
            // sprmTDefTable has a 16-bit size, counting itself less one byte.
            _ if sprm == 0xD608 || sprm == 0xD606 => usize::from(u16_at(rest, 0)?) + 1,
            // sprmPChgTabs with size 255 has a layout of its own; nothing after it matters.
            _ if sprm == 0xC615 && rest.first() == Some(&255) => break,
            _ => usize::from(*rest.first()?) + 1,
        };
        let on = rest.first().is_some_and(|b| *b != 0);
        match sprm {
            0x2416 => para.in_table = on,
            0x6649 => para.in_table = u32_at(rest, 0).is_some_and(|v| v > 0),
            0x2417 => para.row_end = on,
            0x244B => para.inner_cell = on,
            0x244C => para.inner_row_end = on,
            _ => {}
        }
        at += 2 + len;
    }
    Some(para)
}

struct Doc {
    units: Vec<u16>,
    pieces: Vec<Piece>,
    paragraphs: Paragraphs,
}

impl Doc {
    fn para(&self, cp: usize) -> Option<Para> {
        let i = self.pieces.partition_point(|p| p.cp <= cp).checked_sub(1)?;
        let p = &self.pieces[i];
        let fc = p.fc + (cp - p.cp) * if p.compressed { 1 } else { 2 };
        self.paragraphs.at(fc)
    }

    /// Renders the characters `range` as the .docx reader renders the same story.
    fn render(&self, range: std::ops::Range<usize>, out: &mut String) {
        // Per open field: whether its result (after the separator) has begun.
        let mut fields: Vec<bool> = Vec::new();
        let (mut has_text, mut after_cell) = (false, false);
        let units = &self.units[range.clone()];
        let mut i = 0;
        while i < units.len() {
            let (cp, u) = (range.start + i, units[i]);
            i += 1;
            match u {
                0x13 => fields.push(false),
                0x14 => {
                    if let Some(f) = fields.last_mut() {
                        *f = true;
                    }
                }
                0x15 => {
                    fields.pop();
                }
                _ if !fields.iter().all(|f| *f) => {}
                0x0D => {
                    let para = self.para(cp).unwrap_or_default();
                    if para.inner_row_end {
                        out.push('\n');
                    } else if para.inner_cell {
                        end_cell(out, has_text);
                    } else if para.in_table {
                        if has_text {
                            out.push(' ');
                        }
                    } else {
                        out.push('\n');
                    }
                    (has_text, after_cell) = (false, false);
                }
                // Cell and row ends; without paragraph properties a doubled mark ends the row.
                0x07 => {
                    let row_end = self.para(cp).map_or(after_cell, |p| p.row_end);
                    if row_end {
                        out.push('\n');
                    } else {
                        end_cell(out, has_text);
                    }
                    (has_text, after_cell) = (false, !row_end);
                }
                0x09 => out.push('\t'),
                // Line, page/section and column breaks.
                0x0B | 0x0C | 0x0E => out.push('\n'),
                0x1E => out.push('\u{2011}'),
                // Optional hyphens, note and comment references, object anchors.
                0..0x20 => {}
                0xD800..0xDC00 => {
                    if let Some(c) = units.get(i).and_then(|lo| char::decode_utf16([u, *lo]).next()?.ok()) {
                        out.push(c);
                        i += 1;
                        has_text = true;
                    }
                }
                _ => {
                    out.push(char::from_u32(u.into()).unwrap_or(char::REPLACEMENT_CHARACTER));
                    has_text = true;
                }
            }
        }
    }

    /// Renders a story that ends with a paragraph mark of its own (headers, notes):
    /// that mark is left out, the text ends with a newline.
    fn story(&self, range: std::ops::Range<usize>) -> String {
        let end = if range.end > range.start && self.units[range.end - 1] == 0x0D { range.end - 1 } else { range.end };
        let mut out = String::new();
        self.render(range.start..end, &mut out);
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }
}

fn end_cell(out: &mut String, has_text: bool) {
    if has_text {
        out.push(' ');
    }
    out.push_str(" │ ");
}

fn read(data: &[u8]) -> Result<String> {
    let mut file =
        cfb::CompoundFile::open(Cursor::new(data)).map_err(|e| Error::Unsupported(format!("not a readable .doc file: {e}")))?;
    let word = stream(&mut file, "/WordDocument")?;
    let fib = Fib::parse(&word)?;
    let table = stream(&mut file, if fib.table1 { "/1Table" } else { "/0Table" })?;
    let (units, pieces) = characters(&word, fib.part(&table, CLX))?;
    let paragraphs = Paragraphs::read(&word, fib.part(&table, PLCF_BTE_PAPX));
    let doc = Doc { units, pieces, paragraphs };
    // Story ranges in stream order.
    let mut ranges = Vec::with_capacity(8);
    let mut cp = 0usize;
    for n in fib.ccp {
        let end = cp.checked_add(n).filter(|e| *e <= doc.units.len()).ok_or_else(damaged)?;
        ranges.push(cp..end);
        cp = end;
    }
    let mut out = String::new();
    doc.render(ranges[0].clone(), &mut out);

    // Headers and footers: six separator stories, then six stories per section
    // (even/odd header, even/odd footer, first-page header and footer), then a guard.
    let (mut headers, mut footers) = (Vec::new(), Vec::new());
    let hdd = fib.part(&table, PLCF_HDD);
    let cps: Vec<usize> = hdd.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c) as usize).collect();
    let base = ranges[2].start;
    for i in 6..cps.len().saturating_sub(2) {
        let (start, end) = (base + cps[i], base + cps[i + 1]);
        if start >= end || end > ranges[2].end {
            continue;
        }
        let text = doc.story(start..end);
        if text.trim().is_empty() {
            continue;
        }
        match (i - 6) % 6 {
            0 | 1 | 4 => headers.push(text),
            _ => footers.push(text),
        }
    }
    for (title, stories) in [("Header", headers), ("Footer", footers)] {
        for (n, text) in stories.iter().enumerate() {
            out.push_str(&format!("\n=== {title} {} ===\n", n + 1));
            out.push_str(text);
        }
    }
    // Each story ends with one more paragraph mark than its notes or comments have.
    // A .docx starts its notes with the separator and continuation separator,
    // each an empty paragraph; they are added here so both read the same.
    for (title, range, separators) in
        [("Footnotes", &ranges[1], true), ("Endnotes", &ranges[5], true), ("Comments", &ranges[4], false)]
    {
        if range.is_empty() {
            continue;
        }
        out.push_str(&format!("\n=== {title} ===\n"));
        if separators {
            out.push_str("\n\n");
        }
        out.push_str(&doc.story(range.clone()));
    }
    let mut boxes = String::new();
    for range in [&ranges[6], &ranges[7]] {
        let text = doc.story(range.clone());
        for line in text.trim_end().lines() {
            if !line.trim().is_empty() {
                boxes.push_str(line);
                boxes.push('\n');
            }
        }
    }
    if !boxes.is_empty() {
        out.push_str("\n=== Text boxes ===\n");
        out.push_str(&boxes);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-office/", $name))
        };
    }

    #[test]
    fn dump() {
        println!("{}", text(fixture!("doc-report.doc")).unwrap());
        println!("----\n{}", super::super::office::ooxml_text("docx", fixture!("doc-report.docx")).unwrap());
    }
}
