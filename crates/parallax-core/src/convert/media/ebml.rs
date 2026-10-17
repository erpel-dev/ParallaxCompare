//! Matroska and WebM (EBML).
//!
//! EBML is a tree of elements, each an id and a size as variable-length
//! integers, then the body. A Matroska file is an EBML header and one Segment
//! whose children are the level-1 elements: SeekHead (an index of the others),
//! Info, Tracks, Chapters, Tags, Attachments, Cues, and the Clusters with the
//! frames. Muxers put the small elements before the first Cluster, except Tags,
//! Cues and sometimes Chapters, which are written at the end and listed in the
//! SeekHead. So the walk reads the level-1 elements up to the first Cluster and
//! then jumps to the ones the SeekHead points to, without touching any Cluster.

use std::collections::HashMap;

use super::{Chapter, Summary, Track, channels, decimal, size, utf8};
use crate::{Error, Result};

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const CHAPTERS: u32 = 0x1043_A770;
const TAGS: u32 = 0x1254_C367;
const ATTACHMENTS: u32 = 0x1941_A469;
const CLUSTER: u32 = 0x1F43_B675;
/// Level-1 elements read without a SeekHead, when the file has none.
const MAX_LEVEL1_WALK: usize = 4096;

/// One element: id and body.
#[derive(Debug, Clone, Copy)]
struct El<'a> {
    id: u32,
    body: &'a [u8],
}

/// A variable-length integer: value and length. Ids keep their marker bit.
fn vint(data: &[u8], pos: usize, keep_marker: bool) -> Option<(u64, usize)> {
    let first = *data.get(pos)?;
    if first == 0 {
        return None;
    }
    let len = first.leading_zeros() as usize + 1;
    let bytes = data.get(pos..pos + len)?;
    let mut v = if keep_marker { first as u64 } else { (first as u64) & (0xFF >> len) };
    for &b in &bytes[1..] {
        v = (v << 8) | b as u64;
    }
    Some((v, len))
}

/// The elements in a byte range. An element with unknown size, or one that runs
/// past the end, extends to the end of the range.
struct Elements<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Elements<'a> {
    fn new(data: &'a [u8]) -> Self {
        Elements { data, pos: 0 }
    }

    fn at(data: &'a [u8], pos: usize) -> Self {
        Elements { data, pos }
    }
}

impl<'a> Iterator for Elements<'a> {
    type Item = El<'a>;

    fn next(&mut self) -> Option<El<'a>> {
        let start = self.pos;
        let (id, id_len) = vint(self.data, start, true).filter(|(_, l)| *l <= 4)?;
        let (len, len_len) = vint(self.data, start + id_len, false)?;
        let body_start = start + id_len + len_len;
        let unknown = len == (1u64 << (7 * len_len)) - 1;
        let end = if unknown {
            self.data.len()
        } else {
            usize::try_from(len).ok().and_then(|l| body_start.checked_add(l)).unwrap_or(usize::MAX).min(self.data.len())
        };
        let body = self.data.get(body_start..end.max(body_start))?;
        self.pos = end;
        Some(El { id: id as u32, body })
    }
}

fn uint(b: &[u8]) -> u64 {
    b.iter().take(8).fold(0, |v, &x| (v << 8) | x as u64)
}

fn float(b: &[u8]) -> Option<f64> {
    match b.len() {
        4 => Some(f32::from_be_bytes(b.try_into().ok()?) as f64),
        8 => Some(f64::from_be_bytes(b.try_into().ok()?)),
        _ => None,
    }
}

fn text(b: &[u8]) -> String {
    utf8(b).trim_end_matches('\0').to_string()
}

fn child(data: &[u8], id: u32) -> Option<&[u8]> {
    Elements::new(data).find(|e| e.id == id).map(|e| e.body)
}

