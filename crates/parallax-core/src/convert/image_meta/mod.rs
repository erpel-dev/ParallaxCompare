//! Image metadata: the "Image metadata" view of raster images.
//!
//! The pixel view shows what an image looks like; this view shows what the file
//! says about it: format and dimensions, colour type and bit depth, density, and
//! the embedded metadata blocks: EXIF (camera, lens, exposure, date taken, GPS
//! position), XMP (as a list of properties, not the XML), IPTC, the ICC colour
//! profile, PNG text chunks. So a changed capture date, a moved GPS position, a
//! new orientation or stripped metadata show as the lines that changed.
//!
//! Each format's container is walked by hand (JPEG markers up to the first scan,
//! PNG/WebP chunks by their lengths, HEIF/AVIF boxes, GIF blocks), and only the
//! metadata blocks are read: no pixel is decoded, so a 100 MB TIFF costs about
//! as much as a small one. EXIF itself is decoded with `kamadak-exif`, which
//! knows the tag names and how to show their values ("1/250 s", "f/2.8").
//!
//! Volatile parts are left out or reduced: offsets of strips and thumbnails, and
//! binary blobs (maker notes) as a size and checksum. Tags are sorted by name
//! within their group (EXIF image, photo, GPS, thumbnail; XMP; IPTC).

mod blocks;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Cursor;

use exif::{Context, Field, In, Tag, Value};

use super::media::{Boxes, Cur, clean, fourcc, latin1, line, size};
use crate::{Error, Result};

/// Embedded blocks (ICC profiles, XMP packets) larger than this are not inflated or parsed.
const MAX_BLOCK: usize = 16 * 1024 * 1024;
/// A TIFF that kamadak-exif rejects in strict mode (more than 8 pages, a broken
/// entry) is parsed again leniently from a copy, up to this size.
const MAX_LENIENT_COPY: usize = 256 * 1024 * 1024;

/// What a file says about its image, collected from the format's container.
#[derive(Default)]
struct Meta {
    format: String,
    /// Facts about the image (dimensions, colour, density …), in display order.
    facts: Vec<(&'static str, String)>,
    /// EXIF as a TIFF structure ("II*\0" or "MM\0*" …).
    exif: Option<Vec<u8>>,
    xmp: Vec<Vec<u8>>,
    icc: Option<Vec<u8>>,
    /// IPTC-IIM records.
    iptc: Option<Vec<u8>>,
    /// Text chunks and comments: key and value.
    text: Vec<(String, String)>,
    warnings: Vec<String>,
}

impl Meta {
    fn fact(&mut self, label: &'static str, value: impl Into<String>) {
        self.facts.push((label, value.into()));
    }
}

/// The "Image metadata" view.
pub fn metadata(data: &[u8]) -> Result<String> {
    let at = |o: usize, m: &[u8]| data.get(o..o + m.len()) == Some(m);
    let mut tiff_fields = None;
    let meta = if at(0, b"\x89PNG\r\n\x1a\n") {
        png(data)
    } else if at(0, &[0xFF, 0xD8, 0xFF]) {
        jpeg(data)
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        webp(data)
    } else if at(0, b"GIF87a") || at(0, b"GIF89a") {
        gif(data)
    } else if super::media::is_heif(data) {
        heif(data)
    } else if at(0, b"II*\0") || at(0, b"MM\0*") {
        let (m, fields) = tiff(data);
        tiff_fields = Some(fields);
        m
    } else {
        other(data)?
    };
    Ok(render(meta, tiff_fields))
}

/// Width and height from the header (for pictures embedded in audio files).
pub(super) fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(Cursor::new(data)).with_guessed_format().ok()?.into_dimensions().ok()
}

fn render(meta: Meta, tiff_fields: Option<(Vec<Field>, bool)>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "=== Image ===");
    line(&mut out, "Format", &meta.format);
    for (label, value) in &meta.facts {
        line(&mut out, label, clean(value));
    }
    for w in &meta.warnings {
        let _ = writeln!(out, "! {w}");
    }
    let mut exif_fields = tiff_fields;
    if exif_fields.is_none()
        && let Some(e) = &meta.exif
    {
        match parse_exif(e) {
            Ok(f) => exif_fields = Some(f),
            Err(err) => {
                let _ = writeln!(out, "! EXIF could not be read: {err}");
            }
        }
    }
    let mut xmp = meta.xmp;
    let mut icc = meta.icc;
    let mut iptc = meta.iptc;
    if let Some((fields, le)) = &exif_fields {
        exif_sections(&mut out, fields, *le, &mut xmp, &mut icc, &mut iptc);
    }
    if !meta.text.is_empty() {
        let _ = writeln!(out, "\n=== Text ===");
        let mut text = meta.text;
        text.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in text {
            line(&mut out, &clean(&k), clean(&v));
        }
    }
    for (i, packet) in xmp.iter().enumerate() {
        let title = if xmp.len() > 1 { format!("XMP ({})", i + 1) } else { "XMP".to_string() };
        let _ = writeln!(out, "\n=== {title} ===");
        blocks::xmp(&mut out, packet);
    }
    if let Some(iptc) = iptc {
        let _ = writeln!(out, "\n=== IPTC ===");
        blocks::iptc(&mut out, &iptc);
    }
    if let Some(icc) = icc {
        let _ = writeln!(out, "\n=== ICC profile ===");
        blocks::icc(&mut out, &icc);
    }
    out
}

