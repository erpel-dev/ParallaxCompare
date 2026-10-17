//! RIFF and IFF files: WAV (also RF64/BW64), AVI and AIFF.
//!
//! These are sequences of chunks (id, size, body, padded to an even length);
//! RIFF is little-endian, IFF (AIFF) big-endian. The audio format is in the
//! `fmt ` (WAV) or `COMM` (AIFF) chunk, the samples in `data` or `SSND`, and
//! tags in `LIST`/`INFO`, `bext`, an embedded ID3v2 tag, or AIFF's text chunks.
//! AVI keeps its stream headers in `LIST hdrl`; `LIST movi` with the frames is
//! skipped by its size.

use std::ops::Range;

use super::{Cur, Summary, Track, bitrate, channels, decimal, fourcc, latin1, mpeg};
use crate::{Error, Result};

const MAX_CHUNKS: usize = 100_000;

#[derive(Clone, Copy)]
struct Chunk<'a> {
    id: [u8; 4],
    body: &'a [u8],
    /// Position of the body in the data the chunk was read from.
    start: usize,
}

/// Chunks of a RIFF (little-endian) or IFF (big-endian) body. `data_size`
/// replaces a `data` chunk's 0xFFFFFFFF size (RF64).
fn chunks(data: &[u8], big_endian: bool, data_size: Option<u64>) -> impl Iterator<Item = Chunk<'_>> {
    let mut pos = 0usize;
    let mut n = 0;
    std::iter::from_fn(move || {
        n += 1;
        if n > MAX_CHUNKS {
            return None;
        }
        let mut c = Cur::at(data, pos);
        let id: [u8; 4] = c.take(4)?.try_into().ok()?;
        let mut size = if big_endian { c.be32()? } else { c.le32()? } as u64;
        if size == 0xFFFF_FFFF
            && &id == b"data"
            && let Some(d) = data_size
        {
            size = d;
        }
        let start = c.pos;
        let end = usize::try_from(size).ok().and_then(|s| start.checked_add(s)).unwrap_or(usize::MAX).min(data.len());
        pos = end.saturating_add(end & 1);
        Some(Chunk { id, body: &data[start..end], start })
    })
}

fn info_name(id: &[u8]) -> Option<&'static str> {
    Some(match id {
        b"INAM" => "Title",
        b"IART" => "Artist",
        b"IPRD" => "Album",
        b"ICMT" => "Comment",
        b"ICRD" => "Date",
        b"IGNR" => "Genre",
        b"ISFT" => "Software",
        b"ICOP" => "Copyright",
        b"IENG" => "Engineer",
        b"ITCH" => "Technician",
        b"IKEY" => "Keywords",
        b"ISBJ" => "Subject",
        b"ISRC" => "Source",
        b"ITRK" | b"IPRT" => "Track",
        b"ILNG" => "Language",
        b"IMED" => "Medium",
        b"ICMS" => "Commissioned",
        b"IARL" => "Archival location",
        b"ISTR" => "Starring",
        b"IDIT" => "Digitization date",
        _ => return None,
    })
}

/// `LIST INFO` text chunks.
fn info_tags(list: &[u8]) -> Vec<(String, String)> {
    chunks(list, false, None)
        .map(|c| {
            let name = info_name(&c.id).map_or_else(|| fourcc(&c.id), |n| format!("{n} ({})", fourcc(&c.id)));
            (name, super::utf8(c.body).trim_end_matches('\0').to_string())
        })
        .collect()
}

/// Codec name of a WAVE format tag.
fn wave_format(tag: u16) -> String {
    let name = match tag {
        0x0001 => "PCM",
        0x0002 => "Microsoft ADPCM",
        0x0003 => "PCM float",
        0x0006 => "A-law",
        0x0007 => "µ-law",
        0x0011 => "IMA ADPCM",
        0x0031 => "GSM 6.10",
        0x0050 => "MPEG audio",
        0x0055 => "MP3",
        0x00FF | 0x1610 | 0x4143 | 0xA106 => "AAC",
        0x0160..=0x0163 => "Windows Media Audio",
        0x2000 => "AC-3",
        0x2001 => "DTS",
        0x674F | 0x6750 | 0x6751 | 0x676F | 0x6770 | 0x6771 => "Vorbis",
        0x704F => "Opus",
        0xF1AC => "FLAC",
        0xFFFE => "Extensible",
        _ => return format!("format 0x{tag:04X}"),
    };
    format!("{name} (0x{tag:04X})")
}

