//! Ogg files (Vorbis, Opus, FLAC, Speex, Theora).
//!
//! An Ogg file is a sequence of pages, each carrying segments of the packets
//! of one logical stream (serial number). Every stream begins with header
//! packets: an identification header (codec, rate, channels) and a comment
//! header (Vorbis comments). They come first in the file, so the summary reads
//! pages from the start until each stream's headers are complete. The duration
//! is the granule position of each stream's last page, found in the last 64 KB.

use std::collections::BTreeMap;

use super::flac::{StreamInfo, picture_block, vorbis_comment};
use super::{Cur, Summary, Track, channels, decimal};
use crate::{Error, Result};

/// Header pages are read from at most this many bytes (cover art in comments can be large).
const MAX_HEADER_BYTES: usize = 32 * 1024 * 1024;
/// The last pages are looked for in this many bytes at the end.
const TAIL: usize = 64 * 1024;

struct Page<'a> {
    flags: u8,
    granule: u64,
    serial: u32,
    /// Segment lengths and the page body.
    lacing: &'a [u8],
    body: &'a [u8],
    len: usize,
}

fn page(data: &[u8], pos: usize) -> Option<Page<'_>> {
    let mut c = Cur::at(data, pos);
    if c.take(4)? != b"OggS" || c.u8()? != 0 {
        return None;
    }
    let flags = c.u8()?;
    let granule = c.le64()?;
    let serial = c.le32()?;
    c.skip(8)?;
    let n = c.u8()? as usize;
    let lacing = c.take(n)?;
    let body_len: usize = lacing.iter().map(|&l| l as usize).sum();
    let body = c.take(body_len.min(c.remaining()))?;
    Some(Page { flags, granule, serial, lacing, body, len: c.pos - pos })
}

#[derive(Default)]
struct Stream {
    packets: Vec<Vec<u8>>,
    partial: Vec<u8>,
    needed: usize,
    last_granule: Option<u64>,
}

pub(super) fn summary(data: &[u8]) -> Result<Summary> {
    let mut streams: BTreeMap<u32, Stream> = BTreeMap::new();
    let mut order = Vec::new();
    let mut pos = 0;
    while pos < data.len().min(MAX_HEADER_BYTES) {
        let Some(p) = page(data, pos) else { break };
        pos += p.len;
        if p.flags & 2 != 0 && !streams.contains_key(&p.serial) {
            order.push(p.serial);
            streams.insert(p.serial, Stream { needed: 1, ..Stream::default() });
        }
        let Some(st) = streams.get_mut(&p.serial) else { continue };
        if st.packets.len() < st.needed {
            let mut off = 0;
            for &l in p.lacing {
                let seg = p.body.get(off..off + l as usize).unwrap_or_default();
                off += l as usize;
                if st.partial.len() < MAX_HEADER_BYTES {
                    st.partial.extend_from_slice(seg);
                }
                if l < 255 {
                    let packet = std::mem::take(&mut st.partial);
                    if st.packets.is_empty() {
                        st.needed = headers_needed(&packet);
                    }
                    st.packets.push(packet);
                    if st.packets.len() >= st.needed {
                        break;
                    }
                }
            }
        }
        if streams.values().all(|s| s.packets.len() >= s.needed) && p.flags & 2 == 0 {
            break;
        }
    }
    if streams.is_empty() {
        return Err(Error::Unsupported("not an Ogg file".into()));
    }
    // Last granule positions.
    let tail_start = data.len().saturating_sub(TAIL);
    let tail = &data[tail_start..];
    let mut i = 0;
    while let Some(off) = tail.get(i..).and_then(|t| t.windows(4).position(|w| w == b"OggS")) {
        let at = tail_start + i + off;
        if let Some(p) = page(data, at)
            && p.granule != u64::MAX
            && let Some(st) = streams.get_mut(&p.serial)
        {
            st.last_granule = Some(p.granule);
        }
        i += off + 4;
    }

    let mut s = Summary::new("Ogg", data.len());
    let mut codecs = Vec::new();
    for (n, serial) in order.iter().enumerate() {
        let st = &streams[serial];
        let (mut t, seconds, tags, vendor) = stream(st);
        t.number = Some(n as u64 + 1);
        if let Some(sec) = seconds {
            t.field("Duration", super::duration(sec));
            s.duration = Some(s.duration.unwrap_or(0.0).max(sec));
        }
        codecs.push(t.fields.first().map(|(_, v)| v.split(' ').next().unwrap_or("").to_string()).unwrap_or_default());
        if let Some(v) = vendor {
            t.field("Encoder", super::clean(&v));
        }
        let mut plain = Vec::new();
        for (k, v) in tags {
            if k == "METADATA_BLOCK_PICTURE" {
                match base64(&v).and_then(|b| picture_block(&b)) {
                    Some(p) => s.attachments.push(p),
                    None => plain.push((k, v)),
                }
            } else {
                plain.push((k, v));
            }
        }
        if order.len() == 1 {
            s.tag_group("Vorbis comments", plain);
        } else {
            t.tags = plain;
        }
        s.tracks.push(t);
    }
    s.container = format!("Ogg ({})", codecs.join(", "));
    Ok(s)
}

