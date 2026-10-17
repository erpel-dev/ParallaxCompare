//! MP3 (MPEG audio) files and ID3 tags.
//!
//! An MP3 file is a run of self-contained frames, usually with an ID3v2 tag in
//! front and maybe an APEv2 and an ID3v1 tag at the end. The summary reads the
//! tags and the first frame: its header gives the codec parameters, and a VBR
//! file's first frame is a Xing/Info or VBRI header with the frame count, from
//! which the duration follows exactly. Without one the duration is estimated
//! from the size and the bitrate of the first frame, as players do; the frames
//! are never walked.
//!
//! ID3v2 is also embedded in WAV and AIFF files, so its parser is shared.

use std::ops::Range;

use super::{Cur, Summary, Track, channels, latin1};
use crate::{Error, Result};

/// The first frame is looked for in this many bytes after the tags.
const SYNC_SEARCH: usize = 64 * 1024;

/// Length of the ID3v2 tag at the start of `data` (header, body and footer).
pub(super) fn id3v2_len(data: &[u8]) -> Option<usize> {
    let h = data.get(..10)?;
    if &h[..3] != b"ID3" || !(2..=4).contains(&h[3]) || h[4] == 0xFF || h[6..10].iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    let size = h[6..10].iter().fold(0usize, |v, &b| (v << 7) | b as usize);
    let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct FrameHeader {
    /// 1, 2 or 2.5 (as 25).
    version: u8,
    layer: u8,
    bitrate: u32,
    sample_rate: u32,
    padding: bool,
    mode: u8,
}

impl FrameHeader {
    pub(super) fn parse(b: &[u8]) -> Option<FrameHeader> {
        let h = u32::from_be_bytes(b.get(..4)?.try_into().ok()?);
        if h >> 21 != 0x7FF {
            return None;
        }
        let version = match (h >> 19) & 3 {
            0 => 25,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let layer = match (h >> 17) & 3 {
            1 => 3,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let bi = ((h >> 12) & 15) as usize;
        let si = ((h >> 10) & 3) as usize;
        if bi == 0 || bi == 15 || si == 3 {
            return None;
        }
        const V1: [[u32; 14]; 3] = [
            [32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
            [32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
            [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
        ];
        const V2: [[u32; 14]; 2] = [
            [32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],
            [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        ];
        let kbps = if version == 1 { V1[layer as usize - 1][bi - 1] } else { V2[usize::from(layer != 1)][bi - 1] };
        let base = [44100, 48000, 32000][si];
        let sample_rate = match version {
            1 => base,
            2 => base / 2,
            _ => base / 4,
        };
        Some(FrameHeader {
            version,
            layer,
            bitrate: kbps * 1000,
            sample_rate,
            padding: (h >> 9) & 1 == 1,
            mode: ((h >> 6) & 3) as u8,
        })
    }

    fn samples(&self) -> u32 {
        match (self.layer, self.version) {
            (1, _) => 384,
            (2, _) | (3, 1) => 1152,
            _ => 576,
        }
    }

    fn len(&self) -> usize {
        let pad = usize::from(self.padding);
        if self.layer == 1 {
            (12 * self.bitrate / self.sample_rate) as usize * 4 + pad * 4
        } else {
            (self.samples() / 8 * self.bitrate / self.sample_rate) as usize + pad
        }
    }

    fn name(&self) -> String {
        let v = if self.version == 25 { "2.5".to_string() } else { self.version.to_string() };
        let layer = ["I", "II", "III"][self.layer as usize - 1];
        format!("MPEG-{v} Layer {layer}")
    }

    fn mono(&self) -> bool {
        self.mode == 3
    }
}

/// Offset of the first frame at or after `from`: a valid header followed by another.
pub(super) fn first_frame(data: &[u8], from: usize) -> Option<(usize, FrameHeader)> {
    let end = from.saturating_add(SYNC_SEARCH).min(data.len());
    let mut i = from;
    while i + 4 <= end {
        if data[i] == 0xFF
            && let Some(h) = FrameHeader::parse(&data[i..])
        {
            let next = i + h.len();
            // A second frame must follow, unless this one ends the file.
            let confirmed = next == data.len()
                || data
                    .get(next..)
                    .and_then(FrameHeader::parse)
                    .is_some_and(|n| n.version == h.version && n.layer == h.layer);
            if confirmed {
                return Some((i, h));
            }
        }
        i += 1;
    }
    None
}

/// Where the tags are: end of the ID3v2 tag, start of the trailing tags (APE, ID3v1).
struct Layout {
    id3v2: Option<Range<usize>>,
    ape: Option<Range<usize>>,
    id3v1: Option<Range<usize>>,
}

fn layout(data: &[u8]) -> Layout {
    let id3v2 = id3v2_len(data).map(|n| 0..n.min(data.len()));
    let mut end = data.len();
    let id3v1 = (end >= 128 && &data[end - 128..end - 125] == b"TAG").then(|| end - 128..end);
    if let Some(r) = &id3v1 {
        end = r.start;
    }
    let ape = ape_range(data, end);
    Layout { id3v2, ape, id3v1 }
}

/// An APEv2 tag ending at `end` (its footer is the last 32 bytes).
fn ape_range(data: &[u8], end: usize) -> Option<Range<usize>> {
    let footer = data.get(end.checked_sub(32)?..end)?;
    if &footer[..8] != b"APETAGEX" {
        return None;
    }
    let mut c = Cur::new(&footer[8..]);
    let _version = c.le32()?;
    let size = c.le32()? as usize;
    let _items = c.le32()?;
    let flags = c.le32()?;
    let header = if flags & 0x8000_0000 != 0 { 32 } else { 0 };
    let start = end.checked_sub(size.checked_add(header)?)?;
    Some(start..end)
}

/// The bytes between the leading and trailing tags (used by the ≈ similar check).
pub(super) fn audio_range(data: &[u8]) -> Option<Range<usize>> {
    let l = layout(data);
    let mut start = l.id3v2.map_or(0, |r| r.end);
    let end = l.ape.as_ref().or(l.id3v1.as_ref()).map_or(data.len(), |r| r.start);
    // Padding some taggers leave after the tag.
    while start < end && data[start] == 0 {
        start += 1;
    }
    (start < end).then_some(start..end)
}

/// The audio frames for the ≈ similar check: [`audio_range`] without a leading
/// Xing/Info/VBRI frame. That frame holds no audio, and taggers and remuxers
/// rewrite the encoder name in it.
pub(super) fn audio_payload(data: &[u8]) -> Option<Range<usize>> {
    let r = audio_range(data)?;
    match FrameHeader::parse(&data[r.start..]) {
        Some(h) if vbr_header(&data[r.start..r.end], &h).is_some() => Some((r.start + h.len()).min(r.end)..r.end),
        _ => Some(r),
    }
}

pub(super) fn summary(data: &[u8]) -> Result<Summary> {
    let l = layout(data);
    let audio = audio_range(data).unwrap_or(0..0);
    let frame = first_frame(data, audio.start);
    let name = frame.map(|(_, h)| h.name()).unwrap_or_else(|| "MPEG audio".into());
    let mut s = Summary::new(
        if frame.is_some_and(|(_, h)| h.layer == 3) { "MP3".to_string() } else { name.clone() },
        data.len(),
    );
    let mut tag_formats = Vec::new();
    if let Some(r) = &l.id3v2 {
        let id3 = id3v2(&data[r.clone()]);
        tag_formats.push(format!("ID3v2.{}", data[3]));
        s.attachments.extend(id3.pictures);
        s.tag_group("ID3v2 tags", id3.tags);
        s.warnings.extend(id3.warnings);
    }
    if let Some(r) = &l.ape {
        tag_formats.push("APEv2".into());
        s.tag_group("APE tags", ape(&data[r.clone()]));
    }
    if let Some(r) = &l.id3v1 {
        tag_formats.push("ID3v1".into());
        s.tag_group("ID3v1 tags", id3v1(&data[r.clone()]));
    }
    if !tag_formats.is_empty() {
        s.info.push(("Tags", tag_formats.join(", ")));
    }
    let Some((pos, h)) = frame else {
        if l.id3v2.is_none() {
            return Err(Error::Unsupported("no MPEG audio frame found".into()));
        }
        s.warnings.push("no MPEG audio frame found after the tag".into());
        return Ok(s);
    };
    let mut t = Track::new("Audio");
    t.field("Codec", name);
    t.field("Sample rate", format!("{} Hz", h.sample_rate));
    t.field("Channels", channels(if h.mono() { 1 } else { 2 }));
    let mode = ["stereo", "joint stereo", "dual channel", "mono"][h.mode as usize];
    t.field("Channel mode", mode);
    let audio_bytes = audio.end.saturating_sub(pos) as f64;
    let vbr = vbr_header(&data[pos..audio.end.max(pos)], &h);
    match &vbr {
        Some(v) if v.frames > 0 => {
            let seconds = v.frames as f64 * h.samples() as f64 / h.sample_rate as f64;
            s.duration = Some(seconds);
            let bytes = v.bytes.map_or(audio_bytes, |b| b as f64);
            // The Info frame itself may use a higher bitrate to fit the header: take the next frame's.
            let next = data.get(pos + h.len()..).and_then(FrameHeader::parse);
            if let (true, Some(n)) = (v.kind == "Info", next) {
                t.field("Bitrate", format!("{} (constant)", super::bitrate(n.bitrate as f64)));
            } else {
                t.field("Bitrate", format!("{} (variable, average)", super::bitrate(bytes * 8.0 / seconds)));
            }
            t.field("Frames", v.frames.to_string());
            t.field("VBR header", v.kind);
        }
        _ => {
            s.duration = Some(audio_bytes * 8.0 / h.bitrate as f64);
            s.duration_estimated = true;
            t.field("Bitrate", format!("{} (first frame)", super::bitrate(h.bitrate as f64)));
        }
    }
    if let Some(enc) = vbr.as_ref().and_then(|v| v.encoder.clone()) {
        t.field("Encoder", enc);
    }
    if pos != audio.start {
        t.field("Skipped bytes", format!("{} before the first frame", pos - audio.start));
    }
    s.tracks.push(t);
    Ok(s)
}

struct Vbr {
    kind: &'static str,
    frames: u32,
    bytes: Option<u32>,
    encoder: Option<String>,
}

/// The Xing/Info or VBRI header in the first frame.
fn vbr_header(frame: &[u8], h: &FrameHeader) -> Option<Vbr> {
    let side = match (h.version == 1, h.mono()) {
        (true, false) => 32,
        (true, true) => 17,
        (false, false) => 17,
        (false, true) => 9,
    };
    let x = 4 + side;
    if let Some(tag @ (b"Xing" | b"Info")) = frame.get(x..x + 4) {
        let mut c = Cur::at(frame, x + 4);
        let flags = c.be32()?;
        let frames = if flags & 1 != 0 { c.be32()? } else { 0 };
        let bytes = if flags & 2 != 0 { Some(c.be32()?) } else { None };
        if flags & 4 != 0 {
            c.skip(100)?;
        }
        if flags & 8 != 0 {
            c.skip(4)?;
        }
        let encoder = c
            .take(9)
            .map(latin1)
            .map(|e| e.trim_end_matches(['\0', ' ']).to_string())
            .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_graphic() || c == ' '));
        return Some(Vbr { kind: if tag == b"Xing" { "Xing" } else { "Info" }, frames, bytes, encoder });
    }
    if frame.get(36..40) == Some(b"VBRI") {
        let mut c = Cur::at(frame, 40 + 6);
        let bytes = c.be32()?;
        let frames = c.be32()?;
        return Some(Vbr { kind: "VBRI", frames, bytes: Some(bytes), encoder: None });
    }
    None
}

// ---- ID3v2 ----

pub(super) struct Id3 {
    pub tags: Vec<(String, String)>,
    pub pictures: Vec<String>,
    pub warnings: Vec<String>,
}

/// Reverses the unsynchronisation scheme: `FF 00` → `FF`.
fn resync(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut prev = 0u8;
    for &b in data {
        if !(prev == 0xFF && b == 0) {
            out.push(b);
        }
        prev = b;
    }
    out
}

/// Tags of an ID3v2 tag (the whole tag, header included).
pub(super) fn id3v2(tag: &[u8]) -> Id3 {
    let mut out = Id3 { tags: Vec::new(), pictures: Vec::new(), warnings: Vec::new() };
    let Some(header) = tag.get(..10) else { return out };
    let (major, flags) = (header[3], header[5]);
    let len = id3v2_len(tag).unwrap_or(tag.len()).min(tag.len());
    let body = &tag[10..len.saturating_sub(if flags & 0x10 != 0 { 10 } else { 0 }).max(10)];
    let resynced;
    let mut body = if flags & 0x80 != 0 && major < 4 {
        resynced = resync(body);
        &resynced[..]
    } else {
        body
    };
    if flags & 0x40 != 0 {
        let ext = match major {
            3 => body.get(..4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize + 4),
            4 => body.get(..4).map(|b| b.iter().fold(0usize, |v, &x| (v << 7) | (x & 0x7F) as usize)),
            _ => None,
        };
        body = body.get(ext.unwrap_or(body.len())..).unwrap_or_default();
    }
    let mut c = Cur::new(body);
    while c.remaining() > 0 && out.tags.len() + out.pictures.len() < super::MAX_ITEMS {
        let (id, size, fflags) = if major == 2 {
            let Some(id) = c.take(3) else { break };
            let Some(size) = c.be24() else { break };
            (id, size as usize, 0u16)
        } else {
            let Some(id) = c.take(4) else { break };
            let Some(size) = c.be32() else { break };
            let size = if major == 4 {
                ((size >> 3) & 0x0FE0_0000 | (size >> 2) & 0x1F_C000 | (size >> 1) & 0x3F80 | size & 0x7F) as usize
            } else {
                size as usize
            };
            let Some(fflags) = c.be16() else { break };
            (id, size, fflags)
        };
        if id[0] == 0 || !id.iter().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()) {
            break; // padding
        }
        let Some(mut data) = c.take(size) else {
            out.warnings.push(format!("ID3v2 frame {} runs past the end of the tag", latin1(id)));
            break;
        };
        let id = latin1(id);
        // Format flags: v2.3 compression 0x80, encryption 0x40, grouping 0x20;
        // v2.4 grouping 0x40, compression 0x08, encryption 0x04, unsync 0x02, length 0x01.
        let fmt = (fflags & 0xFF) as u8;
        let (compressed, encrypted) = match major {
            3 => (fmt & 0x80 != 0, fmt & 0x40 != 0),
            4 => (fmt & 0x08 != 0, fmt & 0x04 != 0),
            _ => (false, false),
        };
        if (major == 3 && fmt & 0x20 != 0) || (major == 4 && fmt & 0x40 != 0) {
            data = data.get(1..).unwrap_or_default();
        }
        if major == 4 && fmt & 0x01 != 0 || major == 3 && compressed {
            data = data.get(4..).unwrap_or_default();
        }
        let resynced_frame;
        if major == 4 && (fmt & 0x02 != 0 || flags & 0x80 != 0) {
            resynced_frame = resync(data);
            data = &resynced_frame;
        }
        let inflated;
        if compressed {
            use std::io::Read as _;
            let mut v = Vec::new();
            let ok = flate2::read::ZlibDecoder::new(data).take(16 << 20).read_to_end(&mut v).is_ok();
            if !ok {
                out.tags.push((frame_name(&id), "(compressed frame that could not be read)".into()));
                continue;
            }
            inflated = v;
            data = &inflated;
        }
        if encrypted {
            out.tags.push((frame_name(&id), format!("(encrypted, {} bytes)", data.len())));
            continue;
        }
        frame(&id, data, &mut out);
    }
    out
}

/// Text in an ID3 encoding; several values (v2.4, NUL-separated) joined by " / ".
fn decode(enc: u8, b: &[u8]) -> String {
    let s = match enc {
        1 | 2 => {
            let (be, b) = match b {
                [0xFF, 0xFE, rest @ ..] => (false, rest),
                [0xFE, 0xFF, rest @ ..] => (true, rest),
                _ => (enc == 2, b),
            };
            let units: Vec<u16> = b
                .as_chunks::<2>()
                .0
                .iter()
                .map(|p| if be { u16::from_be_bytes([p[0], p[1]]) } else { u16::from_le_bytes([p[0], p[1]]) })
                .collect();
            // A BOM may start each value of a list.
            String::from_utf16_lossy(&units).replace('\u{FEFF}', "")
        }
        3 => String::from_utf8_lossy(b).into_owned(),
        _ => latin1(b),
    };
    let s = s.trim_end_matches('\0');
    s.split('\0').filter(|v| !v.is_empty()).collect::<Vec<_>>().join(" / ")
}

/// Splits at the first terminator of the encoding (one NUL, or two for UTF-16).
fn split_terminated(enc: u8, b: &[u8]) -> (&[u8], &[u8]) {
    if enc == 1 || enc == 2 {
        let mut i = 0;
        while i + 1 < b.len() {
            if b[i] == 0 && b[i + 1] == 0 {
                return (&b[..i], &b[i + 2..]);
            }
            i += 2;
        }
        (b, &[])
    } else {
        match b.iter().position(|&x| x == 0) {
            Some(i) => (&b[..i], &b[i + 1..]),
            None => (b, &[]),
        }
    }
}

fn frame(id: &str, data: &[u8], out: &mut Id3) {
    let v22 = id.len() == 3;
    let id = v22_to_v23(id);
    let name = frame_name(id);
    let enc = data.first().copied().unwrap_or(0);
    let rest = data.get(1..).unwrap_or_default();
    match id {
        "TXXX" => {
            let (desc, value) = split_terminated(enc, rest);
            out.tags.push((format!("TXXX:{}", decode(enc, desc)), decode(enc, value)));
        }
        "WXXX" => {
            let (desc, url) = split_terminated(enc, rest);
            out.tags.push((format!("WXXX:{}", decode(enc, desc)), latin1(url).trim_end_matches('\0').to_string()));
        }
        "TCON" => {
            let v = decode(enc, rest);
            out.tags.push((name, genre_text(&v)));
        }
        _ if id.starts_with('T') => out.tags.push((name, decode(enc, rest))),
        _ if id.starts_with('W') => out.tags.push((name, latin1(data).trim_end_matches('\0').to_string())),
        "COMM" | "USLT" => {
            let lang = rest.get(..3).map(latin1).unwrap_or_default();
            let (desc, text) = split_terminated(enc, rest.get(3..).unwrap_or_default());
            let desc = decode(enc, desc);
            let lang = if lang.chars().all(|c| c.is_ascii_alphabetic()) && lang != "XXX" {
                format!(" [{lang}]")
            } else {
                String::new()
            };
            let key = if desc.is_empty() { format!("{name}{lang}") } else { format!("{name} ({desc}){lang}") };
            out.tags.push((key, decode(enc, text)));
        }
        "APIC" => {
            // v2.3+: MIME type (NUL-terminated); v2.2 (`PIC`): a 3-letter format.
            let (mime, rest) = if v22 {
                let format = rest.get(..3).map(latin1).unwrap_or_default().to_lowercase();
                (format!("image/{format}"), rest.get(3..).unwrap_or_default())
            } else {
                let (m, r) = split_terminated(0, rest);
                (latin1(m), r)
            };
            let kind = rest.first().copied().unwrap_or(0);
            let (desc, image) = split_terminated(enc, rest.get(1..).unwrap_or_default());
            out.pictures.push(super::picture(picture_type(kind), &mime, &decode(enc, desc), image));
        }
        "PRIV" | "UFID" => {
            let (owner, value) = split_terminated(0, data);
            let value = if value.len() <= 32 && value.iter().all(|b| b.is_ascii_graphic()) {
                latin1(value)
            } else {
                format!("({} bytes, crc {:08x})", value.len(), crc32fast::hash(value))
            };
            out.tags.push((format!("{name} ({})", latin1(owner)), value));
        }
        "POPM" => {
            let (email, rest) = split_terminated(0, data);
            let rating = rest.first().map(|r| r.to_string()).unwrap_or_default();
            out.tags.push((format!("{name} ({})", latin1(email)), rating));
        }
        _ => out.tags.push((name, format!("({} bytes, crc {:08x})", data.len(), crc32fast::hash(data)))),
    }
}

fn v22_to_v23(id: &str) -> &str {
    match id {
        "TT1" => "TIT1",
        "TT2" => "TIT2",
        "TT3" => "TIT3",
        "TP1" => "TPE1",
        "TP2" => "TPE2",
        "TP3" => "TPE3",
        "TP4" => "TPE4",
        "TAL" => "TALB",
        "TYE" => "TYER",
        "TRK" => "TRCK",
        "TPA" => "TPOS",
        "TCO" => "TCON",
        "TCM" => "TCOM",
        "TEN" => "TENC",
        "TSS" => "TSSE",
        "TBP" => "TBPM",
        "TCR" => "TCOP",
        "TPB" => "TPUB",
        "TXX" => "TXXX",
        "WXX" => "WXXX",
        "COM" => "COMM",
        "ULT" => "USLT",
        "PIC" => "APIC",
        "UFI" => "UFID",
        "POP" => "POPM",
        id => id,
    }
}

fn frame_name(id: &str) -> String {
    let name = match id {
        "TIT1" => "Grouping",
        "TIT2" => "Title",
        "TIT3" => "Subtitle",
        "TPE1" => "Artist",
        "TPE2" => "Album artist",
        "TPE3" => "Conductor",
        "TPE4" => "Remixer",
        "TALB" => "Album",
        "TYER" => "Year",
        "TDAT" => "Date (DDMM)",
        "TDRC" => "Recording date",
        "TDRL" => "Release date",
        "TDOR" | "TORY" => "Original release date",
        "TDEN" => "Encoding date",
        "TDTG" => "Tagging date",
        "TRCK" => "Track",
        "TPOS" => "Disc",
        "TCON" => "Genre",
        "TCOM" => "Composer",
        "TEXT" => "Lyricist",
        "TENC" => "Encoded by",
        "TSSE" => "Encoder settings",
        "TBPM" => "BPM",
        "TCOP" => "Copyright",
        "TPUB" => "Publisher",
        "TLAN" => "Language",
        "TLEN" => "Length (ms)",
        "TKEY" => "Initial key",
        "TMED" => "Media type",
        "TMOO" => "Mood",
        "TSRC" => "ISRC",
        "TSOA" => "Sort album",
        "TSOP" => "Sort artist",
        "TSOT" => "Sort title",
        "TSO2" => "Sort album artist",
        "TCMP" => "Compilation",
        "COMM" => "Comment",
        "USLT" => "Lyrics",
        "APIC" => "Picture",
        "PRIV" => "Private",
        "UFID" => "Unique file id",
        "POPM" => "Popularimeter",
        "PCNT" => "Play counter",
        "WOAR" => "Artist URL",
        "WCOM" => "Commercial URL",
        "WOAS" => "Source URL",
        "WPUB" => "Publisher URL",
        "MCDI" => "Music CD id",
        "GEOB" => "Encapsulated object",
        "CHAP" => "Chapter",
        "CTOC" => "Table of contents",
        _ => return id.to_string(),
    };
    format!("{name} ({id})")
}

/// `(17)Rock` and `17` become `Rock`.
fn genre_text(v: &str) -> String {
    let v = v.trim();
    let num = v.strip_prefix('(').and_then(|r| r.split_once(')')).or(Some((v, "")));
    match num {
        Some((n, rest)) if !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) => {
            let g = n.parse::<u8>().ok().and_then(genre).unwrap_or("");
            if rest.is_empty() { if g.is_empty() { v.into() } else { g.into() } } else { rest.to_string() }
        }
        _ => v.to_string(),
    }
}

fn picture_type(t: u8) -> &'static str {
    match t {
        0 => "Picture",
        1 => "File icon",
        2 => "Other file icon",
        3 => "Cover (front)",
        4 => "Cover (back)",
        5 => "Leaflet page",
        6 => "Media",
        7 => "Lead artist",
        8 => "Artist",
        9 => "Conductor",
        10 => "Band",
        11 => "Composer",
        12 => "Lyricist",
        13 => "Recording location",
        14 => "During recording",
        15 => "During performance",
        16 => "Screen capture",
        17 => "Bright coloured fish",
        18 => "Illustration",
        19 => "Band logotype",
        20 => "Publisher logotype",
        _ => "Picture",
    }
}

pub(super) fn picture_type_name(t: u32) -> &'static str {
    picture_type(u8::try_from(t).unwrap_or(0))
}

/// The ID3v1 genres (0–79, the original list).
pub(super) fn genre(n: u8) -> Option<&'static str> {
    const GENRES: [&str; 80] = [
        "Blues",
        "Classic Rock",
        "Country",
        "Dance",
        "Disco",
        "Funk",
        "Grunge",
        "Hip-Hop",
        "Jazz",
        "Metal",
        "New Age",
        "Oldies",
        "Other",
        "Pop",
        "R&B",
        "Rap",
        "Reggae",
        "Rock",
        "Techno",
        "Industrial",
        "Alternative",
        "Ska",
        "Death Metal",
        "Pranks",
        "Soundtrack",
        "Euro-Techno",
        "Ambient",
        "Trip-Hop",
        "Vocal",
        "Jazz+Funk",
        "Fusion",
        "Trance",
        "Classical",
        "Instrumental",
        "Acid",
        "House",
        "Game",
        "Sound Clip",
        "Gospel",
        "Noise",
        "Alternative Rock",
        "Bass",
        "Soul",
        "Punk",
        "Space",
        "Meditative",
        "Instrumental Pop",
        "Instrumental Rock",
        "Ethnic",
        "Gothic",
        "Darkwave",
        "Techno-Industrial",
        "Electronic",
        "Pop-Folk",
        "Eurodance",
        "Dream",
        "Southern Rock",
        "Comedy",
        "Cult",
        "Gangsta",
        "Top 40",
        "Christian Rap",
        "Pop/Funk",
        "Jungle",
        "Native American",
        "Cabaret",
        "New Wave",
        "Psychedelic",
        "Rave",
        "Showtunes",
        "Trailer",
        "Lo-Fi",
        "Tribal",
        "Acid Punk",
        "Acid Jazz",
        "Polka",
        "Retro",
        "Musical",
        "Rock & Roll",
        "Hard Rock",
    ];
    GENRES.get(n as usize).copied()
}

