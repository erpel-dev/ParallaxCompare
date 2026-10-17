//! Certificates and keys: PEM, DER and PKCS#12 files, shown by what they say.
//!
//! The base64 of a PEM file changes completely when a certificate is renewed, and
//! a DER file is binary, so neither diffs well. The "Certificate" view decodes
//! each object: per certificate the subject, issuer, serial, validity, key type
//! and size, signature algorithm, subject alternative names, key usages, basic
//! constraints, key identifiers and the SHA-256 fingerprint; for certificate
//! requests and revocation lists the same where it applies; for keys only the
//! type and size. Private key material is never printed, and neither is anything
//! derived from it.
//!
//! PKCS#12 (`.p12`, `.pfx`) is read as far as possible without a password:
//! unencrypted bags are shown, encrypted ones are opened with the empty password
//! when they use PBES2 (PBKDF2 + AES-CBC, OpenSSL 3's default); otherwise the
//! view says what is encrypted and how.
//!
//! Certificates are parsed with `x509-parser`; keys, PKCS#12 containers and PEM
//! armour with a small DER reader below, as they need only a few fields.

use std::fmt::Write as _;

use base64::Engine as _;
use sha2::{Digest, Sha256};
use x509_parser::certificate::X509Certificate;
use x509_parser::certification_request::X509CertificationRequest;
use x509_parser::extensions::{GeneralName, ParsedExtension, X509Extension};
use x509_parser::prelude::FromDer;
use x509_parser::revocation_list::CertificateRevocationList;
use x509_parser::time::ASN1Time;

use crate::{Error, Result};

/// Revoked serials listed per CRL before "… N more".
const MAX_REVOKED: usize = 1000;
/// Objects shown from one file (a CA bundle has a few hundred).
const MAX_OBJECTS: usize = 20_000;
/// PBKDF2 iterations tried for the empty password; more is treated as hostile.
const MAX_ITERATIONS: u32 = 1_000_000;

// ---- Detection ----

const PEM_EXTENSIONS: &[&str] = &["pem", "crt", "cer", "cert", "key", "csr", "crl", "pub", "p7b", "ca-bundle"];

/// Labels of the PEM blocks this view decodes (PGP armour and the like are left to the text view).
const PEM_LABELS: &[&[u8]] = &[
    b"CERTIFICATE-----",
    b"X509 CERTIFICATE-----",
    b"TRUSTED CERTIFICATE-----",
    b"CERTIFICATE REQUEST-----",
    b"NEW CERTIFICATE REQUEST-----",
    b"X509 CRL-----",
    b"PUBLIC KEY-----",
    b"PRIVATE KEY-----",
    b"RSA PRIVATE KEY-----",
    b"EC PRIVATE KEY-----",
    b"ENCRYPTED PRIVATE KEY-----",
    b"OPENSSH PRIVATE KEY-----",
];

/// A text file with certificate or key PEM blocks: it starts with one, or has a
/// certificate extension and one in the first 64 KB.
pub fn is_pem(ext: &str, data: &[u8]) -> bool {
    let start = data.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(data.len());
    let known = |at: usize| PEM_LABELS.iter().any(|l| data[at + 11..].starts_with(l));
    if data[start..].starts_with(b"-----BEGIN ") {
        return known(start);
    }
    PEM_EXTENSIONS.contains(&ext)
        && memchr::memmem::find(&data[..data.len().min(65536)], b"-----BEGIN ").is_some_and(known)
}

/// A DER certificate, request, CRL or key under a certificate extension.
pub fn is_der(ext: &str, data: &[u8]) -> bool {
    matches!(ext, "der" | "cer" | "crt" | "cert" | "key" | "csr" | "crl" | "pub")
        && data.first() == Some(&0x30)
        && tlv(data).is_some_and(|(_, rest)| rest.is_empty())
        && !matches!(classify_der(data), Object::Unknown(..))
}

/// A PKCS#12 file: the extension and a DER sequence starting with version 3.
pub fn is_pkcs12(ext: &str, data: &[u8]) -> bool {
    matches!(ext, "p12" | "pfx")
        && tlv(data).is_some_and(|(d, _)| d.tag == 0x30 && items(d.body).next().is_some_and(|v| v.tag == 0x02))
}

// ---- A minimal DER reader ----

#[derive(Clone, Copy)]
struct Der<'a> {
    tag: u8,
    body: &'a [u8],
    /// Tag, length and body.
    raw: &'a [u8],
}