/// EXIF from a TIFF structure, strictly from the slice, or leniently from a copy.
fn parse_exif(tiff: &[u8]) -> std::result::Result<(Vec<Field>, bool), exif::Error> {
    match exif::parse_exif(tiff) {
        Ok(r) => Ok(r),
        Err(e) if tiff.len() > MAX_LENIENT_COPY => Err(e),
        Err(_) => {
            let exif = match exif::Reader::new().continue_on_error(true).read_raw(tiff.to_vec()) {
                Ok(exif) => exif,
                Err(exif::Error::PartialResult(p)) => p.into_inner().0,
                Err(e) => return Err(e),
            };
            Ok((exif.fields().cloned().collect(), exif.little_endian()))
        }
    }
}

// ---- EXIF ----

/// Tags not shown: offsets into the file, and blocks shown in their own section.
fn skipped(tag: Tag) -> bool {
    matches!(
        tag,
        Tag::StripOffsets
            | Tag::TileOffsets
            | Tag::JPEGInterchangeFormat
            | Tag::ExifIFDPointer
            | Tag::GPSInfoIFDPointer
            | Tag::InteropIFDPointer
    ) || (tag.context() == Context::Tiff && matches!(tag.number(), 0x02BC | 0x8773 | 0x83BB | 0x014A | 0x0111 | 0x0144))
}

/// Tags whose unit names another tag (`with_unit` would fall back to a default).
fn unit_from_other_tag(tag: Tag) -> Option<Tag> {
    Some(match tag {
        Tag::XResolution | Tag::YResolution => Tag::ResolutionUnit,
        Tag::FocalPlaneXResolution | Tag::FocalPlaneYResolution => Tag::FocalPlaneResolutionUnit,
        Tag::GPSLatitude
        | Tag::GPSLongitude
        | Tag::GPSAltitude
        | Tag::GPSSpeed
        | Tag::GPSTrack
        | Tag::GPSImgDirection
        | Tag::GPSDestLatitude
        | Tag::GPSDestLongitude
        | Tag::GPSDestBearing
        | Tag::GPSDestDistance => Tag::GPSVersionID, // their own Ref tags are shown beside them
        _ => return None,
    })
}

fn value_bytes(v: &Value, le: bool) -> Option<Vec<u8>> {
    Some(match v {
        Value::Byte(b) | Value::Undefined(b, _) => b.clone(),
        Value::Long(l) => l.iter().flat_map(|x| if le { x.to_le_bytes() } else { x.to_be_bytes() }).collect(),
        Value::Ascii(a) => a.concat(),
        _ => return None,
    })
}

fn tag_name(tag: Tag) -> String {
    let name = tag.to_string();
    if name.starts_with("Tag(") { format!("Tag 0x{:04X}", tag.number()) } else { name }
}

fn show_value(f: &Field, fields: &[Field]) -> String {
    match &f.value {
        Value::Undefined(b, _) | Value::Byte(b) if b.len() > 64 => {
            return format!("({}, crc {:08x})", size(b.len() as u64), crc32fast::hash(b));
        }
        Value::Short(v) if v.len() > 16 => return format!("({} values)", v.len()),
        Value::Long(v) if v.len() > 16 => return format!("({} values)", v.len()),
        _ => {}
    }
    // Text without the quotes kamadak-exif puts around it (dates it formats itself).
    if let Value::Ascii(a) = &f.value
        && a.len() == 1
        && f.display_value().to_string().starts_with('"')
    {
        return super::media::utf8(&a[0]);
    }
    if f.tag == Tag::Compression
        && let Some(c) = f.value.get_uint(0)
    {
        let name = match c {
            1 => "uncompressed",
            2 => "CCITT modified Huffman",
            3 => "CCITT Group 3",
            4 => "CCITT Group 4",
            5 => "LZW",
            6 | 7 => "JPEG",
            8 | 32946 => "Deflate",
            32773 => "PackBits",
            34712 => "JPEG 2000",
            34892 => "lossy JPEG (DNG)",
            50000 => "Zstandard",
            50001 => "WebP",
            _ => return format!("{c}"),
        };
        return format!("{name} ({c})");
    }
    match unit_from_other_tag(f.tag) {
        Some(Tag::GPSVersionID) if f.tag == Tag::GPSAltitude => format!("{} m", f.display_value()),
        Some(Tag::GPSVersionID) => f.display_value().to_string(),
        Some(unit) => {
            let u = fields.iter().find(|u| u.tag == unit && u.ifd_num == f.ifd_num);
            match u {
                Some(u) => format!("{} pixels per {}", f.display_value(), u.display_value()),
                None => format!("{} pixels per inch", f.display_value()),
            }
        }
        None => f.display_value().with_unit(()).to_string(),
    }
}

/// Decimal degrees of a GPS latitude or longitude (degrees, minutes, seconds).
fn degrees(fields: &[Field], tag: Tag, reference: Tag) -> Option<f64> {
    let f = fields.iter().find(|f| f.tag == tag)?;
    let Value::Rational(r) = &f.value else { return None };
    let part = |i: usize| r.get(i).filter(|x| x.denom != 0).map_or(0.0, |x| x.to_f64());
    let v = part(0) + part(1) / 60.0 + part(2) / 3600.0;
    let negative = fields.iter().find(|f| f.tag == reference).is_some_and(|f| {
        matches!(&f.value, Value::Ascii(a) if a.first().is_some_and(|s| s.first().is_some_and(|c| matches!(c, b'S' | b'W'))))
    });
    Some(if negative { -v } else { v })
}

