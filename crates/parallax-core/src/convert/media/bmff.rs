//! ISO base media files (MP4, MOV, M4A, 3GP), and the box walker the HEIF/AVIF
//! image metadata reuses.
//!
//! A file is a sequence of boxes (`size`, `type`, body), most of them containers
//! of more boxes. Everything the summary needs is in `ftyp` and `moov` (movie
//! header, per track `tkhd`, `mdhd`, `hdlr` and the sample tables, user data
//! with the iTunes-style `ilst` tags and Nero `chpl` chapters). `mdat`, the
//! media data, is skipped by its size wherever it is, so `moov` at the end of a
//! multi-gigabyte file is found without reading the data in between.

use super::{Chapter, Cur, Summary, Track, bitrate, channels, decimal, duration, fourcc, latin1, utf8};
use crate::{Error, Result};

/// The top-level boxes of a fragmented file can be many; stop counting after this.
const MAX_BOXES: usize = 1_000_000;

/// One box: its type and body (the bytes after the header).
#[derive(Debug, Clone, Copy)]
pub(in crate::convert) struct Bx<'a> {
    pub kind: [u8; 4],
    pub body: &'a [u8],
}

/// The boxes in a byte range, in order. Stops at the first malformed header; a
/// box that claims more bytes than there are is cut at the end (truncated file).
pub(in crate::convert) struct Boxes<'a> {
    data: &'a [u8],
    pos: usize,
    count: usize,
}

impl<'a> Boxes<'a> {
    pub fn new(data: &'a [u8]) -> Boxes<'a> {
        Boxes { data, pos: 0, count: 0 }
    }

    /// The body of the first child of this type.
    pub fn find(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
        Boxes::new(data).find(|b| &b.kind == kind).map(|b| b.body)
    }

    /// The body at a path of nested box types.
    pub fn path(data: &'a [u8], path: &[&[u8; 4]]) -> Option<&'a [u8]> {
        path.iter().try_fold(data, |d, kind| Boxes::find(d, kind))
    }
}

impl<'a> Iterator for Boxes<'a> {
    type Item = Bx<'a>;

    fn next(&mut self) -> Option<Bx<'a>> {
        self.count += 1;
        if self.count > MAX_BOXES {
            return None;
        }
        let mut c = Cur::at(self.data, self.pos);
        let size = c.be32()? as u64;
        let kind: [u8; 4] = c.take(4)?.try_into().ok()?;
        let size = match size {
            0 => (self.data.len() - self.pos) as u64,
            1 => c.be64()?,
            s => s,
        };
        let header = c.pos - self.pos;
        if size < header as u64 {
            self.pos = self.data.len();
            return None;
        }
        let end =
            usize::try_from(size).ok().and_then(|s| self.pos.checked_add(s)).unwrap_or(usize::MAX).min(self.data.len());
        let body = &self.data[c.pos..end.max(c.pos)];
        self.pos = end;
        Some(Bx { kind, body })
    }
}

/// Version and flags of a "full box", and its body after them.
fn full(body: &[u8]) -> Option<(u8, Cur<'_>)> {
    let mut c = Cur::new(body);
    let version = c.u8()?;
    c.skip(3)?;
    Some((version, c))
}

fn major_brand(data: &[u8]) -> Option<[u8; 4]> {
    (data.get(4..8)? == b"ftyp").then(|| data.get(8..12)?.try_into().ok())?
}

/// Whether an ISO media file is a HEIF or AVIF image (still or sequence).
pub(in crate::convert) fn is_heif(data: &[u8]) -> bool {
    major_brand(data).is_some_and(|b| {
        matches!(
            &b,
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1" | b"avif" | b"avis" | b"avio"
        )
    })
}

fn container_name(brand: Option<&[u8; 4]>) -> &'static str {
    match brand {
        None | Some(b"qt  ") => "QuickTime movie",
        Some(b"M4A " | b"M4B " | b"M4P ") => "MPEG-4 audio (M4A)",
        Some(b"M4V " | b"M4VH" | b"M4VP") => "MPEG-4 video (M4V)",
        Some(b) if b.starts_with(b"3gp") || b.starts_with(b"3ge") || b.starts_with(b"3gg") => "3GPP",
        Some(b) if b.starts_with(b"3g2") => "3GPP2",
        Some(b"crx ") => "Canon RAW (CR3)",
        Some(b"dash" | b"msdh" | b"msix") => "MPEG-4 (DASH segment)",
        Some(b"f4v ") => "Flash video (F4V)",
        _ => "MPEG-4",
    }
}

