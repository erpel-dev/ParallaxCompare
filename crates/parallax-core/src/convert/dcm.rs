//! DCM 2.x calibration exports: named values, arrays, curves, maps and axes.
//!
//! Values and axes keep their order, while calibration objects and known metadata
//! fields are sorted. Unknown statements remain visible and disable the similar
//! rule: a vendor extension must not accidentally become proof of equivalence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::{Error, Result};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_VALUES: usize = 2_000_000;
const KINDS: &[&str] = &[
    "FESTWERT",
    "TEXTSTRING",
    "FESTWERTEBLOCK",
    "KENNLINIE",
    "FESTKENNLINIE",
    "GRUPPENKENNLINIE",
    "KENNFELD",
    "FESTKENNFELD",
    "GRUPPENKENNFELD",
    "STUETZSTELLENVERTEILUNG",
];
const METADATA: &[&str] = &[
    "LANGNAME",
    "DISPLAYNAME",
    "FUNKTION",
    "EINHEIT_W",
    "EINHEIT_X",
    "EINHEIT_Y",
    "VAR",
    "SSTX",
    "SSTY",
    "S_SST_X",
    "S_SST_Y",
];

#[derive(Clone, Debug)]
struct Token {
    value: String,
    quoted: bool,
}

impl Token {
    fn shown(&self) -> String {
        if self.quoted { quoted(&self.value) } else { self.value.clone() }
    }
}

struct Object {
    kind: String,
    name: String,
    dims: Vec<usize>,
    metadata: BTreeMap<String, Vec<Vec<Token>>>,
    values: Vec<String>,
    x: Vec<String>,
    y: Vec<String>,
    /// Starting value offset of each explicit ST/Y row, checked against its width.
    rows: Vec<usize>,
}

struct Document {
    header: Vec<Vec<Token>>,
    objects: Vec<Object>,
    understood: bool,
    statements: Vec<String>,
}

fn bad(line: usize, message: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("DCM line {line}: {message}"))
}

fn quoted(s: &str) -> String {
    // JSON quoting makes line breaks, backslashes and quotes unambiguous in a view.
    serde_json::to_string(s).expect("strings can be serialized")
}

/// This signature distinguishes calibration DCM from DICOM and unrelated .dcm files.
pub fn is_dcm(data: &[u8]) -> bool {
    let head = crate::text::decode(&data[..data.len().min(64 * 1024)]);
    head.text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .any(|line| line.strip_prefix("KONSERVIERUNG_FORMAT").is_some_and(|tail| tail.starts_with(char::is_whitespace)))
}

pub fn values(text: &str) -> Result<String> {
    Ok(parse(text)?.render())
}

pub fn same_values(left: &str, right: &str) -> Option<bool> {
    let (left, right) = (parse(left).ok()?, parse(right).ok()?);
    (left.understood && right.understood).then(|| left.render() == right.render())
}

/// Exact decimal normalization: do not round calibration integers through f64.
pub(super) fn number(s: &str) -> Option<String> {
    let (negative, s) =
        if let Some(s) = s.strip_prefix('-') { (true, s) } else { (false, s.strip_prefix('+').unwrap_or(s)) };
    let (mantissa, exponent) =
        s.split_once(['e', 'E']).map_or(Some((s, 0i64)), |(m, e)| Some((m, e.parse::<i64>().ok()?)))?;
    let mut digits = String::new();
    let mut fraction = None;
    for c in mantissa.chars() {
        match c {
            '0'..='9' => digits.push(c),
            '.' if fraction.is_none() => fraction = Some(digits.len()),
            _ => return None,
        }
    }
    if digits.is_empty() {
        return None;
    }
    let decimals = fraction.map_or(0, |at| digits.len() - at);
    let mut power = exponent.checked_sub(decimals as i64)?;
    let mut digits = digits.trim_start_matches('0').to_string();
    if digits.is_empty() {
        return Some(if negative { "-0" } else { "0" }.into());
    }
    let without_zeros = digits.trim_end_matches('0').len();
    power = power.checked_add((digits.len() - without_zeros) as i64)?;
    digits.truncate(without_zeros);
    let sign = if negative { "-" } else { "" };
    // Enormous exponents stay compact, rather than allocating their zeroes.
    if power.unsigned_abs() > 100 || digits.len() > 256 {
        return Some(format!("{sign}{digits}e{power}"));
    }
    if power >= 0 {
        return Some(format!("{sign}{digits}{}", "0".repeat(power as usize)));
    }
    let point = digits.len() as i64 + power;
    if point > 0 {
        let (a, b) = digits.split_at(point as usize);
        Some(format!("{sign}{a}.{b}"))
    } else {
        Some(format!("{sign}0.{}{digits}", "0".repeat((-point) as usize)))
    }
}