/// Codec, rate, channels, bits and byte rate of a WAVEFORMATEX.
fn wave_format_ex(fmt: &[u8], t: &mut Track) -> Option<u32> {
    let mut c = Cur::new(fmt);
    let mut tag = c.le16()?;
    let ch = c.le16()?;
    let rate = c.le32()?;
    let byte_rate = c.le32()?;
    let _align = c.le16()?;
    let bits = c.le16().unwrap_or(0);
    let mut valid_bits = None;
    if tag == 0xFFFE {
        c.skip(2)?;
        valid_bits = c.le16().filter(|&v| v != 0 && v != bits);
        c.skip(4)?;
        tag = c.le16()?; // first two bytes of the sub-format GUID
    }
    t.field("Codec", wave_format(tag));
    t.field("Sample rate", format!("{rate} Hz"));
    t.field("Channels", channels(ch as u64));
    if bits > 0 && matches!(tag, 1 | 3 | 6 | 7) {
        t.field("Bits per sample", valid_bits.map_or(bits.to_string(), |v| format!("{v} (in {bits})")));
    }
    if !matches!(tag, 1 | 3) {
        t.field("Bitrate", bitrate(byte_rate as f64 * 8.0));
    }
    Some(byte_rate)
}

pub(super) fn summary(data: &[u8]) -> Result<Summary> {
    let form = data.get(8..12).ok_or_else(|| Error::Unsupported("truncated RIFF file".into()))?;
    if form == b"AVI " {
        return avi(data);
    }
    let body = &data[12..];
    let rf64 = &data[..4] != b"RIFF";
    let mut s =
        Summary::new(if rf64 { format!("WAV ({})", latin1(&data[..4])) } else { "WAV".to_string() }, data.len());
    // RF64: the real sizes are in the `ds64` chunk.
    let data_size = rf64.then(|| chunks(body, false, None).find(|c| &c.id == b"ds64")).flatten().and_then(|c| {
        let mut r = Cur::new(c.body);
        r.skip(8)?;
        r.le64()
    });
    let mut t = Track::new("Audio");
    let mut byte_rate = None;
    let mut data_len = None;
    let mut other = Vec::new();
    for c in chunks(body, false, data_size) {
        match &c.id {
            b"fmt " => byte_rate = wave_format_ex(c.body, &mut t),
            b"data" => data_len = Some(c.body.len()),
            b"LIST" if c.body.starts_with(b"INFO") => s.tag_group("INFO tags", info_tags(&c.body[4..])),
            b"id3 " | b"ID3 " => {
                let id3 = mpeg::id3v2(c.body);
                s.attachments.extend(id3.pictures);
                s.tag_group("ID3v2 tags", id3.tags);
            }
            b"bext" => s.tag_group("Broadcast extension (bext)", bext(c.body)),
            b"ds64" | b"fact" | b"JUNK" | b"junk" | b"PAD " | b"FLLR" => {}
            id => other.push(fourcc(id).trim_end().to_string()),
        }
    }
    if let (Some(len), Some(rate)) = (data_len, byte_rate.filter(|&r| r > 0)) {
        s.duration = Some(len as f64 / rate as f64);
    }
    if !other.is_empty() {
        other.sort();
        other.dedup();
        s.info.push(("Other chunks", other.join(", ")));
    }
    s.tracks.push(t);
    Ok(s)
}

/// Broadcast Wave `bext`: description, originator, date and time, coding history.
fn bext(b: &[u8]) -> Vec<(String, String)> {
    let field = |r: Range<usize>| super::slice(b, r.start, r.len());
    let text = |r: Range<usize>| latin1(field(r)).trim_end_matches(['\0', ' ']).to_string();
    let mut tags = vec![
        ("Description".to_string(), text(0..256)),
        ("Originator".to_string(), text(256..288)),
        ("Originator reference".to_string(), text(288..320)),
        ("Origination date".to_string(), format!("{} {}", text(320..330), text(330..338)).trim().to_string()),
    ];
    if b.len() > 602 {
        tags.push((
            "Coding history".to_string(),
            latin1(&b[602..]).trim_end_matches(['\0', ' ', '\r', '\n']).to_string(),
        ));
    }
    tags.retain(|(_, v)| !v.is_empty());
    tags
}