pub(super) fn summary(data: &[u8]) -> Result<Summary> {
    let mut top = Elements::new(data);
    let header = top.next().filter(|e| e.id == EBML).ok_or_else(|| Error::Unsupported("not a Matroska file".into()))?;
    let doc_type = child(header.body, 0x4282).map(text).unwrap_or_else(|| "matroska".into());
    let doc_version = child(header.body, 0x4287).map(uint);
    let segment =
        top.find(|e| e.id == SEGMENT).ok_or_else(|| Error::Unsupported("Matroska file without a segment".into()))?;
    let name = match doc_type.as_str() {
        "webm" => "WebM",
        "matroska" => "Matroska",
        _ => "EBML",
    };
    let mut s = Summary::new(name, data.len());
    let version = doc_version.map(|v| format!(", version {v}")).unwrap_or_default();
    s.info.push(("Document type", format!("{doc_type}{version}")));

    // Level 1: in order until the first Cluster, then the SeekHead's targets.
    let seg = segment.body;
    let mut found: HashMap<u32, El<'_>> = HashMap::new();
    let mut seeks: Vec<(u32, u64)> = Vec::new();
    for (n, e) in Elements::new(seg).enumerate() {
        if n > MAX_LEVEL1_WALK {
            break;
        }
        match e.id {
            CLUSTER if !seeks.is_empty() => break,
            CLUSTER => {}
            SEEK_HEAD => seeks.extend(seek_entries(e.body)),
            id => {
                found.entry(id).or_insert(e);
            }
        }
    }
    // A SeekHead may point to a second SeekHead (at the end of the file).
    let mut i = 0;
    while i < seeks.len() && i < 64 {
        let (id, pos) = seeks[i];
        i += 1;
        if found.contains_key(&id) || id == CLUSTER {
            continue;
        }
        let Ok(pos) = usize::try_from(pos) else { continue };
        let Some(e) = Elements::at(seg, pos).next().filter(|e| e.id == id) else { continue };
        if id == SEEK_HEAD {
            seeks.extend(seek_entries(e.body));
        } else {
            found.insert(id, e);
        }
    }

    let mut timescale = 1_000_000u64;
    if let Some(info) = found.get(&INFO) {
        for e in Elements::new(info.body) {
            match e.id {
                0x2AD7B1 => timescale = uint(e.body).max(1),
                0x7BA9 => s.info.push(("Title", super::clean(&text(e.body)))),
                0x4D80 => s.info.push(("Muxing app", super::clean(&text(e.body)))),
                0x5741 => s.info.push(("Writing app", super::clean(&text(e.body)))),
                _ => {}
            }
        }
        if let Some(d) = child(info.body, 0x4489).and_then(float) {
            s.duration = Some(d * timescale as f64 / 1e9);
        }
    }
    let mut track_uids = HashMap::new();
    if let Some(tracks) = found.get(&TRACKS) {
        for e in Elements::new(tracks.body).filter(|e| e.id == 0xAE) {
            let (t, uid) = track(e.body);
            if let Some(uid) = uid {
                track_uids.insert(uid, s.tracks.len());
            }
            s.tracks.push(t);
        }
    }
    if let Some(chapters) = found.get(&CHAPTERS) {
        let editions: Vec<_> = Elements::new(chapters.body).filter(|e| e.id == 0x45B9).collect();
        for (n, edition) in editions.iter().enumerate() {
            if editions.len() > 1 {
                s.chapters.push(Chapter { start: 0.0, end: None, title: format!("Edition {}", n + 1), depth: 0 });
            }
            chapter_atoms(edition.body, usize::from(editions.len() > 1), &mut s.chapters);
        }
    }
    if let Some(tags) = found.get(&TAGS) {
        let mut global = Vec::new();
        for tag in Elements::new(tags.body).filter(|e| e.id == 0x7373) {
            let targets = child(tag.body, 0x63C0).unwrap_or_default();
            let track_uid = child(targets, 0x63C5).map(uint);
            let target_type = child(targets, 0x63CA).map(text);
            let mut pairs = Vec::new();
            for st in Elements::new(tag.body).filter(|e| e.id == 0x67C8) {
                simple_tag(st.body, "", &mut pairs);
            }
            match track_uid.and_then(|u| track_uids.get(&u)) {
                Some(&i) => s.tracks[i].tags.extend(pairs),
                None if track_uid.is_some_and(|u| u != 0) => global
                    .extend(pairs.into_iter().map(|(k, v)| (format!("Track {} / {k}", track_uid.unwrap_or(0)), v))),
                None => match target_type.filter(|t| !t.is_empty()) {
                    Some(t) => global.extend(pairs.into_iter().map(|(k, v)| (format!("{t} / {k}"), v))),
                    None => global.extend(pairs),
                },
            }
        }
        s.tag_group("Tags", global);
    }
    if let Some(att) = found.get(&ATTACHMENTS) {
        for f in Elements::new(att.body).filter(|e| e.id == 0x61A7) {
            let name = child(f.body, 0x466E).map(text).unwrap_or_default();
            let mime = child(f.body, 0x4660).map(text).unwrap_or_default();
            let desc =
                child(f.body, 0x467E).map(text).map(|d| format!(" \"{}\"", super::clean(&d))).unwrap_or_default();
            let data = child(f.body, 0x465C).unwrap_or_default();
            s.attachments.push(format!(
                "{}{desc}: {mime}, {}, crc {:08x}",
                super::clean(&name),
                size(data.len() as u64),
                crc32fast::hash(data)
            ));
        }
    }
    if !found.contains_key(&TRACKS) {
        s.warnings.push("no track list (Tracks element) found".into());
    }
    Ok(s)
}