fn tokenize(line: &str, n: usize) -> Result<Vec<Token>> {
    let mut chars = line.trim().chars().peekable();
    let mut out = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c != '"' {
            let mut value = String::from(c);
            while chars.peek().is_some_and(|c| !c.is_whitespace()) {
                value.push(chars.next().expect("peeked"));
            }
            out.push(Token { value, quoted: false });
            continue;
        }
        let mut value = String::new();
        let mut closed = false;
        while let Some(c) = chars.next() {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    value.push('"');
                }
                '"' => {
                    closed = true;
                    break;
                }
                '\\' => match chars.next() {
                    Some(c @ ('"' | '\\')) => value.push(c),
                    Some(c) => {
                        value.push('\\');
                        value.push(c);
                    }
                    None => return Err(bad(n, "unfinished string escape")),
                },
                c => value.push(c),
            }
        }
        if !closed {
            return Err(bad(n, "unterminated string"));
        }
        if chars.peek().is_some_and(|c| !c.is_whitespace()) {
            return Err(bad(n, "expected whitespace after a string"));
        }
        out.push(Token { value, quoted: true });
    }
    Ok(out)
}

fn parse(text: &str) -> Result<Document> {
    if text.len() > MAX_BYTES {
        return Err(bad(1, "file exceeds the 64 MiB structured-view limit"));
    }
    let mut doc = Document { header: Vec::new(), objects: Vec::new(), understood: true, statements: Vec::new() };
    let mut open: Option<Object> = None;
    let mut names = BTreeSet::new();
    let mut format_seen = false;
    for (n, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let n = n + 1;
        let line = line.trim();
        if line.is_empty() || line.starts_with(['*', '#']) || line.starts_with("//") {
            continue;
        }
        let tokens = tokenize(line, n)?;
        let key = &tokens[0].value;
        if tokens[0].quoted {
            return Err(bad(n, "expected a statement keyword"));
        }
        doc.statements.push(tokens_text(&tokens));
        if KINDS.contains(&key.as_str()) {
            if open.is_some() {
                return Err(bad(n, "missing END before the next calibration object"));
            }
            let name = tokens.get(1).filter(|t| !t.value.is_empty()).ok_or_else(|| bad(n, "missing object name"))?;
            if !names.insert(name.value.clone()) {
                return Err(bad(n, format!("duplicate calibration name {}", name.value)));
            }
            let dims = tokens[2..]
                .iter()
                .map(|t| {
                    t.value
                        .parse::<usize>()
                        .ok()
                        .filter(|v| *v > 0 && *v <= MAX_VALUES)
                        .ok_or_else(|| bad(n, "invalid or excessive dimensions"))
                })
                .collect::<Result<Vec<_>>>()?;
            let expected = if key.ends_with("KENNFELD") {
                2
            } else if matches!(key.as_str(), "FESTWERT" | "TEXTSTRING") {
                0
            } else {
                1
            };
            if dims.len() != expected && !(key == "FESTWERTEBLOCK" && dims.len() == 2) {
                return Err(bad(n, format!("{key} expects {expected} dimension(s)")));
            }
            if dims.iter().try_fold(1usize, |a, b| a.checked_mul(*b)).is_none_or(|v| v > MAX_VALUES) {
                return Err(bad(n, "too many calibration values"));
            }
            open = Some(Object {
                kind: key.clone(),
                name: name.value.clone(),
                dims,
                metadata: BTreeMap::new(),
                values: Vec::new(),
                x: Vec::new(),
                y: Vec::new(),
                rows: Vec::new(),
            });
        } else if let Some(obj) = open.as_mut() {
            match key.as_str() {
                "END" => {
                    if tokens.len() != 1 {
                        return Err(bad(n, "unexpected text after END"));
                    }
                    let obj = open.take().expect("open object");
                    obj.validate(n)?;
                    doc.objects.push(obj);
                }
                "WERT" | "ST/X" | "ST/Y" | "TEXT" => {
                    if tokens.len() == 1 {
                        return Err(bad(n, "missing values"));
                    }
                    let values = tokens[1..]
                        .iter()
                        .map(|t| {
                            if key == "TEXT" {
                                if t.quoted {
                                    Ok(quoted(&t.value))
                                } else {
                                    Err(bad(n, "TEXT requires quoted strings"))
                                }
                            } else if !t.quoted {
                                number(&t.value).ok_or_else(|| bad(n, format!("invalid number {}", t.value)))
                            } else {
                                Err(bad(n, "expected a numeric value"))
                            }
                        })
                        .collect::<Result<Vec<_>>>()?;
                    match key.as_str() {
                        "ST/X" => obj.x.extend(values),
                        "ST/Y" => {
                            obj.y.extend(values);
                            obj.rows.push(obj.values.len());
                        }
                        _ => obj.values.extend(values),
                    }
                    if obj.values.len().max(obj.x.len()).max(obj.y.len()) > MAX_VALUES {
                        return Err(bad(n, "too many calibration values"));
                    }
                }
                key if METADATA.contains(&key) => {
                    obj.metadata.entry(key.to_string()).or_default().push(tokens[1..].to_vec());
                }
                _ => {
                    doc.understood = false;
                }
            }
        } else {
            if key == "KONSERVIERUNG_FORMAT" {
                if format_seen || tokens.len() != 2 || number(&tokens[1].value).is_none() {
                    return Err(bad(n, "invalid or repeated KONSERVIERUNG_FORMAT"));
                }
                format_seen = true;
            } else if key != "MODULKOPF" {
                doc.understood = false;
            }
            doc.header.push(tokens);
        }
    }
    if open.is_some() {
        return Err(bad(text.lines().count(), "file ends before END"));
    }
    if !format_seen {
        return Err(bad(1, "no KONSERVIERUNG_FORMAT header"));
    }
    doc.objects.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(doc)
}

