//! Eclipse Xpand / openArchitectureWare (oAW): templates (`.xpt`), Xtend
//! extensions (`.ext`) and Check constraints (`.chk`).
//!
//! All three use the Xtend expression language; this module lexes it the way
//! the Xpand ANTLR grammar does (identifiers with `::`, strings in either quote
//! compared by value, `//` and `/* */` comments). Templates are text with
//! `«…»` directives: `«REM»…«ENDREM»` is a comment, and text outside
//! `«DEFINE»…«ENDDEFINE»` and `«AROUND»…«ENDAROUND»` is not output, so neither
//! counts; text inside them counts character for character, as does the `-`
//! that trims whitespace after a directive.
//!
//! The outlines list the imports and extensions, then the template
//! definitions (with the files they open and the definitions they expand),
//! the extension functions or the check constraints, sorted, each with a
//! checksum of its body.

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Ident,
    Number,
    Str,
    Punct,
    /// Template text between directives (only in `.xpt`).
    Text,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tok {
    pub kind: Kind,
    pub text: String,
}

impl Tok {
    fn new(kind: Kind, text: impl Into<String>) -> Self {
        Tok { kind, text: text.into() }
    }
    fn is(&self, s: &str) -> bool {
        matches!(self.kind, Kind::Punct | Kind::Ident) && self.text == s
    }
    fn open(&self) -> bool {
        self.kind == Kind::Punct && matches!(self.text.as_str(), "(" | "[" | "{")
    }
    fn close(&self) -> bool {
        self.kind == Kind::Punct && matches!(self.text.as_str(), ")" | "]" | "}")
    }
}

const PUNCT: &[&str] = &["::", "->", "==", "!=", ">=", "<=", "&&", "||"];

fn letter(c: char) -> bool {
    c == '$'
        || c == '_'
        || c.is_ascii_alphabetic()
        || (c as u32 >= 0xC0 && c != '\u{D7}' && c != '\u{F7}' && c.is_alphabetic())
}

