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
    let mut s =
        file.open_stream(path).map_err(|_| Error::Unsupported(format!("the .doc file has no {path} stream")))?;
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
    let (start, len) =
        if cb == 0 { (offset + 2, usize::from(*page.get(offset + 1)?) * 2) } else { (offset + 1, cb * 2 - 1) };
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
    let mut file = cfb::CompoundFile::open(Cursor::new(data))
        .map_err(|e| Error::Unsupported(format!("not a readable .doc file: {e}")))?;
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

    const REPORT: &str = "Überblick
Der Umsatz stieg um 12 € auf 1.234 €. Grüße aus Köln.
日本語のテキストと中文。
First line
second line after a break.
Visit the example site for details.
Reviewed text here.
Region  │ Q1  │ Q2  │\x20
Nord  │ 100  │ 120  │\x20
Süd und West  │  │ 90  │\x20
Silbentrennung und Bindestrich\u{2011}fest.
Before the box.
Closing paragraph on a new page.

=== Header 1 ===
Quarterly report – Müller & Söhne

=== Footer 1 ===
Page 2

=== Footnotes ===


\tAlle Beträge in Euro.
\tSecond footnote.

=== Endnotes ===


\tAn endnote.

=== Comments ===
Please check this number.

=== Text boxes ===
Boxed text
";

    #[test]
    fn report() {
        assert_eq!(text(fixture!("doc-report.doc")).unwrap(), REPORT);
    }

    #[test]
    fn compressed_and_utf16_pieces() {
        let expected = "Plain 8-bit text: café\nA second paragraph with a tab:\there.\nGröße – Ende 3€\n";
        assert_eq!(text(fixture!("doc-mixed.doc")).unwrap(), expected);
        assert_eq!(
            text(fixture!("doc-plain.doc")).unwrap(),
            "Plain ASCII text only.\nA second paragraph with a tab:\there.\nThird and last.\n"
        );
    }

    /// The same documents converted to .docx by LibreOffice read the same, except
    /// for text boxes: the .docx reader shows them where they are anchored, twice
    /// (the drawing and its VML fallback), after the page break LibreOffice moves
    /// there; the .doc keeps them apart from the text, so they come last.
    #[test]
    fn same_as_docx() {
        let docx = |data: &[u8]| super::super::office::ooxml_text("docx", data).unwrap();
        let native = text(fixture!("doc-report.doc")).unwrap();
        let (native, boxes) = native.split_once("\n=== Text boxes ===\n").unwrap();
        assert_eq!(boxes, "Boxed text\n");
        let converted = docx(fixture!("doc-report.docx"));
        assert_eq!(native, converted.replacen("\nBoxed text\nBoxed text\n", "", 1));
        for (doc, docx_data) in [
            (&fixture!("doc-plain.doc")[..], &fixture!("doc-plain.docx")[..]),
            (fixture!("doc-mixed.doc"), fixture!("doc-mixed.docx")),
        ] {
            assert_eq!(text(doc).unwrap(), docx(docx_data));
        }
    }

    #[test]
    fn damaged_input() {
        for data in [&fixture!("doc-report.doc")[..], fixture!("doc-mixed.doc")] {
            for n in (0..data.len()).step_by(509).chain([1, 511, 513, data.len() - 1]) {
                let _ = text(&data[..n]);
            }
            for step in [97, 13, 5] {
                let mut garbage = data.to_vec();
                for i in (512..garbage.len()).step_by(step) {
                    garbage[i] ^= 0x5A;
                }
                let _ = text(&garbage);
            }
        }
        assert!(text(b"").is_err());
        assert!(text(&fixture!("doc-report.doc")[..4096]).is_err());
    }

    /// Rewrites a header field of the WordDocument stream (always the first
    /// 512-byte sector after the header and FAT in these small V3 files).
    fn patched(at: usize, value: u16) -> Vec<u8> {
        let mut data = fixture!("doc-plain.doc").to_vec();
        let word = data.windows(2).position(|w| w == [0xEC, 0xA5]).unwrap();
        data[word + at..word + at + 2].copy_from_slice(&value.to_le_bytes());
        data
    }

    #[test]
    fn unsupported_variants() {
        let message = |data: &[u8]| match text(data) {
            Err(Error::Unsupported(m)) => m,
            other => panic!("{other:?}"),
        };
        assert_eq!(message(&patched(2, 0x0068)), "Word 6/95 documents are not read natively");
        assert_eq!(message(&patched(0x0A, 0x13F0)), "the .doc file is encrypted");
        assert_eq!(message(&patched(0, 0x1234)), "not a Word document");
        assert!(message(fixture!("../email/order.msg")).contains("WordDocument"));
    }

    /// LibreOffice writes all text as UTF-16. Word stores runs of Windows-1252
    /// text compressed (8 bits per character), so `doc-mixed.doc` is `doc-plain.doc`
    /// rewritten with a compressed piece followed by a UTF-16 one; the
    /// formatting tables are moved along. Run with
    /// `cargo test -p parallax-core regenerate_mixed -- --ignored`, then convert
    /// the result to `doc-mixed.docx` with LibreOffice (see doc-regenerate.sh).
    #[test]
    #[ignore]
    fn regenerate_mixed_fixture() {
        use std::io::Write;
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-office/");
        let source = std::fs::read(format!("{dir}doc-plain.doc")).unwrap();
        let mut file = cfb::CompoundFile::open(Cursor::new(&source[..])).unwrap();
        let mut word = stream(&mut file, "/WordDocument").unwrap();
        let mut table = stream(&mut file, "/1Table").unwrap();
        let fib = Fib::parse(&word).unwrap();
        let (units, pieces) = characters(&word, fib.part(&table, CLX)).unwrap();
        assert_eq!((pieces.len(), pieces[0].fc), (1, 0x800));
        let mut text = String::from_utf16(&units).unwrap();
        // Same lengths, so that every character position stays valid.
        text = text.replacen("Plain ASCII text only.", "Plain 8-bit text: café", 1);
        text = text.replacen("Third and last.", "Größe – Ende 3€", 1);
        let split = text.find("Größe").unwrap();
        let (first, second) = text.split_at(split);
        let a = first.chars().count();
        assert_eq!(a % 2, 0);
        let (compressed, _, _) = encoding_rs::WINDOWS_1252.encode(first);
        assert_eq!(compressed.len(), a);
        let base = 0x800usize;
        let utf16: Vec<u8> = second.encode_utf16().flat_map(u16::to_le_bytes).collect();
        word[base..base + a].copy_from_slice(&compressed);
        word[base + a..base + a + utf16.len()].copy_from_slice(&utf16);
        let old_end = base + units.len() * 2;
        word[base + a + utf16.len()..old_end].fill(0);
        let remap = |fc: u32| -> u32 {
            let fc = fc as usize;
            (if fc <= base {
                fc
            } else if fc <= base + 2 * a {
                base + (fc - base) / 2
            } else {
                fc - a
            }) as u32
        };
        let put = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        let shift = |b: &mut [u8], at: usize| {
            let v = remap(u32_at(b, at).unwrap());
            put(b, at, v);
        };
        // fcMac, then the bin tables of character and paragraph runs and their pages.
        shift(&mut word, 0x1C);
        for i in [12, 13] {
            let (fc, lcb) = fib.fc_lcb[i];
            let n = (lcb - 4) / 8;
            for k in 0..=n {
                shift(&mut table, fc + k * 4);
            }
            for k in 0..n {
                let page = (u32_at(&table, fc + (n + 1) * 4 + k * 4).unwrap() & 0x3F_FFFF) as usize * 512;
                let crun = usize::from(word[page + 511]);
                for r in 0..=crun {
                    shift(&mut word, page + r * 4);
                }
            }
        }
        // A new piece table at the end of the table stream.
        let (clx_fc, clx_lcb) = fib.fc_lcb[CLX];
        let old = &table[clx_fc..clx_fc + clx_lcb];
        let (pcd_flags, prm) = (&old[5 + 8..5 + 10].to_vec(), &old[5 + 14..5 + 16].to_vec());
        let mut clx = vec![0x02];
        clx.extend(28u32.to_le_bytes());
        for cp in [0, a, units.len()] {
            clx.extend((cp as u32).to_le_bytes());
        }
        for fc in [(base as u32 * 2) | 0x4000_0000, (base + a) as u32] {
            clx.extend(pcd_flags);
            clx.extend(fc.to_le_bytes());
            clx.extend(prm);
        }
        let new_fc = table.len();
        table.extend(&clx);
        let fc_at = 34 + 14 * 2 + 2 + 22 * 4 + 2 + CLX * 8;
        assert_eq!(u32_at(&word, fc_at).unwrap() as usize, clx_fc);
        put(&mut word, fc_at, new_fc as u32);
        put(&mut word, fc_at + 4, clx.len() as u32);

        let path = format!("{dir}doc-mixed.doc");
        let mut out =
            cfb::CompoundFile::create_with_version(cfb::Version::V3, std::fs::File::create(path).unwrap()).unwrap();
        out.set_storage_clsid("/", *file.root_entry().clsid()).unwrap();
        let names: Vec<String> = file.read_root_storage().map(|e| e.name().to_string()).collect();
        for name in names {
            let data = match name.as_str() {
                "WordDocument" => word.clone(),
                "1Table" => table.clone(),
                _ => stream(&mut file, &format!("/{name}")).unwrap(),
            };
            out.create_stream(format!("/{name}")).unwrap().write_all(&data).unwrap();
        }
        out.flush().unwrap();
    }
}