fn exif_sections(
    out: &mut String,
    fields: &[Field],
    le: bool,
    xmp: &mut Vec<Vec<u8>>,
    icc: &mut Option<Vec<u8>>,
    iptc: &mut Option<Vec<u8>>,
) {
    // Blocks stored as TIFF tags (in TIFF files): XMP, ICC, IPTC.
    for f in fields.iter().filter(|f| f.ifd_num == In::PRIMARY && f.tag.context() == Context::Tiff) {
        match f.tag.number() {
            0x02BC => xmp.extend(value_bytes(&f.value, le)),
            0x8773 if icc.is_none() => *icc = value_bytes(&f.value, le),
            0x83BB if iptc.is_none() => *iptc = value_bytes(&f.value, le),
            _ => {}
        }
    }
    let mut groups: BTreeMap<(u8, u16), Vec<(String, String)>> = BTreeMap::new();
    for f in fields.iter().filter(|f| !skipped(f.tag)) {
        let group = match f.tag.context() {
            Context::Tiff => 0,
            Context::Exif => 1,
            Context::Gps => 2,
            Context::Interop => 3,
            _ => 4,
        };
        let name = if f.tag == Tag::StripByteCounts || f.tag == Tag::TileByteCounts {
            let total: u64 = f.value.iter_uint().map_or(0, |i| i.map(u64::from).sum());
            groups.entry((group, f.ifd_num.index())).or_default().push((
                tag_name(f.tag),
                format!("{} in {} parts", size(total), f.value.iter_uint().map_or(0, |i| i.count())),
            ));
            continue;
        } else {
            tag_name(f.tag)
        };
        groups.entry((group, f.ifd_num.index())).or_default().push((name, show_value(f, fields)));
    }
    if let (Some(lat), Some(lon)) = (
        degrees(fields, Tag::GPSLatitude, Tag::GPSLatitudeRef),
        degrees(fields, Tag::GPSLongitude, Tag::GPSLongitudeRef),
    ) {
        groups.entry((2, 0)).or_default().push(("Position".into(), format!("{lat:.6}, {lon:.6}")));
    }
    for ((group, ifd), mut tags) in groups {
        let title = match (group, ifd) {
            (0, 0) => "EXIF: Image".to_string(),
            (0, 1) => "EXIF: Thumbnail".to_string(),
            (0, n) => format!("EXIF: Image {}", n + 1),
            (1, 0) => "EXIF: Photo".to_string(),
            (2, 0) => "EXIF: GPS".to_string(),
            (3, 0) => "EXIF: Interoperability".to_string(),
            (g, n) => format!(
                "EXIF: {} of image {}",
                ["Image", "Photo", "GPS", "Interoperability", "Other"][g as usize],
                n + 1
            ),
        };
        let _ = writeln!(out, "\n=== {title} ===");
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in tags {
            line(out, &k, clean(&v));
        }
    }
}

// ---- Formats ----

fn png(data: &[u8]) -> Meta {
    let mut m = Meta { format: "PNG".into(), ..Meta::default() };
    let mut c = Cur::at(data, 8);
    let mut frames = None;
    let mut idat = false;
    while let (Some(len), Some(kind)) = (c.be32(), c.take(4)) {
        let Some(body) = c.take(len as usize) else {
            m.warnings.push(format!("chunk {} is cut off: the file is truncated", fourcc(kind)));
            break;
        };
        c.skip(4); // CRC
        let mut b = Cur::new(body);
        match kind {
            b"IHDR" => {
                let (w, h) = (b.be32().unwrap_or(0), b.be32().unwrap_or(0));
                let (depth, color) = (b.u8().unwrap_or(0), b.u8().unwrap_or(0));
                b.skip(2);
                let interlace = b.u8().unwrap_or(0);
                m.fact("Dimensions", format!("{w} × {h}"));
                let color = match color {
                    0 => "Grayscale",
                    2 => "RGB",
                    3 => "Indexed (palette)",
                    4 => "Grayscale with alpha",
                    6 => "RGB with alpha",
                    _ => "unknown",
                };
                m.fact("Color", color);
                m.fact("Bit depth", depth.to_string());
                if interlace == 1 {
                    m.fact("Interlaced", "Adam7");
                }
            }
            b"PLTE" => m.fact("Palette", format!("{} colors", body.len() / 3)),
            b"tRNS" => m.fact("Transparency", "tRNS chunk"),
            b"gAMA" => m.fact("Gamma", format!("{:.5}", b.be32().unwrap_or(0) as f64 / 100_000.0)),
            b"sRGB" => {
                let intent = ["perceptual", "relative colorimetric", "saturation", "absolute colorimetric"];
                m.fact("sRGB", intent.get(b.u8().unwrap_or(9) as usize).copied().unwrap_or("unknown intent"));
            }
            b"cHRM" => m.fact("Chromaticities", "cHRM chunk"),
            b"pHYs" => {
                let (x, y, unit) = (b.be32().unwrap_or(0), b.be32().unwrap_or(0), b.u8().unwrap_or(0));
                if unit == 1 {
                    let dpi = |v: u32| super::media::decimal((v as f64 * 0.0254 * 100.0).round() / 100.0);
                    m.fact("Density", format!("{} × {} dpi", dpi(x), dpi(y)));
                } else {
                    m.fact("Pixel aspect", format!("{x}:{y}"));
                }
            }
            b"tIME" => {
                let (y, mo, d, h, mi, s) = (b.be16(), b.u8(), b.u8(), b.u8(), b.u8(), b.u8());
                if let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s)) = (y, mo, d, h, mi, s) {
                    m.fact("Modified (tIME)", format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}"));
                }
            }
            b"acTL" => frames = Some((b.be32().unwrap_or(0), b.be32().unwrap_or(0))),
            b"iCCP" => {
                let (name, rest) = split_nul(body);
                m.fact("ICC profile name", latin1(name));
                m.icc = rest.get(1..).and_then(inflate);
            }
            b"eXIf" => m.exif = Some(strip_exif_header(body).to_vec()),
            b"tEXt" => {
                let (k, v) = split_nul(body);
                m.text.push((latin1(k), latin1(v)));
            }
            b"zTXt" => {
                let (k, v) = split_nul(body);
                let v = v
                    .get(1..)
                    .and_then(inflate)
                    .map(|t| latin1(&t))
                    .unwrap_or_else(|| "(unreadable compressed text)".into());
                m.text.push((format!("{} (compressed)", latin1(k)), v));
            }
            b"iTXt" => {
                let (k, rest) = split_nul(body);
                let (flag, rest) = (rest.first().copied().unwrap_or(0), rest.get(2..).unwrap_or_default());
                let (lang, rest) = split_nul(rest);
                let (translated, text) = split_nul(rest);
                let text = if flag == 1 { inflate(text).unwrap_or_default() } else { text.to_vec() };
                let key = latin1(k);
                if key == "XML:com.adobe.xmp" {
                    m.xmp.push(text);
                    continue;
                }
                let mut label = key;
                let (lang, translated) = (latin1(lang), String::from_utf8_lossy(translated).into_owned());
                if !lang.is_empty() || !translated.is_empty() {
                    let extra: Vec<_> = [lang, translated].into_iter().filter(|s| !s.is_empty()).collect();
                    label = format!("{label} [{}]", extra.join(", "));
                }
                m.text.push((label, String::from_utf8_lossy(&text).into_owned()));
            }
            b"IDAT" => idat = true,
            b"IEND" => break,
            _ => {}
        }
    }
    if let Some((n, plays)) = frames {
        let loops = if plays == 0 { "loops forever".to_string() } else { format!("plays {plays} times") };
        m.fact("Animation", format!("{n} frames, {loops}"));
        m.format = "PNG (animated, APNG)".into();
    }
    if !idat {
        m.warnings.push("no image data (IDAT)".into());
    }
    m
}

