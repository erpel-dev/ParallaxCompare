//! Token-preserving outlines for configuration schemas and automotive IDLs.
//!
//! Wire fields, enum values, parameters, model fields and deployment overrides
//! retain their order. Documentation comments in FBS are retained too: S-CORE
//! uses them to generate JSON Schema constraints.

use std::collections::BTreeSet;

use crate::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Flatbuffers,
    Protobuf,
    Franca,
    Trlc,
    Rsl,
}

impl Format {
    pub fn of(name: &str) -> Option<Self> {
        match name.rsplit('.').next()?.to_ascii_lowercase().as_str() {
            "fbs" => Some(Self::Flatbuffers),
            "proto" => Some(Self::Protobuf),
            "fidl" | "fdepl" => Some(Self::Franca),
            "trlc" => Some(Self::Trlc),
            "rsl" => Some(Self::Rsl),
            _ => None,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Flatbuffers => "FlatBuffers schema",
            Self::Protobuf => "Protobuf definitions",
            Self::Franca => "Franca definitions",
            Self::Trlc => "TRLC requirements",
            Self::Rsl => "RSL model",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Token {
    pub raw: String,
    pub line: usize,
    pub doc: bool,
    pub start: usize,
    pub end: usize,
}

fn bad(message: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("Definitions: {message}"))
}

/// Strings and numeric/operator tokens retain their spelling and boundaries.
pub(super) fn lex(text: &str, fbs_docs: bool) -> Result<Vec<Token>> {
    if text.contains('\0') {
        return Err(bad("NUL bytes are not valid definition source"));
    }
    if text.len() > 16 * 1024 * 1024 {
        return Err(bad("file exceeds the 16 MiB definition-view limit"));
    }
    let mut out = Vec::new();
    let mut at = 0;
    let mut line = 1;
    let bytes = text.as_bytes();
    let mut stack = Vec::new();
    while at < bytes.len() {
        let c = bytes[at];
        if c.is_ascii_whitespace() {
            if c == b'\n' {
                line += 1;
            }
            at += 1;
            continue;
        }
        let start = at;
        let start_line = line;
        if text[at..].starts_with("//") {
            let doc = fbs_docs && text[at..].starts_with("///");
            at += text[at..].find('\n').unwrap_or(bytes.len() - at);
            if doc {
                out.push(Token { raw: text[start..at].trim_end().into(), line: start_line, doc: true, start, end: at });
            }
            continue;
        }
        if text[at..].starts_with("/*") {
            let doc = fbs_docs && text[at..].starts_with("/**");
            at += 2;
            let end = text[at..].find("*/").ok_or_else(|| bad(format!("line {line}: unterminated comment")))?;
            at += end + 2;
            line += text[start..at].bytes().filter(|c| *c == b'\n').count();
            if doc {
                out.push(Token { raw: text[start..at].into(), line: start_line, doc: true, start, end: at });
            }
            continue;
        }
        if c == b'"' || c == b'\'' {
            let triple = c == b'"' && text[at..].starts_with("\"\"\"");
            at += if triple { 3 } else { 1 };
            let mut closed = false;
            while at < bytes.len() {
                if triple && bytes[at..].starts_with(b"\"\"\"") {
                    at += 3;
                    closed = true;
                    break;
                }
                if !triple && bytes[at] == c {
                    at += 1;
                    closed = true;
                    break;
                }
                if bytes[at] == b'\n' {
                    line += 1;
                    if !triple {
                        return Err(bad(format!("line {start_line}: newline in a quoted string")));
                    }
                }
                if bytes[at] == b'\\' {
                    at += 1;
                }
                at += 1;
            }
            if !closed {
                return Err(bad(format!("line {start_line}: unterminated string")));
            }
        } else if c.is_ascii_alphabetic() || c == b'_' || c >= 128 {
            at += 1;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_' || bytes[at] >= 128) {
                at += 1;
            }
        } else if c.is_ascii_digit() {
            at += 1;
            while at < bytes.len() {
                let c = bytes[at];
                if c.is_ascii_alphanumeric()
                    || matches!(c, b'_' | b'.')
                    || (matches!(c, b'+' | b'-') && matches!(bytes[at - 1], b'e' | b'E'))
                {
                    at += 1;
                } else {
                    break;
                }
            }
        } else if let Some(op) = ["==", "!=", "<=", ">=", "&&", "||", "..", "->", "=>", "::", "+=", "-=", "<<", ">>"]
            .into_iter()
            .find(|s| text[at..].starts_with(s))
        {
            at += op.len();
        } else {
            at += 1;
            match c {
                b'{' | b'[' | b'(' => {
                    stack.push(c);
                    if stack.len() > 128 {
                        return Err(bad("excessive nesting"));
                    }
                }
                b'}' | b']' | b')' => {
                    let expected = match c {
                        b'}' => b'{',
                        b']' => b'[',
                        _ => b'(',
                    };
                    if stack.pop() != Some(expected) {
                        return Err(bad(format!("line {line}: mismatched delimiter")));
                    }
                }
                _ => {}
            }
        }
        out.push(Token { raw: text[start..at].into(), line: start_line, doc: false, start, end: at });
        if out.len() > 1_000_000 {
            return Err(bad("too many definition tokens"));
        }
    }
    if !stack.is_empty() {
        return Err(bad("file ends inside a declaration"));
    }
    Ok(out)
}

fn word(t: &Token) -> bool {
    t.raw.starts_with(|c: char| c.is_alphabetic() || c == '_') && t.raw.chars().all(|c| c.is_alphanumeric() || c == '_')
}

struct Chunk<'a> {
    tokens: &'a [Token],
    key: Option<String>,
}

