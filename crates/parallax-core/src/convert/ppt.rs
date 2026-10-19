//! PowerPoint 97–2003 presentations (.ppt) read from their record tree ([MS-PPT]).
//! The text matches what the .pptx reader shows for the same deck saved as .pptx:
//! a header per slide, a line per paragraph, then the speaker notes.
//!
//! The live state of a .ppt is reached through its last edit: the "Current User"
//! stream points to the newest UserEditAtom, whose chain of persist directories
//! maps object ids to stream offsets. The slide list of the DocumentContainer gives
//! the slide order; placeholder text is stored there too, and each slide's drawing
//! refers to it (OutlineTextRefAtom) or carries its text itself (text boxes, tables).

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};

use crate::{Error, Result};

const DOCUMENT: u16 = 0x03E8;
const SLIDE: u16 = 0x03EE;
const SLIDE_ATOM: u16 = 0x03EF;
const NOTES: u16 = 0x03F0;
const SLIDE_PERSIST: u16 = 0x03F3;
const TEXT_HEADER: u16 = 0x0F9F;
const OUTLINE_TEXT_REF: u16 = 0x0F9E;
const TEXT_CHARS: u16 = 0x0FA0;
const TEXT_BYTES: u16 = 0x0FA8;
const SLIDE_NUMBER_FIELD: u16 = 0x0FD8;
const SLIDE_LIST: u16 = 0x0FF0;
const USER_EDIT: u16 = 0x0FF5;
const PERSIST_DIRECTORY: u16 = 0x1772;

/// Deeper nesting than this only occurs in damaged files.
const MAX_DEPTH: usize = 64;

pub fn text(data: &[u8]) -> Result<String> {
    let mut file = cfb::CompoundFile::open(Cursor::new(data))
        .map_err(|e| Error::Unsupported(format!("not a readable .ppt file: {e}")))?;
    let mut read = |name: &str| -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        file.open_stream(name).ok()?.read_to_end(&mut buf).ok()?;
        Some(buf)
    };
    let doc = read("PowerPoint Document").ok_or_else(|| bad("no PowerPoint Document stream"))?;
    // CurrentUserAtom: record header, size, header token, then the offset of the current edit.
    let current = read("Current User").and_then(|s| Some(u32_at(&s, 16)? as usize));
    slides(&doc, current)
}

/// The text of the slides in the "PowerPoint Document" stream `doc`.
fn slides(doc: &[u8], current_edit: Option<usize>) -> Result<String> {
    let deck = Deck::open(doc, current_edit)?;
    let notes_by_id: HashMap<u32, u32> = deck.list(2).into_iter().map(|s| (s.slide_id, s.persist)).collect();
    let mut out = String::new();
    for (i, slide) in deck.list(0).iter().enumerate() {
        let n = i + 1;
        out.push_str(&format!("=== Slide {n} ===\n"));
        let (mut notes_id, mut referenced) = (None, false);
        let start = out.len();
        if let Some(body) = deck.object(slide.persist, SLIDE) {
            let mut texts = Texts::new(&mut out, n);
            walk(body, 0, &mut |rec| match rec.kind {
                // SlideAtom: geometry, eight placeholder types, master id, then the notes id.
                SLIDE_ATOM => notes_id = u32_at(rec.body, 16).filter(|&id| id != 0),
                OUTLINE_TEXT_REF => {
                    referenced = true;
                    let text = u32_at(rec.body, 0).and_then(|i| slide.texts.get(i as usize));
                    texts.push_str(text.map_or("", String::as_str));
                }
                _ => texts.record(&rec),
            });
        }
        // A drawing that does not refer to the slide list's text (or no drawing at all):
        // add the list's text it does not carry itself.
        if !referenced {
            for text in &slide.texts {
                if !out[start..].contains(text.as_str()) {
                    out.push_str(text);
                }
            }
        }
        let notes = notes_id.and_then(|id| deck.object(*notes_by_id.get(&id)?, NOTES));
        if let Some(body) = notes {
            let mut text = String::new();
            let mut texts = Texts::new(&mut text, n);
            walk(body, 0, &mut |rec| texts.record(&rec));
            // Notes pages often hold just the slide number; skip notes without real text, as for .pptx.
            if text.lines().any(|l| !l.trim().is_empty() && l.trim() != n.to_string()) {
                out.push_str("--- Notes ---\n");
                out.push_str(&text);
            }
        }
    }
    Ok(out)
}