pub(super) fn summary(data: &[u8]) -> Result<Summary> {
    let mut moov = None;
    let mut ftyp = None;
    let mut fragments = 0usize;
    let mut boxes = 0usize;
    for b in Boxes::new(data) {
        boxes += 1;
        match &b.kind {
            b"ftyp" => ftyp = Some(b.body),
            b"moov" => moov = Some(b.body),
            b"moof" => fragments += 1,
            _ => {}
        }
    }
    if boxes == 0 {
        return Err(Error::Unsupported("not an ISO media file".into()));
    }
    let brand = ftyp.and_then(|f| f.get(..4)).and_then(|b| <[u8; 4]>::try_from(b).ok());
    let mut s = Summary::new(container_name(brand.as_ref()), data.len());
    if let Some(f) = ftyp {
        let mut c = Cur::new(f);
        let major = c.take(4).map(fourcc).unwrap_or_default();
        let minor = c.be32().unwrap_or(0);
        let mut compatible = Vec::new();
        while let Some(b) = c.take(4) {
            if compatible.len() < 32 {
                compatible.push(fourcc(b).trim_end().to_string());
            }
        }
        let compatible =
            if compatible.is_empty() { String::new() } else { format!("; compatible: {}", compatible.join(", ")) };
        s.info.push(("Brand", format!("{} (version {minor}{compatible})", major.trim_end())));
    }
    if fragments > 0 {
        s.info.push(("Fragments", fragments.to_string()));
    }
    let Some(moov) = moov else {
        s.warnings.push("no movie header (moov box): the file is incomplete".into());
        return Ok(s);
    };
    if let Some((version, mut c)) = Boxes::find(moov, b"mvhd").and_then(full) {
        let (timescale, dur) = if version == 1 {
            c.skip(16);
            (c.be32(), c.be64())
        } else {
            c.skip(8);
            (c.be32(), c.be32().map(u64::from))
        };
        if let (Some(ts), Some(d)) = (timescale.filter(|&t| t > 0), dur)
            && d != u64::MAX
            && d != u32::MAX as u64
        {
            s.duration = Some(d as f64 / ts as f64);
        }
    }
    // Fragmented files: the duration is in mvex/mehd when the muxer wrote it.
    if s.duration.is_none()
        && let Some((version, mut c)) = Boxes::path(moov, &[b"mvex", b"mehd"]).and_then(full)
        && let Some(ts) = Boxes::find(moov, b"mvhd").and_then(full).and_then(|(v, mut c)| {
            c.skip(if v == 1 { 16 } else { 8 })?;
            c.be32().filter(|&t| t > 0)
        })
    {
        let d = if version == 1 { c.be64() } else { c.be32().map(u64::from) };
        s.duration = d.map(|d| d as f64 / ts as f64);
    }
    for b in Boxes::new(moov).filter(|b| &b.kind == b"trak") {
        if let Some(t) = track(b.body) {
            s.tracks.push(t);
        }
    }
    if let Some(udta) = Boxes::find(moov, b"udta") {
        let tags = user_data_tags(udta, &mut s.attachments);
        s.tag_group("Tags", tags);
        if let Some(chpl) = Boxes::find(udta, b"chpl") {
            s.chapters = nero_chapters(chpl, s.duration);
        }
    }
    // Tags in moov/meta directly (QuickTime `mdta` keys).
    if let Some(meta) = Boxes::find(moov, b"meta") {
        let tags = meta_tags(meta, &mut s.attachments);
        s.tag_group("Metadata", tags);
    }
    Ok(s)
}