fn string_value(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let h: String = chars.by_ref().take(4).collect();
                match u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => out.push_str(&h),
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Lexes expression tokens from `s[*i..]` until `stop` returns true at a
/// position outside of strings and comments (or the end).
fn expression(s: &str, i: &mut usize, out: &mut Vec<Tok>, stop: impl Fn(&str) -> bool) {
    while *i < s.len() {
        let rest = &s[*i..];
        if stop(rest) {
            return;
        }
        let c = rest.chars().next().expect("not at end");
        if c.is_whitespace() {
            *i += c.len_utf8();
        } else if rest.starts_with("//") {
            *i += rest.find('\n').unwrap_or(rest.len());
        } else if let Some(body) = rest.strip_prefix("/*") {
            *i += body.find("*/").map_or(rest.len(), |n| n + 4);
        } else if c == '"' || c == '\'' {
            let b = rest.as_bytes();
            let mut j = 1;
            while j < b.len() && b[j] != c as u8 {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(rest.len());
            out.push(Tok::new(Kind::Str, string_value(&rest[1..j.min(rest.len())])));
            *i += end;
        } else if c.is_ascii_digit() {
            let b = rest.as_bytes();
            let mut j = 0;
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.' && b.get(j + 1).is_some_and(u8::is_ascii_digit))
            {
                j += 1;
            }
            out.push(Tok::new(Kind::Number, &rest[..j]));
            *i += j;
        } else if letter(c) || c == '^' && rest[1..].starts_with(letter) {
            let n: usize = rest
                .char_indices()
                .skip(1)
                .find(|&(_, c)| !(letter(c) || c.is_ascii_digit()))
                .map_or(rest.len(), |(k, _)| k);
            // `^name` escapes a keyword; it names the same thing.
            out.push(Tok::new(Kind::Ident, rest[..n].trim_start_matches('^')));
            *i += n;
        } else {
            let p = PUNCT.iter().find(|p| rest.starts_with(**p)).map_or(&rest[..c.len_utf8()], |p| *p);
            out.push(Tok::new(Kind::Punct, p));
            *i += p.len();
        }
    }
}

/// Tokens of an Xtend (`.ext`) or Check (`.chk`) file, without comments.
pub fn tokens(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    expression(text, &mut i, &mut out, |_| false);
    out
}

const LG: char = '\u{AB}';
const RG: char = '\u{BB}';

/// Tokens of an Xpand template: `Text` for the text between directives, the
/// expression tokens of each directive, without comments; text outside
/// definitions is empty.
pub fn template_tokens(text: &str) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    // Text before the first `«` is not part of the template.
    let Some(start) = text.find(LG) else { return out };
    let mut i = start + LG.len_utf8();
    loop {
        // Inside a directive.
        let rest = &text[i..];
        if rest.starts_with("REM\u{BB}") {
            // `«REM»…«ENDREM` is a comment; the directive goes on after it.
            match rest.find("\u{AB}ENDREM") {
                Some(n) => i += n + LG.len_utf8() + 6,
                None => break,
            }
        }
        expression(text, &mut i, &mut out, |r| r.starts_with(RG) || r.starts_with(LG));
        if i >= text.len() {
            break;
        }
        if text[i..].starts_with(LG) {
            // A `«` inside a directive: the grammar would reject it; start over.
            i += LG.len_utf8();
            continue;
        }
        // Text up to the next `«`.
        i += RG.len_utf8();
        let end = text[i..].find(LG).map_or(text.len(), |n| i + n);
        let t = &text[i..end];
        // After a comment, the text continues the text before it.
        match out.last_mut() {
            Some(last) if last.kind == Kind::Text && just_after_comment(&text[..i]) => last.text.push_str(t),
            _ => out.push(Tok::new(Kind::Text, t)),
        }
        if end >= text.len() {
            break;
        }
        i = end + LG.len_utf8();
    }
    drop_text_outside_definitions(out)
}

/// Whether the directive just closed was `«REM»…«ENDREM»` (so the text after
/// it continues the text before it).
fn just_after_comment(before: &str) -> bool {
    before.trim_end_matches(RG).trim_end().ends_with("\u{AB}ENDREM")
}

fn drop_text_outside_definitions(toks: Vec<Tok>) -> Vec<Tok> {
    let mut out = Vec::with_capacity(toks.len());
    let mut inside = false;
    for t in toks {
        if t.is("DEFINE") || t.is("AROUND") {
            inside = true;
        } else if t.is("ENDDEFINE") || t.is("ENDAROUND") {
            inside = false;
            out.push(t);
            continue;
        }
        if t.kind == Kind::Text && !inside {
            // Not output; it only separates the directives, and a `-` trimming it does nothing.
            if out.last().is_some_and(|l: &Tok| l.is("-")) {
                out.pop();
            }
            // A comment at the start of the file or between two texts leaves no extra separator.
            if out.last().is_some_and(|l: &Tok| l.kind != Kind::Text || !l.text.is_empty()) {
                out.push(Tok::new(Kind::Text, ""));
            }
            continue;
        }
        out.push(t);
    }
    out
}

/// Whether two templates are the same, apart from comments, text outside
/// definitions and spacing inside directives.
pub fn same_template(left: &str, right: &str) -> bool {
    template_tokens(left) == template_tokens(right)
}

/// Whether two `.ext` or `.chk` files are the same code, apart from comments and layout.
pub fn same_code(left: &str, right: &str) -> bool {
    tokens(left) == tokens(right)
}

// ---- outlines ----

fn display(t: &Tok) -> String {
    match t.kind {
        Kind::Str => {
            let mut s = String::from('"');
            for c in t.text.chars() {
                match c {
                    '"' => s.push_str("\\\""),
                    '\\' => s.push_str("\\\\"),
                    '\n' => s.push_str("\\n"),
                    '\t' => s.push_str("\\t"),
                    c => s.push(c),
                }
            }
            s.push('"');
            s
        }
        _ => t.text.clone(),
    }
}

fn render(toks: &[Tok]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Tok> = None;
    for t in toks {
        if let Some(p) = prev {
            let tight = p.is("(")
                || p.is("[")
                || p.is("{")
                || p.is(".")
                || p.is("::")
                || p.is("!")
                || t.is(")")
                || t.is("]")
                || t.is("}")
                || t.is(",")
                || t.is(";")
                || t.is(".")
                || t.is("::")
                || (t.is("(") || t.is("[")) && p.kind == Kind::Ident;
            if !tight {
                out.push(' ');
            }
        }
        out.push_str(&display(t));
        prev = Some(t);
    }
    out
}

fn checksum(toks: &[Tok]) -> String {
    let mut h = crc32fast::Hasher::new();
    for t in toks {
        h.update(&[t.kind as u8]);
        h.update(t.text.as_bytes());
        h.update(&[0]);
    }
    format!("{:08x}", h.finalize())
}

/// Statements of an `.ext` or `.chk` file: up to `;` outside brackets.
fn statements(toks: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, t) in toks.iter().enumerate() {
        if t.open() {
            depth += 1;
        } else if t.close() {
            depth = depth.saturating_sub(1);
        } else if t.is(";") && depth == 0 {
            if i > start {
                out.push(&toks[start..i]);
            }
            start = i + 1;
        }
    }
    if start < toks.len() {
        out.push(&toks[start..]);
    }
    out
}