fn split_nul(b: &[u8]) -> (&[u8], &[u8]) {
    match b.iter().position(|&x| x == 0) {
        Some(i) => (&b[..i], &b[i + 1..]),
        None => (b, &[]),
    }
}

fn inflate(b: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(b).take(MAX_BLOCK as u64).read_to_end(&mut out).ok()?;
    Some(out)
}

fn strip_exif_header(b: &[u8]) -> &[u8] {
    b.strip_prefix(b"Exif\0\0").unwrap_or(b)
}

fn jpeg(data: &[u8]) -> Meta {
    let mut m = Meta { format: "JPEG".into(), ..Meta::default() };
    let mut c = Cur::at(data, 2);
    let mut icc_parts: Vec<(u8, &[u8])> = Vec::new();
    let mut adobe_transform = None;
    let mut jfif = false;
    let mut components = Vec::new();
    // Markers may be preceded by fill bytes.
    while let Some(b) = c.u8() {
        if b != 0xFF {
            m.warnings.push("garbage between JPEG segments".into());
            break;
        }
        let Some(mut marker) = c.u8() else { break };
        while marker == 0xFF {
            let Some(next) = c.u8() else { break };
            marker = next;
        }
        if matches!(marker, 0x01 | 0xD0..=0xD7) {
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            break; // end of image, or start of scan: the pixel data follows
        }
        let Some(len) = c.be16() else { break };
        let Some(body) = c.take((len as usize).saturating_sub(2)) else {
            m.warnings.push("a JPEG segment is cut off: the file is truncated".into());
            break;
        };
        match marker {
            0xC0..=0xCF if !matches!(marker, 0xC4 | 0xC8 | 0xCC) => {
                let mut b = Cur::new(body);
                let precision = b.u8().unwrap_or(0);
                let (h, w) = (b.be16().unwrap_or(0), b.be16().unwrap_or(0));
                let n = b.u8().unwrap_or(0);
                for _ in 0..n {
                    if let (Some(id), Some(s), Some(_)) = (b.u8(), b.u8(), b.u8()) {
                        components.push((id, s >> 4, s & 15));
                    }
                }
                let process = match marker {
                    0xC0 => "baseline",
                    0xC1 => "extended sequential",
                    0xC2 => "progressive",
                    0xC3 => "lossless",
                    0xC9 => "extended sequential, arithmetic coding",
                    0xCA => "progressive, arithmetic coding",
                    0xCB => "lossless, arithmetic coding",
                    _ => "hierarchical",
                };
                m.format = format!("JPEG ({process})");
                m.fact("Dimensions", format!("{w} × {h}"));
                m.fact("Bit depth", precision.to_string());
            }
            0xE0 if body.starts_with(b"JFIF\0") => {
                jfif = true;
                let mut b = Cur::at(body, 5);
                let (major, minor) = (b.u8().unwrap_or(0), b.u8().unwrap_or(0));
                let unit = b.u8().unwrap_or(0);
                let (x, y) = (b.be16().unwrap_or(0), b.be16().unwrap_or(0));
                m.fact("JFIF", format!("version {major}.{minor:02}"));
                match unit {
                    1 => m.fact("Density", format!("{x} × {y} dpi")),
                    2 => m.fact("Density", format!("{x} × {y} dots per cm")),
                    _ if x != y => m.fact("Pixel aspect", format!("{x}:{y}")),
                    _ => {}
                }
            }
            0xE1 if body.starts_with(b"Exif\0") => {
                if m.exif.is_none() {
                    m.exif = Some(body.get(6..).unwrap_or_default().to_vec());
                }
            }
            0xE1 if body.starts_with(b"http://ns.adobe.com/xap/1.0/\0") => m.xmp.push(body[29..].to_vec()),
            0xE1 if body.starts_with(b"http://ns.adobe.com/xmp/extension/\0") => {
                if !m.facts.iter().any(|(l, _)| *l == "Extended XMP") {
                    m.fact("Extended XMP", "present (not shown)");
                }
            }
            0xE2 if body.starts_with(b"ICC_PROFILE\0") && body.len() > 14 => icc_parts.push((body[12], &body[14..])),
            0xED if body.starts_with(b"Photoshop 3.0\0") => m.iptc = blocks::photoshop_iptc(&body[14..]),
            0xEE if body.starts_with(b"Adobe") => adobe_transform = body.get(11).copied(),
            0xFE => m.text.push(("Comment".into(), super::media::utf8(body))),
            _ => {}
        }
    }
    if !components.is_empty() {
        let ids: Vec<u8> = components.iter().map(|c| c.0).collect();
        let color = match (components.len(), adobe_transform) {
            (1, _) => "Grayscale",
            (3, Some(0)) => "RGB",
            (3, _) if !jfif && ids == b"RGB" => "RGB",
            (3, _) => "YCbCr",
            (4, Some(2)) => "YCCK",
            (4, _) => "CMYK",
            _ => "unknown",
        };
        m.fact("Color", format!("{color}, {} components", components.len()));
        if components.len() >= 3 {
            let (h0, v0) = (components[0].1, components[0].2);
            let same = components[1..].iter().all(|c| c.1 == 1 && c.2 == 1);
            let sub = match (h0, v0) {
                (1, 1) => Some("4:4:4"),
                (2, 1) => Some("4:2:2"),
                (2, 2) => Some("4:2:0"),
                (4, 1) => Some("4:1:1"),
                (1, 2) => Some("4:4:0"),
                _ => None,
            };
            if let (true, Some(sub)) = (same, sub) {
                m.fact("Chroma subsampling", sub);
            }
        }
    }
    if !icc_parts.is_empty() {
        icc_parts.sort_by_key(|p| p.0);
        m.icc = Some(icc_parts.iter().flat_map(|p| p.1.iter().copied()).collect());
    }
    m
}