/// One DER element and what follows it; `None` when truncated or not DER.
fn tlv(data: &[u8]) -> Option<(Der<'_>, &[u8])> {
    let tag = *data.first()?;
    if tag & 0x1F == 0x1F {
        return None; // multi-byte tags do not occur in these formats
    }
    let first = *data.get(1)? as usize;
    let (len, header) = if first < 0x80 {
        (first, 2)
    } else {
        let n = first & 0x7F;
        if n == 0 || n > 4 {
            return None; // indefinite (BER) or absurd lengths
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *data.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let end = header.checked_add(len)?;
    let raw = data.get(..end)?;
    Some((Der { tag, body: &raw[header..], raw }, &data[end..]))
}

/// The elements of a constructed body; stops at the first malformed one.
fn items(mut body: &[u8]) -> impl Iterator<Item = Der<'_>> {
    std::iter::from_fn(move || {
        let (d, rest) = tlv(body)?;
        body = rest;
        Some(d)
    })
}

fn oid_text(body: &[u8]) -> String {
    let mut parts: Vec<u64> = Vec::new();
    let mut value = 0u64;
    for &b in body {
        value = (value << 7) | u64::from(b & 0x7F);
        if b & 0x80 == 0 {
            if parts.is_empty() {
                let first = (value / 40).min(2);
                parts.push(first);
                parts.push(value - first * 40);
            } else {
                parts.push(value);
            }
            value = 0;
        }
        if value > u64::MAX >> 8 {
            return "?".into();
        }
    }
    parts.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// Short names of the algorithm and curve identifiers met here.
fn oid_name(oid: &str) -> String {
    let name = match oid {
        "1.2.840.113549.1.1.1" => "RSA",
        "1.2.840.113549.1.1.10" => "RSASSA-PSS",
        "1.2.840.10045.2.1" => "EC",
        "1.2.840.10040.4.1" => "DSA",
        "1.3.101.110" => "X25519",
        "1.3.101.111" => "X448",
        "1.3.101.112" => "Ed25519",
        "1.3.101.113" => "Ed448",
        "1.2.840.10045.3.1.7" => "P-256",
        "1.3.132.0.34" => "P-384",
        "1.3.132.0.35" => "P-521",
        "1.3.132.0.10" => "secp256k1",
        "1.3.36.3.3.2.8.1.1.7" => "brainpoolP256r1",
        "1.3.36.3.3.2.8.1.1.11" => "brainpoolP384r1",
        "1.3.36.3.3.2.8.1.1.13" => "brainpoolP512r1",
        "1.2.840.113549.1.5.13" => "PBES2",
        "1.2.840.113549.1.5.12" => "PBKDF2",
        "1.2.840.113549.2.7" => "hmacWithSHA1",
        "1.2.840.113549.2.9" => "hmacWithSHA256",
        "1.2.840.113549.2.10" => "hmacWithSHA384",
        "1.2.840.113549.2.11" => "hmacWithSHA512",
        "2.16.840.1.101.3.4.1.2" => "aes-128-cbc",
        "2.16.840.1.101.3.4.1.22" => "aes-192-cbc",
        "2.16.840.1.101.3.4.1.42" => "aes-256-cbc",
        "1.2.840.113549.3.7" => "des-ede3-cbc",
        "1.2.840.113549.1.12.1.1" => "pbeWithSHAAnd128BitRC4",
        "1.2.840.113549.1.12.1.3" => "pbeWithSHAAnd3-KeyTripleDES-CBC",
        "1.2.840.113549.1.12.1.6" => "pbeWithSHAAnd40BitRC2-CBC",
        "1.3.14.3.2.26" => "sha1",
        "2.16.840.1.101.3.4.2.1" => "sha256",
        "2.16.840.1.101.3.4.2.2" => "sha384",
        "2.16.840.1.101.3.4.2.3" => "sha512",
        _ => return oid.to_string(),
    };
    name.to_string()
}

/// Bits of a big-endian unsigned integer.
fn bits(int: &[u8]) -> usize {
    let int = &int[int.iter().position(|&b| b != 0).unwrap_or(int.len())..];
    match int.first() {
        Some(&b) => (int.len() - 1) * 8 + (8 - b.leading_zeros() as usize),
        None => 0,
    }
}

/// `RSA 2048 bits`, `EC P-256`, `Ed25519` from an AlgorithmIdentifier and the key bytes.
fn key_type(algorithm: Der, key: Option<&[u8]>) -> String {
    let mut parts = items(algorithm.body);
    let oid = parts.next().filter(|o| o.tag == 0x06).map(|o| oid_text(o.body)).unwrap_or_default();
    let params = parts.next();
    let name = oid_name(&oid);
    match name.as_str() {
        "RSA" | "RSASSA-PSS" => {
            // RSAPublicKey and RSAPrivateKey: the modulus is the first integer that is not the version.
            let modulus =
                key.and_then(|k| tlv(k)).and_then(|(s, _)| items(s.body).find(|i| i.tag == 0x02 && i.body.len() > 8));
            match modulus {
                Some(m) => format!("{name} {} bits", bits(m.body)),
                None => name,
            }
        }
        "EC" => match params.filter(|p| p.tag == 0x06) {
            Some(curve) => format!("EC {}", oid_name(&oid_text(curve.body))),
            None => "EC (explicit curve parameters)".into(),
        },
        "DSA" => {
            let p = params.and_then(|p| items(p.body).next()).map(|p| bits(p.body));
            p.map_or(name.clone(), |b| format!("DSA {b} bits"))
        }
        _ => name,
    }
}

/// Key type of a SubjectPublicKeyInfo.
fn spki_type(spki: &[u8]) -> Option<String> {
    let (seq, _) = tlv(spki)?;
    let mut it = items(seq.body);
    let alg = it.next().filter(|a| a.tag == 0x30)?;
    let key = it.next().filter(|k| k.tag == 0x03)?;
    Some(key_type(alg, key.body.get(1..)))
}

fn fingerprint(der: &[u8]) -> String {
    hex_colons(&Sha256::digest(der))
}

fn hex_colons(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(':');
        }
        let _ = write!(s, "{b:02X}");
    }
    s
}

// ---- Objects in a file ----