/// The `:` at the top level after position `from`, which starts a body.
fn body_colon(stmt: &[Tok], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in stmt.iter().enumerate().skip(from) {
        if t.open() {
            depth += 1;
        } else if t.close() {
            depth = depth.saturating_sub(1);
        } else if t.is(":") && depth == 0 {
            return Some(i);
        }
    }
    None
}

fn lines_to_text(lines: Vec<String>) -> String {
    let mut out = String::new();
    for l in lines {
        let _ = writeln!(out, "{l}");
    }
    out
}

/// The outline of an Xtend extension file.
pub fn extension_outline(text: &str) -> String {
    let toks = tokens(text);
    let mut imports = Vec::new();
    let mut functions: Vec<(String, String)> = Vec::new();
    for stmt in statements(&toks) {
        if stmt[0].is("import") || stmt[0].is("extension") {
            imports.push(format!("{};", render(stmt)));
            continue;
        }
        // `[private] [cached] [create] [Type] name(params) : body` or `around pointcut : body`.
        let paren = stmt.iter().position(|t| t.is("("));
        let colon = body_colon(stmt, paren.map_or(0, |p| p + 1));
        match colon {
            Some(c) => {
                let body = &stmt[c + 1..];
                let name =
                    paren.and_then(|p| p.checked_sub(1)).map_or_else(|| render(&stmt[..c]), |n| stmt[n].text.clone());
                let line = if body.first().is_some_and(|t| t.is("JAVA")) {
                    format!("{} : {};", render(&stmt[..c]), render(body))
                } else {
                    format!("{}  // code {}, {} tokens", render(&stmt[..c]), checksum(body), body.len())
                };
                functions.push((name, line));
            }
            None => functions.push((render(stmt), format!("{};", render(stmt)))),
        }
    }
    imports.sort();
    imports.dedup();
    functions.sort();
    let mut lines = imports;
    if !lines.is_empty() && !functions.is_empty() {
        lines.push(String::new());
    }
    lines.extend(functions.into_iter().map(|(_, l)| l));
    lines_to_text(lines)
}

