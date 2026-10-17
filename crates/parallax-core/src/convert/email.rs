//! E-mails: `.eml` (MIME) and Outlook `.msg`, shown as a person reads them.
//!
//! The raw form of a message is dominated by transport detail: `Received`
//! chains, DKIM signatures, MIME boundaries, quoted-printable or base64 bodies,
//! encoded-word headers. The "E-mail" view keeps the main headers (decoded), the
//! body as text (the text/plain part; HTML converted to text when there is no
//! plain part), and one line per attachment with its name, type, size and a
//! short SHA-256, so a changed attachment shows as one changed line.
//!
//! `.eml` is parsed with `mail-parser`. `.msg` is an OLE compound file (read with
//! `cfb`) holding MAPI properties as streams named `__substg1.0_IIIITTTT`
//! (property id and type); recipients and attachments are sub-storages. The
//! same fields are read from there; RTF-only bodies (compressed RTF) are not
//! decoded.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::{Cursor, Read};

use mail_parser::{Address, MessageParser, MimeHeaders};
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// Body lines shown before "… N more lines".
const MAX_BODY_LINES: usize = 20_000;
/// Attachments listed per message.
const MAX_ATTACHMENTS: usize = 1000;

pub fn is_eml(ext: &str) -> bool {
    matches!(ext, "eml" | "mht" | "mhtml")
}

/// `.msg` with the OLE compound file signature.
pub fn is_msg(ext: &str, data: &[u8]) -> bool {
    ext == "msg" && data.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
}