fn track(trak: &[u8]) -> Option<Track> {
    let mdia = Boxes::find(trak, b"mdia")?;
    let handler = Boxes::find(mdia, b"hdlr").and_then(|h| h.get(8..12)).map(|h| [h[0], h[1], h[2], h[3]]);
    let kind = match handler.as_ref() {
        Some(b"vide") => "Video",
        Some(b"soun") => "Audio",
        Some(b"subt" | b"sbtl" | b"clcp") => "Subtitles",
        Some(b"text") => "Text",
        Some(b"tmcd") => "Timecode",
        Some(b"hint") => "Hint",
        Some(b"meta") => "Metadata",
        Some(h) => return Some(Track { kind: format!("Other ({})", fourcc(h)), ..Track::default() }),
        None => "Unknown",
    };
    let mut t = Track::new(kind);
    let mut display = None;
    let mut enabled = true;
    if let Some(tkhd) = Boxes::find(trak, b"tkhd")
        && let Some((version, mut c)) = full(tkhd)
    {
        enabled = tkhd.get(3).is_none_or(|f| f & 1 == 1);
        c.skip(if version == 1 { 16 } else { 8 });
        t.number = c.be32().map(u64::from);
        c.skip(if version == 1 { 4 + 8 } else { 4 + 4 });
        c.skip(8 + 2 + 2 + 2 + 2 + 36);
        if let (Some(w), Some(h)) = (c.be32(), c.be32()) {
            display = Some((w >> 16, h >> 16));
        }
    }
    let mut track_seconds = None;
    let mut language = None;
    if let Some((version, mut c)) = Boxes::find(mdia, b"mdhd").and_then(full) {
        let (ts, d) = if version == 1 {
            c.skip(16);
            (c.be32(), c.be64())
        } else {
            c.skip(8);
            (c.be32(), c.be32().map(u64::from))
        };
        if let (Some(ts), Some(d)) = (ts.filter(|&t| t > 0), d) {
            track_seconds = (d != u32::MAX as u64 && d != u64::MAX).then(|| d as f64 / ts as f64);
        }
        language = c.be16().and_then(packed_language);
    }
    let stbl = Boxes::path(mdia, &[b"minf", b"stbl"]);
    let entry = stbl.and_then(|s| Boxes::find(s, b"stsd")).and_then(full).and_then(|(_, mut c)| {
        c.skip(4)?;
        Boxes::new(c.rest()).next()
    });
    if let Some(e) = entry {
        sample_entry(&mut t, e, handler.as_ref())?;
    }
    if let (Some(display), "Video") = (display, kind)
        && display.0 > 0
        && !t.fields.iter().any(|(l, v)| *l == "Resolution" && *v == format!("{} × {}", display.0, display.1))
    {
        t.field("Display size", format!("{} × {}", display.0, display.1));
    }
    let samples = stbl.and_then(|s| Boxes::find(s, b"stsz")).and_then(sample_sizes);
    if let Some(d) = track_seconds {
        t.field("Duration", duration(d));
    }
    if let Some((count, bytes)) = samples {
        let label = if kind == "Video" { "Frames" } else { "Samples" };
        t.field(label, count.to_string());
        if let Some(d) = track_seconds.filter(|&d| d > 0.0) {
            if kind == "Video" && count > 0 {
                t.field("Frame rate", format!("{} fps", decimal(count as f64 / d)));
            }
            if let Some(bytes) = bytes.filter(|&b| b > 0) {
                t.field("Bitrate", bitrate(bytes as f64 * 8.0 / d));
            }
        }
    }
    if let Some(l) = language {
        t.field("Language", l);
    }
    if !enabled {
        t.field("Enabled", "no");
    }
    // A track's own name, as QuickTime and ffmpeg write it.
    if let Some(name) = Boxes::find(trak, b"udta").and_then(|u| Boxes::find(u, b"name")) {
        t.field("Title", super::clean(&utf8(name)));
    }
    Some(t)
}

/// ISO 639-2/T code packed into 15 bits; `und` is left out.
fn packed_language(v: u16) -> Option<String> {
    let l: String = [(v >> 10) & 31, (v >> 5) & 31, v & 31].iter().map(|&c| char::from(c as u8 + 0x60)).collect();
    (l.chars().all(|c| c.is_ascii_lowercase()) && l != "und").then_some(l)
}

/// Sample count and, when the table lists them, the total bytes of the samples.
fn sample_sizes(stsz: &[u8]) -> Option<(u64, Option<u64>)> {
    let (_, mut c) = full(stsz)?;
    let fixed = c.be32()? as u64;
    let count = c.be32()? as u64;
    if fixed != 0 {
        return Some((count, Some(fixed * count)));
    }
    let table = c.rest();
    if (table.len() as u64) < count * 4 {
        return Some((count, None));
    }
    let total =
        table.chunks_exact(4).take(count as usize).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as u64).sum();
    Some((count, Some(total)))
}