/// The outline of a Check file.
pub fn check_outline(text: &str) -> String {
    let toks = tokens(text);
    let mut imports = Vec::new();
    let mut checks: Vec<(String, String, String)> = Vec::new();
    for stmt in statements(&toks) {
        if stmt[0].is("import") || stmt[0].is("extension") {
            imports.push(format!("{};", render(stmt)));
            continue;
        }
        // `context Type [if guard] ERROR|WARNING message : constraint`
        let severity = stmt.iter().position(|t| t.is("ERROR") || t.is("WARNING"));
        let colon = body_colon(stmt, severity.map_or(0, |s| s + 1));
        match (severity, colon) {
            (Some(s), Some(c)) => {
                let body = &stmt[c + 1..];
                let ty = stmt.get(1).map_or(String::new(), |t| t.text.clone());
                let head = render(&stmt[..c]);
                // Sorted without the severity, so a check turned from WARNING into ERROR stays in place.
                let key = format!("{} {}", render(&stmt[..s]), render(&stmt[s + 1..c]));
                checks.push((ty, key, format!("{head}  // check {}, {} tokens", checksum(body), body.len())));
            }
            _ => checks.push((String::new(), render(stmt), format!("{};", render(stmt)))),
        }
    }
    imports.sort();
    imports.dedup();
    checks.sort();
    let mut lines = imports;
    if !lines.is_empty() && !checks.is_empty() {
        lines.push(String::new());
    }
    lines.extend(checks.into_iter().map(|(_, _, l)| l));
    lines_to_text(lines)
}