fn webp(data: &[u8]) -> Meta {
    let mut m = Meta { format: "WebP".into(), ..Meta::default() };
    let mut c = Cur::at(data, 12);
    let mut frames = 0;
    let mut chunks = 0;
    while let (Some(id), Some(len)) = (c.take(4), c.le32()) {
        chunks += 1;
        let Some(body) = c.take(len as usize).or_else(|| Some(c.rest())) else { break };
        c.skip(len as usize & 1);
        let mut b = Cur::new(body);
        match id {
            b"VP8X" => {
                let flags = b.u8().unwrap_or(0);
                b.skip(3);
                let (w, h) = (b.take(3), b.take(3));
                if let (Some(w), Some(h)) = (w, h) {
                    let v = |x: &[u8]| u32::from_le_bytes([x[0], x[1], x[2], 0]) + 1;
                    m.fact("Dimensions", format!("{} × {}", v(w), v(h)));
                }
                if flags & 0x10 != 0 {
                    m.fact("Alpha", "yes");
                }
                m.format = "WebP (extended)".into();
            }
            b"VP8 " => {
                if !m.facts.iter().any(|f| f.0 == "Dimensions") && body.get(3..6) == Some(&[0x9D, 0x01, 0x2A]) {
                    b.skip(6);
                    let (w, h) = (b.le16().unwrap_or(0) & 0x3FFF, b.le16().unwrap_or(0) & 0x3FFF);
                    m.fact("Dimensions", format!("{w} × {h}"));
                }
                m.fact("Compression", "lossy (VP8)");
            }
            b"VP8L" => {
                if !m.facts.iter().any(|f| f.0 == "Dimensions") && body.first() == Some(&0x2F) {
                    b.skip(1);
                    let bits = b.le32().unwrap_or(0);
                    m.fact("Dimensions", format!("{} × {}", (bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1));
                }
                m.fact("Compression", "lossless (VP8L)");
            }
            b"ALPH" => {
                if !m.facts.iter().any(|f| f.0 == "Alpha") {
                    m.fact("Alpha", "yes");
                }
            }
            b"ANIM" => {
                b.skip(4);
                let loops = b.le16().unwrap_or(0);
                m.fact("Loops", if loops == 0 { "forever".to_string() } else { loops.to_string() });
            }
            b"ANMF" => frames += 1,
            b"ICCP" => m.icc = Some(body.to_vec()),
            b"EXIF" => m.exif = Some(strip_exif_header(body).to_vec()),
            b"XMP " => m.xmp.push(body.to_vec()),
            _ => {}
        }
        if chunks > 100_000 {
            break;
        }
    }
    if frames > 0 {
        m.fact("Animation", format!("{frames} frames"));
    }
    m
}

fn gif(data: &[u8]) -> Meta {
    let mut m = Meta { format: latin1(&data[..6]), ..Meta::default() };
    let mut c = Cur::at(data, 6);
    let (w, h) = (c.le16().unwrap_or(0), c.le16().unwrap_or(0));
    let flags = c.u8().unwrap_or(0);
    c.skip(2);
    m.fact("Dimensions", format!("{w} × {h}"));
    if flags & 0x80 != 0 {
        m.fact("Global palette", format!("{} colors", 2u32 << (flags & 7)));
        c.skip(3 * (2usize << (flags & 7)));
    }
    let mut frames = 0u32;
    let mut loops = None;
    // Data sub-blocks: length byte, data, until a zero length.
    let sub_blocks = |c: &mut Cur<'_>, keep: bool| -> Option<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let n = c.u8()? as usize;
            if n == 0 {
                return Some(out);
            }
            let b = c.take(n)?;
            if keep && out.len() < MAX_BLOCK {
                out.extend_from_slice(b);
            }
        }
    };
    loop {
        match c.u8() {
            Some(0x21) => {
                let label = c.u8().unwrap_or(0);
                if label == 0xFF {
                    let Some(n) = c.u8() else { break };
                    let app = c.take(n as usize).unwrap_or_default().to_vec();
                    let start = c.pos;
                    let Some(body) = sub_blocks(&mut c, true) else { break };
                    if app.starts_with(b"NETSCAPE") && body.len() >= 3 {
                        loops = Some(u16::from_le_bytes([body[1], body[2]]));
                    } else if app.starts_with(b"XMP Data") {
                        // XMP is stored raw: its bytes double as sub-block lengths,
                        // and a "magic trailer" ends the walk.
                        m.xmp.push(blocks::gif_xmp(&data[start..c.pos]));
                    }
                } else if label == 0xFE {
                    let Some(body) = sub_blocks(&mut c, true) else { break };
                    m.text.push(("Comment".into(), latin1(&body)));
                } else if sub_blocks(&mut c, false).is_none() {
                    break;
                }
            }
            Some(0x2C) => {
                frames += 1;
                c.skip(8);
                let f = c.u8().unwrap_or(0);
                if f & 0x80 != 0 {
                    c.skip(3 * (2usize << (f & 7)));
                }
                c.skip(1); // LZW minimum code size
                if sub_blocks(&mut c, false).is_none() {
                    m.warnings.push("the image data is cut off: the file is truncated".into());
                    break;
                }
            }
            _ => break,
        }
    }
    if frames > 1 {
        let loops = match loops {
            Some(0) => ", loops forever".to_string(),
            Some(n) => format!(", loops {n} times"),
            None => String::new(),
        };
        m.fact("Animation", format!("{frames} frames{loops}"));
    }
    m
}