fn bad(what: &str) -> Error {
    Error::Unsupported(format!("damaged .ppt file: {what}"))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

struct Record<'a> {
    instance: u16,
    kind: u16,
    container: bool,
    body: &'a [u8],
}

/// The record at `at`, if its header and body fit into `data`.
fn record(data: &[u8], at: usize) -> Option<Record<'_>> {
    let head = data.get(at..at.checked_add(8)?)?;
    let ver_inst = u16::from_le_bytes([head[0], head[1]]);
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    Some(Record {
        instance: ver_inst >> 4,
        kind: u16::from_le_bytes([head[2], head[3]]),
        container: ver_inst & 0xF == 0xF,
        body: data.get(at + 8..(at + 8).checked_add(len)?)?,
    })
}

/// The records directly inside a container's body.
fn children(body: &[u8]) -> impl Iterator<Item = Record<'_>> {
    let mut at = 0;
    std::iter::from_fn(move || {
        let rec = record(body, at)?;
        at += 8 + rec.body.len();
        Some(rec)
    })
}

/// Calls `f` with every record below `body`, depth first in file order.
fn walk<'a>(body: &'a [u8], depth: usize, f: &mut impl FnMut(Record<'a>)) {
    if depth > MAX_DEPTH {
        return;
    }
    for rec in children(body) {
        if rec.container {
            walk(rec.body, depth + 1, f);
        } else {
            f(rec);
        }
    }
}

/// Appends the text of a TextCharsAtom or TextBytesAtom, a line per paragraph.
fn push_text(rec: &Record<'_>, out: &mut String) {
    let text = match rec.kind {
        TEXT_CHARS => {
            let units: Vec<u16> = rec.body.as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
            String::from_utf16_lossy(&units)
        }
        // The low bytes of UTF-16 code units, so Latin-1.
        TEXT_BYTES => rec.body.iter().map(|&b| b as char).collect(),
        _ => return,
    };
    // \r ends a paragraph, a vertical tab breaks a line.
    out.extend(text.chars().map(|c| if c == '\r' || c == '\x0B' { '\n' } else { c }));
    out.push('\n');
}

/// Collects the text of one slide or notes page.
struct Texts<'o> {
    out: &'o mut String,
    /// Where the latest text starts in `out`.
    last: usize,
    slide: usize,
}

impl<'o> Texts<'o> {
    fn new(out: &'o mut String, slide: usize) -> Self {
        Texts { last: out.len(), out, slide }
    }

    fn push_str(&mut self, text: &str) {
        self.last = self.out.len();
        self.out.push_str(text);
    }

    fn record(&mut self, rec: &Record<'_>) {
        match rec.kind {
            TEXT_CHARS | TEXT_BYTES => {
                self.last = self.out.len();
                push_text(rec, self.out);
            }
            // A slide number field: its character position in the text, which holds a '*'.
            SLIDE_NUMBER_FIELD => {
                let Some(pos) = u32_at(rec.body, 0) else { return };
                let at = self.out[self.last..].char_indices().nth(pos as usize).map(|(i, c)| (self.last + i, c));
                if let Some((at, '*')) = at {
                    self.out.replace_range(at..at + 1, &self.slide.to_string());
                }
            }
            _ => {}
        }
    }
}

/// A slide, master or notes page as listed in the DocumentContainer.
struct Listed {
    persist: u32,
    slide_id: u32,
    /// Placeholder text kept in the list, each with its trailing newline.
    texts: Vec<String>,
}

struct Deck<'a> {
    data: &'a [u8],
    /// Persist object id to stream offset, newest edit winning.
    offsets: HashMap<u32, usize>,
    document: &'a [u8],
}