enum Object<'a> {
    Certificate(&'a [u8]),
    Request(&'a [u8]),
    Crl(&'a [u8]),
    PublicKey(&'a [u8]),
    /// PKCS#8, PKCS#1 RSA or SEC1 EC private key; only its type is shown.
    PrivateKey(&'a [u8], KeyForm),
    EncryptedKey(&'a [u8]),
    Unknown(String, usize),
}

#[derive(Clone, Copy)]
enum KeyForm {
    Pkcs8,
    Rsa,
    Ec,
}

/// What a DER object is, by trying the structures in turn.
fn classify_der(der: &[u8]) -> Object<'_> {
    if X509Certificate::from_der(der).is_ok() {
        return Object::Certificate(der);
    }
    if CertificateRevocationList::from_der(der).is_ok() {
        return Object::Crl(der);
    }
    if X509CertificationRequest::from_der(der).is_ok() {
        return Object::Request(der);
    }
    if spki_type(der).is_some() {
        return Object::PublicKey(der);
    }
    let Some((seq, _)) = tlv(der) else { return Object::Unknown("DER".into(), der.len()) };
    let kids: Vec<Der> = items(seq.body).take(4).collect();
    match kids.as_slice() {
        [v, a, k, ..] if v.tag == 0x02 && a.tag == 0x30 && k.tag == 0x04 => Object::PrivateKey(der, KeyForm::Pkcs8),
        [a, k] if a.tag == 0x30 && k.tag == 0x04 => Object::EncryptedKey(der),
        [v, k, ..] if v.tag == 0x02 && k.tag == 0x04 => Object::PrivateKey(der, KeyForm::Ec),
        [v, n, ..] if v.tag == 0x02 && n.tag == 0x02 => Object::PrivateKey(der, KeyForm::Rsa),
        _ => Object::Unknown("DER".into(), der.len()),
    }
}

/// PEM blocks: label, decoded bytes, whether OpenSSL header encryption is used.
fn pem_blocks(text: &[u8]) -> Vec<(String, Option<Vec<u8>>, bool)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while out.len() < MAX_OBJECTS
        && let Some(start) = memchr::memmem::find(&text[pos..], b"-----BEGIN ")
    {
        let label_start = pos + start + 11;
        let Some(label_len) = memchr::memmem::find(&text[label_start..], b"-----") else { break };
        let label = String::from_utf8_lossy(&text[label_start..label_start + label_len]).into_owned();
        let body_start = label_start + label_len + 5;
        let end_marker = format!("-----END {label}-----");
        let Some(body_len) = memchr::memmem::find(&text[body_start..], end_marker.as_bytes()) else {
            out.push((label, None, false));
            break;
        };
        let body = &text[body_start..body_start + body_len];
        let mut encrypted = false;
        let mut b64 = Vec::with_capacity(body.len());
        for line in body.split(|&b| b == b'\n') {
            if line.contains(&b':') {
                encrypted |=
                    line.starts_with(b"Proc-Type:") && line.ends_with(b"ENCRYPTED") || line.starts_with(b"DEK-Info:");
                continue;
            }
            b64.extend(line.iter().filter(|b| !b.is_ascii_whitespace()));
        }
        out.push((label, base64::engine::general_purpose::STANDARD.decode(&b64).ok(), encrypted));
        pos = body_start + body_len + end_marker.len();
    }
    out
}

fn line(out: &mut String, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:<22} {value}", format!("{label}:"));
}

/// `2026-03-01 00:00:00 UTC`.
fn time(t: ASN1Time) -> String {
    let secs = t.timestamp();
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", rem / 3600, rem / 60 % 60, rem % 60)
}

fn general_name(n: &GeneralName) -> String {
    match n {
        GeneralName::DNSName(s) => format!("DNS:{s}"),
        GeneralName::RFC822Name(s) => format!("email:{s}"),
        GeneralName::URI(s) => format!("URI:{s}"),
        GeneralName::IPAddress(b) if b.len() == 4 => format!("IP:{}", std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3])),
        GeneralName::IPAddress(b) if b.len() == 16 => {
            let a: [u8; 16] = (*b).try_into().unwrap_or([0; 16]);
            format!("IP:{}", std::net::Ipv6Addr::from(a))
        }
        GeneralName::DirectoryName(name) => format!("DirName:{name}"),
        other => format!("{other:?}"),
    }
}

fn signature_name(alg: &x509_parser::x509::AlgorithmIdentifier) -> String {
    let oid = &alg.algorithm;
    x509_parser::objects::oid2sn(oid, x509_parser::objects::oid_registry())
        .map_or_else(|_| oid.to_id_string(), str::to_string)
}

/// The extensions worth reading, one line each, then the names of the others.
fn extensions<'a>(
    out: &mut String,
    exts: impl Iterator<Item = (&'a ParsedExtension<'a>, Option<&'a X509Extension<'a>>)>,
) {
    let mut others = Vec::new();
    for (parsed, raw) in exts {
        let critical = if raw.is_some_and(|r| r.critical) { " (critical)" } else { "" };
        match parsed {
            ParsedExtension::SubjectAlternativeName(san) => {
                for n in &san.general_names {
                    line(out, "Alternative name", general_name(n));
                }
            }
            ParsedExtension::KeyUsage(ku) => line(out, "Key usage", format_args!("{ku}{critical}")),
            ParsedExtension::ExtendedKeyUsage(eku) => {
                let mut names = Vec::new();
                for (on, name) in [
                    (eku.any, "any"),
                    (eku.server_auth, "TLS server"),
                    (eku.client_auth, "TLS client"),
                    (eku.code_signing, "code signing"),
                    (eku.email_protection, "e-mail protection"),
                    (eku.time_stamping, "time stamping"),
                    (eku.ocsp_signing, "OCSP signing"),
                ] {
                    if on {
                        names.push(name.to_string());
                    }
                }
                names.extend(eku.other.iter().map(|o| o.to_id_string()));
                line(out, "Extended key usage", format_args!("{}{critical}", names.join(", ")));
            }
            ParsedExtension::BasicConstraints(bc) => {
                let path = bc.path_len_constraint.map(|n| format!(", path length {n}")).unwrap_or_default();
                line(
                    out,
                    "Basic constraints",
                    format_args!("CA: {}{path}{critical}", if bc.ca { "yes" } else { "no" }),
                );
            }
            ParsedExtension::SubjectKeyIdentifier(id) => line(out, "Subject key id", hex_colons(id.0)),
            ParsedExtension::AuthorityKeyIdentifier(aki) => {
                if let Some(id) = &aki.key_identifier {
                    line(out, "Authority key id", hex_colons(id.0));
                }
            }
            _ => {
                if let Some(raw) = raw {
                    let oid = &raw.oid;
                    let name = x509_parser::objects::oid2sn(oid, x509_parser::objects::oid_registry())
                        .map_or_else(|_| oid.to_id_string(), str::to_string);
                    others.push(name);
                }
            }
        }
    }
    if !others.is_empty() {
        others.sort();
        line(out, "Other extensions", others.join(", "));
    }
}