/// WAV and AIFF: the format chunk and the range of the sample data, for the ≈ similar check.
pub(super) fn audio_parts(data: &[u8]) -> Option<(&[u8], Range<usize>)> {
    let big = data.starts_with(b"FORM");
    let body = data.get(12..)?;
    let (fmt_id, data_id) = if big { (b"COMM", b"SSND") } else { (b"fmt ", b"data") };
    let mut fmt = None;
    let mut samples = None;
    for c in chunks(body, big, None) {
        if &c.id == fmt_id {
            fmt = Some(c.body);
        } else if &c.id == data_id {
            samples = Some(12 + c.start..12 + c.start + c.body.len());
        }
    }
    Some((fmt?, samples?))
}

// ---- AIFF ----

/// An 80-bit IEEE extended float (AIFF sample rate).
fn extended(b: &[u8]) -> Option<f64> {
    let b = b.get(..10)?;
    let exp = (u16::from_be_bytes([b[0], b[1]]) & 0x7FFF) as i32;
    let mant = u64::from_be_bytes(b[2..10].try_into().ok()?);
    if exp == 0 && mant == 0 {
        return Some(0.0);
    }
    let v = mant as f64 * 2f64.powi(exp - 16383 - 63);
    Some(if b[0] & 0x80 != 0 { -v } else { v })
}

pub(super) fn aiff_summary(data: &[u8]) -> Result<Summary> {
    let aifc = data.get(8..12) == Some(b"AIFC");
    let mut s = Summary::new(if aifc { "AIFF-C" } else { "AIFF" }, data.len());
    let mut t = Track::new("Audio");
    let mut tags = Vec::new();
    for c in chunks(data.get(12..).unwrap_or_default(), true, None) {
        match &c.id {
            b"COMM" => {
                let mut r = Cur::new(c.body);
                let ch = r.be16().unwrap_or(0);
                let frames = r.be32().unwrap_or(0);
                let bits = r.be16().unwrap_or(0);
                let rate = r.take(10).and_then(extended).unwrap_or(0.0);
                let codec = if aifc {
                    let code = r.take(4).unwrap_or(b"NONE");
                    let name_len = r.u8().unwrap_or(0) as usize;
                    let name = r.take(name_len).map(latin1).unwrap_or_default();
                    if name.is_empty() { fourcc(code) } else { format!("{name} ({})", fourcc(code)) }
                } else {
                    "PCM (big-endian)".to_string()
                };
                t.field("Codec", codec);
                t.field("Sample rate", format!("{} Hz", decimal(rate)));
                t.field("Channels", channels(ch as u64));
                t.field("Bits per sample", bits.to_string());
                t.field("Samples", frames.to_string());
                if rate > 0.0 {
                    s.duration = Some(frames as f64 / rate);
                }
            }
            b"NAME" => tags.push(("Title (NAME)".into(), latin1(c.body))),
            b"AUTH" => tags.push(("Author (AUTH)".into(), latin1(c.body))),
            b"(c) " => tags.push(("Copyright ((c) )".into(), latin1(c.body))),
            b"ANNO" => tags.push(("Annotation (ANNO)".into(), latin1(c.body))),
            b"ID3 " | b"id3 " => {
                let id3 = mpeg::id3v2(c.body);
                s.attachments.extend(id3.pictures);
                s.tag_group("ID3v2 tags", id3.tags);
            }
            _ => {}
        }
    }
    s.tag_group("Text chunks", tags);
    s.tracks.push(t);
    Ok(s)
}

// ---- AVI ----

