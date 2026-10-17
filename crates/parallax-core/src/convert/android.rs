//! Android binary XML: `AndroidManifest.xml` and `res/**/*.xml` inside an APK.
//!
//! aapt compiles these files into chunks (little-endian): a string pool, a map of
//! attribute names to resource ids, then namespace and element start/end chunks
//! whose attributes carry typed values (strings, integers, booleans, colors,
//! dimensions, resource references). The view decodes them back to XML text with
//! one attribute per line, so a changed attribute is one changed line. Resource
//! references stay numeric (`@0x7f040001`): their names live in `resources.arsc`.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::{Error, Result};

const MAX_DEPTH: usize = 256;

/// Whether the data starts like a binary XML document (`RES_XML_TYPE`, header size 8).
pub fn is_binary_xml(data: &[u8]) -> bool {
    data.len() >= 8 && data.starts_with(&[0x03, 0x00, 0x08, 0x00])
}

fn bad(what: &str) -> Error {
    Error::Unsupported(format!("Android binary XML: {what}"))
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

/// The string pool chunk (`RES_STRING_POOL_TYPE`).
struct Strings<'a> {
    chunk: &'a [u8],
    count: u32,
    utf8: bool,
    start: usize,
}

impl<'a> Strings<'a> {
    fn parse(chunk: &'a [u8]) -> Option<Self> {
        let count = u32_at(chunk, 8)?;
        let flags = u32_at(chunk, 16)?;
        let start = u32_at(chunk, 20)? as usize;
        let header = u16_at(chunk, 2)? as usize;
        // The offsets table must fit in the chunk.
        chunk.get(header..header.checked_add((count as usize).checked_mul(4)?)?)?;
        Some(Strings { chunk, count, utf8: flags & 0x100 != 0, start })
    }

    fn get(&self, index: u32) -> Option<String> {
        if index >= self.count {
            return None;
        }
        let header = u16_at(self.chunk, 2)? as usize;
        let offset = u32_at(self.chunk, header + 4 * index as usize)? as usize;
        let at = self.start.checked_add(offset)?;
        let d = self.chunk.get(at..)?;
        if self.utf8 {
            // Length in characters, then in bytes; each 1 or 2 bytes.
            let skip = |d: &[u8], at: usize| -> Option<(usize, usize)> {
                let b = *d.get(at)? as usize;
                if b & 0x80 != 0 { Some((((b & 0x7F) << 8) | *d.get(at + 1)? as usize, 2)) } else { Some((b, 1)) }
            };
            let (_, n1) = skip(d, 0)?;
            let (len, n2) = skip(d, n1)?;
            let bytes = d.get(n1 + n2..n1 + n2 + len)?;
            Some(String::from_utf8_lossy(bytes).into_owned())
        } else {
            let first = u16_at(d, 0)? as usize;
            let (len, skip) =
                if first & 0x8000 != 0 { ((((first & 0x7FFF) << 16) | u16_at(d, 2)? as usize), 4) } else { (first, 2) };
            let bytes = d.get(skip..skip + len.checked_mul(2)?)?;
            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Some(String::from_utf16_lossy(&units))
        }
    }
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            c => out.push(c),
        }
    }
}

/// A complex value (dimension or fraction) as aapt writes it: `16.0dp`, `50%`.
fn complex(data: u32, units: &[&str]) -> String {
    const SHIFTS: [u32; 4] = [0, 7, 15, 23];
    let mantissa = (data as i32) >> 8;
    let value = mantissa as f64 / f64::from(1u32 << SHIFTS[((data >> 4) & 3) as usize]);
    let unit = units.get((data & 0xF) as usize).copied().unwrap_or("?");
    let value = if unit.starts_with('%') { value * 100.0 } else { value };
    format!("{}{unit}", trim_float(value))
}

fn trim_float(v: f64) -> String {
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0');
    if s.ends_with('.') { format!("{s}0") } else { s.to_string() }
}