fn subject_title(name: &x509_parser::x509::X509Name) -> String {
    name.iter_common_name().next().and_then(|cn| cn.as_str().ok()).map_or_else(|| name.to_string(), str::to_string)
}

fn certificate(out: &mut String, der: &[u8]) {
    let Ok((_, cert)) = X509Certificate::from_der(der) else {
        let _ = writeln!(out, "=== Certificate ===\n! not readable");
        return;
    };
    let _ = writeln!(out, "=== Certificate: {} ===", subject_title(cert.subject()));
    line(out, "Subject", cert.subject());
    line(out, "Issuer", cert.issuer());
    line(out, "Serial", cert.raw_serial_as_string().to_uppercase());
    line(out, "Not before", time(cert.validity().not_before));
    line(out, "Not after", time(cert.validity().not_after));
    line(out, "Key", spki_type(cert.public_key().raw).unwrap_or_else(|| "?".into()));
    line(out, "Signature", signature_name(&cert.signature_algorithm));
    if cert.subject() == cert.issuer() {
        line(out, "Self-signed", "yes");
    }
    extensions(out, cert.extensions().iter().map(|e| (e.parsed_extension(), Some(e))));
    line(out, "SHA-256 fingerprint", fingerprint(der));
}

fn request(out: &mut String, der: &[u8]) {
    let Ok((_, csr)) = X509CertificationRequest::from_der(der) else { return };
    let info = &csr.certification_request_info;
    let _ = writeln!(out, "=== Certificate request: {} ===", subject_title(&info.subject));
    line(out, "Subject", &info.subject);
    line(out, "Key", spki_type(info.subject_pki.raw).unwrap_or_else(|| "?".into()));
    line(out, "Signature", signature_name(&csr.signature_algorithm));
    if let Some(exts) = csr.requested_extensions() {
        extensions(out, exts.map(|e| (e, None)));
    }
    line(out, "SHA-256 fingerprint", fingerprint(der));
}

fn reason(code: u8) -> &'static str {
    match code {
        1 => "key compromise",
        2 => "CA compromise",
        3 => "affiliation changed",
        4 => "superseded",
        5 => "cessation of operation",
        6 => "certificate hold",
        8 => "remove from CRL",
        9 => "privilege withdrawn",
        10 => "AA compromise",
        _ => "unspecified",
    }
}

fn crl(out: &mut String, der: &[u8]) {
    let Ok((_, crl)) = CertificateRevocationList::from_der(der) else { return };
    let _ = writeln!(out, "=== Revocation list: {} ===", subject_title(crl.issuer()));
    line(out, "Issuer", crl.issuer());
    if let Some(n) = crl.crl_number() {
        line(out, "CRL number", n);
    }
    line(out, "This update", time(crl.last_update()));
    if let Some(next) = crl.next_update() {
        line(out, "Next update", time(next));
    }
    line(out, "Signature", signature_name(&crl.signature_algorithm));
    let mut revoked: Vec<String> = crl
        .iter_revoked_certificates()
        .map(|r| {
            let why = r.reason_code().map(|(_, c)| format!(", {}", reason(c.0))).unwrap_or_default();
            format!("{} since {}{why}", r.raw_serial_as_string().to_uppercase(), time(r.revocation_date))
        })
        .collect();
    line(out, "Revoked", revoked.len());
    revoked.sort();
    let total = revoked.len();
    for r in revoked.iter().take(MAX_REVOKED) {
        line(out, "Revoked serial", r);
    }
    if total > MAX_REVOKED {
        let _ = writeln!(out, "… {} more revoked serials", total - MAX_REVOKED);
    }
    line(out, "SHA-256 fingerprint", fingerprint(der));
}

/// The type of a private key, never its contents.
fn private_key_type(der: &[u8], form: KeyForm) -> String {
    let Some((seq, _)) = tlv(der) else { return "?".into() };
    let kids: Vec<Der> = items(seq.body).collect();
    match form {
        KeyForm::Pkcs8 => match kids.as_slice() {
            [_, alg, key, ..] => key_type(*alg, Some(key.body)),
            _ => "?".into(),
        },
        KeyForm::Rsa => kids.get(1).map_or("RSA".into(), |n| format!("RSA {} bits", bits(n.body))),
        KeyForm::Ec => {
            // ECPrivateKey: version, key, [0] curve, [1] public key.
            let curve = kids.iter().find(|k| k.tag == 0xA0).and_then(|p| tlv(p.body)).filter(|(o, _)| o.tag == 0x06);
            curve.map_or("EC".into(), |(o, _)| format!("EC {}", oid_name(&oid_text(o.body))))
        }
    }
}

/// PBES2 or PKCS#12 PBE parameters in words.
fn encryption_name(alg: Der) -> String {
    let mut it = items(alg.body);
    let oid = it.next().map(|o| oid_text(o.body)).unwrap_or_default();
    let name = oid_name(&oid);
    if name != "PBES2" {
        return name;
    }
    match pbes2_params(it.next()) {
        Some(p) => format!("PBES2 (PBKDF2 {}, {}, {} iterations)", p.prf, p.cipher, p.iterations),
        None => name,
    }
}

fn private_key(out: &mut String, der: &[u8], form: KeyForm) {
    let _ = writeln!(out, "=== Private key ===");
    line(out, "Key", private_key_type(der, form));
    line(
        out,
        "Format",
        match form {
            KeyForm::Pkcs8 => "PKCS#8",
            KeyForm::Rsa => "PKCS#1",
            KeyForm::Ec => "SEC1",
        },
    );
}