fn tiff(data: &[u8]) -> (Meta, (Vec<Field>, bool)) {
    let order = if data.starts_with(b"II") { "little-endian" } else { "big-endian" };
    let mut m = Meta { format: format!("TIFF ({order})"), ..Meta::default() };
    let fields = match parse_exif(data) {
        Ok(f) => f,
        Err(e) => {
            m.warnings.push(format!("the TIFF structure could not be read: {e}"));
            (Vec::new(), true)
        }
    };
    let get = |tag: Tag| fields.0.iter().find(|f| f.tag == tag && f.ifd_num == In::PRIMARY);
    if let (Some(w), Some(h)) = (get(Tag::ImageWidth), get(Tag::ImageLength)) {
        m.fact("Dimensions", format!("{} × {}", w.display_value(), h.display_value()));
    }
    let pages = fields.0.iter().map(|f| f.ifd_num.index()).max().map_or(0, |n| n + 1);
    if pages > 1 {
        m.fact("Images (IFDs)", pages.to_string());
    }
    (m, fields)
}

/// HEIF and AVIF: the primary item's properties and the EXIF/XMP items.
fn heif(data: &[u8]) -> Meta {
    let brand = data.get(8..12).map(fourcc).unwrap_or_default();
    let format = if brand.starts_with("avi") { format!("AVIF ({brand})") } else { format!("HEIF ({brand})") };
    let mut m = Meta { format, ..Meta::default() };
    let Some(meta) = Boxes::find(data, b"meta").and_then(|b| b.get(4..)) else {
        m.warnings.push("no meta box".into());
        return m;
    };
    let full = |b: &[u8]| -> Option<(u8, usize)> { Some((*b.first()?, 4)) };
    let primary = Boxes::find(meta, b"pitm").and_then(|b| {
        let (v, o) = full(b)?;
        let mut c = Cur::at(b, o);
        if v == 0 { c.be16().map(u32::from) } else { c.be32() }
    });
    // Items: id → (type, content type).
    let mut items: Vec<(u32, [u8; 4], String)> = Vec::new();
    if let Some(iinf) = Boxes::find(meta, b"iinf")
        && let Some((v, o)) = full(iinf)
    {
        let skip = if v == 0 { 2 } else { 4 };
        for infe in Boxes::new(iinf.get(o + skip..).unwrap_or_default()).filter(|b| &b.kind == b"infe").take(100_000) {
            let Some((iv, o)) = full(infe.body) else { continue };
            if iv < 2 {
                continue;
            }
            let mut c = Cur::at(infe.body, o);
            let id = if iv == 2 { c.be16().map(u32::from) } else { c.be32() };
            c.skip(2);
            let (Some(id), Some(kind)) = (id, c.take(4)) else { continue };
            let (_name, rest) = split_nul(c.rest());
            let content_type = if kind == b"mime" { latin1(split_nul(rest).0) } else { String::new() };
            items.push((id, kind.try_into().unwrap_or_default(), content_type));
        }
    }
    let primary_kind = items.iter().find(|i| Some(i.0) == primary).map(|i| i.1);
    if let Some(k) = primary_kind {
        let codec = match &k {
            b"hvc1" => "HEVC",
            b"av01" => "AV1",
            b"grid" => "grid of tiles",
            b"jpeg" => "JPEG",
            b"avc1" => "H.264",
            b"unci" => "uncompressed",
            _ => "",
        };
        m.fact("Primary image", format!("{codec} ({})", fourcc(&k)).replace(" ()", ""));
    }
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (_, k, _) in &items {
        *counts.entry(fourcc(k).trim_end().to_string()).or_default() += 1;
    }
    m.fact("Items", counts.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", "));
    // Properties of the primary item.
    if let Some(iprp) = Boxes::find(meta, b"iprp") {
        let props: Vec<_> =
            Boxes::find(iprp, b"ipco").map(|b| Boxes::new(b).take(100_000).collect()).unwrap_or_default();
        let assoc =
            Boxes::find(iprp, b"ipma").and_then(|b| primary.and_then(|p| associations(b, p))).unwrap_or_default();
        for idx in assoc {
            let Some(p) = props.get(idx.wrapping_sub(1)) else { continue };
            let mut c = Cur::new(p.body);
            match &p.kind {
                b"ispe" => {
                    c.skip(4);
                    if let (Some(w), Some(h)) = (c.be32(), c.be32()) {
                        m.fact("Dimensions", format!("{w} × {h}"));
                    }
                }
                b"pixi" => {
                    c.skip(4);
                    let n = c.u8().unwrap_or(0) as usize;
                    let bits: Vec<String> = c.take(n).unwrap_or_default().iter().map(u8::to_string).collect();
                    m.fact("Bit depth", bits.join(", "));
                }
                b"colr" => match c.take(4) {
                    Some(b"nclx") => {
                        let (p, t, mx) = (c.be16().unwrap_or(0), c.be16().unwrap_or(0), c.be16().unwrap_or(0));
                        let full_range = c.u8().unwrap_or(0) >> 7 == 1;
                        let range = if full_range { "full range" } else { "limited range" };
                        m.fact("Color (nclx)", format!("primaries {p}, transfer {t}, matrix {mx}, {range}"));
                    }
                    Some(b"prof" | b"rICC") => m.icc = Some(c.rest().to_vec()),
                    _ => {}
                },
                b"hvcC" => m.fact("Codec", format!("HEVC{}", super::media::hevc_profile(p.body))),
                b"av1C" => m.fact("Codec", format!("AV1{}", super::media::av1_profile(p.body))),
                b"irot" => m.fact("Rotation", format!("{}° counter-clockwise", (c.u8().unwrap_or(0) & 3) as u32 * 90)),
                b"imir" => {
                    m.fact("Mirror", if c.u8().unwrap_or(0) & 1 == 0 { "vertical axis" } else { "horizontal axis" })
                }
                b"auxC" => {}
                _ => {}
            }
        }
    }
    let locations = Boxes::find(meta, b"iloc").map(item_locations).unwrap_or_default();
    let idat = Boxes::find(meta, b"idat").unwrap_or_default();
    for (id, kind, content_type) in &items {
        let is_exif = kind == b"Exif";
        let is_xmp = kind == b"mime" && content_type.contains("rdf+xml");
        if !is_exif && !is_xmp {
            continue;
        }
        let Some(bytes) = locations.get(id).and_then(|l| item_data(data, idat, l)) else { continue };
        if is_exif {
            // A 4-byte offset to the TIFF header, usually 6 for "Exif\0\0".
            let off = bytes.get(..4).map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
            if let Some(tiff) = bytes.get(4 + off..) {
                m.exif = Some(tiff.to_vec());
            }
        } else {
            m.xmp.push(bytes);
        }
    }
    m
}