fn chunks(tokens: &[Token], format: Format) -> Result<Vec<Chunk<'_>>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        let start = at;
        while tokens.get(at).is_some_and(|t| t.doc) {
            at += 1;
        }
        if at == tokens.len() {
            out.push(Chunk { tokens: &tokens[start..], key: None });
            break;
        }
        let header = at;
        let first = &tokens[at].raw;
        let line_header = matches!(format, Format::Trlc | Format::Rsl | Format::Franca)
            && matches!(first.as_str(), "package" | "import");
        if line_header {
            let line = tokens[at].line;
            while at < tokens.len() && tokens[at].line == line {
                at += 1;
            }
            out.push(Chunk { tokens: &tokens[start..at], key: None });
            continue;
        }
        let mut braces = 0usize;
        let mut nesting = 0usize;
        let mut opening = None;
        while at < tokens.len() {
            let t = tokens[at].raw.as_str();
            match t {
                "{" => {
                    if braces == 0 && nesting == 0 {
                        opening = Some(at);
                    }
                    braces += 1;
                }
                "}" => {
                    braces = braces.checked_sub(1).ok_or_else(|| bad("unexpected closing brace"))?;
                    if braces == 0 && nesting == 0 {
                        at += 1;
                        if tokens.get(at).is_some_and(|t| t.raw == ";") {
                            at += 1;
                        }
                        break;
                    }
                }
                "[" | "(" => nesting += 1,
                "]" | ")" => nesting = nesting.checked_sub(1).ok_or_else(|| bad("unexpected closing bracket"))?,
                ";" if braces == 0 && nesting == 0 => {
                    at += 1;
                    break;
                }
                _ => {}
            }
            at += 1;
        }
        let key = opening.and_then(|open| {
            let head = &tokens[header..open];
            if format == Format::Trlc && head.len() >= 2 && head.iter().all(|t| word(t) || t.raw == ".") {
                Some(head.iter().map(|t| t.raw.as_str()).collect::<Vec<_>>().join(" "))
            } else if matches!(format, Format::Flatbuffers | Format::Protobuf)
                && head.len() >= 2
                && matches!(first.as_str(), "table" | "struct" | "enum" | "union" | "message" | "service")
                && word(&head[1])
            {
                Some(format!("{} {}", first, head[1].raw))
            } else {
                None
            }
        });
        out.push(Chunk { tokens: &tokens[start..at], key });
    }
    // Only contiguous independent definitions are sorted. Namespace/package,
    // import, option and root-type directives delimit each run.
    let mut start = 0;
    while start < out.len() {
        if out[start].key.is_none() {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < out.len() && out[end].key.is_some() {
            end += 1;
        }
        let keys = out[start..end].iter().map(|c| c.key.as_ref().expect("definition")).collect::<BTreeSet<_>>();
        if keys.len() != end - start {
            return Err(bad("duplicate definition identity"));
        }
        out[start..end].sort_by(|a, b| a.key.cmp(&b.key));
        start = end;
    }
    Ok(out)
}