/// A typed attribute value (`Res_value`).
fn typed_value(kind: u8, data: u32, strings: &Strings) -> String {
    match kind {
        0x00 => {
            if data == 1 {
                "@empty".into()
            } else {
                "@null".into()
            }
        }
        0x01 => {
            if data == 0 {
                "@null".into()
            } else {
                format!("@0x{data:08x}")
            }
        }
        0x02 => format!("?0x{data:08x}"),
        0x03 => strings.get(data).unwrap_or_default(),
        0x04 => trim_float(f64::from(f32::from_bits(data))),
        0x05 => complex(data, &["px", "dp", "sp", "pt", "in", "mm"]),
        0x06 => complex(data, &["%", "%p"]),
        0x07 => format!("@0x{data:08x}"),
        0x10 => (data as i32).to_string(),
        0x11 => format!("0x{data:x}"),
        0x12 => (data != 0).to_string(),
        0x1C | 0x1E => format!("#{data:08x}"),
        0x1D | 0x1F => format!("#{:06x}", data & 0xFF_FFFF),
        _ => format!("(type 0x{kind:02x}) 0x{data:08x}"),
    }
}

/// The document as XML text: one attribute per line, namespaces declared on the
/// element where they start.
pub fn to_xml(data: &[u8]) -> Result<String> {
    if !is_binary_xml(data) {
        return Err(bad("not a binary XML file"));
    }
    let total = (u32_at(data, 4).ok_or_else(|| bad("truncated"))? as usize).min(data.len());
    let mut strings: Option<Strings> = None;
    let mut resource_ids: Vec<u32> = Vec::new();
    let mut prefixes: HashMap<String, String> = HashMap::new();
    let mut pending_ns: Vec<(String, String)> = Vec::new();
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    let mut depth = 0usize;
    // Whether the last start tag is still open (`<a` without `>`), to write `/>` for empty elements.
    let mut open = false;
    let mut pos = 8;
    while pos + 8 <= total {
        let kind = u16_at(data, pos).unwrap_or(0);
        let header = u16_at(data, pos + 2).unwrap_or(0) as usize;
        let size = u32_at(data, pos + 4).unwrap_or(0) as usize;
        if size < 8 || header < 8 || header > size || pos + size > total {
            return Err(bad("corrupt chunk"));
        }
        let chunk = &data[pos..pos + size];
        let s = |i: u32| -> String {
            if i == u32::MAX { String::new() } else { strings.as_ref().and_then(|p| p.get(i)).unwrap_or_default() }
        };
        match kind {
            0x0001 => strings = Some(Strings::parse(chunk).ok_or_else(|| bad("corrupt string pool"))?),
            0x0180 => {
                resource_ids =
                    chunk[header..].chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
            }
            0x0100 => {
                let (prefix, uri) =
                    (s(u32_at(chunk, header).unwrap_or(u32::MAX)), s(u32_at(chunk, header + 4).unwrap_or(u32::MAX)));
                prefixes.insert(uri.clone(), prefix.clone());
                pending_ns.push((prefix, uri));
            }
            0x0102 => {
                if open {
                    out.push_str(">\n");
                }
                if depth >= MAX_DEPTH {
                    return Err(bad("elements nested too deeply"));
                }
                // The element extension follows the header: namespace, name, then where the attributes are.
                let name = s(u32_at(chunk, header + 4).unwrap_or(u32::MAX));
                let attr_start = u16_at(chunk, header + 8).unwrap_or(20) as usize;
                let attr_size = (u16_at(chunk, header + 10).unwrap_or(20) as usize).max(20);
                let count = u16_at(chunk, header + 12).unwrap_or(0) as usize;
                let indent = "  ".repeat(depth);
                let _ = write!(out, "{indent}<{name}");
                let mut attrs = Vec::new();
                for (prefix, uri) in pending_ns.drain(..) {
                    let mut v = String::new();
                    escape(&uri, &mut v);
                    attrs.push(if prefix.is_empty() {
                        format!("xmlns=\"{v}\"")
                    } else {
                        format!("xmlns:{prefix}=\"{v}\"")
                    });
                }
                for i in 0..count {
                    let at = header + attr_start + i * attr_size;
                    let Some(a) = chunk.get(at..at + 20) else {
                        return Err(bad("truncated attribute"));
                    };
                    let ns = s(u32_at(a, 0).unwrap_or(u32::MAX));
                    let name_index = u32_at(a, 4).unwrap_or(u32::MAX);
                    let mut attr_name = s(name_index);
                    if attr_name.is_empty() {
                        // Stripped names: the resource id says which attribute it is.
                        attr_name =
                            resource_ids.get(name_index as usize).map_or("?".into(), |id| format!("attr0x{id:08x}"));
                    }
                    let raw = u32_at(a, 8).unwrap_or(u32::MAX);
                    let value = if raw != u32::MAX {
                        s(raw)
                    } else {
                        typed_value(
                            a[15],
                            u32_at(a, 16).unwrap_or(0),
                            strings.as_ref().ok_or_else(|| bad("no string pool"))?,
                        )
                    };
                    let prefix = prefixes.get(&ns).cloned().unwrap_or_default();
                    let qualified = if prefix.is_empty() { attr_name } else { format!("{prefix}:{attr_name}") };
                    let mut v = String::new();
                    escape(&value, &mut v);
                    attrs.push(format!("{qualified}=\"{v}\""));
                }
                match attrs.len() {
                    0 => {}
                    1 => {
                        let _ = write!(out, " {}", attrs[0]);
                    }
                    _ => {
                        for a in &attrs {
                            let _ = write!(out, "\n{indent}    {a}");
                        }
                    }
                }
                open = true;
                depth += 1;
            }
            0x0103 => {
                depth = depth.saturating_sub(1);
                if open {
                    out.push_str(" />\n");
                    open = false;
                } else {
                    let name = s(u32_at(chunk, header + 4).unwrap_or(u32::MAX));
                    let _ = writeln!(out, "{}</{name}>", "  ".repeat(depth));
                }
            }
            0x0104 => {
                if open {
                    out.push_str(">\n");
                    open = false;
                }
                let text = s(u32_at(chunk, header).unwrap_or(u32::MAX));
                let mut v = String::new();
                escape(text.trim(), &mut v);
                if !v.is_empty() {
                    let _ = writeln!(out, "{}{v}", "  ".repeat(depth));
                }
            }
            _ => {}
        }
        pos += size;
    }
    if open {
        out.push_str(" />\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/android/", $name))
        };
    }
    const V16: &[u8] = fixture!("selendroid-0.16.0-AndroidManifest.xml");
    const V17: &[u8] = fixture!("selendroid-0.17.0-AndroidManifest.xml");

    #[test]
    fn decodes_a_real_manifest() {
        let x = to_xml(V17).unwrap();
        for expected in [
            "<manifest\n    xmlns:android=\"http://schemas.android.com/apk/res/android\"\n    android:versionCode=\"1\"\n    android:versionName=\"0.17.0\"\n    package=\"io.selendroid.server\"",
            "  <uses-sdk android:minSdkVersion=\"10\" />\n",
            "  <uses-permission android:name=\"android.permission.INTERNET\" />\n",
            "      android:icon=\"@0x7f020000\"\n      android:debuggable=\"true\">\n    <uses-library android:name=\"android.test.runner\" />\n  </application>\n</manifest>\n",
        ] {
            assert!(x.contains(expected), "{expected}\n---\n{x}");
        }
    }

    #[test]
    fn a_diff_shows_only_what_changed() {
        let (a, b) = (to_xml(V16).unwrap(), to_xml(V17).unwrap());
        let changed: Vec<&str> = a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect();
        assert_eq!(changed, ["    android:versionName=\"0.16.0\""]);
    }

    /// A small document with a UTF-8 string pool and typed values, built by hand.
    fn handmade() -> Vec<u8> {
        let strings = [
            "android",
            "http://schemas.android.com/apk/res/android",
            "layout",
            "width",
            "color",
            "count",
            "hidden",
            "Grüße",
        ];
        let mut pool = Vec::new();
        let mut offsets = Vec::new();
        for s in strings {
            offsets.push(pool.len() as u32);
            pool.push(s.chars().count() as u8);
            pool.push(s.len() as u8);
            pool.extend_from_slice(s.as_bytes());
            pool.push(0);
        }
        while pool.len() % 4 != 0 {
            pool.push(0);
        }
        let mut sp = Vec::new();
        let start = 28 + 4 * strings.len() as u32;
        sp.extend_from_slice(&[1, 0, 28, 0]);
        sp.extend_from_slice(&(start + pool.len() as u32).to_le_bytes());
        sp.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        sp.extend_from_slice(&0u32.to_le_bytes());
        sp.extend_from_slice(&0x100u32.to_le_bytes());
        sp.extend_from_slice(&start.to_le_bytes());
        sp.extend_from_slice(&0u32.to_le_bytes());
        for o in offsets {
            sp.extend_from_slice(&o.to_le_bytes());
        }
        sp.extend_from_slice(&pool);
        let chunk = |kind: u16, body: &[u32]| {
            let mut c = Vec::new();
            c.extend_from_slice(&kind.to_le_bytes());
            c.extend_from_slice(&16u16.to_le_bytes());
            c.extend_from_slice(&(16 + 4 * body.len() as u32).to_le_bytes());
            c.extend_from_slice(&[1, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0xFF]);
            for v in body {
                c.extend_from_slice(&v.to_le_bytes());
            }
            c
        };
        let attr = |name: u32, kind: u32, data: u32| [1, name, u32::MAX, 8 | (kind << 24), data];
        let mut element = vec![u32::MAX, 2, 20 | (20 << 16), 4, 0];
        element.extend(attr(3, 0x05, (16 << 8) | 1)); // 16dp
        element.extend(attr(4, 0x1C, 0xFF00_80FF));
        element.extend(attr(5, 0x10, (-3i32) as u32));
        element.extend(attr(6, 0x12, 0xFFFF_FFFF));
        let mut body = Vec::new();
        body.extend(chunk(0x0100, &[0, 1]));
        body.extend(chunk(0x0102, &element));
        body.extend(chunk(0x0104, &[7, 8, 0]));
        body.extend(chunk(0x0103, &[u32::MAX, 2]));
        body.extend(chunk(0x0101, &[0, 1]));
        let mut doc = vec![3, 0, 8, 0];
        doc.extend_from_slice(&((8 + sp.len() + body.len()) as u32).to_le_bytes());
        doc.extend(sp);
        doc.extend(body);
        doc
    }

    #[test]
    fn typed_values_and_utf8_strings() {
        let x = to_xml(&handmade()).unwrap();
        assert_eq!(
            x,
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<layout\n    xmlns:android=\"http://schemas.android.com/apk/res/android\"\n    android:width=\"16.0dp\"\n    android:color=\"#ff0080ff\"\n    android:count=\"-3\"\n    android:hidden=\"true\">\n  Grüße\n</layout>\n"
        );
    }

    #[test]
    fn detection_and_garbage() {
        assert!(is_binary_xml(V17));
        assert!(!is_binary_xml(b"<?xml version=\"1.0\"?>"));
        let doc = handmade();
        for data in [V17, &doc[..]] {
            for len in 0..data.len() {
                let _ = to_xml(&data[..len]);
            }
            let mut corrupt = data.to_vec();
            for i in (8..corrupt.len()).step_by(3) {
                corrupt[i] ^= 0xA5;
                let _ = to_xml(&corrupt);
            }
        }
    }
}