fn headers_needed(first: &[u8]) -> usize {
    if first.starts_with(b"\x01vorbis") || first.starts_with(b"\x80theora") {
        3
    } else if first.starts_with(b"OpusHead") || first.starts_with(b"Speex   ") {
        2
    } else if first.starts_with(b"\x7FFLAC") {
        // The mapping header says how many metadata packets follow (0 = unknown).
        let n = first.get(7..9).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as usize);
        1 + n.clamp(1, 64)
    } else {
        1
    }
}

/// Track fields, duration, comments and vendor of one logical stream.
/// A stream's track, duration in seconds, comments and encoder (vendor string).
type StreamSummary = (Track, Option<f64>, Vec<(String, String)>, Option<String>);

fn stream(st: &Stream) -> StreamSummary {
    let first = st.packets.first().map(Vec::as_slice).unwrap_or_default();
    let packet = |i: usize| st.packets.get(i).map(Vec::as_slice).unwrap_or_default();
    let mut comments = None;
    let mut seconds = None;
    let mut t;
    if first.starts_with(b"\x01vorbis") {
        t = Track::new("Audio");
        let mut c = Cur::at(first, 7);
        c.skip(4);
        let ch = c.u8().unwrap_or(0);
        let rate = c.le32().unwrap_or(0);
        let (_max, nominal) = (c.le32(), c.le32().map(|n| n as i32));
        t.field("Codec", "Vorbis");
        t.field("Sample rate", format!("{rate} Hz"));
        t.field("Channels", channels(ch as u64));
        if let Some(n) = nominal.filter(|&n| n > 0) {
            t.field("Nominal bitrate", super::bitrate(n as f64));
        }
        seconds = st.last_granule.filter(|_| rate > 0).map(|g| g as f64 / rate as f64);
        comments = packet(1).strip_prefix(b"\x03vorbis").and_then(vorbis_comment);
    } else if first.starts_with(b"OpusHead") {
        t = Track::new("Audio");
        let mut c = Cur::at(first, 8);
        let _version = c.u8();
        let ch = c.u8().unwrap_or(0);
        let pre_skip = c.le16().unwrap_or(0) as u64;
        let input_rate = c.le32().unwrap_or(0);
        t.field("Codec", "Opus");
        t.field("Sample rate", "48000 Hz");
        if input_rate > 0 && input_rate != 48000 {
            t.field("Input sample rate", format!("{input_rate} Hz"));
        }
        t.field("Channels", channels(ch as u64));
        seconds = st.last_granule.map(|g| g.saturating_sub(pre_skip) as f64 / 48000.0);
        comments = packet(1).strip_prefix(b"OpusTags").and_then(vorbis_comment);
    } else if first.starts_with(b"\x7FFLAC") {
        t = Track::new("Audio");
        t.field("Codec", "FLAC");
        if let Some(info) = first.get(17..).and_then(StreamInfo::parse) {
            info.describe(&mut t);
            seconds = info.seconds();
        }
        comments = st
            .packets
            .iter()
            .skip(1)
            .find(|p| p.first().map(|b| b & 0x7F) == Some(4))
            .and_then(|p| vorbis_comment(&p[4..]));
    } else if first.starts_with(b"Speex   ") {
        t = Track::new("Audio");
        let mut c = Cur::at(first, 36);
        let rate = c.le32().unwrap_or(0);
        c.skip(8);
        let ch = c.le32().unwrap_or(0);
        t.field("Codec", "Speex");
        t.field("Sample rate", format!("{rate} Hz"));
        t.field("Channels", channels(ch as u64));
        seconds = st.last_granule.filter(|_| rate > 0).map(|g| g as f64 / rate as f64);
        comments = vorbis_comment(packet(1));
    } else if first.starts_with(b"\x80theora") {
        t = Track::new("Video");
        let mut c = Cur::at(first, 14);
        let (w, h) = (c.be24().unwrap_or(0), c.be24().unwrap_or(0));
        c.skip(2);
        let (num, den) = (c.be32().unwrap_or(0), c.be32().unwrap_or(0));
        t.field("Codec", "Theora");
        t.field("Resolution", format!("{w} × {h}"));
        if den > 0 {
            t.field("Frame rate", format!("{} fps", decimal(num as f64 / den as f64)));
        }
        comments = packet(1).strip_prefix(b"\x81theora").and_then(vorbis_comment);
    } else {
        t = Track::new("Unknown");
        t.field("Codec", super::fourcc(first.get(..8).unwrap_or(first)));
    }
    let (vendor, tags) = comments.map_or((None, Vec::new()), |(v, t)| (Some(v), t));
    (t, seconds, tags, vendor)
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}