/// The outline of an Xpand template.
pub fn template_outline(text: &str) -> String {
    let toks = template_tokens(text);
    // Directives: the tokens between texts.
    let mut directives: Vec<&[Tok]> = Vec::new();
    let mut start = 0;
    for (i, t) in toks.iter().enumerate() {
        if t.kind == Kind::Text {
            if i > start {
                directives.push(&toks[start..i]);
            }
            start = i + 1;
        }
    }
    if start < toks.len() {
        directives.push(&toks[start..]);
    }
    let mut imports = Vec::new();
    let mut definitions: Vec<(String, Vec<String>)> = Vec::new();
    let directive = |d: &[Tok]| format!("\u{AB}{}\u{BB}", render(d));
    let mut k = 0;
    while k < directives.len() {
        let d = directives[k];
        if d[0].is("IMPORT") || d[0].is("EXTENSION") {
            imports.push(directive(d));
            k += 1;
            continue;
        }
        if d[0].is("DEFINE") || d[0].is("AROUND") {
            let end_word = if d[0].is("DEFINE") { "ENDDEFINE" } else { "ENDAROUND" };
            // The body: from after this directive to the matching end.
            let begin = toks.iter().position(|t| std::ptr::eq(t, &d[0])).unwrap_or(0) + d.len();
            let end = toks[begin..].iter().position(|t| t.is(end_word)).map_or(toks.len(), |n| begin + n);
            let body = &toks[begin..end];
            let mut lines = vec![format!("{}  // code {}, {} tokens", directive(d), checksum(body), body.len())];
            // The files it writes and the definitions it calls, in order.
            k += 1;
            while k < directives.len() && !directives[k][0].is(end_word) {
                let inner = directives[k];
                if inner[0].is("FILE") || inner[0].is("EXPAND") {
                    lines.push(format!("    {}", directive(inner)));
                }
                k += 1;
            }
            k += 1;
            let key = format!("{} {}", d.get(1).map_or("", |t| t.text.as_str()), render(d));
            definitions.push((key, lines));
            continue;
        }
        k += 1;
    }
    imports.sort();
    imports.dedup();
    definitions.sort_by(|a, b| a.0.cmp(&b.0));
    let mut lines = imports;
    if !lines.is_empty() && !definitions.is_empty() {
        lines.push(String::new());
    }
    for (_, l) in definitions {
        lines.extend(l);
    }
    lines_to_text(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(toks: Vec<Tok>) -> Vec<String> {
        toks.into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn lexes_templates() {
        let t = "«IMPORT metamodel»\nheader is ignored\n«DEFINE main FOR Model»\n«REM»a comment «with» guillemets«ENDREM»\
                 «FILE name + \".java\"»class «name» {«FOREACH attributes AS a SEPARATOR ', '-»\n  «a.name»«ENDFOREACH»}«ENDFILE»\n«ENDDEFINE»\ntrailer";
        assert_eq!(
            texts(template_tokens(t)),
            [
                "IMPORT",
                "metamodel",
                "",
                "DEFINE",
                "main",
                "FOR",
                "Model",
                "\n",
                "FILE",
                "name",
                "+",
                ".java",
                "class ",
                "name",
                " {",
                "FOREACH",
                "attributes",
                "AS",
                "a",
                "SEPARATOR",
                ", ",
                "-",
                "\n  ",
                "a",
                ".",
                "name",
                "",
                "ENDFOREACH",
                "}",
                "ENDFILE",
                "\n",
                "ENDDEFINE",
                ""
            ]
        );
        // Comments, spacing in directives, quotes and text outside definitions do not count.
        let u = "«IMPORT  metamodel» «REM»x«ENDREM»\n«DEFINE main FOR Model»\n«FILE name+'.java' /* f */»class «name» {«FOREACH attributes AS a SEPARATOR \", \" -»\n  «a.name»«ENDFOREACH»}«ENDFILE»\n«ENDDEFINE»";
        assert!(same_template(t, u));
        assert!(!same_template(t, &t.replace("class «name»", "class  «name»")));
        assert!(!same_template(t, &t.replace("', '-»", "', '»")));
        assert!(same_template(t, &t.replace("«ENDDEFINE»", "«ENDDEFINE-»")));
    }

    #[test]
    fn extensions_and_checks() {
        let a = "import metamodel;\nextension other::Util;\n// helpers\nString javaName(Entity e) :\n  e.name.toFirstUpper();\n\ncached List[Attribute] keys(Entity e) : e.attributes.select(a|a.key);\nString now() : JAVA util.Dates.now();\n";
        let b = "import metamodel; extension other::Util;\nString javaName(Entity e) : e.name.toFirstUpper();\n/* cached */\ncached List[Attribute] keys(Entity e) :\n    e.attributes.select(a | a.key);\nString now() : JAVA util.Dates.now();";
        assert!(same_code(a, b));
        assert!(!same_code(a, &a.replace("a.key", "!a.key")));
        let o = extension_outline(a);
        assert!(
            o.starts_with("extension other::Util;\nimport metamodel;\n\nString javaName(Entity e)  // code "),
            "{o}"
        );
        assert!(o.contains("\ncached List[Attribute] keys(Entity e)  // code "), "{o}");
        assert!(o.ends_with("\nString now() : JAVA util.Dates.now();\n"), "{o}");
        assert_eq!(o, extension_outline(b));
        let c = "import metamodel;\ncontext Entity ERROR \"Duplicate entity \" + name :\n  model().entities.select(e|e.name == name).size == 1;\ncontext Model if types.size > 0 WARNING 'No entities' : types.exists(e|Entity.isInstance(e));";
        let co = check_outline(c);
        assert!(
            co.starts_with("import metamodel;\n\ncontext Entity ERROR \"Duplicate entity \" + name  // check "),
            "{co}"
        );
        assert!(co.contains("\ncontext Model if types.size > 0 WARNING \"No entities\"  // check "), "{co}");
    }

    #[test]
    fn outlines_a_template() {
        let t = "«IMPORT metamodel»\n«EXTENSION templates::Names»\n«DEFINE main FOR Model»\n«EXPAND entity FOREACH entities»\n«ENDDEFINE»\n\n«DEFINE entity FOR Entity»\n«FILE javaName() + \".java\"»\nclass «javaName()» {}\n«ENDFILE»\n«ENDDEFINE»\n";
        let o = template_outline(t);
        assert!(
            o.starts_with("«EXTENSION templates::Names»\n«IMPORT metamodel»\n\n«DEFINE entity FOR Entity»  // code "),
            "{o}"
        );
        assert!(o.contains("\n    «FILE javaName() + \".java\"»\n«DEFINE main FOR Model»  // code "), "{o}");
        assert!(o.ends_with("\n    «EXPAND entity FOREACH entities»\n"), "{o}");
    }
}