fn render_object(out: &mut String, label: &str, object: &Object) {
    match *object {
        Object::Certificate(der) => certificate(out, der),
        Object::Request(der) => request(out, der),
        Object::Crl(der) => crl(out, der),
        Object::PublicKey(der) => {
            let _ = writeln!(out, "=== Public key ===");
            line(out, "Key", spki_type(der).unwrap_or_default());
            line(out, "SHA-256 fingerprint", fingerprint(der));
        }
        Object::PrivateKey(der, form) => private_key(out, der, form),
        Object::EncryptedKey(der) => {
            let _ = writeln!(out, "=== Encrypted private key ===");
            let alg = tlv(der).and_then(|(s, _)| items(s.body).next());
            line(out, "Encryption", alg.map_or("?".into(), encryption_name));
        }
        Object::Unknown(ref what, len) => {
            let _ = writeln!(out, "=== {label} ===");
            line(out, "Content", format_args!("{what}, {len} bytes, not decoded"));
        }
    }
}

/// The objects of a PEM file, by label, with the DER they compare by.
fn pem_objects(blocks: &[(String, Option<Vec<u8>>, bool)]) -> Vec<(&str, Object<'_>)> {
    blocks
        .iter()
        .map(|(label, der, encrypted)| {
            let object = match (label.as_str(), der) {
                (_, None) => Object::Unknown("damaged base64".into(), 0),
                (_, Some(der)) if *encrypted => Object::Unknown(format!("encrypted {label}"), der.len()),
                ("CERTIFICATE" | "X509 CERTIFICATE", Some(der)) => Object::Certificate(der),
                // OpenSSL's trusted certificate: the certificate followed by trust settings.
                ("TRUSTED CERTIFICATE", Some(der)) => match tlv(der) {
                    Some((cert, _)) => Object::Certificate(cert.raw),
                    None => Object::Unknown("damaged".into(), der.len()),
                },
                ("CERTIFICATE REQUEST" | "NEW CERTIFICATE REQUEST", Some(der)) => Object::Request(der),
                ("X509 CRL", Some(der)) => Object::Crl(der),
                ("PUBLIC KEY", Some(der)) => Object::PublicKey(der),
                ("PRIVATE KEY", Some(der)) => Object::PrivateKey(der, KeyForm::Pkcs8),
                ("RSA PRIVATE KEY", Some(der)) => Object::PrivateKey(der, KeyForm::Rsa),
                ("EC PRIVATE KEY", Some(der)) => Object::PrivateKey(der, KeyForm::Ec),
                ("ENCRYPTED PRIVATE KEY", Some(der)) => Object::EncryptedKey(der),
                (_, Some(der)) => Object::Unknown(label.clone(), der.len()),
            };
            (label.as_str(), object)
        })
        .collect()
}

/// An OpenSSH private key (`openssh-key-v1`): the key type and whether it is encrypted.
/// Layout: magic, cipher, KDF, KDF options, key count (u32), then each public key blob.
fn openssh(out: &mut String, data: &[u8]) {
    fn take<'a>(rest: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
        let v = rest.get(..n)?;
        *rest = &rest[n..];
        Some(v)
    }
    fn string<'a>(rest: &mut &'a [u8]) -> Option<&'a [u8]> {
        let len = u32::from_be_bytes(take(rest, 4)?.try_into().ok()?) as usize;
        take(rest, len)
    }
    let _ = writeln!(out, "=== OpenSSH private key ===");
    let mut rest = data.strip_prefix(b"openssh-key-v1\0").unwrap_or_default();
    let cipher = string(&mut rest);
    let key_type = (|| {
        string(&mut rest)?;
        string(&mut rest)?;
        take(&mut rest, 4)?;
        let mut blob = string(&mut rest)?;
        string(&mut blob)
    })();
    line(out, "Key", key_type.map_or("?".into(), String::from_utf8_lossy));
    line(
        out,
        "Encrypted",
        match cipher {
            Some(b"none") => "no".to_string(),
            Some(c) => format!("yes ({})", String::from_utf8_lossy(c)),
            None => "?".into(),
        },
    );
}

/// The certificate view of a PEM file.
pub fn pem(data: &[u8]) -> Result<String> {
    let blocks = pem_blocks(data);
    if blocks.is_empty() {
        return Err(Error::Unsupported("no PEM blocks".into()));
    }
    let mut out = String::new();
    for (i, (label, object)) in pem_objects(&blocks).iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        match (label, &blocks[i].1) {
            (&"OPENSSH PRIVATE KEY", Some(der)) => openssh(&mut out, der),
            _ => render_object(&mut out, label, object),
        }
    }
    if blocks.len() >= MAX_OBJECTS {
        let _ = writeln!(out, "\n… only the first {MAX_OBJECTS} objects are shown");
    }
    Ok(out)
}

/// The certificate view of a DER file.
pub fn der(data: &[u8]) -> Result<String> {
    let mut out = String::new();
    render_object(&mut out, "DER", &classify_der(data));
    Ok(out)
}

/// The DER objects of a PEM or DER file (private keys included: equality is all that is used).
fn der_list(data: &[u8], is_pem: bool) -> Option<Vec<Vec<u8>>> {
    if is_pem {
        let blocks = pem_blocks(data);
        let list: Option<Vec<Vec<u8>>> = blocks.into_iter().map(|(_, der, enc)| der.filter(|_| !enc)).collect();
        list.filter(|l| !l.is_empty())
    } else {
        (!matches!(classify_der(data), Object::Unknown(..))).then(|| vec![data.to_vec()])
    }
}

/// Whether two PEM or DER files hold the same objects (certificates, keys …), byte
/// for byte in their DER form. `None` when either is unreadable.
pub fn same_objects(left: (&[u8], bool), right: (&[u8], bool)) -> Option<bool> {
    Some(der_list(left.0, left.1)? == der_list(right.0, right.1)?)
}

// ---- PKCS#12 ----

struct Pbes2 {
    salt: Vec<u8>,
    iterations: u32,
    prf: String,
    cipher: String,
    iv: Vec<u8>,
}