/// SeekHead entries: element id and position relative to the segment body.
fn seek_entries(body: &[u8]) -> Vec<(u32, u64)> {
    Elements::new(body)
        .filter(|e| e.id == 0x4DBB)
        .filter_map(|seek| {
            let id = child(seek.body, 0x53AB)?;
            let pos = child(seek.body, 0x53AC)?;
            Some((uint(id) as u32, uint(pos)))
        })
        .take(1024)
        .collect()
}

fn codec_name(id: &str, private: Option<&[u8]>) -> String {
    let p = private.unwrap_or_default();
    let name = match id {
        "V_MPEG4/ISO/AVC" => return format!("H.264 ({id}){}", super::bmff::avc_profile(p)),
        "V_MPEGH/ISO/HEVC" => return format!("H.265 ({id}){}", super::bmff::hevc_profile(p)),
        "V_AV1" => return format!("AV1 ({id}){}", super::bmff::av1_profile(p)),
        "A_AAC" => {
            let (profile, _, _) = super::bmff::aac_config(p);
            return format!("AAC {profile} ({id})").replace("AAC  (", "AAC (");
        }
        "V_VP8" => "VP8",
        "V_VP9" => "VP9",
        "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/AP" => "MPEG-4 Visual",
        "V_MPEG2" => "MPEG-2 video",
        "V_MPEG1" => "MPEG-1 video",
        "V_MJPEG" => "Motion JPEG",
        "V_THEORA" => "Theora",
        "V_PRORES" => "Apple ProRes",
        "V_FFV1" => "FFV1",
        "V_MS/VFW/FOURCC" => "Video for Windows",
        "V_UNCOMPRESSED" => "Uncompressed",
        "A_OPUS" => "Opus",
        "A_VORBIS" => "Vorbis",
        "A_FLAC" => "FLAC",
        "A_MPEG/L3" => "MP3",
        "A_MPEG/L2" => "MPEG audio layer II",
        "A_AC3" => "AC-3",
        "A_EAC3" => "E-AC-3",
        "A_TRUEHD" => "TrueHD",
        "A_DTS" => "DTS",
        "A_ALAC" => "Apple Lossless",
        "A_PCM/INT/LIT" | "A_PCM/INT/BIG" | "A_PCM/FLOAT/IEEE" => "PCM",
        "A_MS/ACM" => "ACM audio",
        "S_TEXT/UTF8" => "SubRip text",
        "S_TEXT/ASS" | "S_ASS" => "ASS subtitles",
        "S_TEXT/SSA" | "S_SSA" => "SSA subtitles",
        "S_TEXT/WEBVTT" => "WebVTT",
        "S_HDMV/PGS" => "PGS bitmap subtitles",
        "S_VOBSUB" => "VobSub bitmap subtitles",
        "S_DVBSUB" => "DVB subtitles",
        _ => return id.to_string(),
    };
    format!("{name} ({id})")
}