/// Codec and format of a track from its first sample description.
fn sample_entry(t: &mut Track, e: Bx<'_>, handler: Option<&[u8; 4]>) -> Option<()> {
    let code = fourcc(&e.kind);
    let mut c = Cur::new(e.body);
    c.skip(8)?; // reserved, data reference index
    match handler {
        Some(b"vide") => {
            c.skip(16)?;
            let (w, h) = (c.be16()?, c.be16()?);
            c.skip(4 + 4 + 4 + 2)?;
            let compressor = c.take(32)?;
            c.skip(4)?;
            let children = c.rest();
            t.field("Codec", video_codec(&e.kind, children));
            t.field("Resolution", format!("{w} × {h}"));
            if let Some(pasp) = Boxes::find(children, b"pasp") {
                let mut p = Cur::new(pasp);
                if let (Some(h), Some(v)) = (p.be32(), p.be32())
                    && h != v
                    && h > 0
                    && v > 0
                {
                    t.field("Pixel aspect", format!("{h}:{v}"));
                }
            }
            let name_len = (compressor[0] as usize).min(31);
            let name = latin1(&compressor[1..1 + name_len]);
            if !name.trim().is_empty() {
                t.field("Compressor", super::clean(&name));
            }
        }
        Some(b"soun") => {
            let version = c.be16()?;
            c.skip(6)?;
            let mut ch = c.be16()? as u64;
            let bits = c.be16()?;
            c.skip(4)?;
            let mut rate = c.be32()? as f64 / 65536.0;
            match version {
                1 => c.skip(16)?,
                2 => {
                    c.skip(4)?;
                    rate = f64::from_bits(c.be64()?);
                    ch = c.be32()? as u64;
                    c.skip(20)?;
                }
                _ => {}
            }
            let children = c.rest();
            let (codec, real_rate, real_channels) = audio_codec(&e.kind, children);
            t.field("Codec", codec);
            t.field("Sample rate", format!("{} Hz", decimal(real_rate.unwrap_or(rate))));
            t.field("Channels", channels(real_channels.unwrap_or(ch)));
            if matches!(
                &e.kind,
                b"lpcm" | b"sowt" | b"twos" | b"in24" | b"in32" | b"fl32" | b"fl64" | b"raw " | b"ipcm" | b"fpcm"
            ) {
                t.field("Bits per sample", bits.to_string());
            }
        }
        _ => {
            let name = match &e.kind {
                b"tx3g" => "3GPP timed text (tx3g)".to_string(),
                b"text" => "QuickTime text (text)".to_string(),
                b"wvtt" => "WebVTT (wvtt)".to_string(),
                b"stpp" => "TTML (stpp)".to_string(),
                b"c608" => "CEA-608 captions (c608)".to_string(),
                b"tmcd" => "Timecode (tmcd)".to_string(),
                _ => code,
            };
            t.field("Codec", name);
        }
    }
    Some(())
}

fn video_codec(kind: &[u8; 4], children: &[u8]) -> String {
    let code = fourcc(kind);
    let detail = |b: &[u8; 4]| Boxes::find(children, b);
    match kind {
        b"avc1" | b"avc2" | b"avc3" | b"avc4" => {
            format!("H.264 ({code}){}", detail(b"avcC").map(avc_profile).unwrap_or_default())
        }
        b"hvc1" | b"hev1" => format!("H.265 ({code}){}", detail(b"hvcC").map(hevc_profile).unwrap_or_default()),
        b"dvh1" | b"dvhe" | b"dva1" | b"dvav" => format!("Dolby Vision ({code})"),
        b"av01" => format!("AV1 ({code}){}", detail(b"av1C").map(av1_profile).unwrap_or_default()),
        b"vp08" => format!("VP8 ({code})"),
        b"vp09" => format!(
            "VP9 ({code}){}",
            detail(b"vpcC").and_then(|v| v.get(4)).map(|p| format!(", profile {p}")).unwrap_or_default()
        ),
        b"mp4v" => format!("MPEG-4 Visual ({code})"),
        b"s263" | b"h263" => format!("H.263 ({code})"),
        b"jpeg" | b"mjpa" | b"mjpb" => format!("Motion JPEG ({code})"),
        b"apch" | b"apcn" | b"apcs" | b"apco" | b"ap4h" | b"ap4x" => format!("Apple ProRes ({code})"),
        b"mp2v" | b"m2v1" | b"xdvc" => format!("MPEG-2 video ({code})"),
        _ => code,
    }
}