/// Property indexes (1-based) associated with an item in an `ipma` box.
fn associations(ipma: &[u8], item: u32) -> Option<Vec<usize>> {
    let (version, flags) = (*ipma.first()?, *ipma.get(3)?);
    let mut c = Cur::at(ipma, 4);
    let n = c.be32()?;
    for _ in 0..n.min(1_000_000) {
        let id = if version < 1 { c.be16()? as u32 } else { c.be32()? };
        let count = c.u8()?;
        let mut v = Vec::new();
        for _ in 0..count {
            v.push(if flags & 1 != 0 { (c.be16()? & 0x7FFF) as usize } else { (c.u8()? & 0x7F) as usize });
        }
        if id == item {
            return Some(v);
        }
    }
    None
}

/// Item locations from `iloc`: construction method and extents (offset, length).
fn item_locations(iloc: &[u8]) -> BTreeMap<u32, (u16, Vec<(u64, u64)>)> {
    let mut out = BTreeMap::new();
    let mut parse = || -> Option<()> {
        let version = *iloc.first()?;
        let mut c = Cur::at(iloc, 4);
        let sizes = c.u8()?;
        let (offset_size, length_size) = (sizes >> 4, sizes & 15);
        let sizes = c.u8()?;
        let (base_size, index_size) = (sizes >> 4, if version > 0 { sizes & 15 } else { 0 });
        let n = if version < 2 { c.be16()? as u32 } else { c.be32()? };
        let read = |c: &mut Cur<'_>, size: u8| -> Option<u64> {
            match size {
                0 => Some(0),
                4 => c.be32().map(u64::from),
                8 => c.be64(),
                _ => None,
            }
        };
        for _ in 0..n.min(100_000) {
            let id = if version < 2 { c.be16()? as u32 } else { c.be32()? };
            let method = if version > 0 { c.be16()? & 15 } else { 0 };
            c.skip(2)?;
            let base = read(&mut c, base_size)?;
            let extents = c.be16()?;
            let mut v = Vec::new();
            for _ in 0..extents {
                if index_size > 0 {
                    read(&mut c, index_size)?;
                }
                let off = read(&mut c, offset_size)?;
                let len = read(&mut c, length_size)?;
                v.push((base.saturating_add(off), len));
            }
            out.insert(id, (method, v));
        }
        Some(())
    };
    parse();
    out
}

fn item_data(file: &[u8], idat: &[u8], loc: &(u16, Vec<(u64, u64)>)) -> Option<Vec<u8>> {
    let source = match loc.0 {
        0 => file,
        1 => idat,
        _ => return None,
    };
    let mut out = Vec::new();
    for &(off, len) in &loc.1 {
        let off = usize::try_from(off).ok()?;
        let len = if len == 0 { source.len().saturating_sub(off) } else { usize::try_from(len).ok()? };
        if out.len() + len > MAX_BLOCK {
            return None;
        }
        out.extend_from_slice(source.get(off..off.checked_add(len)?)?);
    }
    Some(out)
}