fn int_value(body: &[u8]) -> Option<u32> {
    if body.is_empty() || body.len() > 5 || body[0] & 0x80 != 0 {
        return None;
    }
    let v = body.iter().fold(0u64, |a, &b| (a << 8) | u64::from(b));
    u32::try_from(v).ok()
}

/// PBES2-params: key derivation (PBKDF2 salt, iterations, PRF) and cipher with IV.
fn pbes2_params(params: Option<Der>) -> Option<Pbes2> {
    let mut it = items(params?.body);
    let kdf = it.next()?;
    let scheme = it.next()?;
    let mut kdf_it = items(kdf.body);
    if oid_name(&oid_text(kdf_it.next()?.body)) != "PBKDF2" {
        return None;
    }
    let mut p = items(kdf_it.next()?.body);
    let salt = p.next().filter(|s| s.tag == 0x04)?.body.to_vec();
    let iterations = int_value(p.next().filter(|i| i.tag == 0x02)?.body)?;
    let mut prf = "hmacWithSHA1".to_string();
    for rest in p {
        if rest.tag == 0x30 {
            prf = oid_name(&items(rest.body).next().map(|o| oid_text(o.body)).unwrap_or_default());
        }
    }
    let mut s = items(scheme.body);
    let cipher = oid_name(&oid_text(s.next()?.body));
    let iv = s.next().filter(|v| v.tag == 0x04)?.body.to_vec();
    Some(Pbes2 { salt, iterations, prf, cipher, iv })
}

/// Decrypts PBES2 data with the empty password; `None` when the scheme is not
/// supported or the password is not empty.
fn decrypt_empty(alg: Der, data: &[u8]) -> Option<Vec<u8>> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    let mut it = items(alg.body);
    if oid_name(&oid_text(it.next()?.body)) != "PBES2" {
        return None;
    }
    let p = pbes2_params(it.next())?;
    if p.iterations > MAX_ITERATIONS || p.iv.len() != 16 {
        return None;
    }
    let key_len = match p.cipher.as_str() {
        "aes-128-cbc" => 16,
        "aes-192-cbc" => 24,
        "aes-256-cbc" => 32,
        _ => return None,
    };
    let mut key = vec![0u8; key_len];
    match p.prf.as_str() {
        "hmacWithSHA256" => pbkdf2::pbkdf2_hmac::<Sha256>(b"", &p.salt, p.iterations, &mut key),
        "hmacWithSHA384" => pbkdf2::pbkdf2_hmac::<sha2::Sha384>(b"", &p.salt, p.iterations, &mut key),
        "hmacWithSHA512" => pbkdf2::pbkdf2_hmac::<sha2::Sha512>(b"", &p.salt, p.iterations, &mut key),
        // hmacWithSHA1 (rare with PBES2) would need another hash crate.
        _ => return None,
    }
    let mut buf = data.to_vec();
    let plain_len = match key_len {
        16 => cbc::Decryptor::<aes::Aes128>::new_from_slices(&key, &p.iv)
            .ok()?
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .ok()?
            .len(),
        24 => cbc::Decryptor::<aes::Aes192>::new_from_slices(&key, &p.iv)
            .ok()?
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .ok()?
            .len(),
        _ => cbc::Decryptor::<aes::Aes256>::new_from_slices(&key, &p.iv)
            .ok()?
            .decrypt_padded_mut::<Pkcs7>(&mut buf)
            .ok()?
            .len(),
    };
    buf.truncate(plain_len);
    // A wrong password gives garbage that is very unlikely to be one DER element.
    tlv(&buf).filter(|(d, rest)| d.tag == 0x30 && rest.is_empty())?;
    Some(buf)
}

const OID_DATA: &str = "1.2.840.113549.1.7.1";
const OID_ENCRYPTED_DATA: &str = "1.2.840.113549.1.7.6";

/// The PKCS#12 view.
pub fn pkcs12(data: &[u8]) -> Result<String> {
    let bad = || Error::Unsupported("not a readable PKCS#12 file".into());
    let (pfx, _) = tlv(data).ok_or_else(bad)?;
    let mut it = items(pfx.body);
    let version = it.next().filter(|v| v.tag == 0x02).and_then(|v| int_value(v.body)).ok_or_else(bad)?;
    let auth_safe = it.next().ok_or_else(bad)?;
    let mac = it.next();
    let mut out = String::new();
    line(&mut out, "PKCS#12", format_args!("version {version}"));
    match mac {
        Some(mac) => {
            // MacData: DigestInfo (algorithm, digest), salt, iterations.
            let mut m = items(mac.body);
            let digest = m
                .next()
                .and_then(|d| items(d.body).next())
                .and_then(|a| items(a.body).next())
                .map(|o| oid_name(&oid_text(o.body)));
            let iterations = m.nth(1).and_then(|i| int_value(i.body)).unwrap_or(1);
            line(&mut out, "Integrity", format_args!("HMAC {}, {iterations} iterations", digest.unwrap_or_default()));
        }
        None => line(&mut out, "Integrity", "none (no MAC)"),
    }
    // ContentInfo { data, [0] OCTET STRING { AuthenticatedSafe } }
    let content = content_octets(auth_safe).ok_or_else(bad)?;
    let (safe, _) = tlv(&content).ok_or_else(bad)?;
    let mut sections = Vec::new();
    for info in items(safe.body) {
        let mut ci = items(info.body);
        let oid = ci.next().map(|o| oid_text(o.body)).unwrap_or_default();
        let body = ci.next();
        match oid.as_str() {
            OID_DATA => {
                if let Some(octets) = body.and_then(|b| tlv(b.body)).and_then(|(o, _)| octet_content(o)) {
                    sections.push(("unencrypted".to_string(), Some(octets)));
                }
            }
            OID_ENCRYPTED_DATA => {
                // [0] EncryptedData { version, EncryptedContentInfo { type, algorithm, [0] content } }
                let eci = body.and_then(|b| tlv(b.body)).and_then(|(ed, _)| items(ed.body).nth(1));
                let Some(eci) = eci else { continue };
                let mut e = items(eci.body);
                let (_, alg, enc) = (e.next(), e.next(), e.next());
                let Some(alg) = alg else { continue };
                let name = encryption_name(alg);
                let opened = enc.and_then(octet_content).and_then(|c| decrypt_empty(alg, &c));
                let how = if opened.is_some() {
                    format!("encrypted with {name}, opened with the empty password")
                } else {
                    format!("encrypted with {name}, needs a password")
                };
                sections.push((how, opened));
            }
            other => sections.push((format!("content type {other}, not decoded"), None)),
        }
    }
    for (how, content) in sections {
        let _ = writeln!(out, "\n--- {how} ---");
        let Some(content) = content else { continue };
        let Some((bags, _)) = tlv(&content) else { continue };
        for bag in items(bags.body).take(MAX_OBJECTS) {
            out.push('\n');
            safe_bag(&mut out, bag);
        }
    }
    Ok(out)
}