/// `, High profile, level 3.1` from an `avcC` (also Matroska's CodecPrivate).
pub(super) fn avc_profile(avcc: &[u8]) -> String {
    let (Some(&profile), Some(&compat), Some(&level)) = (avcc.get(1), avcc.get(2), avcc.get(3)) else {
        return String::new();
    };
    let name = match profile {
        66 if compat & 0x40 != 0 => "Constrained Baseline",
        66 => "Baseline",
        77 => "Main",
        88 => "Extended",
        100 => "High",
        110 => "High 10",
        122 => "High 4:2:2",
        244 => "High 4:4:4 Predictive",
        44 => "CAVLC 4:4:4",
        _ => return format!(", profile {profile}, level {}", decimal(level as f64 / 10.0)),
    };
    format!(", {name} profile, level {}", decimal(level as f64 / 10.0))
}

/// `, Main 10 profile, level 4` from an `hvcC`.
pub(in crate::convert) fn hevc_profile(hvcc: &[u8]) -> String {
    let (Some(&p), Some(&level)) = (hvcc.get(1), hvcc.get(12)) else { return String::new() };
    let name = match p & 31 {
        1 => "Main".to_string(),
        2 => "Main 10".to_string(),
        3 => "Main Still Picture".to_string(),
        4 => "Range extensions".to_string(),
        n => format!("{n}"),
    };
    let tier = if p & 0x20 != 0 { ", High tier" } else { "" };
    format!(", {name} profile{tier}, level {}", decimal(level as f64 / 30.0))
}

/// `, Main profile, level 3.0` from an `av1C`.
pub(in crate::convert) fn av1_profile(av1c: &[u8]) -> String {
    let Some(&b) = av1c.get(1) else { return String::new() };
    let profile = match b >> 5 {
        0 => "Main",
        1 => "High",
        2 => "Professional",
        _ => "unknown",
    };
    let idx = b & 31;
    let level = if idx == 31 { "max".to_string() } else { format!("{}.{}", 2 + idx / 4, idx % 4) };
    format!(", {profile} profile, level {level}")
}

/// Codec name, and sample rate and channels when the codec configuration
/// knows them better than the sample entry (AAC with SBR, Opus).
fn audio_codec(kind: &[u8; 4], children: &[u8]) -> (String, Option<f64>, Option<u64>) {
    let code = fourcc(kind);
    match kind {
        b"mp4a" => {
            let esds = Boxes::find(children, b"esds").or_else(|| {
                // QuickTime: mp4a/wave/esds.
                Boxes::find(children, b"wave").and_then(|w| Boxes::find(w, b"esds"))
            });
            match esds.and_then(full).and_then(|(_, c)| es_descriptor(c.rest())) {
                Some((0x40 | 0x66 | 0x67 | 0x68, asc)) => {
                    let (name, rate, ch) = aac_config(asc.unwrap_or_default());
                    (format!("AAC {name} ({code})"), rate, ch)
                }
                Some((0x69 | 0x6B, _)) => (format!("MP3 ({code})"), None, None),
                Some((0xA5, _)) => (format!("AC-3 ({code})"), None, None),
                Some((0xA9, _)) => (format!("DTS ({code})"), None, None),
                Some((0xDD, _)) => (format!("Vorbis ({code})"), None, None),
                Some((oti, _)) => (format!("MPEG-4 audio, object type 0x{oti:02X} ({code})"), None, None),
                None => (format!("AAC ({code})"), None, None),
            }
        }
        b"Opus" => {
            let dops = Boxes::find(children, b"dOps");
            let ch = dops.and_then(|d| d.get(1)).map(|&c| c as u64);
            (format!("Opus ({code})"), Some(48000.0), ch)
        }
        b"fLaC" => (format!("FLAC ({code})"), None, None),
        b"alac" => (format!("Apple Lossless ({code})"), None, None),
        b"ac-3" => (format!("AC-3 ({code})"), None, None),
        b"ec-3" => (format!("E-AC-3 ({code})"), None, None),
        b"ac-4" => (format!("AC-4 ({code})"), None, None),
        b"dtsc" | b"dtsh" | b"dtsl" | b"dtse" => (format!("DTS ({code})"), None, None),
        b".mp3" | b"mp3 " => (format!("MP3 ({code})"), None, None),
        b"samr" => (format!("AMR-NB ({code})"), None, None),
        b"sawb" => (format!("AMR-WB ({code})"), None, None),
        b"ulaw" => (format!("µ-law ({code})"), None, None),
        b"alaw" => (format!("A-law ({code})"), None, None),
        b"lpcm" | b"sowt" | b"twos" | b"in24" | b"in32" | b"fl32" | b"fl64" | b"raw " | b"ipcm" | b"fpcm" => {
            (format!("PCM ({code})"), None, None)
        }
        _ => (code, None, None),
    }
}