/// Other formats the image crate knows (BMP, ICO, TGA …): format and dimensions from the header.
fn other(data: &[u8]) -> Result<Meta> {
    let format = image::guess_format(data).map_err(|_| Error::Unsupported("not a known image format".into()))?;
    let mut m = Meta { format: format!("{format:?}").to_uppercase(), ..Meta::default() };
    if let Some((w, h)) = dimensions(data) {
        m.fact("Dimensions", format!("{w} × {h}"));
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/image-meta/", $name))
        };
    }

    fn has(text: &str, expected: &[&str]) {
        for e in expected {
            assert!(text.contains(e), "missing {e:?} in\n{text}");
        }
    }

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn jpeg_with_exif_xmp_iptc_icc() {
        let v = metadata(fixture!("photo-v1.jpg")).unwrap();
        has(
            &v,
            &[
                "Format:            JPEG (baseline)",
                "Dimensions:        64 × 48",
                "Color:             YCbCr, 3 components",
                "Chroma subsampling: 4:2:0",
                "=== EXIF: Image ===\nArtist:            Jane Doe",
                "Model:             Canon EOS R6",
                "DateTimeOriginal:  2024-05-17 14:03:22",
                "ExposureTime:      1/250 s",
                "FNumber:           f/2.8",
                "LensModel:         RF50mm F1.8 STM",
                "MakerNote:         (512 bytes, crc",
                "GPSAltitude:       35 m",
                "Position:          48.858222, 2.294500",
                "dc:subject[2]:     tower",
                "dc:title[x-default]: Eiffel tower at noon",
                "xmp:Rating:        4",
                "Keywords:          paris\nKeywords:          tower",
                "Description:       sRGB built-in",
            ],
        );
    }

    #[test]
    fn edits_show_as_their_lines() {
        let v1 = metadata(fixture!("photo-v1.jpg")).unwrap();
        let v2 = metadata(fixture!("photo-v2.jpg")).unwrap();
        let mut changed: Vec<_> = lines_only_in(&v2, &v1).iter().map(|l| l.split(':').next().unwrap()).collect();
        changed.sort();
        assert_eq!(changed, ["DateTimeDigitized", "DateTimeOriginal", "GPSLatitude", "GPSLongitude", "Position"]);
        let rotated = metadata(fixture!("photo-rotated.jpg")).unwrap();
        assert_eq!(lines_only_in(&rotated, &v1), ["Orientation:       row 0 at right and column 0 at top"]);
        let stripped = metadata(fixture!("photo-stripped.jpg")).unwrap();
        assert!(!stripped.contains("EXIF") && !stripped.contains("XMP") && !stripped.contains("ICC"), "{stripped}");
        has(&stripped, &["Dimensions:        64 × 48"]);
    }

    #[test]
    fn png_tiff_webp_avif() {
        has(
            &metadata(fixture!("image.png")).unwrap(),
            &[
                "Color:             RGB\nBit depth:         8",
                "Gamma:             0.45455",
                "Density:           150.01 × 150.01 dpi",
                "ICC profile name:  ICC Profile",
                "Comment [de, Kommentar]: Grüße aus Köln",
                "Description (compressed): A line\\nand a second line",
                "Model:             Canon EOS R6",
                "dc:creator[1]:     Jane Doe",
            ],
        );
        let v2 = metadata(fixture!("image-v2.png")).unwrap();
        has(&v2, &["Title:             Test chart, revised"]);
        has(&metadata(fixture!("gray-alpha.png")).unwrap(), &["Color:             Grayscale with alpha"]);
        has(
            &metadata(fixture!("image.tif")).unwrap(),
            &[
                "Format:            TIFF (little-endian)",
                "Compression:       LZW (5)",
                "Model:             Nikon Z 6",
                "xmp:Rating:",
            ],
        );
        has(
            &metadata(fixture!("image.webp")).unwrap(),
            &[
                "Format:            WebP (extended)",
                "Compression:       lossy (VP8)",
                "FNumber:           f/2.8",
                "Description:       sRGB",
            ],
        );
        has(
            &metadata(fixture!("image.avif")).unwrap(),
            &[
                "Format:            AVIF (avif)",
                "Dimensions:        64 × 48",
                "Codec:             AV1, Main profile",
                "LensModel:",
                "xmp:Rating:",
                "sRGB built-in",
            ],
        );
    }

    #[test]
    fn views_and_similar_check() {
        use crate::convert::{View, content_equal, views_for};
        assert_eq!(views_for("a.jpg", fixture!("photo-v1.jpg")), [View::Image, View::ImageMetadata, View::Hex]);
        assert_eq!(views_for("a.avif", fixture!("image.avif")), [View::ImageMetadata, View::Hex]);
        // Equal pixels with other metadata stay ≈ similar, as before.
        assert!(content_equal("a.jpg", fixture!("photo-v1.jpg"), fixture!("photo-v2.jpg")).is_some());
    }

    #[test]
    fn truncated_and_corrupt_files_do_not_panic() {
        for data in [
            &fixture!("photo-v1.jpg")[..],
            fixture!("image.png"),
            fixture!("image.tif"),
            fixture!("image.webp"),
            fixture!("image.avif"),
        ] {
            for len in 0..data.len() {
                let _ = metadata(&data[..len]);
            }
            let mut seed = 0x9E3779B9u32;
            for _ in 0..300 {
                let mut d = data.to_vec();
                for _ in 0..8 {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    let i = seed as usize % d.len();
                    d[i] = (seed >> 8) as u8;
                }
                let _ = metadata(&d);
            }
        }
    }
}
