//! FLAC files, and the Vorbis comments that FLAC, Ogg Vorbis and Opus share.
//!
//! A FLAC file is `fLaC`, metadata blocks (STREAMINFO first, then Vorbis
//! comments, pictures, seek table, padding …), and the audio frames. STREAMINFO
//! holds the sample rate, channels, bit depth, sample count and the MD5 of the
//! decoded samples; the latter makes the ≈ similar check cheap and exact: equal
//! MD5 and format means the same audio, however it was compressed or tagged.

use std::ops::Range;

use super::{Cur, Summary, Track, channels, mpeg};
use crate::{Error, Result};

pub(super) struct StreamInfo {
    pub sample_rate: u32,
    pub channels: u8,
    pub bits: u8,
    pub samples: u64,
    pub md5: [u8; 16],
}

impl StreamInfo {
    pub fn parse(b: &[u8]) -> Option<StreamInfo> {
        let b = b.get(..34)?;
        let packed = u64::from_be_bytes(b[10..18].try_into().ok()?);
        Some(StreamInfo {
            sample_rate: (packed >> 44) as u32,
            channels: ((packed >> 41) & 7) as u8 + 1,
            bits: ((packed >> 36) & 31) as u8 + 1,
            samples: packed & 0xF_FFFF_FFFF,
            md5: b[18..34].try_into().ok()?,
        })
    }

    /// Codec fields of the track.
    pub fn describe(&self, t: &mut Track) {
        t.field("Sample rate", format!("{} Hz", self.sample_rate));
        t.field("Channels", channels(self.channels as u64));
        t.field("Bits per sample", self.bits.to_string());
        if self.samples > 0 {
            t.field("Samples", self.samples.to_string());
        }
        if self.md5 != [0; 16] {
            t.field("Audio MD5", self.md5.iter().map(|b| format!("{b:02x}")).collect::<String>());
        }
    }

    pub fn seconds(&self) -> Option<f64> {
        (self.samples > 0 && self.sample_rate > 0).then(|| self.samples as f64 / self.sample_rate as f64)
    }
}

/// Vendor string and `KEY=value` pairs of a Vorbis comment block (little-endian lengths).
pub(super) fn vorbis_comment(b: &[u8]) -> Option<(String, Vec<(String, String)>)> {
    let mut c = Cur::new(b);
    let vendor_len = c.le32()? as usize;
    let vendor = super::utf8(c.take(vendor_len)?);
    let n = c.le32()? as usize;
    let mut tags = Vec::new();
    for _ in 0..n.min(100_000) {
        let Some(len) = c.le32() else { break };
        let Some(entry) = c.take(len as usize) else { break };
        let entry = String::from_utf8_lossy(entry);
        let (k, v) = entry.split_once('=').unwrap_or((&entry, ""));
        tags.push((k.to_ascii_uppercase(), v.to_string()));
    }
    Some((vendor, tags))
}

/// A FLAC PICTURE block (also base64-encoded in Vorbis comments).
pub(super) fn picture_block(b: &[u8]) -> Option<String> {
    let mut c = Cur::new(b);
    let kind = c.be32()?;
    let mime_len = c.be32()? as usize;
    let mime = super::latin1(c.take(mime_len)?);
    let desc_len = c.be32()? as usize;
    let desc = super::utf8(c.take(desc_len)?);
    c.skip(16)?;
    let len = c.be32()? as usize;
    let data = c.take(len.min(c.remaining()))?;
    Some(super::picture(mpeg::picture_type_name(kind), &mime, &desc, data))
}

struct Flac<'a> {
    info: Option<StreamInfo>,
    /// Metadata blocks: type and body.
    blocks: Vec<(u8, &'a [u8])>,
    /// Where the audio frames are.
    frames: Range<usize>,
}

fn parse(data: &[u8]) -> Option<Flac<'_>> {
    let start = mpeg::id3v2_len(data).unwrap_or(0);
    if data.get(start..start + 4)? != b"fLaC" {
        return None;
    }
    let mut c = Cur::at(data, start + 4);
    let mut blocks = Vec::new();
    loop {
        let header = c.u8()?;
        let len = c.be24()? as usize;
        let body = c.take(len.min(c.remaining()))?;
        blocks.push((header & 0x7F, body));
        if header & 0x80 != 0 || c.remaining() == 0 || blocks.len() > 10_000 {
            break;
        }
    }
    let end = if data.len() >= 128 && &data[data.len() - 128..data.len() - 125] == b"TAG" {
        data.len() - 128
    } else {
        data.len()
    };
    let info = blocks.first().filter(|(t, _)| *t == 0).and_then(|(_, b)| StreamInfo::parse(b));
    Some(Flac { info, blocks, frames: c.pos.min(end)..end })
}

pub(super) fn summary(data: &[u8]) -> Result<Summary> {
    let f = parse(data).ok_or_else(|| Error::Unsupported("not a FLAC file".into()))?;
    let mut s = Summary::new("FLAC", data.len());
    let mut t = Track::new("Audio");
    t.field("Codec", "FLAC");
    match &f.info {
        Some(info) => {
            info.describe(&mut t);
            s.duration = info.seconds();
        }
        None => s.warnings.push("no STREAMINFO block".into()),
    }
    s.tracks.push(t);
    let mut other = Vec::new();
    for (kind, body) in &f.blocks {
        match kind {
            4 => {
                if let Some((vendor, tags)) = vorbis_comment(body) {
                    s.info.push(("Encoder", super::clean(&vendor)));
                    s.tag_group("Vorbis comments", tags);
                }
            }
            6 => s.attachments.extend(picture_block(body)),
            0 | 1 => {}
            2 => other.push(format!("application {}", super::fourcc(body.get(..4).unwrap_or_default()))),
            3 => other.push(format!("seek table ({} points)", body.len() / 18)),
            5 => other.push("cue sheet".to_string()),
            n => other.push(format!("block type {n}")),
        }
    }
    if !other.is_empty() {
        s.info.push(("Metadata blocks", other.join(", ")));
    }
    if data.len() >= 128 && f.frames.end == data.len() - 128 {
        s.warnings.push("ID3v1 tag at the end of a FLAC file".into());
    }
    Ok(s)
}

pub(super) fn same_audio(left: &[u8], right: &[u8]) -> Option<&'static str> {
    let (l, r) = (parse(left)?, parse(right)?);
    let (li, ri) = (l.info.as_ref()?, r.info.as_ref()?);
    let format = |i: &StreamInfo| (i.sample_rate, i.channels, i.bits, i.samples, i.md5);
    if format(li) != format(ri) {
        return None;
    }
    if l.frames.len() == r.frames.len()
        && l.frames.len() <= super::MAX_COMPARE_BYTES
        && left.get(l.frames.clone()) == right.get(r.frames.clone())
    {
        return Some("same audio, only tags differ");
    }
    // The MD5 of the decoded samples: equal audio, maybe compressed differently.
    (li.md5 != [0; 16]).then_some("same audio (equal MD5 of the decoded samples)")
}
