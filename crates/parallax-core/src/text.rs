//! Decoding file contents into lines: encoding detection, BOM, line endings, binary detection.

use std::ops::Range;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum LineEnding {
    #[serde(rename = "LF")]
    Lf,
    #[serde(rename = "CRLF")]
    CrLf,
    #[serde(rename = "CR")]
    Cr,
    #[serde(rename = "Mixed")]
    Mixed,
    #[serde(rename = "None")]
    None,
}

pub struct TextDoc {
    pub text: String,
    /// Byte ranges of the lines in `text`, without their line terminators.
    pub lines: Vec<Range<usize>>,
    pub encoding: &'static str,
    pub bom: bool,
    pub line_ending: LineEnding,
    /// Terminator for lines added by editing: the file's own, the most common one
    /// in a mixed file, `\n` when there is none yet.
    pub newline: &'static str,
}

impl TextDoc {
    pub fn line(&self, i: usize) -> &str {
        &self.text[self.lines[i].clone()]
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// An empty document saved like `other`: same encoding, BOM and line terminator.
    pub fn empty_like(other: &TextDoc) -> TextDoc {
        TextDoc { text: String::new(), lines: Vec::new(), line_ending: LineEnding::None, ..*other }
    }

    /// Byte offset of a position given as line and UTF-16 column (the unit of
    /// JavaScript strings). Columns past the end of the line, and lines past the
    /// end of the document, are clamped.
    pub fn offset(&self, line: usize, col: u32) -> usize {
        let Some(range) = self.lines.get(line) else {
            return self.text.len();
        };
        let mut units = 0u32;
        for (i, c) in self.text[range.clone()].char_indices() {
            if units >= col {
                return range.start + i;
            }
            units += c.len_utf16() as u32;
        }
        range.end
    }

    /// Start of line `line` including what precedes it, or the end of the text.
    pub fn line_start(&self, line: usize) -> usize {
        self.lines.get(line).map_or(self.text.len(), |r| r.start)
    }

    /// Whether the last line ends with a terminator.
    pub fn ends_with_newline(&self) -> bool {
        self.text.ends_with(['\n', '\r'])
    }

    /// Replaces a byte range of the text and returns what was there.
    pub fn splice(&mut self, range: Range<usize>, with: &str) -> String {
        let removed = self.text[range.clone()].to_string();
        self.text.replace_range(range, with);
        let (lines, line_ending, _) = split_lines(&self.text);
        self.lines = lines;
        self.line_ending = line_ending;
        removed
    }

    /// The bytes to save: the text in the document's encoding, with its BOM.
    /// Fails when the encoding cannot represent a character of the text.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let encoding = encoding_rs::Encoding::for_label(self.encoding.as_bytes()).unwrap_or(encoding_rs::UTF_8);
        let mut out = Vec::with_capacity(self.text.len() + 3);
        if encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE {
            let le = encoding == encoding_rs::UTF_16LE;
            if self.bom {
                out.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
            }
            for unit in self.text.encode_utf16() {
                out.extend_from_slice(&if le { unit.to_le_bytes() } else { unit.to_be_bytes() });
            }
            return Ok(out);
        }
        if self.bom && encoding == encoding_rs::UTF_8 {
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        let (bytes, _, unmappable) = encoding.encode(&self.text);
        if unmappable {
            let bad = self.text.chars().find(|c| encoding.encode(c.encode_utf8(&mut [0; 4])).2);
            return Err(format!(
                "{} cannot store the character {}",
                encoding.name(),
                bad.map_or(String::new(), |c| format!("'{c}' (U+{:04X})", c as u32))
            ));
        }
        out.extend_from_slice(&bytes);
        Ok(out)
    }
}

/// Heuristic used by Git and most diff tools: a NUL byte in the first 8000 bytes
/// means binary, unless the data starts with a UTF-16 byte order mark.
pub fn is_binary(data: &[u8]) -> bool {
    if data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]) {
        return false;
    }
    data[..data.len().min(8000)].contains(&0)
}

pub fn decode(data: &[u8]) -> TextDoc {
    let (encoding, bom_len) = match encoding_rs::Encoding::for_bom(data) {
        Some((enc, len)) => (enc, len),
        None if std::str::from_utf8(data).is_ok() => (encoding_rs::UTF_8, 0),
        None => {
            // The detector costs about 200 ns per byte; a window starting at the first
            // non-ASCII byte decides as well as the whole file does.
            const SAMPLE: usize = 256 * 1024;
            let first = data.iter().position(|b| !b.is_ascii()).unwrap_or(0);
            let start = first.saturating_sub(1024);
            let sample = &data[start..data.len().min(start + SAMPLE)];
            let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
            detector.feed(sample, sample.len() == data.len() - start);
            (detector.guess(None, chardetng::Utf8Detection::Allow), 0)
        }
    };
    let (text, _) = encoding.decode_without_bom_handling(&data[bom_len..]);
    let text = text.into_owned();
    let (lines, line_ending, newline) = split_lines(&text);
    TextDoc { text, lines, encoding: encoding.name(), bom: bom_len > 0, line_ending, newline }
}

