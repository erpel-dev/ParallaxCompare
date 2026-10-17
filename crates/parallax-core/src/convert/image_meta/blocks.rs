//! Metadata blocks that several image formats embed: XMP packets, IPTC-IIM
//! records (in Photoshop resources or a TIFF tag), and ICC colour profiles.

use std::fmt::Write as _;

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use super::super::media::{Cur, clean, fourcc, latin1, line, size, utf8};

/// XMP properties shown at most.
const MAX_PROPERTIES: usize = 2_000;

// ---- XMP ----

struct Frame {
    /// This element's part of the property path (`dc:title`, `[1]`, `[x-default]`), if any.
    part: Option<String>,
    /// Items seen so far, when this is an `rdf:Seq`, `rdf:Bag` or `rdf:Alt`.
    items: usize,
    text: String,
}

fn qname(e: &BytesStart<'_>) -> String {
    e.name().as_ref().to_string()
}

fn unescape(s: &str) -> String {
    quick_xml::escape::unescape(s).map(|c| c.into_owned()).unwrap_or_else(|_| s.to_string())
}

/// The property path of the stack plus one more part.
fn path(stack: &[Frame], last: Option<&str>) -> String {
    let mut p = String::new();
    for part in stack.iter().filter_map(|f| f.part.as_deref()).chain(last) {
        if !p.is_empty() && !part.starts_with('[') {
            p.push('/');
        }
        p.push_str(part);
    }
    p
}

