//! Audio and video files, shown from their headers: the "Media summary" view.
//!
//! A media file is a container (MP4, Matroska, Ogg, RIFF …) around one or more
//! streams of compressed frames. What a person compares is not the frames but what
//! the container says about them: duration, codecs, resolution, sample rate,
//! languages, tags, chapters and attachments. All of that lives in a few header
//! boxes or elements, so each parser here walks the container's structure by the
//! sizes it declares and never reads the media data: a 4 GB video with its `moov`
//! box at the end costs a handful of page faults, not a scan.
//!
//! The summary is one fact per line, tags sorted by name, durations as
//! `H:MM:SS.mmm`. Volatile fields (creation times written by the muxer, random
//! segment and track UIDs, byte offsets) are left out, so a remux of the same
//! content shows as the same summary.
//!
//! The ≈ similar check ("same audio, only tags differ") is done for formats where
//! the tags live in a separate block from the audio frames: MP3 (ID3v2/ID3v1/APE
//! around the frames), FLAC (metadata blocks before the frames, plus the MD5 of the
//! decoded samples in STREAMINFO), WAV and AIFF (tag chunks beside the data
//! chunk). For MP4 and Matroska a tag change moves chunk offsets and cue
//! positions, so equality could only be shown by comparing the frame data sample
//! by sample; that is not done.

mod bmff;
mod ebml;
mod flac;
mod mpeg;
mod ogg;
mod riff;

use std::fmt::Write as _;

use crate::{Error, Result};

pub(super) use bmff::{Boxes, av1_profile, hevc_profile, is_heif};

/// Payloads above this size are not compared byte by byte for the ≈ similar
/// check: that would read the whole file for every pair in a folder compare.
const MAX_COMPARE_BYTES: usize = 64 * 1024 * 1024;
/// Tag values longer than this are cut (lyrics, embedded base64 pictures).
const MAX_VALUE_CHARS: usize = 400;
/// At most this many lines per list (tags, chapters, tracks, attachments).
const MAX_ITEMS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Container {
    /// ISO base media: MP4, MOV, M4A, 3GP.
    Bmff,
    /// Matroska and WebM.
    Matroska,
    Mp3,
    Flac,
    Ogg,
    Wav,
    Aiff,
    Avi,
}

/// The container of a media file, from its first bytes (and, for MP3 without an
/// ID3 tag, the extension). HEIF/AVIF images are not media.
pub(super) fn detect(ext: &str, data: &[u8]) -> Option<Container> {
    let at = |offset: usize, magic: &[u8]| data.get(offset..offset + magic.len()) == Some(magic);
    if at(4, b"ftyp") {
        return (!is_heif(data)).then_some(Container::Bmff);
    }
    // QuickTime files from before `ftyp`: a known top-level box first.
    if matches!(ext, "mov" | "qt" | "mp4" | "m4a" | "m4v")
        && [&b"moov"[..], b"mdat", b"wide", b"free", b"skip"].iter().any(|m| at(4, m))
    {
        return Some(Container::Bmff);
    }
    if at(0, &[0x1A, 0x45, 0xDF, 0xA3]) {
        return Some(Container::Matroska);
    }
    if at(0, b"fLaC") {
        return Some(Container::Flac);
    }
    if at(0, b"OggS") && data.get(4) == Some(&0) {
        return Some(Container::Ogg);
    }
    if (at(0, b"RIFF") || at(0, b"RF64") || at(0, b"BW64")) && at(8, b"WAVE") {
        return Some(Container::Wav);
    }
    if at(0, b"RIFF") && at(8, b"AVI ") {
        return Some(Container::Avi);
    }
    if at(0, b"FORM") && (at(8, b"AIFF") || at(8, b"AIFC")) {
        return Some(Container::Aiff);
    }
    if mpeg::id3v2_len(data).is_some() {
        // ID3 tags also precede FLAC streams now and then.
        let after = mpeg::id3v2_len(data).unwrap_or(0);
        if data.get(after..after + 4) == Some(b"fLaC") {
            return Some(Container::Flac);
        }
        return Some(Container::Mp3);
    }
    if matches!(ext, "mp3" | "mp2" | "mpga") && mpeg::first_frame(data, 0).is_some_and(|(pos, _)| pos == 0) {
        return Some(Container::Mp3);
    }
    None
}