/// The bytes of a `[0] EXPLICIT OCTET STRING` or an implicit `[0]` (primitive or in pieces).
fn content_octets(info: Der) -> Option<Vec<u8>> {
    let mut it = items(info.body);
    let _oid = it.next()?;
    let explicit = it.next()?;
    octet_content(tlv(explicit.body)?.0)
}

fn octet_content(d: Der) -> Option<Vec<u8>> {
    match d.tag {
        0x04 | 0x80 => Some(d.body.to_vec()),
        0x24 | 0xA0 => Some(items(d.body).flat_map(|p| p.body.iter().copied()).collect()),
        _ => None,
    }
}

fn safe_bag(out: &mut String, bag: Der) {
    let mut it = items(bag.body);
    let kind = it.next().map(|o| oid_text(o.body)).unwrap_or_default();
    let value = it.next().and_then(|v| tlv(v.body)).map(|(v, _)| v);
    let attributes = it.next();
    match (kind.rsplit('.').next(), value) {
        (Some("3"), Some(v)) if kind.starts_with("1.2.840.113549.1.12.10.1.") => {
            // CertBag { x509Certificate, [0] OCTET STRING }
            let cert = items(v.body).nth(1).and_then(|c| tlv(c.body)).and_then(|(o, _)| octet_content(o));
            match cert {
                Some(der) => certificate(out, &der),
                None => {
                    let _ = writeln!(out, "=== Certificate ===\n! not decoded");
                }
            }
        }
        (Some("1"), Some(v)) if kind.starts_with("1.2.840.113549.1.12.10.1.") => {
            private_key(out, v.raw, KeyForm::Pkcs8)
        }
        (Some("2"), Some(v)) if kind.starts_with("1.2.840.113549.1.12.10.1.") => {
            let mut e = items(v.body);
            let alg = e.next();
            let enc = e.next();
            let opened = alg.zip(enc).and_then(|(a, c)| decrypt_empty(a, c.body));
            match opened {
                Some(key) => {
                    private_key(out, &key, KeyForm::Pkcs8);
                    line(out, "Protection", "encrypted, opened with the empty password");
                }
                None => {
                    let _ = writeln!(out, "=== Encrypted private key ===");
                    line(out, "Encryption", alg.map_or("?".into(), encryption_name));
                }
            }
        }
        _ => {
            let _ = writeln!(out, "=== Bag {kind} ===");
        }
    }
    // Attributes: friendly name (BMPString) and local key id.
    for attr in attributes.into_iter().flat_map(|a| items(a.body)) {
        let mut a = items(attr.body);
        let oid = a.next().map(|o| oid_text(o.body)).unwrap_or_default();
        let Some(value) = a.next().and_then(|set| tlv(set.body)).map(|(v, _)| v) else { continue };
        match oid.as_str() {
            "1.2.840.113549.1.9.20" => {
                let units: Vec<u16> = value.body.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                line(out, "Friendly name", String::from_utf16_lossy(&units));
            }
            "1.2.840.113549.1.9.21" => line(out, "Local key id", hex_colons(value.body)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cert/", $name))
        };
    }
    const SERVER: &[u8] = fixture!("server.pem");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn certificate_fields() {
        let v = pem(SERVER).unwrap();
        for expected in [
            "=== Certificate: shop.example.com ===\n",
            "Subject:               C=DE, O=Parallax Test, CN=shop.example.com\n",
            "Issuer:                C=DE, O=Parallax Test, CN=Parallax Test Root CA\n",
            "Serial:                10:01\n",
            "Not before:            2026-03-01 00:00:00 UTC\n",
            "Not after:             2027-03-01 00:00:00 UTC\n",
            "Key:                   RSA 2048 bits\n",
            "Signature:             ecdsa-with-SHA256\n",
            "Alternative name:      DNS:shop.example.com\n",
            "Alternative name:      IP:192.0.2.10\n",
            "Alternative name:      email:admin@example.com\n",
            "Key usage:             Digital Signature, Key Encipherment (critical)\n",
            "Extended key usage:    TLS server, TLS client\n",
            "Basic constraints:     CA: no\n",
            "SHA-256 fingerprint:   ",
        ] {
            assert!(v.contains(expected), "{expected}\n---\n{v}");
        }
        let ca = pem(fixture!("ca.pem")).unwrap();
        assert!(
            ca.contains("Key:                   EC P-256\n") && ca.contains("Self-signed:           yes\n"),
            "{ca}"
        );
        assert!(ca.contains("Basic constraints:     CA: yes, path length 0 (critical)\n"), "{ca}");
        // The openssl -text form around the PEM block changes nothing.
        assert_eq!(pem(fixture!("server-with-text.pem")).unwrap(), v);
        assert_eq!(der(fixture!("server.der")).unwrap(), v);
    }

    #[test]
    fn renewal_changes_only_dates_serial_and_fingerprint() {
        let (a, b) = (pem(SERVER).unwrap(), pem(fixture!("server-renewed.pem")).unwrap());
        let labels: Vec<&str> = lines_only_in(&a, &b).iter().map(|l| l.split(':').next().unwrap()).collect();
        assert_eq!(labels, ["Serial", "Not before", "Not after", "SHA-256 fingerprint"]);
    }

    #[test]
    fn requests_crls_and_keys() {
        let csr = pem(fixture!("server.csr")).unwrap();
        assert!(
            csr.contains("=== Certificate request: shop.example.com ===")
                && csr.contains("Alternative name:      DNS:www.shop.example.com"),
            "{csr}"
        );
        let crl = pem(fixture!("ca.crl.pem")).unwrap();
        assert!(
            crl.contains("Revoked:               1\n")
                && crl.contains("Revoked serial:        10:01 since ")
                && crl.contains(", key compromise\n"),
            "{crl}"
        );
        let key = |f: &[u8]| pem(f).unwrap();
        assert_eq!(
            key(fixture!("server.key")),
            "=== Private key ===\nKey:                   RSA 2048 bits\nFormat:                PKCS#8\n"
        );
        assert!(key(fixture!("rsa-pkcs1.key")).contains("RSA 2048 bits\nFormat:                PKCS#1"));
        assert!(key(fixture!("ec-sec1.key")).contains("Key:                   EC P-384\n"));
        assert!(key(fixture!("ed25519.key")).contains("Key:                   Ed25519\n"));
        assert!(key(fixture!("server.pub.pem")).contains("=== Public key ===\nKey:                   RSA 2048 bits\n"));
        let enc = key(fixture!("encrypted.key"));
        assert!(
            enc.contains("Encryption:            PBES2 (PBKDF2 hmacWithSHA256, aes-256-cbc, 2048 iterations)"),
            "{enc}"
        );
        let ssh = key(fixture!("openssh.key"));
        assert_eq!(ssh, "=== OpenSSH private key ===\nKey:                   ssh-ed25519\nEncrypted:             no\n");
        // No private key material, whatever the container.
        for f in [
            &fixture!("server.key")[..],
            fixture!("rsa-pkcs1.key"),
            fixture!("ec-sec1.key"),
            fixture!("empty-password.p12"),
        ] {
            let v = if f[0] == 0x30 { pkcs12(f).unwrap() } else { pem(f).unwrap() };
            assert!(!v.contains("Modulus") && v.lines().all(|l| l.len() < 200), "{v}");
        }
    }

    #[test]
    fn pkcs12_with_and_without_password() {
        let empty = pkcs12(fixture!("empty-password.p12")).unwrap();
        assert!(empty.contains("Integrity:             HMAC sha256, 2048 iterations\n"), "{empty}");
        assert!(empty.contains("--- encrypted with PBES2 (PBKDF2 hmacWithSHA256, aes-256-cbc, 2048 iterations), opened with the empty password ---"), "{empty}");
        assert!(
            empty.contains("=== Certificate: shop.example.com ===")
                && empty.contains("=== Certificate: Parallax Test Root CA ==="),
            "{empty}"
        );
        assert!(empty.contains("Key:                   RSA 2048 bits\nFormat:                PKCS#8\nProtection:            encrypted, opened with the empty password\nFriendly name:         shop\n"), "{empty}");
        let locked = pkcs12(fixture!("bundle.p12")).unwrap();
        assert!(locked.contains("needs a password ---") && !locked.contains("=== Certificate"), "{locked}");
        let legacy = pkcs12(fixture!("legacy.p12")).unwrap();
        assert!(legacy.contains("encrypted with pbeWithSHAAnd40BitRC2-CBC, needs a password"), "{legacy}");
        let open = pkcs12(fixture!("unencrypted.p12")).unwrap();
        assert!(
            open.contains("Integrity:             none (no MAC)")
                && open.contains("--- unencrypted ---\n\n=== Certificate: shop.example.com"),
            "{open}"
        );
    }

    #[test]
    fn same_certificates_in_other_encodings() {
        let der_file = fixture!("server.der");
        assert_eq!(same_objects((SERVER, true), (der_file, false)), Some(true));
        assert_eq!(same_objects((SERVER, true), (fixture!("server-with-text.pem"), true)), Some(true));
        assert_eq!(same_objects((SERVER, true), (fixture!("server-renewed.pem"), true)), Some(false));
        assert_eq!(same_objects((SERVER, true), (fixture!("chain.pem"), true)), Some(false));
        assert_eq!(same_objects((SERVER, true), (b"garbage", true)), None);
    }

    #[test]
    fn detection_and_damaged_input() {
        assert!(is_pem("pem", SERVER) && is_pem("txt", SERVER) && is_pem("crt", fixture!("server-with-text.pem")));
        assert!(!is_pem("txt", fixture!("server-with-text.pem")));
        assert!(!is_pem("asc", b"-----BEGIN PGP MESSAGE-----\nabc\n-----END PGP MESSAGE-----\n"));
        assert!(is_der("cer", fixture!("server.der")) && !is_der("cer", b"\x30\x03\x02\x01\x01"));
        assert!(is_pkcs12("p12", fixture!("bundle.p12")) && !is_pkcs12("p12", SERVER));
        for data in [SERVER, &fixture!("server.der")[..], fixture!("empty-password.p12"), fixture!("chain.pem")] {
            for n in [0, 1, 2, 10, data.len() / 2, data.len() - 1] {
                let _ = pem(&data[..n]);
                let _ = der(&data[..n]);
                let _ = pkcs12(&data[..n]);
            }
        }
        assert!(
            pem(b"-----BEGIN CERTIFICATE-----\n!!!!\n-----END CERTIFICATE-----\n").unwrap().contains("damaged base64")
        );
        assert!(tlv(b"\x30\x84\xff\xff\xff\xff").is_none());
    }
}