/// A TrackEntry, and its UID (for tags that target it).
fn track(body: &[u8]) -> (Track, Option<u64>) {
    let get = |id| child(body, id);
    let kind = match get(0x83).map(uint) {
        Some(1) => "Video",
        Some(2) => "Audio",
        Some(3) => "Complex",
        Some(0x10) => "Logo",
        Some(0x11) => "Subtitles",
        Some(0x12) => "Buttons",
        Some(0x20) => "Control",
        Some(0x21) => "Metadata",
        _ => "Unknown",
    };
    let mut t = Track::new(kind);
    t.number = get(0xD7).map(uint);
    let codec = get(0x86).map(text).unwrap_or_default();
    t.field("Codec", codec_name(&codec, get(0x63A2)));
    if let Some(n) = get(0x258688).map(text) {
        t.field("Codec name", super::clean(&n));
    }
    if let Some(v) = get(0xE0) {
        let (w, h) = (child(v, 0xB0).map(uint), child(v, 0xBA).map(uint));
        if let (Some(w), Some(h)) = (w, h) {
            t.field("Resolution", format!("{w} × {h}"));
            let (dw, dh) = (child(v, 0x54B0).map(uint), child(v, 0x54BA).map(uint));
            if let (Some(dw), Some(dh)) = (dw, dh)
                && (dw, dh) != (w, h)
            {
                let unit = match child(v, 0x54B2).map(uint) {
                    Some(1) => " cm",
                    Some(2) => " in",
                    Some(3) => " (aspect ratio)",
                    _ => "",
                };
                t.field("Display size", format!("{dw} × {dh}{unit}"));
            }
        }
        match child(v, 0x9A).map(uint) {
            Some(1) => t.field("Scan", "interlaced"),
            Some(2) => t.field("Scan", "progressive"),
            _ => {}
        }
    }
    if let Some(dd) = get(0x23E383).map(uint).filter(|&d| d > 0) {
        if kind == "Video" {
            t.field("Frame rate", format!("{} fps", decimal(1e9 / dd as f64)));
        } else {
            t.field("Frame duration", format!("{} ms", decimal(dd as f64 / 1e6)));
        }
    }
    if let Some(a) = get(0xE1) {
        let rate = child(a, 0xB5).and_then(float).unwrap_or(8000.0);
        t.field("Sample rate", format!("{} Hz", decimal(rate)));
        if let Some(out) = child(a, 0x78B5).and_then(float).filter(|&o| o != rate) {
            t.field("Output sample rate", format!("{} Hz", decimal(out)));
        }
        t.field("Channels", channels(child(a, 0x9F).map(uint).unwrap_or(1)));
        if let Some(bits) = child(a, 0x6264).map(uint) {
            t.field("Bits per sample", bits.to_string());
        }
    }
    let language = get(0x22B59D).map(text).or_else(|| get(0x22B59C).map(text)).unwrap_or_else(|| "eng".into());
    if language != "und" {
        t.field("Language", super::clean(&language));
    }
    if let Some(name) = get(0x536E).map(text) {
        t.field("Title", super::clean(&name));
    }
    if get(0x88).map(uint) == Some(0) {
        t.field("Default", "no");
    }
    if get(0x55AA).map(uint) == Some(1) {
        t.field("Forced", "yes");
    }
    if get(0xB9).map(uint) == Some(0) {
        t.field("Enabled", "no");
    }
    if get(0x6D80).is_some() {
        t.field("Content encoding", "compressed or encrypted");
    }
    (t, get(0x73C5).map(uint))
}

fn chapter_atoms(body: &[u8], depth: usize, out: &mut Vec<Chapter>) {
    for atom in Elements::new(body).filter(|e| e.id == 0xB6) {
        if out.len() > super::MAX_ITEMS || depth > 16 {
            return;
        }
        let start = child(atom.body, 0x91).map(uint).unwrap_or(0);
        let end = child(atom.body, 0x92).map(uint);
        let titles: Vec<String> = Elements::new(atom.body)
            .filter(|e| e.id == 0x80)
            .filter_map(|d| {
                let s = child(d.body, 0x85).map(text)?;
                let lang = child(d.body, 0x437C).map(text).filter(|l| l != "eng" && l != "und");
                Some(match lang {
                    Some(l) => format!("{s} [{l}]"),
                    None => s,
                })
            })
            .collect();
        let hidden = if child(atom.body, 0x98).map(uint) == Some(1) { " (hidden)" } else { "" };
        out.push(Chapter {
            start: start as f64 / 1e9,
            end: end.map(|e| e as f64 / 1e9),
            title: format!("{}{hidden}", titles.join(" / ")),
            depth,
        });
        chapter_atoms(atom.body, depth + 1, out);
    }
}

/// A SimpleTag and its nested tags as `PARENT/NAME` pairs.
fn simple_tag(body: &[u8], prefix: &str, out: &mut Vec<(String, String)>) {
    if out.len() > super::MAX_ITEMS || prefix.len() > 256 {
        return;
    }
    let name = child(body, 0x45A3).map(text).unwrap_or_default();
    let lang = child(body, 0x447A).map(text).or_else(|| child(body, 0x447B).map(text));
    let key = match lang.filter(|l| l != "und" && l != "eng") {
        Some(l) => format!("{prefix}{name} [{l}]"),
        None => format!("{prefix}{name}"),
    };
    if let Some(v) = child(body, 0x4487) {
        out.push((key.clone(), text(v)));
    } else if let Some(b) = child(body, 0x4485) {
        out.push((key.clone(), format!("({}, crc {:08x})", size(b.len() as u64), crc32fast::hash(b))));
    }
    for nested in Elements::new(body).filter(|e| e.id == 0x67C8) {
        simple_tag(nested.body, &format!("{key}/"), out);
    }
}