/// Line ranges, the line ending style, and the most common terminator.
fn split_lines(text: &str) -> (Vec<Range<usize>>, LineEnding, &'static str) {
    let bytes = text.as_bytes();
    let mut lines = Vec::with_capacity(bytes.len() / 40 + 1);
    let (mut lf, mut crlf, mut cr) = (0usize, 0usize, 0usize);
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                lf += 1;
                lines.push(start..i);
                start = i + 1;
            }
            b'\r' if bytes.get(i + 1) == Some(&b'\n') => {
                crlf += 1;
                lines.push(start..i);
                i += 1;
                start = i + 1;
            }
            b'\r' => {
                cr += 1;
                lines.push(start..i);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < bytes.len() {
        lines.push(start..bytes.len());
    }
    let kinds = [lf, crlf, cr].iter().filter(|&&n| n > 0).count();
    let ending = match (kinds, lf, crlf) {
        (0, _, _) => LineEnding::None,
        (1, n, _) if n > 0 => LineEnding::Lf,
        (1, _, n) if n > 0 => LineEnding::CrLf,
        (1, _, _) => LineEnding::Cr,
        _ => LineEnding::Mixed,
    };
    let newline = if crlf > lf && crlf >= cr {
        "\r\n"
    } else if cr > lf && cr > crlf {
        "\r"
    } else {
        "\n"
    };
    (lines, ending, newline)
}

/// Length of a string in UTF-16 code units, the unit JavaScript string offsets use.
pub fn utf16_len(s: &str) -> usize {
    if s.is_ascii() { s.len() } else { s.encode_utf16().count() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_endings() {
        let doc = decode(b"a\r\nb\r\nc");
        assert_eq!(doc.line_ending, LineEnding::CrLf);
        assert_eq!((doc.len(), doc.line(1), doc.line(2)), (3, "b", "c"));
        assert_eq!(decode(b"a\nb\r\n").line_ending, LineEnding::Mixed);
        assert_eq!(decode(b"a\n").len(), 1);
        assert_eq!(decode(b"").len(), 0);
    }

    #[test]
    fn offsets_and_splices() {
        let mut doc = decode("añ😀b\r\nc".as_bytes());
        assert_eq!(doc.newline, "\r\n");
        // "ñ" is one UTF-16 unit, the emoji two.
        assert_eq!(doc.offset(0, 2), 3);
        assert_eq!(doc.offset(0, 4), 7);
        assert_eq!(doc.offset(0, 99), 8);
        assert_eq!(doc.offset(5, 0), doc.text.len());
        let removed = doc.splice(doc.lines[0].end..doc.lines[1].start, "");
        assert_eq!(removed, "\r\n");
        assert_eq!((doc.len(), doc.line(0)), (1, "añ😀bc"));
    }

    #[test]
    fn encode_round_trips() {
        for data in [
            &b"plain\nascii\n"[..],
            b"\xEF\xBB\xBFbom \xC3\xA4\r\n",
            b"Gr\xFC\xDFe aus K\xF6ln, sch\xF6ne Gr\xFC\xDFe\n",
            b"\xFF\xFEh\x00i\x00\n\x00",
            b"\xFE\xFF\x00h\x00i",
        ] {
            assert_eq!(decode(data).encode().unwrap(), data);
        }
        let mut latin1 = decode(b"Gr\xFC\xDFe aus K\xF6ln, sch\xF6ne Gr\xFC\xDFe");
        latin1.splice(0..0, "😀");
        assert!(latin1.encode().unwrap_err().contains("U+1F600"));
    }

    #[test]
    fn encodings() {
        let doc = decode(b"\xEF\xBB\xBFhi");
        assert!(doc.bom);
        assert_eq!(doc.text, "hi");
        let latin1 = decode(b"Gr\xFC\xDFe aus K\xF6ln, sch\xF6ne Gr\xFC\xDFe");
        assert_eq!(latin1.encoding, "windows-1252");
        assert!(latin1.text.starts_with("Grüße"));
        let utf16 = decode(b"\xFF\xFEh\x00i\x00");
        assert_eq!(utf16.text, "hi");
        assert!(is_binary(b"\x00\x01\x02"));
        assert!(!is_binary(b"\xFF\xFEh\x00"));
    }
}
