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
            let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
            detector.feed(data, true);
            (detector.guess(None, chardetng::Utf8Detection::Allow), 0)
        }
    };
    let (text, _) = encoding.decode_without_bom_handling(&data[bom_len..]);
    let text = text.into_owned();
    let (lines, line_ending) = split_lines(&text);
    TextDoc { text, lines, encoding: encoding.name(), bom: bom_len > 0, line_ending }
}

fn split_lines(text: &str) -> (Vec<Range<usize>>, LineEnding) {
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
    (lines, ending)
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