// ---- Summary model ----

/// What a container says about its content; rendered by [`Summary::render`].
#[derive(Debug, Default)]
pub(super) struct Summary {
    pub container: String,
    /// Further container facts (brand, writing application, tag formats).
    pub info: Vec<(&'static str, String)>,
    /// Seconds.
    pub duration: Option<f64>,
    /// Whether the duration is an estimate (MP3 without a Xing header).
    pub duration_estimated: bool,
    pub file_size: u64,
    pub tracks: Vec<Track>,
    /// Groups of tags: section title and key/value pairs (sorted when rendered).
    pub tags: Vec<(String, Vec<(String, String)>)>,
    pub chapters: Vec<Chapter>,
    /// Attachments and embedded pictures, one preformatted line each.
    pub attachments: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub(super) struct Track {
    /// Video, Audio, Subtitles …
    pub kind: String,
    /// Track number or id as the container numbers them, when it has one.
    pub number: Option<u64>,
    pub fields: Vec<(&'static str, String)>,
    /// Tags that belong to this track (Matroska).
    pub tags: Vec<(String, String)>,
}

impl Track {
    pub fn new(kind: &str) -> Track {
        Track { kind: kind.to_string(), ..Track::default() }
    }

    pub fn field(&mut self, label: &'static str, value: impl Into<String>) {
        let value = value.into();
        if !value.is_empty() {
            self.fields.push((label, value));
        }
    }
}

#[derive(Debug)]
pub(super) struct Chapter {
    /// Seconds.
    pub start: f64,
    pub end: Option<f64>,
    pub title: String,
    /// Nesting depth (Matroska sub-chapters).
    pub depth: usize,
}

impl Summary {
    pub fn new(container: impl Into<String>, file_size: usize) -> Summary {
        Summary { container: container.into(), file_size: file_size as u64, ..Summary::default() }
    }

    pub fn tag_group(&mut self, title: impl Into<String>, tags: Vec<(String, String)>) {
        if !tags.is_empty() {
            self.tags.push((title.into(), tags));
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "=== Media ===");
        line(&mut out, "Container", &self.container);
        for (label, value) in &self.info {
            line(&mut out, label, value);
        }
        if let Some(d) = self.duration.filter(|d| d.is_finite() && *d > 0.0) {
            let estimate = if self.duration_estimated { " (estimated)" } else { "" };
            line(&mut out, "Duration", format!("{}{estimate}", duration(d)));
            line(&mut out, "Overall bitrate", bitrate(self.file_size as f64 * 8.0 / d));
        }
        for w in &self.warnings {
            let _ = writeln!(out, "! {w}");
        }
        for (i, t) in self.tracks.iter().take(MAX_ITEMS).enumerate() {
            let n = t.number.unwrap_or(i as u64 + 1);
            let _ = writeln!(out, "\n=== Track {n}: {} ===", t.kind);
            for (label, value) in &t.fields {
                line(&mut out, label, value);
            }
            if !t.tags.is_empty() {
                let _ = writeln!(out, "--- Tags ---");
                tag_lines(&mut out, &t.tags);
            }
        }
        more(&mut out, self.tracks.len(), "tracks");
        for (title, tags) in &self.tags {
            let _ = writeln!(out, "\n=== {title} ===");
            tag_lines(&mut out, tags);
        }
        if !self.chapters.is_empty() {
            let _ = writeln!(out, "\n=== Chapters ===");
            for c in self.chapters.iter().take(MAX_ITEMS) {
                let end = c.end.map(|e| format!(" – {}", duration(e))).unwrap_or_default();
                let indent = "  ".repeat(c.depth.min(8));
                let _ = writeln!(out, "{indent}{}{end}  {}", duration(c.start), clean(&c.title));
            }
            more(&mut out, self.chapters.len(), "chapters");
        }
        if !self.attachments.is_empty() {
            let _ = writeln!(out, "\n=== Attachments ===");
            for a in self.attachments.iter().take(MAX_ITEMS) {
                let _ = writeln!(out, "{a}");
            }
            more(&mut out, self.attachments.len(), "attachments");
        }
        out
    }
}

fn tag_lines(out: &mut String, tags: &[(String, String)]) {
    let mut sorted: Vec<_> = tags.iter().map(|(k, v)| (clean(k), clean(v))).collect();
    // Stable: repeated keys (several artists) keep their file order.
    sorted.sort_by_key(|a| a.0.to_lowercase());
    for (k, v) in sorted.iter().take(MAX_ITEMS) {
        line(out, k, v);
    }
    more(out, sorted.len(), "tags");
}

fn more(out: &mut String, n: usize, what: &str) {
    if n > MAX_ITEMS {
        let _ = writeln!(out, "… {} more {what}", n - MAX_ITEMS);
    }
}

// ---- Formatting helpers, shared with the image metadata view ----

/// A `Label: value` line, values aligned unless the label is long.
pub(super) fn line(out: &mut String, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
}

/// A value on one line: control characters escaped, trailing NULs and spaces
/// dropped, long values cut with their length.
pub(super) fn clean(s: &str) -> String {
    let s = s.trim_end_matches(['\0', ' ']);
    let mut out = String::with_capacity(s.len().min(MAX_VALUE_CHARS + 16));
    for (n, c) in s.chars().enumerate() {
        if n == MAX_VALUE_CHARS {
            let _ = write!(out, "… ({} characters)", s.chars().count());
            break;
        }
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// `H:MM:SS.mmm`.
pub(super) fn duration(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "?".into();
    }
    let ms = (seconds * 1000.0).round() as u64;
    format!("{}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// Bits per second as kb/s or Mb/s.
pub(super) fn bitrate(bps: f64) -> String {
    if !bps.is_finite() || bps <= 0.0 {
        return "?".into();
    }
    if bps >= 10_000_000.0 { format!("{:.1} Mb/s", bps / 1e6) } else { format!("{} kb/s", (bps / 1000.0).round()) }
}

/// A number with up to three decimals, trailing zeros dropped (`29.97`, `25`).
pub(super) fn decimal(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

pub(super) fn size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} bytes"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b if b < 1024 * 1024 * 1024 => format!("{:.1} MB", b as f64 / 1048576.0),
        b => format!("{:.2} GB", b as f64 / 1073741824.0),
    }
}

pub(super) fn channels(n: u64) -> String {
    match n {
        1 => "1 (mono)".into(),
        2 => "2 (stereo)".into(),
        6 => "6 (5.1)".into(),
        8 => "8 (7.1)".into(),
        n => n.to_string(),
    }
}

/// A four-character code as text, non-printable bytes escaped.
pub(super) fn fourcc(code: &[u8]) -> String {
    code.iter()
        .map(|&b| if b.is_ascii_graphic() || b == b' ' { char::from(b).to_string() } else { format!("\\x{b:02x}") })
        .collect()
}

/// Latin-1 (and ASCII) bytes as text.
pub(super) fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Text that is meant to be UTF-8, but may not be.
pub(super) fn utf8(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => latin1(bytes),
    }
}

fn unsupported(what: &str) -> Error {
    Error::Unsupported(what.to_string())
}

/// Description of an embedded picture: type, MIME type, size and dimensions
/// (read from the picture's header, not decoded).
pub(super) fn picture(kind: &str, mime: &str, description: &str, data: &[u8]) -> String {
    let dims = super::image_meta::dimensions(data).map(|(w, h)| format!(", {w} × {h}")).unwrap_or_default();
    let desc = if description.trim().is_empty() { String::new() } else { format!(" \"{}\"", clean(description)) };
    let mime = if mime.is_empty() { "picture" } else { mime };
    format!("{kind}{desc}: {mime}{dims}, {}, crc {:08x}", size(data.len() as u64), crc32fast::hash(data))
}

// ---- Byte reading ----

/// A bounds-checked reader over a byte slice; every read returns `None` past the end.
#[derive(Clone)]
pub(super) struct Cur<'a> {
    data: &'a [u8],
    pub pos: usize,
}

macro_rules! read_int {
    ($name:ident, $t:ty, $n:literal, $from:ident) => {
        pub fn $name(&mut self) -> Option<$t> {
            Some(<$t>::$from(self.take($n)?.try_into().ok()?))
        }
    };
}

impl<'a> Cur<'a> {
    pub fn new(data: &'a [u8]) -> Cur<'a> {
        Cur { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Cur<'a> {
        Cur { data, pos }
    }

    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    pub fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or_default()
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    pub fn be24(&mut self) -> Option<u32> {
        self.take(3).map(|b| u32::from_be_bytes([0, b[0], b[1], b[2]]))
    }

    read_int!(be16, u16, 2, from_be_bytes);
    read_int!(be32, u32, 4, from_be_bytes);
    read_int!(be64, u64, 8, from_be_bytes);
    read_int!(le16, u16, 2, from_le_bytes);
    read_int!(le32, u32, 4, from_le_bytes);
    read_int!(le64, u64, 8, from_le_bytes);
}

/// Bytes `[start, start + len)` of `data`, cut at its end.
pub(super) fn slice(data: &[u8], start: usize, len: usize) -> &[u8] {
    let start = start.min(data.len());
    &data[start..start.saturating_add(len).min(data.len())]
}

// ---- Views ----

/// The "Media summary" view.
pub fn summary(ext: &str, data: &[u8]) -> Result<String> {
    let s = match detect(ext, data).ok_or_else(|| unsupported("not an audio or video file"))? {
        Container::Bmff => bmff::summary(data)?,
        Container::Matroska => ebml::summary(data)?,
        Container::Mp3 => mpeg::summary(data)?,
        Container::Flac => flac::summary(data)?,
        Container::Ogg => ogg::summary(data)?,
        Container::Wav | Container::Avi => riff::summary(data)?,
        Container::Aiff => riff::aiff_summary(data)?,
    };
    Ok(s.render())
}

/// Whether two byte-different files hold the same audio: the reason, or `None`
/// when they differ, the format has no such check, or the payload is too large
/// to compare (see [`MAX_COMPARE_BYTES`]).
pub fn same_media(ext: &str, left: &[u8], right: &[u8]) -> Option<&'static str> {
    let container = detect(ext, left)?;
    if detect(ext, right)? != container {
        return None;
    }
    match container {
        Container::Mp3 => same_payload(mpeg::audio_payload(left)?, mpeg::audio_payload(right)?, left, right),
        Container::Flac => flac::same_audio(left, right),
        Container::Wav | Container::Aiff => {
            let (l, r) = (riff::audio_parts(left)?, riff::audio_parts(right)?);
            (l.0 == r.0).then_some(())?;
            same_payload(l.1, r.1, left, right)
        }
        _ => None,
    }
}

/// Equal audio payloads (byte ranges of the two files): sizes first, then bytes.
fn same_payload(
    l: std::ops::Range<usize>,
    r: std::ops::Range<usize>,
    left: &[u8],
    right: &[u8],
) -> Option<&'static str> {
    if l.len() != r.len() || l.len() > MAX_COMPARE_BYTES || l.is_empty() {
        return None;
    }
    (left.get(l)? == right.get(r)?).then_some("same audio, only tags differ")
}

#[cfg(test)]
mod tests;