impl Object {
    fn validate(&self, line: usize) -> Result<()> {
        let expected = self.dims.iter().product::<usize>();
        if self.kind == "STUETZSTELLENVERTEILUNG" {
            if self.x.len() != expected || !self.values.is_empty() || !self.y.is_empty() {
                return Err(bad(line, "axis values do not match the declared size"));
            }
        } else if self.values.len() != expected {
            return Err(bad(line, format!("{} declares {expected} values but has {}", self.name, self.values.len())));
        }
        if self.dims.is_empty() && (!self.x.is_empty() || !self.y.is_empty()) {
            return Err(bad(line, "a scalar cannot have axes"));
        }
        if !self.x.is_empty() && self.x.len() != self.dims[0] {
            return Err(bad(line, "X axis does not match the declared width"));
        }
        if !self.y.is_empty() {
            if self.dims.len() != 2 || self.y.len() != self.dims[1] || self.rows.len() != self.y.len() {
                return Err(bad(line, "Y axis does not match the declared height"));
            }
            if self.rows[0] != 0
                || self.rows.windows(2).any(|pair| pair[1] - pair[0] != self.dims[0])
                || self.values.len() - self.rows.last().expect("Y rows") != self.dims[0]
            {
                return Err(bad(line, "map rows do not match the declared width"));
            }
        }
        Ok(())
    }
}