/// MPEG-4 descriptor length: up to four bytes of 7 bits.
fn descriptor_len(c: &mut Cur<'_>) -> Option<usize> {
    let mut len = 0usize;
    for _ in 0..4 {
        let b = c.u8()?;
        len = (len << 7) | (b & 0x7F) as usize;
        if b & 0x80 == 0 {
            break;
        }
    }
    Some(len)
}

/// The object type of an ES descriptor's decoder config, and its decoder-specific info.
fn es_descriptor(data: &[u8]) -> Option<(u8, Option<&[u8]>)> {
    let mut c = Cur::new(data);
    if c.u8()? != 0x03 {
        return None;
    }
    descriptor_len(&mut c)?;
    c.skip(2)?;
    let flags = c.u8()?;
    if flags & 0x80 != 0 {
        c.skip(2)?;
    }
    if flags & 0x40 != 0 {
        let n = c.u8()? as usize;
        c.skip(n)?;
    }
    if flags & 0x20 != 0 {
        c.skip(2)?;
    }
    if c.u8()? != 0x04 {
        return None;
    }
    let len = descriptor_len(&mut c)?;
    let mut dc = Cur::new(c.take(len.min(c.remaining()))?);
    let oti = dc.u8()?;
    dc.skip(12)?;
    let specific =
        if dc.u8() == Some(0x05) { descriptor_len(&mut dc).and_then(|n| dc.take(n.min(dc.remaining()))) } else { None };
    Some((oti, specific))
}

/// Profile name, sample rate and channels from an AAC AudioSpecificConfig.
pub(super) fn aac_config(asc: &[u8]) -> (String, Option<f64>, Option<u64>) {
    let mut bits = Bits { data: asc, pos: 0 };
    let read_aot = |b: &mut Bits| -> Option<u32> {
        let aot = b.read(5)?;
        if aot == 31 { Some(32 + b.read(6)?) } else { Some(aot) }
    };
    let read_rate = |b: &mut Bits| -> Option<u32> {
        const RATES: [u32; 13] =
            [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
        let idx = b.read(4)?;
        if idx == 15 { b.read(24) } else { RATES.get(idx as usize).copied() }
    };
    let Some(mut aot) = read_aot(&mut bits) else { return ("".into(), None, None) };
    let mut rate = read_rate(&mut bits);
    let ch = bits.read(4).filter(|&c| c > 0).map(|c| if c == 7 { 8 } else { c as u64 });
    let mut sbr = false;
    if aot == 5 || aot == 29 {
        sbr = true;
        let ps = aot == 29;
        rate = read_rate(&mut bits);
        aot = read_aot(&mut bits).unwrap_or(2);
        if ps {
            return ("HE-AAC v2".into(), rate.map(f64::from), ch);
        }
    }
    let name = match aot {
        1 => "Main".to_string(),
        2 if sbr => "HE-AAC".to_string(),
        2 => "LC".to_string(),
        3 => "SSR".to_string(),
        4 => "LTP".to_string(),
        23 => "LD".to_string(),
        39 => "ELD".to_string(),
        42 => "USAC (xHE-AAC)".to_string(),
        n => format!("object type {n}"),
    };
    (name, rate.map(f64::from), ch)
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            v = (v << 1) | ((byte >> (7 - self.pos % 8)) & 1) as u32;
            self.pos += 1;
        }
        Some(v)
    }
}