/// Properties of an XMP packet as `prefix:Name` (structs `a/b`, arrays `a[1]`,
/// language alternatives `a[x-default]`) in document order.
pub(super) fn xmp_properties(packet: &[u8]) -> std::result::Result<Vec<(String, String)>, String> {
    let text = String::from_utf8_lossy(packet);
    let mut reader = Reader::from_str(&text);
    let mut stack: Vec<Frame> = Vec::new();
    let mut props = Vec::new();
    loop {
        let event = reader.read_event().map_err(|e| e.to_string())?;
        let (start, empty) = match &event {
            Event::Start(e) => (Some(e.clone()), false),
            Event::Empty(e) => (Some(e.clone()), true),
            _ => (None, false),
        };
        if let Some(e) = start {
            let name = qname(&e);
            let mut part = None;
            match name.as_str() {
                "x:xmpmeta" | "x:xapmeta" | "rdf:RDF" | "rdf:Description" | "rdf:Seq" | "rdf:Bag" | "rdf:Alt" => {}
                "rdf:li" => {
                    let parent = stack.last_mut();
                    let n = parent.map(|p| {
                        p.items += 1;
                        p.items
                    });
                    let lang =
                        e.attributes().flatten().find(|a| a.key.as_ref() == "xml:lang").map(|a| unescape(&a.value));
                    part = Some(match lang {
                        Some(l) => format!("[{l}]"),
                        None => format!("[{}]", n.unwrap_or(1)),
                    });
                }
                _ => part = Some(name.clone()),
            }
            // Attributes: properties in short form (on rdf:Description), struct
            // fields, or a resource reference.
            for a in e.attributes().flatten() {
                let key = a.key.as_ref().to_string();
                if key.starts_with("xmlns") || key == "xml:lang" || (key.starts_with("rdf:") && key != "rdf:resource") {
                    continue;
                }
                let base = path(&stack, part.as_deref());
                let p = match (key == "rdf:resource", base.is_empty()) {
                    (true, _) => base,
                    (false, true) => key,
                    (false, false) => format!("{base}/{key}"),
                };
                if props.len() < MAX_PROPERTIES {
                    props.push((p, unescape(&a.value)));
                }
            }
            stack.push(Frame { part, items: 0, text: String::new() });
            if !empty {
                continue;
            }
        }
        match event {
            Event::Text(t) => {
                if let Some(f) = stack.last_mut() {
                    f.text.push_str(&t.xml10_content());
                }
            }
            Event::CData(t) => {
                if let Some(f) = stack.last_mut() {
                    f.text.push_str(&t);
                }
            }
            Event::GeneralRef(r) => {
                if let Some(f) = stack.last_mut() {
                    let s = match r.resolve_char_ref() {
                        Ok(Some(c)) => c.to_string(),
                        _ => unescape(&format!("&{};", &*r)),
                    };
                    f.text.push_str(&s);
                }
            }
            Event::End(_) | Event::Empty(_) => {
                let Some(f) = stack.pop() else { continue };
                let value = f.text.trim();
                if !value.is_empty() && f.part.is_some() && props.len() < MAX_PROPERTIES {
                    props.push((path(&stack, f.part.as_deref()), value.to_string()));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(props)
}

pub(super) fn xmp(out: &mut String, packet: &[u8]) {
    match xmp_properties(packet) {
        Ok(mut props) => {
            // Sorted by property; the items of an array keep their order.
            let top = |p: &str| p.split(['/', '[']).next().unwrap_or("").to_string();
            props.sort_by_key(|(p, _)| top(p));
            let n = props.len();
            for (p, v) in props {
                line(out, &clean(&p), clean(&v));
            }
            if n >= MAX_PROPERTIES {
                let _ = writeln!(out, "… more properties not shown");
            }
        }
        Err(e) => {
            let _ = writeln!(out, "! XMP could not be parsed ({e}); {} of XML", size(packet.len() as u64));
        }
    }
}

/// XMP in a GIF application extension: the raw bytes up to the end of the packet.
pub(super) fn gif_xmp(raw: &[u8]) -> Vec<u8> {
    let end = raw
        .windows(13)
        .position(|w| w == b"<?xpacket end")
        .and_then(|i| raw[i..].windows(2).position(|w| w == b"?>").map(|j| i + j + 2))
        .or_else(|| raw.windows(3).position(|w| w == [0x01, 0xFF, 0xFE]))
        .unwrap_or(raw.len());
    raw[..end].to_vec()
}

// ---- IPTC ----

/// The IPTC-IIM block (resource 0x0404) of Photoshop image resources.
pub(super) fn photoshop_iptc(resources: &[u8]) -> Option<Vec<u8>> {
    let mut c = Cur::new(resources);
    while c.take(4)? == b"8BIM" {
        let id = c.be16()?;
        let name_len = c.u8()? as usize;
        c.skip(name_len + usize::from(name_len.is_multiple_of(2)))?;
        let len = c.be32()? as usize;
        let data = c.take(len)?;
        c.skip(len & 1);
        if id == 0x0404 {
            return Some(data.to_vec());
        }
    }
    None
}

fn iptc_name(record: u8, dataset: u8) -> String {
    let name = match (record, dataset) {
        (1, 90) => "Coded character set",
        (2, 0) => "Record version",
        (2, 3) => "Object type",
        (2, 4) => "Object attribute",
        (2, 5) => "Object name",
        (2, 7) => "Edit status",
        (2, 10) => "Urgency",
        (2, 12) => "Subject reference",
        (2, 15) => "Category",
        (2, 20) => "Supplemental category",
        (2, 22) => "Fixture identifier",
        (2, 25) => "Keywords",
        (2, 26) => "Content location code",
        (2, 27) => "Content location name",
        (2, 30) => "Release date",
        (2, 35) => "Release time",
        (2, 37) => "Expiration date",
        (2, 40) => "Special instructions",
        (2, 55) => "Date created",
        (2, 60) => "Time created",
        (2, 62) => "Digital creation date",
        (2, 63) => "Digital creation time",
        (2, 65) => "Originating program",
        (2, 70) => "Program version",
        (2, 75) => "Object cycle",
        (2, 80) => "By-line",
        (2, 85) => "By-line title",
        (2, 90) => "City",
        (2, 92) => "Sub-location",
        (2, 95) => "Province/State",
        (2, 100) => "Country code",
        (2, 101) => "Country",
        (2, 103) => "Original transmission reference",
        (2, 105) => "Headline",
        (2, 110) => "Credit",
        (2, 115) => "Source",
        (2, 116) => "Copyright notice",
        (2, 118) => "Contact",
        (2, 120) => "Caption/Abstract",
        (2, 122) => "Writer/Editor",
        (2, 135) => "Language identifier",
        _ => return format!("IPTC {record}:{dataset}"),
    };
    name.to_string()
}

pub(super) fn iptc(out: &mut String, iim: &[u8]) {
    let mut c = Cur::new(iim);
    let mut tags = Vec::new();
    while c.u8() == Some(0x1C) && tags.len() < MAX_PROPERTIES {
        let (Some(record), Some(dataset), Some(len)) = (c.u8(), c.u8(), c.be16()) else { break };
        if len & 0x8000 != 0 {
            break; // extended length: not used for text
        }
        let Some(value) = c.take(len as usize) else { break };
        let text = match (record, dataset) {
            (1, 90) if value == b"\x1B%G" => "UTF-8".to_string(),
            (2, 0) if value.len() == 2 => u16::from_be_bytes([value[0], value[1]]).to_string(),
            _ if value.iter().any(|&b| b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t')) => {
                format!("({}, crc {:08x})", size(value.len() as u64), crc32fast::hash(value))
            }
            _ => utf8(value),
        };
        tags.push((iptc_name(record, dataset), text));
    }
    tags.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in tags {
        line(out, &k, clean(&v));
    }
}

// ---- ICC ----

/// Text of a `desc`, `mluc` or `text` tag (the first, usually English, record).
fn icc_text(profile: &[u8], offset: usize, len: usize) -> Option<String> {
    let t = profile.get(offset..offset.checked_add(len)?)?;
    let mut c = Cur::new(t);
    let kind = c.take(4)?;
    c.skip(4)?;
    match kind {
        b"desc" => {
            let n = c.be32()? as usize;
            Some(latin1(c.take(n.min(c.remaining()))?).trim_end_matches('\0').to_string())
        }
        b"text" => Some(latin1(c.rest()).trim_end_matches('\0').to_string()),
        b"mluc" => {
            let n = c.be32()?;
            let rec = c.be32()? as usize;
            let mut chosen = None;
            for _ in 0..n.min(1000) {
                let r = c.take(rec.max(12))?;
                let lang = &r[..2];
                let (l, o) = (
                    u32::from_be_bytes(r[4..8].try_into().ok()?) as usize,
                    u32::from_be_bytes(r[8..12].try_into().ok()?) as usize,
                );
                if chosen.is_none() || lang == b"en" {
                    chosen = Some((o, l));
                }
            }
            let (o, l) = chosen?;
            let units: Vec<u16> =
                t.get(o..o.checked_add(l)?)?.chunks_exact(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect();
            Some(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
        }
        _ => None,
    }
}

pub(super) fn icc(out: &mut String, p: &[u8]) {
    if p.len() < 132 || &p[36..40] != b"acsp" {
        let _ = writeln!(out, "! not an ICC profile ({})", size(p.len() as u64));
        return;
    }
    let mut c = Cur::at(p, 128);
    let n = c.be32().unwrap_or(0);
    let mut tags = Vec::new();
    let mut texts = Vec::new();
    for _ in 0..n.min(1000) {
        let (Some(sig), Some(off), Some(len)) = (c.take(4), c.be32(), c.be32()) else { break };
        tags.push(fourcc(sig).trim_end().to_string());
        let label = match sig {
            b"desc" => "Description",
            b"cprt" => "Copyright",
            b"dmnd" => "Device manufacturer",
            b"dmdd" => "Device model",
            _ => continue,
        };
        if let Some(t) = icc_text(p, off as usize, len as usize) {
            texts.push((label, t));
        }
    }
    for (label, t) in texts {
        line(out, label, clean(&t));
    }
    let class = match &p[12..16] {
        b"scnr" => "input device".to_string(),
        b"mntr" => "display".to_string(),
        b"prtr" => "output device".to_string(),
        b"link" => "device link".to_string(),
        b"spac" => "color space".to_string(),
        b"abst" => "abstract".to_string(),
        b"nmcl" => "named color".to_string(),
        other => fourcc(other),
    };
    line(out, "Class", class);
    line(out, "Color space", fourcc(&p[16..20]).trim_end());
    line(out, "Connection space", fourcc(&p[20..24]).trim_end());
    line(out, "Version", format!("{}.{}.{}", p[8], p[9] >> 4, p[9] & 15));
    let field = |r: std::ops::Range<usize>| {
        let v = &p[r];
        (v != [0; 4]).then(|| fourcc(v).trim_end().to_string())
    };
    if let Some(cmm) = field(4..8) {
        line(out, "CMM", cmm);
    }
    if let Some(creator) = field(80..84) {
        line(out, "Creator", creator);
    }
    let d: Vec<u16> = p[24..36].chunks_exact(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect();
    if d[0] != 0 {
        line(out, "Created", format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", d[0], d[1], d[2], d[3], d[4], d[5]));
    }
    let intent = ["perceptual", "relative colorimetric", "saturation", "absolute colorimetric"];
    line(out, "Rendering intent", intent.get(p[67] as usize).copied().unwrap_or("unknown"));
    line(out, "Tags", tags.join(", "));
    let id = &p[84..100];
    let checksum = if id != [0; 16] {
        id.iter().map(|b| format!("{b:02x}")).collect::<String>() + " (profile id)"
    } else {
        format!("crc {:08x}", crc32fast::hash(p))
    };
    line(out, "Size", format!("{}, {checksum}", size(p.len() as u64)));
}