fn tokens_text(tokens: &[Token]) -> String {
    tokens.iter().map(Token::shown).collect::<Vec<_>>().join(" ")
}

impl Document {
    fn render(&self) -> String {
        if !self.understood {
            // An unknown directive can depend on its position relative to an
            // otherwise understood object or metadata field. Keep that order in
            // the view too (the CLI also compares the rendered view directly).
            return format!(
                "=== DCM calibration (unrecognized statements kept in source order) ===\n{}\n",
                self.statements.join("\n")
            );
        }
        let mut out = String::from("=== DCM calibration ===\n");
        for row in &self.header {
            let _ = writeln!(out, "{}", tokens_text(row));
        }
        for obj in &self.objects {
            let _ = writeln!(out, "\n--- {} ---\nType: {}", quoted(&obj.name), obj.kind);
            if !obj.dims.is_empty() {
                let _ =
                    writeln!(out, "Size: {}", obj.dims.iter().map(usize::to_string).collect::<Vec<_>>().join(" × "));
            }
            for (key, rows) in &obj.metadata {
                for row in rows {
                    let _ = writeln!(out, "{key}: {}", tokens_text(row));
                }
            }
            for (label, values) in [("X", &obj.x), ("Y", &obj.y)] {
                for (i, value) in values.iter().enumerate() {
                    let _ = writeln!(out, "Axis {label}[{i}] = {value}");
                }
            }
            for (i, value) in obj.values.iter().enumerate() {
                if obj.dims.len() == 2 {
                    let _ = writeln!(out, "Value[{}][{}] = {value}", i / obj.dims[0], i % obj.dims[0]);
                } else if obj.dims.is_empty() {
                    let _ = writeln!(out, "Value: {value}");
                } else {
                    let _ = writeln!(out, "Value[{i}] = {value}");
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = include_str!("../../tests/fixtures/automotive/engine-v1.dcm");
    const FORMATTED: &str = include_str!("../../tests/fixtures/automotive/engine-v1-reformatted.dcm");
    const V2: &str = include_str!("../../tests/fixtures/automotive/engine-v2.dcm");

    #[test]
    fn renders_scalars_strings_arrays_curves_maps_and_axes() {
        let out = values(V1).unwrap();
        for expected in [
            "--- \"K_IdleSpeed\" ---\nType: FESTWERT",
            "EINHEIT_W: \"rpm\"",
            "Value: 800",
            "Value: \"EU6 diesel\"",
            "Value[1] = 0.25",
            "Axis X[2] = 3000",
            "Axis Y[1] = 75",
            "Value[1][2] = 6",
            "SSTX: \"AX_Speed\"",
        ] {
            assert!(out.contains(expected), "missing {expected}\n{out}");
        }
        assert_eq!(parse(V1).unwrap().objects.len(), 7);
        assert_eq!(same_values(V1, FORMATTED), Some(true));
        assert_eq!(same_values(V1, V2), Some(false));
        assert!(values(V2).unwrap().contains("Value[1][2] = 6.5"));
    }

    #[test]
    fn exact_numbers_and_all_meaningful_fields_count() {
        for (input, expected) in [
            ("+8.000e2", "800"),
            (".0010", "0.001"),
            ("-0.000", "-0"),
            ("1.234567890123456789", "1.234567890123456789"),
            ("9007199254740993.0", "9007199254740993"),
            ("1e999999", "1e999999"),
        ] {
            assert_eq!(number(input).as_deref(), Some(expected));
        }
        for input in ["NaN", "inf", "1e", "1.2.3", "1e9223372036854775808", "--1"] {
            assert!(number(input).is_none(), "{input}");
        }
        for changed in [
            V1.replace("rpm", "rps"),
            V1.replace("Idle speed", "Cold idle speed"),
            V1.replace("EU6 diesel", "EU7 diesel"),
            V1.replace("ST/X 1000", "ST/X 1100"),
            V1.replace("SSTX \"AX_Speed\"", "SSTX \"AX_Other\""),
            V1.replace("WERT 800", "WERT 800.000000000000000001"),
        ] {
            assert_eq!(same_values(V1, &changed), Some(false));
        }
        let large = "KONSERVIERUNG_FORMAT 2.0\nFESTWERT X\nWERT 9007199254740992\nEND\n";
        assert_eq!(same_values(large, &large.replace("992", "993")), Some(false));
    }

    #[test]
    fn fixed_and_grouped_variants_keep_dimensions() {
        for kind in ["FESTKENNLINIE", "GRUPPENKENNLINIE"] {
            assert!(values(&format!("KONSERVIERUNG_FORMAT 2.0\n{kind} X 2\nST/X 0 1\nWERT 2 3\nEND")).is_ok());
        }
        for kind in ["FESTKENNFELD", "GRUPPENKENNFELD", "FESTWERTEBLOCK"] {
            let text = format!("KONSERVIERUNG_FORMAT 2.0\n{kind} X 2 2\nWERT 1 2 3 4\nEND");
            assert!(values(&text).unwrap().contains("Value[1][1] = 4"));
        }
        // Tall maps validate each row once, including the final row.
        let mut tall = String::from("KONSERVIERUNG_FORMAT 2.0\nKENNFELD Tall 1 10000\n");
        for row in 0..10000 {
            let _ = writeln!(tall, "ST/Y {row}\nWERT {row}");
        }
        tall.push_str("END\n");
        assert!(values(&tall).unwrap().contains("Value[9999][0] = 9999"));
        assert!(values(&tall.replace("ST/Y 9999\nWERT 9999", "WERT 9999\nST/Y 9999")).is_err());
    }

    #[test]
    fn malformed_and_unknown_input_is_not_proof_of_equality() {
        let extension = V1.replace("  WERT 800", "  VENDOR_MODE alpha beta\n  WERT 800");
        assert!(values(&extension).unwrap().contains("VENDOR_MODE alpha beta"));
        assert_eq!(same_values(&extension, &extension), None);
        let a = "KONSERVIERUNG_FORMAT 2.0\nVENDOR_VARIANT A\nFESTWERT X\nWERT 1\nEND\nVENDOR_VARIANT B\nFESTWERT Y\nWERT 2\nEND";
        let b = a
            .replace("FESTWERT X", "FESTWERT temporary")
            .replace("FESTWERT Y", "FESTWERT X")
            .replace("FESTWERT temporary", "FESTWERT Y");
        assert_ne!(values(a).unwrap(), values(&b).unwrap());
        for invalid in [
            V1.trim_end().strip_suffix("END").unwrap().to_string(),
            V1.replace("WERT 800", "WERT 800 850"),
            V1.replace("KENNFELD KF_Fuel 3 2", "KENNFELD KF_Fuel 4 2"),
            V1.replace("WERT 1 2 3\n  ST/Y 75\n  WERT 4 5 6", "WERT 1 2\n  ST/Y 75\n  WERT 3 4 5 6"),
            V1.replace("WERT 800", "WERT NaN"),
            V1.replace("\"EU6 diesel\"", "\"unterminated"),
            V1.replace("T_Variant", "K_IdleSpeed"),
            V1.replace("FESTWERTEBLOCK KA_Offsets 3", "FESTWERTEBLOCK KA_Offsets 999999999999999999999999"),
        ] {
            assert!(values(&invalid).is_err(), "{invalid}");
            assert_eq!(same_values(V1, &invalid), None);
        }
        assert!(!is_dcm(b"DICM\0\0medical image"));
        assert!(values("not a calibration file").is_err());
        let quoted_end = "KONSERVIERUNG_FORMAT 2.0\nTEXTSTRING X\nTEXT \"END * literal \\\"quote\\\"\"\nEND";
        assert!(values(quoted_end).unwrap().contains("END * literal"));
    }
}