/// iTunes-style item names.
fn ilst_name(key: &[u8]) -> Option<&'static str> {
    Some(match key {
        b"\xA9nam" => "Title",
        b"\xA9ART" => "Artist",
        b"aART" => "Album artist",
        b"\xA9alb" => "Album",
        b"\xA9day" => "Date",
        b"\xA9cmt" => "Comment",
        b"\xA9gen" | b"gnre" => "Genre",
        b"\xA9wrt" => "Composer",
        b"\xA9too" => "Encoder",
        b"\xA9enc" => "Encoded by",
        b"\xA9lyr" => "Lyrics",
        b"\xA9grp" => "Grouping",
        b"\xA9des" | b"desc" => "Description",
        b"ldes" => "Long description",
        b"\xA9cpy" | b"cprt" => "Copyright",
        b"\xA9pub" => "Publisher",
        b"\xA9swr" => "Software",
        b"\xA9mak" => "Make",
        b"\xA9mod" => "Model",
        b"\xA9xyz" => "Location",
        b"\xA9dir" => "Director",
        b"\xA9prd" => "Producer",
        b"\xA9inf" => "Information",
        b"\xA9key" | b"keyw" => "Keywords",
        b"\xA9st3" => "Subtitle",
        b"trkn" => "Track",
        b"disk" => "Disc",
        b"tmpo" => "Tempo",
        b"cpil" => "Compilation",
        b"pgap" => "Gapless",
        b"covr" => "Cover",
        b"tvsh" => "TV show",
        b"tven" => "TV episode id",
        b"tves" => "TV episode",
        b"tvsn" => "TV season",
        b"stik" => "Media kind",
        b"sonm" => "Sort title",
        b"soar" => "Sort artist",
        b"soal" => "Sort album",
        b"soaa" => "Sort album artist",
        b"soco" => "Sort composer",
        b"purd" => "Purchase date",
        b"rtng" => "Rating",
        _ => return None,
    })
}

fn key_name(key: &[u8]) -> String {
    ilst_name(key)
        .map(str::to_string)
        .unwrap_or_else(|| fourcc(&key.iter().map(|&b| if b == 0xA9 { b'@' } else { b }).collect::<Vec<_>>()))
}

/// Tags in `udta`: an iTunes `meta`/`ilst`, and QuickTime `©xxx` text boxes.
fn user_data_tags(udta: &[u8], attachments: &mut Vec<String>) -> Vec<(String, String)> {
    let mut tags = Vec::new();
    for b in Boxes::new(udta) {
        match &b.kind {
            b"meta" => tags.extend(meta_tags(b.body, attachments)),
            [0xA9, ..] => {
                // QuickTime international text: (length u16, language u16, text)*.
                let mut c = Cur::new(b.body);
                let looks_like_strings = c.clone().be16().is_some_and(|l| l as usize + 4 <= b.body.len());
                if looks_like_strings {
                    while let (Some(len), Some(_lang)) = (c.be16(), c.be16()) {
                        let Some(text) = c.take(len as usize) else { break };
                        tags.push((key_name(&b.kind), utf8(text)));
                    }
                } else {
                    tags.push((key_name(&b.kind), utf8(b.body)));
                }
            }
            _ => {}
        }
    }
    tags
}