fn flush(out: &mut String, line: &mut Vec<String>, depth: usize) {
    if !line.is_empty() {
        out.push_str(&"  ".repeat(depth));
        out.push_str(&line.join(" "));
        out.push('\n');
        line.clear();
    }
}

fn trlc_assignments(tokens: &[Token]) -> Result<Vec<Token>> {
    let Some(open) = tokens.iter().position(|t| t.raw == "{") else {
        return Ok(tokens.to_vec());
    };
    let close = tokens.iter().rposition(|t| t.raw == "}").ok_or_else(|| bad("missing object end"))?;
    let mut starts = Vec::new();
    let mut depth = 0usize;
    for i in open + 1..close {
        let raw = tokens[i].raw.as_str();
        if depth == 0 && word(&tokens[i]) && tokens.get(i + 1).is_some_and(|t| t.raw == "=") {
            starts.push(i);
        }
        match raw {
            "{" | "[" | "(" => depth += 1,
            "}" | "]" | ")" => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let mut keys = BTreeSet::new();
    if starts.iter().any(|i| !keys.insert(tokens[*i].raw.as_str())) {
        return Err(bad("duplicate requirement attribute"));
    }
    if starts.first() != Some(&(open + 1)) {
        return Ok(tokens.to_vec());
    }
    let mut ranges = starts
        .iter()
        .enumerate()
        .map(|(i, start)| (*start, starts.get(i + 1).copied().unwrap_or(close)))
        .collect::<Vec<_>>();
    ranges.sort_by(|a, b| tokens[a.0].raw.cmp(&tokens[b.0].raw));
    let mut out = tokens[..open + 1].to_vec();
    for (start, end) in ranges {
        out.extend_from_slice(&tokens[start..end]);
    }
    out.extend_from_slice(&tokens[close..]);
    Ok(out)
}

fn render(tokens: &[Token], format: Format, out: &mut String) {
    let mut line = Vec::new();
    let mut depth = 0;
    let mut parens: usize = 0;
    let mut previous_line = 0;
    for (i, token) in tokens.iter().enumerate() {
        let t = token.raw.as_str();
        if token.doc {
            flush(out, &mut line, depth);
            out.push_str(&"  ".repeat(depth));
            out.push_str(t);
            out.push('\n');
            continue;
        }
        if depth > 0 && parens == 0 {
            if format == Format::Trlc && word(token) && tokens.get(i + 1).is_some_and(|t| t.raw == "=") {
                flush(out, &mut line, depth);
            }
            if format == Format::Rsl && token.line > previous_line {
                flush(out, &mut line, depth);
            }
        }
        match t {
            "{" => {
                line.push(t.into());
                flush(out, &mut line, depth);
                depth += 1;
            }
            "}" => {
                flush(out, &mut line, depth);
                depth = depth.saturating_sub(1);
                line.push(t.into());
                flush(out, &mut line, depth);
            }
            ";" => {
                line.push(t.into());
                if parens == 0 {
                    flush(out, &mut line, depth);
                }
            }
            "(" | "[" => {
                parens += 1;
                line.push(t.into());
            }
            ")" | "]" => {
                parens = parens.saturating_sub(1);
                line.push(t.into());
            }
            _ => line.push(t.into()),
        }
        previous_line = token.line;
    }
    flush(out, &mut line, depth);
}

pub fn outline(name: &str, data: &[u8]) -> Result<String> {
    let format = Format::of(name).ok_or_else(|| bad("unknown definition format"))?;
    let decoded = crate::text::decode(data);
    if !decoded.encode().is_ok_and(|v| v == data) {
        return Err(bad("damaged text encoding"));
    }
    let tokens = lex(decoded.text.trim_start_matches('\u{feff}'), format == Format::Flatbuffers)?;
    if tokens.is_empty() {
        return Err(bad("no declarations"));
    }
    let chunks = chunks(&tokens, format)?;
    let mut out = format!("=== {} ===\n", format.label());
    for chunk in chunks {
        let normalized =
            if format == Format::Trlc && chunk.key.is_some() { Some(trlc_assignments(chunk.tokens)?) } else { None };
        render(normalized.as_deref().unwrap_or(chunk.tokens), format, &mut out);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/score").join(name)).unwrap()
    }

    #[test]
    fn structured_views_keep_values_and_ignore_only_safe_changes() {
        for ext in ["rsl", "proto", "fidl", "fdepl"] {
            let name = format!("definitions-v1.{ext}");
            let original = outline(&name, &fixture(&name)).unwrap();
            assert_eq!(original, outline(&name, &fixture(&format!("definitions-formatted.{ext}"))).unwrap(), "{ext}");
            assert_ne!(original, outline(&name, &fixture(&format!("definitions-v2.{ext}"))).unwrap(), "{ext}");
        }
        assert_eq!(
            outline("config.fbs", &fixture("config.fbs")).unwrap(),
            outline("config.fbs", &fixture("config-formatted.fbs")).unwrap()
        );
        assert_ne!(
            outline("config.fbs", &fixture("config.fbs")).unwrap(),
            outline("config.fbs", &fixture("config-v2.fbs")).unwrap()
        );
    }

    #[test]
    fn requirements_align_by_identity_and_keep_traceability_and_strings() {
        let original = fixture("requirements-v1.trlc");
        let view = outline("requirements.trlc", &original).unwrap();
        assert_eq!(view, outline("requirements.trlc", &fixture("requirements-reordered.trlc")).unwrap());
        assert_ne!(view, outline("requirements.trlc", &fixture("requirements-v2.trlc")).unwrap());
        assert!(view.contains("Grüße 🚙"));
        let a = "package P\nT X { a = 1 b = [Q.B@2, Q.A@1] }";
        let b = "package P\nT X { b = [Q.B@2, Q.A@1]\n a = 1 }";
        assert_eq!(outline("x.trlc", a.as_bytes()).unwrap(), outline("x.trlc", b.as_bytes()).unwrap());
        assert_ne!(
            outline("x.trlc", a.as_bytes()).unwrap(),
            outline("x.trlc", a.replace("Q.B@2, Q.A@1", "Q.A@1, Q.B@2").as_bytes()).unwrap()
        );
        let a = "package P\nT X { description = \"\"\"line one\n  line two 🚙\"\"\" }";
        assert!(outline("x.trlc", a.as_bytes()).unwrap().contains("  line two 🚙"));
    }

    #[test]
    fn implicit_field_enum_and_parameter_order_are_never_sorted() {
        for (name, a, b) in [
            ("x.fbs", "table T { a:int; b:short; } root_type T;", "table T { b:short; a:int; } root_type T;"),
            ("x.fbs", "enum E:byte { A, B }", "enum E:byte { B, A }"),
            ("x.proto", "message M { int32 a=1; int32 b=2; }", "message M { int32 b=2; int32 a=1; }"),
            (
                "x.fidl",
                "interface I { method M { in { UInt32 a UInt32 b } } }",
                "interface I { method M { in { UInt32 b UInt32 a } } }",
            ),
            (
                "x.rsl",
                "package P\ntuple T {\n a Integer\n b Integer\n}",
                "package P\ntuple T {\n b Integer\n a Integer\n}",
            ),
        ] {
            assert_ne!(outline(name, a.as_bytes()).unwrap(), outline(name, b.as_bytes()).unwrap(), "{name}");
        }
        let a = "/// @required\ntable T { a:int; } root_type T;";
        assert_ne!(
            outline("x.fbs", a.as_bytes()).unwrap(),
            outline("x.fbs", a.replace("@required", "@default: 0").as_bytes()).unwrap()
        );
        assert_ne!(outline("x.proto", b"option x = 1.0;").unwrap(), outline("x.proto", b"option x = 1 . 0;").unwrap());
    }

    #[test]
    fn malformed_and_duplicate_declarations_fail_cleanly() {
        for text in ["({})", "package P (\n) ", "({}); table T {}"] {
            assert!(std::panic::catch_unwind(|| outline("x.fbs", text.as_bytes())).is_ok(), "{text}");
        }
        for text in ["table T {", "table T { x:[int); }", "\"unterminated", "/* never ends", "table T {} table T {}"] {
            assert!(outline("x.fbs", text.as_bytes()).is_err(), "{text}");
        }
        assert!(outline("x.trlc", b"package P\nT X { a = 1 a = 2 }").is_err());
        assert!(outline("x.fbs", &[0xff, 0]).is_err());
        let deep = format!("{}{}", "{".repeat(130), "}".repeat(130));
        assert!(outline("x.fbs", deep.as_bytes()).is_err());
    }
}