fn id3v1(tag: &[u8]) -> Vec<(String, String)> {
    let field = |r: Range<usize>| latin1(&tag[r]).trim_end_matches(['\0', ' ']).to_string();
    let mut tags = vec![
        ("Title".to_string(), field(3..33)),
        ("Artist".to_string(), field(33..63)),
        ("Album".to_string(), field(63..93)),
        ("Year".to_string(), field(93..97)),
    ];
    if tag[125] == 0 && tag[126] != 0 {
        tags.push(("Comment".into(), field(97..125)));
        tags.push(("Track".into(), tag[126].to_string()));
    } else {
        tags.push(("Comment".into(), field(97..127)));
    }
    if tag[127] != 255 {
        tags.push(("Genre".into(), genre(tag[127]).map_or_else(|| tag[127].to_string(), str::to_string)));
    }
    tags.retain(|(_, v)| !v.is_empty());
    tags
}

fn ape(tag: &[u8]) -> Vec<(String, String)> {
    let mut tags = Vec::new();
    let body = if tag.starts_with(b"APETAGEX") { &tag[32..] } else { tag };
    let mut c = Cur::new(body.get(..body.len().saturating_sub(32)).unwrap_or_default());
    while let (Some(len), Some(flags)) = (c.le32(), c.le32()) {
        let key_len = c.rest().iter().position(|&b| b == 0).unwrap_or(c.remaining());
        let Some(key) = c.take(key_len) else { break };
        c.skip(1);
        let Some(value) = c.take(len as usize) else { break };
        let v = if (flags >> 1) & 3 == 0 {
            String::from_utf8_lossy(value).replace('\0', " / ")
        } else {
            format!("({} bytes, crc {:08x})", value.len(), crc32fast::hash(value))
        };
        tags.push((latin1(key), v));
        if tags.len() > super::MAX_ITEMS {
            break;
        }
    }
    tags
}