impl<'a> Deck<'a> {
    fn open(data: &'a [u8], current_edit: Option<usize>) -> Result<Self> {
        let edit = current_edit
            .filter(|&at| record(data, at).is_some_and(|r| r.kind == USER_EDIT))
            .or_else(|| last_user_edit(data))
            .ok_or_else(|| bad("no UserEditAtom"))?;
        // Follow the edits back to the first save; the newer ones override.
        let (mut chain, mut seen, mut at) = (Vec::new(), HashSet::new(), edit);
        while seen.insert(at) {
            let rec = record(data, at).filter(|r| r.kind == USER_EDIT).ok_or_else(|| bad("broken edit chain"))?;
            chain.push(rec.body);
            match u32_at(rec.body, 8) {
                Some(0) | None => break,
                Some(prev) => at = prev as usize,
            }
        }
        let newest = chain[0];
        // An encrypted document has a 32-byte UserEditAtom naming its encryption session.
        if newest.len() >= 32 {
            return Err(Error::Unsupported("encrypted .ppt files are not supported".into()));
        }
        let mut offsets = HashMap::new();
        for edit in chain.iter().rev() {
            let dir = u32_at(edit, 12).and_then(|at| record(data, at as usize));
            let Some(dir) = dir.filter(|r| r.kind == PERSIST_DIRECTORY) else { continue };
            let mut words = dir.body.as_chunks::<4>().0.iter().map(|&c| u32::from_le_bytes(c));
            while let Some(entry) = words.next() {
                let (first, count) = (entry & 0xF_FFFF, entry >> 20);
                for (i, offset) in words.by_ref().take(count as usize).enumerate() {
                    offsets.insert(first + i as u32, offset as usize);
                }
            }
        }
        let mut deck = Deck { data, offsets, document: &[] };
        let doc_ref = u32_at(newest, 16).ok_or_else(|| bad("short UserEditAtom"))?;
        deck.document = deck.object(doc_ref, DOCUMENT).ok_or_else(|| bad("no DocumentContainer"))?;
        Ok(deck)
    }