/// What the view shows of a message, whichever format it came from.
#[derive(Default, PartialEq)]
struct Mail {
    headers: Vec<(&'static str, String)>,
    /// Label (`text/plain`, `converted from HTML`) and text.
    bodies: Vec<(&'static str, String)>,
    attachments: Vec<Attachment>,
}

#[derive(PartialEq)]
struct Attachment {
    name: String,
    content_type: String,
    size: usize,
    sha256: String,
}

impl Attachment {
    fn new(name: String, content_type: String, data: &[u8]) -> Attachment {
        let digest = Sha256::digest(data);
        let sha256 = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        Attachment { name, content_type, size: data.len(), sha256 }
    }
}

fn size(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

impl Mail {
    fn header(&mut self, label: &'static str, value: Option<impl Into<String>>) {
        if let Some(v) = value.map(Into::into).filter(|v: &String| !v.trim().is_empty()) {
            self.headers.push((label, v.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
    }

    fn render(&self) -> String {
        let mut out = String::new();
        for (label, value) in &self.headers {
            let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
        }
        for (label, text) in &self.bodies {
            let _ = writeln!(out, "\n=== Body ({label}) ===");
            let mut lines = text.lines();
            for l in lines.by_ref().take(MAX_BODY_LINES) {
                out.push_str(l.trim_end());
                out.push('\n');
            }
            let rest = lines.count();
            if rest > 0 {
                let _ = writeln!(out, "… {rest} more lines");
            }
        }
        if !self.attachments.is_empty() {
            let _ = writeln!(out, "\n=== Attachments ({}) ===", self.attachments.len());
            for a in self.attachments.iter().take(MAX_ATTACHMENTS) {
                let _ = writeln!(out, "{}  {}, {}, sha256 {}", a.name, a.content_type, size(a.size), a.sha256);
            }
            if self.attachments.len() > MAX_ATTACHMENTS {
                let _ = writeln!(out, "… {} more attachments", self.attachments.len() - MAX_ATTACHMENTS);
            }
        }
        out
    }
}

// ---- .eml ----

fn addresses(a: Option<&Address>) -> Option<String> {
    let list: Vec<String> = a?
        .iter()
        .map(|addr| match (addr.name(), addr.address()) {
            (Some(n), Some(e)) => format!("{n} <{e}>"),
            (None, Some(e)) => e.to_string(),
            (Some(n), None) => n.to_string(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect();
    (!list.is_empty()).then(|| list.join(", "))
}

fn eml_mail(data: &[u8]) -> Result<Mail> {
    let msg = MessageParser::default().parse(data).ok_or_else(|| Error::Unsupported("not a readable e-mail".into()))?;
    if msg.headers().is_empty() {
        return Err(Error::Unsupported("not an e-mail: no headers".into()));
    }
    let mut mail = Mail::default();
    mail.header("From", addresses(msg.from()));
    mail.header("To", addresses(msg.to()));
    mail.header("Cc", addresses(msg.cc()));
    mail.header("Subject", msg.subject());
    mail.header("Date", msg.date().map(|d| d.to_rfc3339()));
    mail.header("Message-ID", msg.message_id().map(|id| format!("<{id}>")));
    let reply = msg.in_reply_to();
    let reply = reply
        .as_text()
        .map(|s| format!("<{s}>"))
        .or_else(|| reply.as_text_list().map(|l| l.iter().map(|s| format!("<{s}>")).collect::<Vec<_>>().join(" ")));
    mail.header("In-Reply-To", reply);
    // The text parts; with only HTML, mail-parser's text conversion of it.
    for i in 0..msg.text_body_count() {
        let html = msg.text_part(i as u32).is_some_and(|p| p.is_text_html());
        if let Some(text) = msg.body_text(i) {
            mail.bodies.push((if html { "converted from HTML" } else { "text/plain" }, text.replace("\r\n", "\n")));
        }
    }
    for part in msg.attachments() {
        let content_type =
            part.content_type().map_or("application/octet-stream".to_string(), |ct| match ct.subtype() {
                Some(sub) => format!("{}/{sub}", ct.ctype()),
                None => ct.ctype().to_string(),
            });
        let name = part
            .attachment_name()
            .map(str::to_string)
            .or_else(|| part.message().and_then(|m| m.subject()).map(|s| format!("{s}.eml")))
            .unwrap_or_else(|| "(unnamed)".into());
        let contents: Cow<[u8]> = match part.message() {
            Some(m) => Cow::Borrowed(m.raw_message()),
            None => Cow::Borrowed(part.contents()),
        };
        mail.attachments.push(Attachment::new(name, content_type, &contents));
    }
    Ok(mail)
}

// ---- .msg ----

/// The MAPI properties of one storage: strings and binaries from `__substg1.0_*`
/// streams, fixed-size values from `__properties_version1.0`.
struct Props {
    strings: Vec<(u16, String)>,
    binaries: Vec<(u16, Vec<u8>)>,
    fixed: Vec<(u16, u16, [u8; 8])>,
}

impl Props {
    fn string(&self, id: u16) -> Option<String> {
        self.strings.iter().find(|(i, _)| *i == id).map(|(_, s)| s.clone())
    }

    fn binary(&self, id: u16) -> Option<&[u8]> {
        self.binaries.iter().find(|(i, _)| *i == id).map(|(_, b)| b.as_slice())
    }

    fn int(&self, id: u16) -> Option<u32> {
        self.fixed
            .iter()
            .find(|(i, t, _)| *i == id && *t == 0x0003)
            .map(|(_, _, v)| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
    }

    fn time(&self, id: u16) -> Option<String> {
        let (_, _, v) = self.fixed.iter().find(|(i, t, _)| *i == id && *t == 0x0040)?;
        let filetime = u64::from_le_bytes(*v);
        let secs = (filetime / 10_000_000).checked_sub(11_644_473_600)? as i64;
        Some(utc(secs))
    }
}

/// `2026-10-01T09:30:00Z` from Unix seconds.
fn utc(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

type Cfb<'a> = cfb::CompoundFile<Cursor<&'a [u8]>>;

/// Streams larger than this are not read (attachment data is hashed in full, up to this).
const MAX_STREAM: u64 = 512 * 1024 * 1024;

fn read_stream(file: &mut Cfb, path: &str) -> Option<Vec<u8>> {
    let mut s = file.open_stream(path).ok()?;
    if s.len() > MAX_STREAM {
        return None;
    }
    let mut buf = Vec::with_capacity(s.len() as usize);
    s.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn read_props(file: &mut Cfb, storage: &str, header: usize) -> Props {
    let mut props = Props { strings: Vec::new(), binaries: Vec::new(), fixed: Vec::new() };
    let names: Vec<String> = match file.read_storage(storage) {
        Ok(entries) => entries.filter(|e| e.is_stream()).map(|e| e.name().to_string()).collect(),
        Err(_) => return props,
    };
    for name in names {
        let path = format!("{}/{name}", storage.trim_end_matches('/'));
        if let Some(tag) = name.strip_prefix("__substg1.0_").and_then(|t| u32::from_str_radix(t.get(..8)?, 16).ok()) {
            let (id, kind) = ((tag >> 16) as u16, tag as u16);
            let Some(data) = read_stream(file, &path) else { continue };
            match kind {
                0x001F => {
                    let units: Vec<u16> =
                        data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                    props.strings.push((id, String::from_utf16_lossy(&units).trim_end_matches('\0').to_string()));
                }
                0x001E => props.strings.push((id, crate::text::decode(&data).text.trim_end_matches('\0').to_string())),
                0x0102 => props.binaries.push((id, data)),
                _ => {}
            }
        } else if name == "__properties_version1.0"
            && let Some(data) = read_stream(file, &path)
        {
            for entry in data.get(header..).unwrap_or_default().as_chunks::<16>().0.iter() {
                let tag = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]);
                let value: [u8; 8] = entry[8..16].try_into().unwrap_or_default();
                props.fixed.push(((tag >> 16) as u16, tag as u16, value));
            }
        }
    }
    props
}

fn msg_mail(data: &[u8]) -> Result<Mail> {
    // cfb validates its input, but a hostile file should never take the app down.
    std::panic::catch_unwind(|| msg_mail_inner(data))
        .unwrap_or_else(|_| Err(Error::Unsupported("the .msg file is damaged".into())))
}

fn msg_mail_inner(data: &[u8]) -> Result<Mail> {
    let mut file = cfb::CompoundFile::open(Cursor::new(data))
        .map_err(|e| Error::Unsupported(format!("not a readable .msg file: {e}")))?;
    let top = read_props(&mut file, "/", 32);
    let mut mail = Mail::default();
    let sender = match (top.string(0x0C1A), top.string(0x5D01).or_else(|| top.string(0x0C1F))) {
        (Some(n), Some(e)) if n != e => Some(format!("{n} <{e}>")),
        (n, e) => e.or(n),
    };
    // Recipients: name and SMTP address per storage, by type (1 To, 2 Cc, 3 Bcc).
    let storages: Vec<String> =
        file.read_root_storage().filter(|e| e.is_storage()).map(|e| e.name().to_string()).collect();
    let (mut to, mut cc) = (Vec::new(), Vec::new());
    let mut attachments = Vec::new();
    for s in &storages {
        if s.starts_with("__recip_version1.0_") {
            let r = read_props(&mut file, &format!("/{s}"), 8);
            let email = r.string(0x39FE).or_else(|| r.string(0x3003));
            let entry = match (r.string(0x3001), email) {
                (Some(n), Some(e)) if n != e => format!("{n} <{e}>"),
                (n, e) => e.or(n).unwrap_or_default(),
            };
            match r.int(0x0C15) {
                Some(2) => cc.push((s.clone(), entry)),
                Some(3) => {}
                _ => to.push((s.clone(), entry)),
            }
        } else if s.starts_with("__attach_version1.0_") {
            let a = read_props(&mut file, &format!("/{s}"), 8);
            let name = a.string(0x3707).or_else(|| a.string(0x3704)).unwrap_or_else(|| "(unnamed)".into());
            let content_type = a.string(0x370E).unwrap_or_else(|| "application/octet-stream".into());
            let embedded = file.is_storage(format!("/{s}/__substg1.0_3701000D"));
            let attachment = match a.binary(0x3701) {
                Some(bytes) => Attachment::new(name, content_type, bytes),
                None if embedded => Attachment::new(name, "message (embedded Outlook item)".into(), &[]),
                None => Attachment::new(name, content_type, &[]),
            };
            attachments.push((s.clone(), attachment));
        }
    }
    // Storage names carry the original order as a hex number.
    to.sort();
    cc.sort();
    attachments.sort_by(|a, b| a.0.cmp(&b.0));
    let join = |v: Vec<(String, String)>| {
        (!v.is_empty()).then(|| v.into_iter().map(|(_, e)| e).collect::<Vec<_>>().join(", "))
    };
    mail.header("From", sender);
    mail.header("To", join(to).or_else(|| top.string(0x0E04)));
    mail.header("Cc", join(cc).or_else(|| top.string(0x0E03)));
    mail.header("Subject", top.string(0x0037));
    mail.header("Date", top.time(0x0039).or_else(|| top.time(0x0E06)));
    mail.header("Message-ID", top.string(0x1035));
    mail.header("In-Reply-To", top.string(0x1042));
    if let Some(body) = top.string(0x1000).filter(|b| !b.trim().is_empty()) {
        mail.bodies.push(("text/plain", body.replace("\r\n", "\n")));
    } else if let Some(html) = top.binary(0x1013).map(|b| crate::text::decode(b).text).or_else(|| top.string(0x1013)) {
        mail.bodies.push(("converted from HTML", mail_parser::decoders::html::html_to_text(&html)));
    } else if top.binary(0x1009).is_some() {
        mail.bodies.push(("RTF", "! the body is stored only as compressed RTF, which is not decoded".into()));
    }
    mail.attachments = attachments.into_iter().map(|(_, a)| a).collect();
    if mail.headers.is_empty() && mail.bodies.is_empty() {
        return Err(Error::Unsupported("no message properties found in the .msg file".into()));
    }
    Ok(mail)
}

fn mail(ext: &str, data: &[u8]) -> Result<Mail> {
    if ext == "msg" { msg_mail(data) } else { eml_mail(data) }
}

/// The e-mail view.
pub fn render(ext: &str, data: &[u8]) -> Result<String> {
    Ok(mail(ext, data)?.render())
}

/// Whether two messages show the same headers, bodies and attachments, so that
/// only transport headers or the MIME encoding differ.
pub fn same_message(ext: &str, left: &[u8], right: &[u8]) -> Option<bool> {
    Some(mail(ext, left).ok()? == mail(ext, right).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/email/", $name))
        };
    }
    const ORDER: &[u8] = fixture!("order.eml");

    #[test]
    fn multipart_with_attachment() {
        let v = render("eml", ORDER).unwrap();
        for expected in [
            "From:              Jürgen Müller <juergen@example.com>\n",
            "To:                Shop Team <team@example.org>, sales@example.org\n",
            "Cc:                audit@example.org\n",
            "Subject:           Bestellung Nr. 4711 – Änderung\n",
            "Date:              2026-10-01T09:30:00+02:00\n",
            "Message-ID:        <order-4711@example.com>\n",
            "=== Body (text/plain) ===\nHallo Team,\n\nbitte die Menge auf 12 Stück ändern.\n",
            "=== Attachments (2) ===\norder-4711.csv  text/csv, 35 B, sha256 ",
            "invoice.pdf  application/pdf, 2.0 KB, sha256 ",
        ] {
            assert!(v.contains(expected), "{expected}\n---\n{v}");
        }
        assert!(!v.contains("<p>") && !v.contains("=C3"), "{v}");
    }

    #[test]
    fn html_only_and_reply() {
        let v = render("eml", fixture!("newsletter-html.eml")).unwrap();
        assert!(
            v.contains("=== Body (converted from HTML) ===\n") && v.contains("Autumn sale") && !v.contains("<td>"),
            "{v}"
        );
        let r = render("eml", fixture!("reply.eml")).unwrap();
        assert!(r.contains("In-Reply-To:       <order-4711@example.com>\n"), "{r}");
    }

    #[test]
    fn same_message_in_another_encoding() {
        assert_eq!(same_message("eml", ORDER, fixture!("order-base64.eml")), Some(true));
        assert_eq!(same_message("eml", ORDER, fixture!("order-attachment-changed.eml")), Some(false));
        let (a, b) = (render("eml", ORDER).unwrap(), render("eml", fixture!("order-attachment-changed.eml")).unwrap());
        let changed: Vec<&str> = a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect();
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert!(changed[0].starts_with("invoice.pdf  application/pdf"), "{changed:?}");
    }

    #[test]
    fn outlook_msg() {
        let data = fixture!("order.msg");
        assert!(is_msg("msg", data));
        let v = render("msg", data).unwrap();
        for expected in [
            "From:              Jürgen Müller <juergen@example.com>\n",
            "To:                Shop Team <team@example.org>, sales@example.org\n",
            "Cc:                audit@example.org\n",
            "Subject:           Bestellung Nr. 4711 – Änderung\n",
            "Date:              2026-10-01T07:30:00Z\n",
            "=== Body (text/plain) ===\nHallo Team,\n\nbitte die Menge auf 12 Stück ändern.\n",
            "=== Attachments (1) ===\norder-4711.csv  text/csv, 35 B, sha256 ",
        ] {
            assert!(v.contains(expected), "{expected}\n---\n{v}");
        }
        // The attachment is the same bytes as in the .eml.
        let eml = render("eml", ORDER).unwrap();
        let line = |s: &str| s.lines().find(|l| l.starts_with("order-4711.csv")).unwrap().to_string();
        assert_eq!(line(&v), line(&eml));
    }

    #[test]
    fn damaged_input() {
        for data in [ORDER, &fixture!("order.msg")[..]] {
            for n in [0, 1, 10, 600, data.len() / 2, data.len() - 1] {
                let _ = render("eml", &data[..n]);
                let _ = render("msg", &data[..n]);
            }
        }
        assert!(render("eml", b"").is_err());
        let mut garbage = fixture!("order.msg").to_vec();
        for i in (512..garbage.len()).step_by(97) {
            garbage[i] ^= 0x5A;
        }
        let _ = render("msg", &garbage);
    }

    /// Writes `order.msg` with the same content as `order.eml`, following [MS-OXMSG].
    /// Run with `cargo test -p parallax-core regenerate_msg -- --ignored`; the
    /// fixture's regenerate.py then checks it with extract_msg (an Outlook reader).
    #[test]
    #[ignore]
    fn regenerate_msg_fixture() {
        use std::io::Write;
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/email/order.msg");
        let mut f = cfb::create(path).unwrap();
        let utf16 = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
        let put = |f: &mut cfb::CompoundFile<std::fs::File>, path: &str, data: &[u8]| {
            f.create_stream(path).unwrap().write_all(data).unwrap();
        };
        let fixed = |header: usize, props: &[(u32, [u8; 8])]| {
            let mut v = vec![0u8; header];
            for (tag, value) in props {
                v.extend(tag.to_le_bytes());
                v.extend(6u32.to_le_bytes()); // readable, writable
                v.extend(value);
            }
            v
        };
        // 2026-10-01 07:30:00 UTC as FILETIME.
        let submit = ((1_790_839_800u64 + 11_644_473_600) * 10_000_000).to_le_bytes();
        let mut top_header = vec![0u8; 32];
        top_header[16..20].copy_from_slice(&2u32.to_le_bytes()); // next recipient id
        top_header[20..24].copy_from_slice(&1u32.to_le_bytes()); // next attachment id
        top_header[24..28].copy_from_slice(&2u32.to_le_bytes()); // recipient count
        top_header[28..32].copy_from_slice(&1u32.to_le_bytes()); // attachment count
        let mut top = top_header;
        top.extend(&fixed(0, &[(0x0039_0040, submit), (0x0E06_0040, submit)]));
        put(&mut f, "/__properties_version1.0", &top);
        for (tag, value) in [
            ("001A001F", "IPM.Note"),
            ("0037001F", "Bestellung Nr. 4711 – Änderung"),
            ("0C1A001F", "Jürgen Müller"),
            ("5D01001F", "juergen@example.com"),
            ("0E04001F", "Shop Team; sales@example.org"),
            ("0E03001F", "audit@example.org"),
            ("1000001F", "Hallo Team,\r\n\r\nbitte die Menge auf 12 Stück ändern.\r\n\r\nGrüße\r\nJürgen\r\n"),
            ("1035001F", "<order-4711@example.com>"),
        ] {
            put(&mut f, &format!("/__substg1.0_{tag}"), &utf16(value));
        }
        f.create_storage("/__nameid_version1.0").unwrap();
        for s in ["00020102", "00030102", "00040102"] {
            put(&mut f, &format!("/__nameid_version1.0/__substg1.0_{s}"), &[]);
        }
        for (i, (name, email, kind)) in [
            ("Shop Team", "team@example.org", 1u32),
            ("sales@example.org", "sales@example.org", 1),
            ("audit@example.org", "audit@example.org", 2),
        ]
        .iter()
        .enumerate()
        {
            let dir = format!("/__recip_version1.0_#{i:08X}");
            f.create_storage(&dir).unwrap();
            let mut kind_value = [0u8; 8];
            kind_value[..4].copy_from_slice(&kind.to_le_bytes());
            put(&mut f, &format!("{dir}/__properties_version1.0"), &fixed(8, &[(0x0C15_0003, kind_value)]));
            put(&mut f, &format!("{dir}/__substg1.0_3001001F"), &utf16(name));
            put(&mut f, &format!("{dir}/__substg1.0_3003001F"), &utf16(email));
            put(&mut f, &format!("{dir}/__substg1.0_39FE001F"), &utf16(email));
        }
        let dir = "/__attach_version1.0_#00000000";
        f.create_storage(dir).unwrap();
        let mut method = [0u8; 8];
        method[0] = 1; // ATTACH_BY_VALUE
        put(&mut f, &format!("{dir}/__properties_version1.0"), &fixed(8, &[(0x3705_0003, method)]));
        put(&mut f, &format!("{dir}/__substg1.0_3707001F"), &utf16("order-4711.csv"));
        put(&mut f, &format!("{dir}/__substg1.0_3704001F"), &utf16("ORDER-~1.CSV"));
        put(&mut f, &format!("{dir}/__substg1.0_370E001F"), &utf16("text/csv"));
        put(&mut f, &format!("{dir}/__substg1.0_37010102"), b"sku,qty\nA-1,12\nB-7,3\nC-9,100\nD-2,4\n");
        f.flush().unwrap();
    }
}