/// `meta` box: a handler, optional `keys` (QuickTime metadata), and `ilst`.
fn meta_tags(meta: &[u8], attachments: &mut Vec<String>) -> Vec<(String, String)> {
    // ISO `meta` is a full box; QuickTime's is not. Children start with a size, then `hdlr`.
    let meta = if meta.get(4..8) == Some(b"hdlr") || meta.get(4..8) == Some(b"keys") {
        meta
    } else {
        meta.get(4..).unwrap_or_default()
    };
    let keys: Vec<String> = Boxes::find(meta, b"keys")
        .and_then(full)
        .map(|(_, mut c)| {
            let n = c.be32().unwrap_or(0).min(10_000);
            let mut keys = Vec::new();
            for _ in 0..n {
                let (Some(size), Some(_ns)) = (c.be32(), c.take(4)) else { break };
                let Some(k) = c.take((size as usize).saturating_sub(8)) else { break };
                keys.push(utf8(k));
            }
            keys
        })
        .unwrap_or_default();
    let mut tags = Vec::new();
    let Some(ilst) = Boxes::find(meta, b"ilst") else { return tags };
    for item in Boxes::new(ilst) {
        let mut name = if item.kind[0] == 0 && !keys.is_empty() {
            let idx = u32::from_be_bytes(item.kind) as usize;
            keys.get(idx.wrapping_sub(1)).cloned().unwrap_or_else(|| format!("key {idx}"))
        } else {
            key_name(&item.kind)
        };
        if &item.kind == b"----" {
            // Freeform: mean (reverse domain), name, data.
            let mean = Boxes::find(item.body, b"mean").and_then(|m| m.get(4..)).map(utf8).unwrap_or_default();
            let n = Boxes::find(item.body, b"name").and_then(|m| m.get(4..)).map(utf8).unwrap_or_default();
            name = if mean.is_empty() || mean == "com.apple.iTunes" { n } else { format!("{mean}:{n}") };
        }
        for data in Boxes::new(item.body).filter(|b| &b.kind == b"data") {
            let mut c = Cur::new(data.body);
            let (Some(ty), Some(_locale)) = (c.be32(), c.be32()) else { continue };
            let value = c.rest();
            let text = match (ty & 0xFFFFFF, &item.kind) {
                (_, b"trkn" | b"disk") if value.len() >= 6 => {
                    let (n, of) = (u16::from_be_bytes([value[2], value[3]]), u16::from_be_bytes([value[4], value[5]]));
                    if of > 0 { format!("{n}/{of}") } else { n.to_string() }
                }
                (_, b"gnre") if value.len() == 2 => {
                    let g = u16::from_be_bytes([value[0], value[1]]);
                    super::mpeg::genre(g.saturating_sub(1) as u8).map(str::to_string).unwrap_or_else(|| g.to_string())
                }
                (1, _) | (4, _) => utf8(value),
                (2 | 5, _) => {
                    let units: Vec<u16> = value.chunks_exact(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect();
                    String::from_utf16_lossy(&units)
                }
                (13 | 14 | 27, _) | (_, b"covr") => {
                    let mime = match ty {
                        13 => "image/jpeg",
                        14 => "image/png",
                        27 => "image/bmp",
                        _ => "",
                    };
                    attachments.push(super::picture(&name, mime, "", value));
                    continue;
                }
                (0 | 21 | 22, _) if value.len() <= 8 => {
                    let mut v: i64 = if ty == 21 && value.first().is_some_and(|b| b & 0x80 != 0) { -1 } else { 0 };
                    for &b in value {
                        v = (v << 8) | b as i64;
                    }
                    v.to_string()
                }
                (23, _) if value.len() == 4 => {
                    decimal(f32::from_be_bytes([value[0], value[1], value[2], value[3]]) as f64)
                }
                (24, _) if value.len() == 8 => decimal(f64::from_be_bytes(value.try_into().unwrap_or_default())),
                _ => format!("({} bytes, crc {:08x})", value.len(), crc32fast::hash(value)),
            };
            tags.push((name.clone(), text));
        }
    }
    tags
}

/// Nero chapter list (`udta/chpl`), as ffmpeg writes it for MP4.
fn nero_chapters(chpl: &[u8], total: Option<f64>) -> Vec<Chapter> {
    let Some((version, mut c)) = full(chpl) else { return Vec::new() };
    if version == 1 {
        c.skip(4);
    }
    let n = c.u8().unwrap_or(0);
    let mut chapters: Vec<Chapter> = Vec::new();
    for _ in 0..n {
        let (Some(start), Some(len)) = (c.be64(), c.u8()) else { break };
        let Some(title) = c.take(len as usize) else { break };
        let start = start as f64 / 1e7;
        if let Some(prev) = chapters.last_mut() {
            prev.end = Some(start);
        }
        chapters.push(Chapter { start, end: None, title: utf8(title), depth: 0 });
    }
    if let Some(last) = chapters.last_mut() {
        last.end = total;
    }
    chapters
}