    /// The body of persist object `id`, if it is a record of type `kind`.
    fn object(&self, id: u32, kind: u16) -> Option<&'a [u8]> {
        let rec = record(self.data, *self.offsets.get(&id)?)?;
        (rec.kind == kind).then_some(rec.body)
    }

    /// The slide list of the given instance: 0 slides, 1 masters, 2 notes.
    fn list(&self, instance: u16) -> Vec<Listed> {
        let mut out: Vec<Listed> = Vec::new();
        let Some(list) = children(self.document).find(|r| r.kind == SLIDE_LIST && r.instance == instance) else {
            return out;
        };
        for rec in children(list.body) {
            match rec.kind {
                SLIDE_PERSIST => {
                    if let (Some(persist), Some(slide_id)) = (u32_at(rec.body, 0), u32_at(rec.body, 12)) {
                        out.push(Listed { persist, slide_id, texts: Vec::new() });
                    }
                }
                TEXT_HEADER => {
                    if let Some(slide) = out.last_mut() {
                        slide.texts.push(String::new());
                    }
                }
                TEXT_CHARS | TEXT_BYTES => {
                    if let Some(text) = out.last_mut().and_then(|s| s.texts.last_mut()) {
                        push_text(&rec, text);
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Without a usable "Current User" stream: the last UserEditAtom among the top-level records.
fn last_user_edit(data: &[u8]) -> Option<usize> {
    let (mut at, mut last) = (0, None);
    while let Some(rec) = record(data, at) {
        if rec.kind == USER_EDIT {
            last = Some(at);
        }
        at += 8 + rec.body.len();
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    const PPT: &[u8] =
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-office/ppt-deck.ppt"));
    const PPTX: &[u8] =
        include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-office/ppt-deck.pptx"));

    #[test]
    fn deck() {
        assert_eq!(
            text(PPT).unwrap(),
            "=== Slide 1 ===\nQuarterly Review\nTeam Parallax\n--- Notes ---\nWelcome everyone.\nKeep it short.\n1\n\
             === Slide 2 ===\nAgenda\nResults\nPlans\nRisks\nand mitigations\nMetric\nValue\nRevenue\n1.2 M€\n\
             === Slide 3 ===\nArchitecture\nCore\nApp\n--- Notes ---\nMention the CLI.\n3\n"
        );
    }

    fn rec(ver: u16, instance: u16, kind: u16, body: &[u8]) -> Vec<u8> {
        let mut v = (ver | instance << 4).to_le_bytes().to_vec();
        v.extend(kind.to_le_bytes());
        v.extend((body.len() as u32).to_le_bytes());
        v.extend(body);
        v
    }

    /// A stream written by hand: one slide whose placeholder text is in the slide list
    /// only, a text box with bytes text, and an older edit that the newer one overrides.
    #[test]
    fn slide_list_text_and_edits() {
        let words = |w: &[u32]| w.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
        let slide = |text: &str| {
            let chars: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let textbox = rec(0xF, 0, 0xF00D, &rec(0, 0, TEXT_CHARS, &chars));
            rec(0xF, 0, SLIDE, &rec(0xF, 0, 0x040C, &rec(0xF, 0, 0xF002, &textbox)))
        };
        let mut list = rec(0, 0, SLIDE_PERSIST, &words(&[2, 0, 1, 256, 0]));
        list.extend(rec(0, 0, TEXT_HEADER, &words(&[0])));
        list.extend(rec(0, 0, TEXT_BYTES, b"Caf\xE9\rMen\x0Bu"));
        let document = rec(0xF, 0, DOCUMENT, &rec(0xF, 0, SLIDE_LIST, &list));
        let mut data = document.clone();
        let edit = |data: &mut Vec<u8>, slide_at: usize, last_edit: u32| {
            let dir_at = data.len() as u32;
            data.extend(rec(0, 0, PERSIST_DIRECTORY, &words(&[1 | 2 << 20, 0, slide_at as u32])));
            let edit_at = data.len();
            data.extend(rec(0, 0, USER_EDIT, &words(&[256, 0, last_edit, dir_at, 1, 3, 0])));
            edit_at
        };
        let old_slide = data.len();
        data.extend(slide("Old box"));
        let first = edit(&mut data, old_slide, 0);
        let new_slide = data.len();
        data.extend(slide("Box ✓"));
        let second = edit(&mut data, new_slide, first as u32);
        let expected = "=== Slide 1 ===\nBox ✓\nCafé\nMen\nu\n";
        assert_eq!(slides(&data, Some(second)).unwrap(), expected);
        // Without "Current User", the last edit in the stream.
        assert_eq!(slides(&data, None).unwrap(), expected);
        assert_eq!(slides(&data, Some(first)).unwrap(), "=== Slide 1 ===\nOld box\nCafé\nMen\nu\n");
        assert!(slides(&document, None).is_err());
    }

    #[test]
    fn same_as_pptx() {
        let native = text(PPT).unwrap();
        // LibreOffice shows the slide number field of a notes page as "<number>".
        let ooxml = super::super::office::ooxml_text("pptx", PPTX).unwrap();
        let ooxml = ooxml.replacen("<number>", "1", 1).replacen("<number>", "3", 1);
        assert_eq!(native, ooxml);
    }

    #[test]
    fn damaged() {
        assert!(text(b"").is_err());
        assert!(text(&PPT[..PPT.len() / 2]).is_err());
        assert!(text(b"\xD0\xCF\x11\xE0 not a compound file").is_err());
        let mut seed = 11u64;
        for _ in 0..300 {
            let mut data = PPT.to_vec();
            for _ in 0..8 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let at = (seed >> 33) as usize % data.len();
                data[at] = (seed >> 13) as u8;
            }
            let _ = text(&data);
        }
    }

    /// Mutates the records themselves, which a damaged compound file rarely reaches.
    #[test]
    fn damaged_records() {
        let mut file = cfb::CompoundFile::open(Cursor::new(PPT)).unwrap();
        let mut doc = Vec::new();
        file.open_stream("PowerPoint Document").unwrap().read_to_end(&mut doc).unwrap();
        let mut current = Vec::new();
        file.open_stream("Current User").unwrap().read_to_end(&mut current).unwrap();
        let current = u32_at(&current, 16).map(|o| o as usize);
        let mut seed = 5u64;
        for round in 0..2000 {
            let mut data = doc.clone();
            for _ in 0..1 + round % 16 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                let at = (seed >> 33) as usize % data.len();
                data[at] = (seed >> 13) as u8;
            }
            if round % 7 == 0 {
                data.truncate((seed >> 40) as usize % data.len());
            }
            let _ = slides(&data, current);
        }
    }

    /// Removes the preview image LibreOffice embeds in the summary information.
    #[test]
    #[ignore]
    fn strip_fixture_preview() {
        use std::io::Write;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/legacy-office/ppt-deck.ppt");
        let mut old = cfb::open(path).unwrap();
        let streams: Vec<_> = old
            .walk()
            .filter(|e| e.is_stream() && e.name() != "\u{5}SummaryInformation")
            .map(|e| e.path().to_path_buf())
            .collect();
        let mut copied = Vec::new();
        for stream in streams {
            let mut data = Vec::new();
            old.open_stream(&stream).unwrap().read_to_end(&mut data).unwrap();
            copied.push((stream, data));
        }
        let (version, clsid) = (old.version(), *old.root_entry().clsid());
        drop(old);
        let file = std::fs::File::create(path).unwrap();
        let mut new = cfb::CompoundFile::create_with_version(version, file).unwrap();
        // LibreOffice recognises the file by the class id.
        new.set_storage_clsid("/", clsid).unwrap();
        for (stream, data) in copied {
            new.create_stream(&stream).unwrap().write_all(&data).unwrap();
        }
        new.flush().unwrap();
    }
}