fn video_fourcc(code: &[u8]) -> String {
    let name = match &code.to_ascii_uppercase()[..] {
        b"H264" | b"X264" | b"AVC1" => "H.264",
        b"HEVC" | b"H265" | b"HVC1" => "H.265",
        b"FMP4" | b"XVID" | b"DIVX" | b"DX50" | b"MP4V" => "MPEG-4 Visual",
        b"MJPG" => "Motion JPEG",
        b"MPG2" | b"MPEG" => "MPEG-2 video",
        b"DIV3" | b"MP43" => "MS MPEG-4 v3",
        b"WMV1" | b"WMV2" | b"WMV3" => "Windows Media Video",
        b"VP80" => "VP8",
        b"VP90" => "VP9",
        b"AV01" => "AV1",
        b"FFV1" => "FFV1",
        b"HFYU" => "HuffYUV",
        b"DVSD" => "DV",
        b"\0\0\0\0" | b"RGB " | b"DIB " => return "Uncompressed RGB".to_string(),
        _ => return fourcc(code),
    };
    format!("{name} ({})", fourcc(code))
}

fn avi(data: &[u8]) -> Result<Summary> {
    let mut s = Summary::new("AVI", data.len());
    let body = &data[12..];
    let mut odml = false;
    for c in chunks(body, false, None) {
        match &c.id {
            b"LIST" if c.body.starts_with(b"hdrl") => {
                let mut frame_us = 0u32;
                let mut total_frames = 0u64;
                for h in chunks(&c.body[4..], false, None) {
                    match &h.id {
                        b"avih" => {
                            let mut r = Cur::new(h.body);
                            frame_us = r.le32().unwrap_or(0);
                            r.skip(12);
                            total_frames = r.le32().unwrap_or(0) as u64;
                        }
                        b"LIST" if h.body.starts_with(b"strl") => {
                            if let Some(t) = avi_stream(&h.body[4..]) {
                                s.tracks.push(t);
                            }
                        }
                        b"LIST" if h.body.starts_with(b"odml") => {
                            odml = true;
                            if let Some(d) = chunks(&h.body[4..], false, None).find(|d| &d.id == b"dmlh") {
                                total_frames = Cur::new(d.body).le32().map_or(total_frames, u64::from);
                            }
                        }
                        _ => {}
                    }
                }
                if frame_us > 0 && total_frames > 0 {
                    s.duration = Some(total_frames as f64 * frame_us as f64 / 1e6);
                }
            }
            b"LIST" if c.body.starts_with(b"INFO") => s.tag_group("INFO tags", info_tags(&c.body[4..])),
            _ => {}
        }
    }
    if odml {
        s.info.push(("Format", "OpenDML (AVI 2.0)".to_string()));
    }
    if s.tracks.is_empty() {
        s.warnings.push("no stream headers (LIST hdrl) found".into());
    }
    Ok(s)
}

fn avi_stream(strl: &[u8]) -> Option<Track> {
    let mut t = Track::default();
    let mut rate = None;
    let mut length = 0u32;
    for c in chunks(strl, false, None) {
        match &c.id {
            b"strh" => {
                let mut r = Cur::new(c.body);
                let kind = r.take(4)?;
                let _handler = r.take(4)?;
                r.skip(4 + 2 + 2 + 4)?;
                let (scale, rt) = (r.le32()?, r.le32()?);
                r.skip(4)?;
                length = r.le32()?;
                t.kind = match kind {
                    b"vids" => "Video",
                    b"auds" => "Audio",
                    b"txts" => "Subtitles",
                    b"mids" => "MIDI",
                    _ => "Other",
                }
                .to_string();
                rate = (scale > 0).then(|| rt as f64 / scale as f64);
            }
            b"strf" if t.kind == "Video" => {
                let mut r = Cur::new(c.body);
                r.skip(4)?;
                let (w, h) = (r.le32()? as i32, r.le32()? as i32);
                r.skip(2)?;
                let bits = r.le16()?;
                let code = r.take(4)?;
                t.field("Codec", video_fourcc(code));
                t.field("Resolution", format!("{} × {}", w.unsigned_abs(), h.unsigned_abs()));
                if code == [0; 4] {
                    t.field("Bits per pixel", bits.to_string());
                }
                if let Some(r) = rate {
                    t.field("Frame rate", format!("{} fps", decimal(r)));
                }
                t.field("Frames", length.to_string());
            }
            b"strf" if t.kind == "Audio" => {
                wave_format_ex(c.body, &mut t);
            }
            b"strn" => t.field("Title", super::clean(&latin1(c.body))),
            _ => {}
        }
    }
    (!t.kind.is_empty()).then_some(t)
}
