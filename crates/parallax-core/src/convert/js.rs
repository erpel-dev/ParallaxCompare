//! JavaScript and TypeScript source (also JSX): an outline of the code, and
//! a comparison that ignores comments and formatting.
//!
//! The lexer tells a regex from a division by what precedes it, follows
//! template literals through nested `${…}`, and reads JSX elements in `.js`,
//! `.jsx` and `.tsx` files, where it cleans text children the way React does
//! (whitespace across line breaks collapses). Strings and numbers compare by
//! value. Semicolons are inserted where JavaScript inserts them by itself, so
//! code with and without semicolons compares equal.
//!
//! What a formatter like Prettier adds or removes does not count: trailing
//! commas, parentheses that precedence makes redundant (`(a && b) || c`), the
//! parentheses of a single arrow parameter, `new Foo` versus `new Foo()`,
//! quotes around object keys, `,` versus `;` between interface members, a
//! leading `|` in a union type, parentheses around JSX, `{" "}` in JSX text and
//! `attr={"x"}` versus `attr="x"`.
//!
//! The outline lists imports, exports, functions with their signature and a
//! checksum of their body, classes with their members, interfaces, types,
//! enums and top-level variables.

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Ident,
    Number,
    Str,
    Template,
    Regex,
    Punct,
    JsxText,
}

#[derive(Debug, Clone)]
pub struct Tok {
    pub kind: Kind,
    pub text: String,
    /// Whether a line break came before the token.
    pub nl: bool,
    /// Set on the last token of a JSX element, which ends an expression.
    pub jsx_end: bool,
}

impl Tok {
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

/// The dialect, from the file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dialect {
    pub ts: bool,
    pub jsx: bool,
}

impl Dialect {
    pub fn of(name: &str) -> Option<Dialect> {
        let lower = name.to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or("");
        Some(match ext {
            "js" | "mjs" | "cjs" | "jsx" => Dialect { ts: false, jsx: true },
            "ts" | "mts" | "cts" => Dialect { ts: true, jsx: false },
            "tsx" => Dialect { ts: true, jsx: true },
            _ => return None,
        })
    }
}

const PUNCT: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=", ">=", "&&",
    "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "**", "</", "/>",
];

/// Words after which an expression (not an operator) follows.
const TERM_AFTER: &[&str] = &[
    "return",
    "typeof",
    "instanceof",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "yield",
    "await",
    "extends",
    "default",
];

const KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "await",
    "async",
    "of",
    "static",
    "get",
    "set",
    "interface",
    "type",
    "enum",
    "namespace",
    "declare",
    "abstract",
    "readonly",
    "public",
    "private",
    "protected",
    "implements",
    "as",
    "satisfies",
    "keyof",
    "infer",
    "is",
    "asserts",
    "module",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Brace {
    Block,
    Template,
    JsxExpr,
}

fn ident_start(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphabetic()
}
fn ident_continue(c: char) -> bool {
    c == '_' || c == '$' || c.is_alphanumeric() || c == '\u{200c}' || c == '\u{200d}'
}

/// The value of a number literal, so `0x10`, `16` and `1.6e1` compare equal.
fn number_value(raw: &str) -> String {
    let t = raw.replace('_', "").to_ascii_lowercase();
    if let Some(big) = t.strip_suffix('n') {
        return format!("{}n", big);
    }
    let radix = |s: &str, r: u32| u128::from_str_radix(s, r).ok().map(|v| v as f64);
    let v = if let Some(h) = t.strip_prefix("0x") {
        radix(h, 16)
    } else if let Some(o) = t.strip_prefix("0o") {
        radix(o, 8)
    } else if let Some(b) = t.strip_prefix("0b") {
        radix(b, 2)
    } else if t.len() > 1 && t.starts_with('0') && t.bytes().all(|c| c.is_ascii_digit()) && !t.contains(['8', '9']) {
        // Legacy octal: 017.
        radix(&t[1..], 8)
    } else {
        t.parse::<f64>().ok()
    };
    match v {
        Some(v) if v.is_finite() => format!("{v}"),
        _ => t,
    }
}

/// The value of a string literal's body.
fn string_value(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else { break };
        match e {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' if !chars.peek().is_some_and(char::is_ascii_digit) => out.push('\0'),
            'x' => {
                let h: String = chars.by_ref().take(2).collect();
                match u32::from_str_radix(&h, 16).ok().and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => out.push_str(&h),
                }
            }
            'u' => {
                let h: String = if chars.peek() == Some(&'{') {
                    chars.next();
                    chars.by_ref().take_while(|&c| c != '}').collect()
                } else {
                    chars.by_ref().take(4).collect()
                };
                match u32::from_str_radix(&h, 16).ok() {
                    // A surrogate pair written as two escapes.
                    Some(hi @ 0xD800..=0xDBFF) => {
                        let mut look = chars.clone();
                        let lo = (look.next() == Some('\\') && look.next() == Some('u'))
                            .then(|| look.by_ref().take(4).collect::<String>())
                            .and_then(|l| u32::from_str_radix(&l, 16).ok())
                            .filter(|l| (0xDC00..=0xDFFF).contains(l));
                        match lo {
                            Some(lo) => {
                                chars = look;
                                out.extend(char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)));
                            }
                            None => {
                                let _ = write!(out, "\\u{hi:04x}");
                            }
                        }
                    }
                    Some(v) => match char::from_u32(v) {
                        Some(c) => out.push(c),
                        None => {
                            let _ = write!(out, "\\u{v:04x}");
                        }
                    },
                    None => out.push_str(&h),
                }
            }
            // A line continuation.
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
            }
            '\n' | '\u{2028}' | '\u{2029}' => {}
            '1'..='7' => {
                // Legacy octal escapes.
                let mut v = e.to_digit(8).expect("octal");
                for _ in 0..2 {
                    match chars.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) if v * 8 + d < 256 => {
                            v = v * 8 + d;
                            chars.next();
                        }
                        _ => break,
                    }
                }
                out.extend(char::from_u32(v));
            }
            other => out.push(other),
        }
    }
    out
}

/// JSX text as React sees it: whitespace with a line break collapses, lines are trimmed.
fn clean_jsx_text(raw: &str) -> String {
    let lines: Vec<&str> = raw.split(['\n']).map(|l| l.trim_end_matches('\r')).collect();
    // As Babel does: the last line with more than spaces, or the first.
    let last_non_empty = Some(lines.iter().rposition(|l| !l.trim_matches([' ', '\t']).is_empty()).unwrap_or(0));
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let mut l = line.replace('\t', " ");
        if i != 0 {
            l = l.trim_start_matches(' ').to_string();
        }
        if i != lines.len() - 1 {
            l = l.trim_end_matches(' ').to_string();
        }
        if !l.is_empty() {
            out.push_str(&l);
            if Some(i) != last_non_empty {
                out.push(' ');
            }
        }
    }
    out
}

struct Lexer<'a> {
    s: &'a str,
    i: usize,
    d: Dialect,
    out: Vec<Tok>,
    braces: Vec<Brace>,
    nl: bool,
    /// Inside a JSX tag or children, where line breaks mean nothing to ASI.
    in_jsx: bool,
}

impl<'a> Lexer<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }
    fn push(&mut self, kind: Kind, text: impl Into<String>) {
        let nl = std::mem::take(&mut self.nl) && !self.in_jsx;
        self.out.push(Tok { kind, text: text.into(), nl, jsx_end: false });
    }

    fn expect_term(&self) -> bool {
        match self.out.last() {
            None => true,
            Some(t) if t.jsx_end => false,
            Some(t) => match t.kind {
                Kind::Punct => !matches!(t.text.as_str(), ")" | "]" | "++" | "--"),
                Kind::Ident => TERM_AFTER.contains(&t.text.as_str()),
                Kind::Template => t.text.ends_with("${"),
                _ => false,
            },
        }
    }

    /// Whitespace and comments; returns false at the end.
    fn skip_trivia(&mut self) -> bool {
        loop {
            let Some(c) = self.rest().chars().next() else { return false };
            if c == '\n' || c == '\r' || c == '\u{2028}' || c == '\u{2029}' {
                self.nl = true;
                self.i += c.len_utf8();
            } else if c.is_whitespace() || c == '\u{feff}' {
                self.i += c.len_utf8();
            } else if self.rest().starts_with("//") {
                self.i += self.rest().find('\n').unwrap_or(self.rest().len());
            } else if self.rest().starts_with("/*") {
                let end = self.rest()[2..].find("*/").map_or(self.rest().len(), |n| n + 4);
                if self.rest()[..end].contains('\n') {
                    self.nl = true;
                }
                self.i += end;
            } else {
                return true;
            }
        }
    }

    /// The rest of a template literal after `` ` `` or `}`: up to `${` or the closing `` ` ``.
    fn template(&mut self, start: &str) {
        let b = self.s.as_bytes();
        let from = self.i;
        let mut j = self.i;
        while j < b.len() {
            match b[j] {
                b'\\' => j += 2,
                b'`' => {
                    let raw = format!("{start}{}`", &self.s[from..j]);
                    self.i = j + 1;
                    self.push(Kind::Template, raw);
                    return;
                }
                b'$' if b.get(j + 1) == Some(&b'{') => {
                    let raw = format!("{start}{}${{", &self.s[from..j]);
                    self.i = j + 2;
                    self.push(Kind::Template, raw);
                    self.braces.push(Brace::Template);
                    return;
                }
                _ => j += 1,
            }
        }
        let raw = format!("{start}{}", &self.s[from..]);
        self.i = self.s.len();
        self.push(Kind::Template, raw);
    }

    fn quoted(&mut self, q: char) -> String {
        let b = self.s.as_bytes();
        let mut j = self.i + 1;
        while j < b.len() && b[j] != q as u8 && b[j] != b'\n' {
            j += if b[j] == b'\\' { 2 } else { 1 };
        }
        let body = &self.s[self.i + 1..j.min(self.s.len())];
        self.i = (j + 1).min(self.s.len());
        body.to_string()
    }

    fn regex(&mut self) {
        let b = self.s.as_bytes();
        let mut j = self.i + 1;
        let mut class = false;
        while j < b.len() && b[j] != b'\n' {
            match b[j] {
                b'\\' => j += 1,
                b'[' => class = true,
                b']' => class = false,
                b'/' if !class => break,
                _ => {}
            }
            j += 1;
        }
        let body = &self.s[self.i..(j + 1).min(self.s.len())];
        let mut k = (j + 1).min(self.s.len());
        let flags_start = k;
        while k < b.len() && (b[k] as char).is_ascii_alphabetic() {
            k += 1;
        }
        let mut flags: Vec<char> = self.s[flags_start..k].chars().collect();
        flags.sort_unstable();
        let text = format!("{body}{}", flags.into_iter().collect::<String>());
        self.i = k;
        self.push(Kind::Regex, text);
    }

    fn ident(&mut self) -> &'a str {
        let start = self.i;
        let n: usize = self.rest().chars().take_while(|&c| ident_continue(c)).map(char::len_utf8).sum();
        self.i += n.max(1);
        &self.s[start..self.i]
    }

    /// Whether `<` starts a JSX element here (not a comparison or, in TSX, a type parameter list).
    fn jsx_start(&self) -> bool {
        if !self.d.jsx || !self.expect_term() {
            return false;
        }
        let after = &self.rest()[1..];
        if after.starts_with('>') {
            return true;
        }
        if !after.starts_with(ident_start) {
            return false;
        }
        if self.d.ts {
            // `<T,>() =>` and `<T extends X>() =>` are type parameters.
            let name_len: usize = after.chars().take_while(|&c| ident_continue(c)).map(char::len_utf8).sum();
            let next = after[name_len..].trim_start();
            if next.starts_with(',') || next.starts_with("extends ") {
                return false;
            }
        }
        true
    }

    fn jsx_name(&mut self) {
        let start = self.i;
        let n: usize = self
            .rest()
            .chars()
            .take_while(|&c| ident_continue(c) || c == '-' || c == '.' || c == ':')
            .map(char::len_utf8)
            .sum();
        self.i += n;
        let name = self.s[start..self.i].to_string();
        self.push(Kind::Ident, name);
    }

    /// An expression container `{…}` inside JSX, lexed as code.
    fn jsx_expression(&mut self) {
        self.i += 1;
        self.push(Kind::Punct, "{");
        self.braces.push(Brace::JsxExpr);
        let outer = std::mem::replace(&mut self.in_jsx, false);
        self.code();
        self.in_jsx = outer;
    }

    fn jsx_element(&mut self) {
        // The `<` that opens the element keeps its line break for ASI.
        self.i += 1;
        self.push(Kind::Punct, "<");
        let outer = std::mem::replace(&mut self.in_jsx, true);
        self.jsx_element_inner();
        self.in_jsx = outer;
        // The element ends an expression; what follows on a new line may need ASI.
        self.nl = false;
    }

    fn jsx_element_inner(&mut self) {
        // A fragment `<>`.
        if self.rest().starts_with('>') {
            self.i += 1;
            self.push(Kind::Punct, ">");
            self.jsx_children();
            return;
        }
        self.jsx_name();
        loop {
            if !self.skip_trivia() {
                return;
            }
            let r = self.rest();
            if r.starts_with("/>") {
                self.i += 2;
                self.push(Kind::Punct, "/>");
                self.out.last_mut().expect("pushed").jsx_end = true;
                return;
            }
            if r.starts_with('>') {
                self.i += 1;
                self.push(Kind::Punct, ">");
                self.jsx_children();
                return;
            }
            if r.starts_with('{') {
                self.jsx_expression();
                continue;
            }
            if r.starts_with(|c: char| ident_start(c)) {
                self.jsx_name();
                self.skip_trivia();
                if self.rest().starts_with('=') {
                    self.i += 1;
                    self.push(Kind::Punct, "=");
                    self.skip_trivia();
                    let r = self.rest();
                    if r.starts_with(['"', '\'']) {
                        let q = r.chars().next().expect("quote");
                        // JSX attribute strings have no escapes.
                        let end = r[1..].find(q).map_or(r.len(), |n| n + 2);
                        let value = r[1..end.saturating_sub(1).max(1)].to_string();
                        self.i += end;
                        self.push(Kind::Str, value);
                    } else if r.starts_with('{') {
                        let at = self.out.len();
                        self.jsx_expression();
                        // `attr={"x"}` is `attr="x"`.
                        let simple = self.out.len() == at + 3
                            && self.out[at + 1].kind == Kind::Str
                            && !self.out[at + 1].text.contains(['"', '&', '\\', '\n']);
                        if simple {
                            let v = self.out[at + 1].clone();
                            self.out.truncate(at);
                            self.out.push(Tok { nl: false, ..v });
                        }
                    } else if r.starts_with('<') {
                        self.jsx_element();
                    }
                }
                continue;
            }
            // Something unexpected: give up on JSX here.
            let c = self.rest().chars().next().expect("not at end");
            self.i += c.len_utf8();
            self.push(Kind::Punct, c.to_string());
            return;
        }
    }

    fn jsx_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.out.last_mut().filter(|t| t.kind == Kind::JsxText) {
            last.text.push_str(&text);
        } else {
            self.push(Kind::JsxText, text);
        }
    }

    fn jsx_children(&mut self) {
        loop {
            let r = self.rest();
            if r.is_empty() {
                return;
            }
            let end = r.find(['<', '{']).unwrap_or(r.len());
            if end > 0 {
                let text = clean_jsx_text(&r[..end]);
                self.i += end;
                self.nl = false;
                self.jsx_text(text);
                continue;
            }
            if r.starts_with("</") {
                self.i += 2;
                self.push(Kind::Punct, "</");
                self.skip_trivia();
                if !self.rest().starts_with('>') {
                    self.jsx_name();
                }
                self.skip_trivia();
                if self.rest().starts_with('>') {
                    self.i += 1;
                }
                self.push(Kind::Punct, ">");
                self.out.last_mut().expect("pushed").jsx_end = true;
                return;
            }
            if r.starts_with('<') {
                self.jsx_element();
                continue;
            }
            // `{expr}`: `{" "}` is text, `{/* comment */}` is nothing.
            let at = self.out.len();
            self.jsx_expression();
            // Unclosed at the end of the file: only the `{`.
            if self.out.len() < at + 2 {
                continue;
            }
            let inner = &self.out[at + 1..self.out.len() - 1];
            if inner.is_empty() {
                self.out.truncate(at);
            } else if inner.len() == 1 && inner[0].kind == Kind::Str && inner[0].text.chars().all(|c| c == ' ') {
                let text = inner[0].text.clone();
                self.out.truncate(at);
                self.jsx_text(text);
            }
        }
    }

    /// Code up to the end, or up to the `}` that closes a JSX expression container.
    fn code(&mut self) {
        while self.skip_trivia() {
            let r = self.rest();
            let c = r.chars().next().expect("not at end");
            if self.i == 0 && r.starts_with("#!") {
                self.i += r.find('\n').unwrap_or(r.len());
                continue;
            }
            match c {
                '"' | '\'' => {
                    let body = self.quoted(c);
                    self.push(Kind::Str, string_value(&body));
                }
                '`' => {
                    self.i += 1;
                    self.template("`");
                }
                '{' => {
                    self.i += 1;
                    self.push(Kind::Punct, "{");
                    self.braces.push(Brace::Block);
                }
                '}' => {
                    self.i += 1;
                    match self.braces.pop() {
                        Some(Brace::Template) => {
                            let nl = self.nl;
                            self.template("}");
                            self.out.last_mut().expect("pushed").nl = nl;
                        }
                        Some(Brace::JsxExpr) => {
                            self.push(Kind::Punct, "}");
                            return;
                        }
                        _ => self.push(Kind::Punct, "}"),
                    }
                }
                '/' if self.expect_term() => self.regex(),
                '<' if self.jsx_start() => self.jsx_element(),
                '#' if r[1..].starts_with(ident_start) => {
                    self.i += 1;
                    let name = format!("#{}", self.ident());
                    self.push(Kind::Ident, name);
                }
                _ if c.is_ascii_digit() || (c == '.' && r[1..].starts_with(|d: char| d.is_ascii_digit())) => {
                    let b = r.as_bytes();
                    let mut j = 0;
                    let hex = r.starts_with("0x") || r.starts_with("0X");
                    while j < b.len() {
                        let d = b[j];
                        if d.is_ascii_alphanumeric() || d == b'_' || d == b'.' {
                            j += 1;
                            if !hex && matches!(d, b'e' | b'E') && matches!(b.get(j), Some(b'+' | b'-')) {
                                j += 1;
                            }
                        } else {
                            break;
                        }
                    }
                    let raw = &r[..j];
                    self.i += j;
                    self.push(Kind::Number, number_value(raw));
                }
                _ if ident_start(c) || c == '\\' => {
                    let w = self.ident();
                    self.push(Kind::Ident, w);
                }
                _ => {
                    let p = PUNCT
                        .iter()
                        .filter(|p| !matches!(**p, "</" | "/>"))
                        .find(|p| {
                            r.starts_with(**p) && !(**p == "?." && r[2..].starts_with(|d: char| d.is_ascii_digit()))
                        })
                        .map_or(&r[..c.len_utf8()], |p| *p);
                    self.i += p.len();
                    self.push(Kind::Punct, p);
                }
            }
        }
    }
}

/// The tokens of JavaScript or TypeScript source, without comments; `nl` marks line breaks.
pub fn tokens(text: &str, d: Dialect) -> Vec<Tok> {
    let mut lx = Lexer { s: text, i: 0, d, out: Vec::new(), braces: Vec::new(), nl: false, in_jsx: false };
    lx.code();
    lx.out
}

fn tok(kind: Kind, text: &str) -> Tok {
    Tok { kind, text: text.to_string(), nl: false, jsx_end: false }
}

/// The index of the bracket that closes the one at `open`.
fn closing(toks: &[Tok], open: usize) -> usize {
    let mut depth = 0usize;
    for (j, t) in toks.iter().enumerate().skip(open) {
        if t.open() {
            depth += 1;
        } else if t.close() {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return j;
            }
        }
    }
    toks.len()
}

/// Reserved words after which an expression cannot have ended.
const NOT_ENDING: &[&str] = &[
    "case",
    "catch",
    "class",
    "const",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "let",
    "new",
    "return",
    "switch",
    "throw",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "await",
    "implements",
    "keyof",
    "infer",
    "satisfies",
    "as",
];

fn ends_expression(t: &Tok) -> bool {
    t.jsx_end
        || match t.kind {
            Kind::Ident => !NOT_ENDING.contains(&t.text.as_str()),
            Kind::Number | Kind::Str | Kind::Regex => true,
            Kind::Template => t.text.ends_with('`'),
            Kind::Punct => matches!(t.text.as_str(), ")" | "]" | "}" | "++" | "--"),
            Kind::JsxText => false,
        }
}

/// Whether a token at the start of a line continues the expression before it.
fn continues(t: &Tok) -> bool {
    match t.kind {
        Kind::Punct => !matches!(t.text.as_str(), "++" | "--" | "{" | "}" | "!" | "~" | "@" | "#" | ";"),
        Kind::Ident => {
            matches!(t.text.as_str(), "in" | "instanceof" | "as" | "satisfies" | "of" | "extends" | "implements")
        }
        Kind::Template => true,
        _ => false,
    }
}

/// The parentheses that close `if (…)`, `for (…)`, `while (…)`, `with (…)`
/// headers, but not the `while (…)` that ends a `do` loop.
fn headers(toks: &[Tok]) -> Vec<bool> {
    let n = toks.len();
    let mut opener = vec![usize::MAX; n];
    let mut stack = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if t.open() {
            stack.push(i);
        } else if t.close()
            && let Some(o) = stack.pop()
        {
            opener[i] = o;
        }
    }
    let mut header_close = vec![false; n];
    for i in 1..n {
        let property = i >= 2 && (toks[i - 2].is(".") || toks[i - 2].is("?."));
        if toks[i].is("(") && !property && ["if", "for", "while", "with"].iter().any(|k| toks[i - 1].is(k)) {
            let do_while = toks[i - 1].is("while")
                && i >= 2
                && toks[i - 2].is("}")
                && opener[i - 2] != usize::MAX
                && opener[i - 2] > 0
                && toks[opener[i - 2] - 1].is("do");
            let c = closing(toks, i);
            if c < n && !do_while {
                header_close[c] = true;
            }
        }
    }
    header_close
}

/// Whether the word at `i` starts an interface or type literal member: `name:` or `name?:`.
fn toks_next_is_member(toks: &[Tok], i: usize) -> bool {
    toks.get(i + 1).is_some_and(|t| t.is(":") || t.is("?") && toks.get(i + 2).is_some_and(|u| u.is(":")))
}

/// Inserts the semicolons JavaScript inserts by itself at line breaks.
fn insert_semicolons(toks: Vec<Tok>) -> Vec<Tok> {
    // Parentheses that close `if (…)`, `for (…)`, `while (…)`, `with (…)`.
    let n = toks.len();
    let header_close = headers(&toks);
    let rest_peek = toks.clone();
    // TypeScript: a `>` that closes type arguments ends an expression.
    let mut angle_close = vec![false; n];
    if toks.iter().any(|t| t.is("<")) {
        let mut stack: Vec<Vec<usize>> = vec![Vec::new()];
        for (i, t) in rest_peek.iter().enumerate() {
            if t.open() {
                stack.push(Vec::new());
            } else if t.close() {
                if stack.len() > 1 {
                    stack.pop();
                }
            } else if t.is(";") {
                stack.last_mut().expect("never empty").clear();
            } else if t.is("<") {
                stack.last_mut().expect("never empty").push(i);
            } else if t.is(">") || t.is(">>") || t.is(">>>") {
                let top = stack.last_mut().expect("never empty");
                for _ in 0..t.text.len() {
                    if top.pop().is_some() {
                        angle_close[i] = true;
                    }
                }
            }
        }
    }
    let mut out: Vec<Tok> = Vec::with_capacity(n + n / 8);
    for (i, t) in toks.into_iter().enumerate() {
        if let Some(p) = out.last() {
            // A word after `.` is a property name, which ends an expression.
            let property =
                out.len() >= 2 && (out[out.len() - 2].is(".") || out[out.len() - 2].is("?.")) && p.kind == Kind::Ident;
            let restricted = !property && ["return", "break", "continue", "throw", "yield"].iter().any(|k| p.is(k));
            let declaration = [
                "export",
                "import",
                "interface",
                "type",
                "const",
                "let",
                "var",
                "function",
                "class",
                "declare",
                "enum",
                "namespace",
                "abstract",
                "async",
                "@",
            ]
            .iter()
            .any(|k| t.is(k));
            let member_next = t.kind == Kind::Ident && toks_next_is_member(&rest_peek, i);
            let type_end = p.is(">") || p.is(">>") || p.is(">>>") || p.is("void");
            let type_args_end = type_end
                && (declaration || member_next || angle_close[i - 1] && t.kind == Kind::Ident && !continues(&t));
            // TypeScript's non-null `x!` ends an expression.
            let non_null = p.is("!") && out.len() >= 2 && ends_expression(&out[out.len() - 2]);
            let needed = t.nl
                && !t.is(";")
                && !t.is("}")
                && !t.is("{")
                && ((restricted && !t.is(")"))
                    || ((ends_expression(p) || property) && !continues(&t) && !(p.is(")") && header_close[i - 1]))
                    || ((ends_expression(p) || property) && (t.is("++") || t.is("--")))
                    || type_args_end
                    || (non_null && !continues(&t)));
            if needed && !p.is(";") {
                out.push(tok(Kind::Punct, ";"));
            }
        }
        out.push(t);
    }
    out
}

/// Whether the brace at `open` is a block (statements, class or interface
/// body) rather than an object literal or type literal.
fn is_block(toks: &[Tok], open: usize) -> bool {
    let Some(p) = open.checked_sub(1).map(|p| &toks[p]) else { return true };
    match p.kind {
        Kind::Punct => matches!(p.text.as_str(), ")" | "=>" | ";" | "{" | "}" | ">"),
        Kind::Ident => !["return", "typeof", "case", "throw", "yield", "await", "in", "of", "delete", "void", "new"]
            .contains(&p.text.as_str()),
        _ => false,
    }
}

/// Whether the `:` at `colon` ends a `case`, `default` or a label, so a brace
/// after it is a block.
fn statement_colon(toks: &[Tok], colon: usize) -> bool {
    let mut depth = 0usize;
    let mut k = colon;
    while k > 0 {
        let u = &toks[k - 1];
        if u.close() {
            depth += 1;
        } else if u.open() {
            if depth == 0 {
                break;
            }
            depth -= 1;
        } else if depth == 0 && (u.is(";") || u.is(":") && k - 1 != colon) {
            break;
        }
        k -= 1;
    }
    toks[k].is("case") || toks[k].is("default") || (k + 1 == colon && toks[k].kind == Kind::Ident)
}

/// Whether the token before `open` is the `}` of a function expression, so
/// the parenthesis calls it: `var x = function () {}(a)`.
fn fn_expression_end(toks: &[Tok], open: usize) -> bool {
    if open == 0 || !toks[open - 1].is("}") {
        return false;
    }
    // Find the `{` of that body, then `function` before its parameters.
    let mut depth = 0usize;
    let mut k = open - 1;
    loop {
        let t = &toks[k];
        if t.close() {
            depth += 1;
        } else if t.open() {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
        if k == 0 {
            return false;
        }
        k -= 1;
    }
    if k == 0 || !toks[k - 1].is(")") {
        return false;
    }
    let mut depth = 0usize;
    let mut j = k - 1;
    loop {
        let t = &toks[j];
        if t.close() {
            depth += 1;
        } else if t.open() {
            depth -= 1;
            if depth == 0 {
                break;
            }
        }
        if j == 0 {
            return false;
        }
        j -= 1;
    }
    let f = if j >= 1 && toks[j - 1].kind == Kind::Ident && !toks[j - 1].is("function") {
        j.saturating_sub(2)
    } else {
        j.saturating_sub(1)
    };
    if !toks.get(f).is_some_and(|t| t.is("function")) {
        return false;
    }
    // An expression position: not at the start of a statement.
    let mut b = f;
    while b > 0
        && (toks[b - 1].is("async")
            || toks[b - 1].is("export")
            || toks[b - 1].is("default")
            || toks[b - 1].is("declare"))
    {
        b -= 1;
    }
    b > 0 && !(toks[b - 1].is(";") || toks[b - 1].is("{") || toks[b - 1].is("}"))
}

/// Whether tokens are a name or member chain: `a`, `a.b[c](d)`.
fn chain_of(inner: &[Tok]) -> bool {
    inner.first().is_some_and(|f| f.kind == Kind::Ident && !NOT_ENDING.contains(&f.text.as_str()) && !f.is("async"))
        && loosest(inner) == Some(16)
        && {
            let mut depth = 0usize;
            inner.iter().all(|x| {
                if x.open() {
                    depth += 1;
                } else if x.close() {
                    depth = depth.saturating_sub(1);
                }
                depth > 0
                    || x.close()
                    || x.open()
                    || x.is(".")
                    || x.kind == Kind::Ident && !NOT_ENDING.contains(&x.text.as_str())
            })
        }
}

/// Binary operator precedence, higher binds tighter.
fn precedence(op: &str) -> Option<u8> {
    Some(match op {
        "??" => 3,
        "||" => 4,
        "&&" => 5,
        "|" => 6,
        "^" => 7,
        "&" => 8,
        "==" | "!=" | "===" | "!==" => 9,
        "<" | ">" | "<=" | ">=" | "instanceof" | "in" => 10,
        "<<" | ">>" | ">>>" => 11,
        "+" | "-" => 12,
        "*" | "/" | "%" => 13,
        "**" => 14,
        _ => return None,
    })
}

/// The loosest operator at the top level of `toks`: a binary operator's
/// precedence, 2 for a conditional, 1 for an assignment or arrow function, 16
/// for none; `None` for a comma, spread or `yield`, which parentheses always
/// change.
fn loosest(toks: &[Tok]) -> Option<u8> {
    // Parentheses around the whole: look inside.
    if toks.len() >= 2 && toks[0].is("(") && closing(toks, 0) + 1 == toks.len() {
        return loosest(&toks[1..toks.len() - 1]);
    }
    let mut depth = 0usize;
    let mut low = 16u8;
    let mut prev: Option<&Tok> = None;
    for t in toks {
        if t.open() {
            depth += 1;
        } else if t.close() {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            if t.is(",") || t.is("...") || t.is("yield") {
                return None;
            }
            if t.kind == Kind::Punct {
                if t.text == "?" {
                    low = low.min(2);
                } else if t.text == "=>"
                    || (t.text.ends_with('=') && !matches!(t.text.as_str(), "==" | "!=" | "===" | "!==" | "<=" | ">="))
                {
                    low = low.min(1);
                } else if t.text == ":" {
                    // A type annotation or the `:` of a conditional; the `?` decides.
                    if !toks.iter().any(|x| x.is("?")) {
                        return None;
                    }
                }
            }
            if t.is("as") || t.is("satisfies") {
                low = low.min(10);
            }
            let binary = prev.is_some_and(ends_expression);
            if binary
                && let Some(p) =
                    precedence(&t.text).filter(|_| t.kind == Kind::Punct || t.is("in") || t.is("instanceof"))
            {
                low = low.min(p);
            }
        }
        prev = Some(t);
    }
    Some(low)
}

/// Tokens in canonical form, with statement structure kept (for the outline).
fn structural(text: &str, d: Dialect) -> Vec<Tok> {
    let toks = insert_semicolons(tokens(text, d));
    let n = toks.len();
    let header_close = headers(&toks);
    let mut drop = vec![false; n];
    let mut insert_after: Vec<Vec<Tok>> = vec![Vec::new(); n];
    let mut replace: Vec<Option<Tok>> = vec![None; n];
    // Members of interfaces and of type literals in type aliases: `,` is `;`.
    if d.ts {
        let mut type_brace = vec![false; n];
        for i in 0..n {
            if !toks[i].is("{") || type_brace[i] {
                continue;
            }
            // Back to the start of the statement.
            let mut depth = 0usize;
            let mut k = i;
            while k > 0 {
                let u = &toks[k - 1];
                if u.close() {
                    depth += 1;
                } else if u.open() {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                } else if depth == 0 && u.is(";") {
                    break;
                }
                k -= 1;
            }
            while k < i && (toks[k].is("export") || toks[k].is("declare") || toks[k].is("default")) {
                k += 1;
            }
            if toks[k].is("interface")
                || (toks[k].is("type") && toks.get(k + 2).is_some_and(|x| x.is("=") || x.is("<")))
            {
                let end = if toks[k].is("interface") {
                    closing(&toks, i)
                } else {
                    // The whole alias, to its `;`.
                    let mut depth = 0usize;
                    let mut e = i;
                    while e < n && !(depth == 0 && toks[e].is(";")) {
                        if toks[e].open() {
                            depth += 1;
                        } else if toks[e].close() {
                            depth = depth.saturating_sub(1);
                        }
                        e += 1;
                    }
                    e
                };
                for j in i..end.min(n) {
                    if toks[j].is("{") {
                        type_brace[j] = true;
                    }
                }
            }
        }
        let mut stack: Vec<usize> = Vec::new();
        for i in 0..n {
            let t = &toks[i];
            if t.open() {
                stack.push(i);
            } else if t.close() {
                stack.pop();
            } else if t.is(",") && stack.last().is_some_and(|&o| type_brace[o]) {
                replace[i] = Some(tok(Kind::Punct, ";"));
            }
        }
    }
    for i in 0..n {
        let t = &toks[i];
        let prev = i.checked_sub(1).map(|p| &toks[p]);
        // A trailing comma (but not an array hole).
        if t.close() && i >= 2 && toks[i - 1].is(",") && !toks[i - 2].is(",") && !toks[i - 2].open() {
            drop[i - 1] = true;
        }
        if t.is(">") && i >= 1 && toks[i - 1].is(",") {
            drop[i - 1] = true;
        }
        // A leading `|` or `&` in a type.
        if (t.is("|") || t.is("&"))
            && prev.is_some_and(|p| {
                ["=", ":", "(", "<", ",", "[", "|", "&", "as", "satisfies", "extends", "?", "=>", "keyof", "readonly"]
                    .iter()
                    .any(|k| p.is(k))
            })
            && d.ts
        {
            drop[i] = true;
        }
        // `new Foo` is `new Foo()`.
        if t.is("new") && !toks.get(i + 1).is_some_and(|x| x.is(".")) {
            let mut k = i + 1;
            while k < n
                && (toks[k].kind == Kind::Ident && !KEYWORDS.contains(&toks[k].text.as_str()) || toks[k].is("."))
            {
                k += 1;
            }
            if k > i + 1 && !toks.get(k).is_some_and(|x| x.is("(") || x.is("<") || x.kind == Kind::Template) {
                insert_after[k - 1].push(tok(Kind::Punct, "("));
                insert_after[k - 1].push(tok(Kind::Punct, ")"));
            }
        }
        // Quoted object keys: `{"a": 1}` is `{a: 1}`.
        if t.kind == Kind::Str
            && prev.is_some_and(|p| p.is("{") || p.is(","))
            && toks.get(i + 1).is_some_and(|x| x.is(":"))
            && t.text.chars().next().is_some_and(ident_start)
            && t.text.chars().all(ident_continue)
        {
            replace[i] = Some(Tok { kind: Kind::Ident, ..t.clone() });
        }
        if t.is("(") {
            let c = closing(&toks, i);
            if c >= n {
                continue;
            }
            let inner = &toks[i + 1..c];
            let next = toks.get(c + 1);
            let after_header = prev.is_some_and(|p| p.is(")")) && header_close[i - 1];
            let property =
                i >= 2 && (toks[i - 2].is(".") || toks[i - 2].is("?.")) && prev.is_some_and(|p| p.kind == Kind::Ident);
            let callable = !after_header && (property || fn_expression_end(&toks, i))
                || !after_header
                    && prev.is_some_and(|p| {
                        (p.kind == Kind::Ident && !KEYWORDS.contains(&p.text.as_str()))
                            || p.is(")")
                            || p.is("]")
                            || p.is("?.")
                            || p.is(">")
                            || p.is(">>")
                            || p.is(">>>")
                            || p.kind == Kind::Template && p.text.ends_with('`')
                            || p.is("super")
                            || p.is("import")
                            || p.is("this")
                    });
            if callable
                || (!property
                    && prev.is_some_and(|p| {
                        ["if", "for", "while", "with", "switch", "catch", "function"].iter().any(|k| p.is(k))
                    }))
            {
                continue;
            }
            // `(x) => …` is `x => …`.
            if inner.len() == 1 && inner[0].kind == Kind::Ident && next.is_some_and(|x| x.is("=>")) {
                drop[i] = true;
                drop[c] = true;
                continue;
            }
            if inner.is_empty()
                || next.is_some_and(|x| {
                    x.is("=>")
                        || x.is(":")
                            && d.ts
                            && prev.is_some_and(|p| p.is("async") || p.is("=") || p.is("(") || p.is(","))
                })
            {
                continue;
            }
            // Parentheses around one JSX element.
            let jsx = inner.first().is_some_and(|f| f.is("<")) && inner.last().is_some_and(|l| l.jsx_end) && {
                let mut depth = 0i32;
                let mut ends = 0;
                for (k, u) in inner.iter().enumerate() {
                    if u.is("<") && (k == 0 || inner[k - 1].kind != Kind::Ident) {
                        depth += 1;
                    }
                    if u.jsx_end {
                        depth -= 1;
                        if depth == 0 {
                            ends += 1;
                        }
                    }
                }
                ends == 1
            };
            let starts_object = inner[0].is("{")
                || inner[0].is("function")
                || inner[0].is("class")
                || inner[0].is("let")
                || inner[0].is("async") && inner.get(1).is_some_and(|f| f.is("function"));
            let statement_start =
                prev.is_none_or(|p| p.is(";") || p.is("{") || p.is("}") || p.is("else") || p.is("do")) || after_header;
            let statement_end = next.is_none_or(|x| x.is(";") || x.is("}"));
            let low = loosest(inner);
            let single_jsx = inner.first().is_some_and(|f| f.is("<")) && inner.last().is_some_and(|l| l.jsx_end);
            if d.ts && !single_jsx {
                // `(name: T);` is a call signature; `extends A, (B)` an expression;
                // `(a < b) > c` would read as type arguments.
                let mut depth = 0usize;
                let mut annotated = false;
                let mut angle = false;
                // A `?` of a conditional, not the `?` of an optional parameter `a?: T`.
                let conditional =
                    inner.iter().enumerate().any(|(k, y)| y.is("?") && !inner.get(k + 1).is_some_and(|z| z.is(":")));
                for (k, x) in inner.iter().enumerate() {
                    if x.open() {
                        depth += 1;
                    } else if x.close() {
                        depth = depth.saturating_sub(1);
                    } else if depth == 0 {
                        annotated |= x.is(":") && (!conditional || k > 0 && inner[k - 1].is("?"));
                        angle |= x.is("<") || x.is(">") || x.is(">>");
                    }
                }
                let mut k = i;
                let mut heritage = false;
                let mut depth = 0usize;
                while k > 0 {
                    let u = &toks[k - 1];
                    if u.close() {
                        depth += 1;
                    } else if u.open() {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    } else if depth == 0 && (u.is(";") || u.is("}")) {
                        break;
                    } else if depth == 0 && (u.is("extends") || u.is("implements")) {
                        heritage = true;
                        break;
                    }
                    k -= 1;
                }
                let angle_next = next.is_some_and(|x| x.is("<") || x.is(">") || x.is(">>"))
                    || prev.is_some_and(|p| p.is("<") || p.is(">"));
                if annotated || heritage || angle || angle_next {
                    continue;
                }
            }
            // `return (a, b)` is `return a, b`.
            if prev.is_some_and(|p| p.is("return") || p.is("throw")) && statement_end && !starts_object {
                drop[i] = true;
                drop[c] = true;
                continue;
            }
            // A whole statement: `(a, b);`
            if statement_start && statement_end && !starts_object && inner.iter().all(|x| !x.is("yield")) {
                drop[i] = true;
                drop[c] = true;
                continue;
            }
            let Some(low) = low else {
                if jsx {
                    drop[i] = true;
                    drop[c] = true;
                }
                continue;
            };
            // What surrounds the parentheses.
            // In TypeScript `typeof (x)` in a type and `[(k) in T]` mean something else.
            if d.ts
                && (prev.is_some_and(|p| p.is("typeof") || p.is("keyof"))
                    || prev.is_some_and(|p| p.is("[")) && next.is_some_and(|x| x.is("in")))
            {
                continue;
            }
            let unary_prev = prev.is_some_and(|p| {
                p.is("!")
                    || p.is("~")
                    || p.is("typeof")
                    || p.is("void")
                    || p.is("delete")
                    || p.is("await")
                    || ((p.is("-") || p.is("+")) && (i < 2 || !ends_expression(&toks[i - 2])))
            });
            let prev_binary = prev.filter(|p| {
                !unary_prev
                    && i >= 2
                    && ends_expression(&toks[i - 2])
                    && precedence(&p.text).is_some()
                    && (p.kind == Kind::Punct || p.is("in") || p.is("instanceof"))
            });
            let assign = |p: &Tok| {
                p.kind == Kind::Punct
                    && (p.text == "="
                        || p.text.ends_with('=')
                            && !matches!(p.text.as_str(), "==" | "!=" | "===" | "!==" | "<=" | ">="))
            };
            let prev_delim = prev.is_some_and(|p| {
                (p.kind == Kind::Punct && matches!(p.text.as_str(), "(" | "[" | "," | ":" | "?" | "=>" | "..."))
                    || (p.kind == Kind::Ident && (p.is("return") || p.is("throw")))
                    || (p.kind == Kind::Template && p.text.ends_with("${"))
                    || assign(p)
            }) && !property
                || after_header;
            let next_binary = next.filter(|x| {
                precedence(&x.text).is_some() && (x.kind == Kind::Punct || x.is("in") || x.is("instanceof"))
            });
            let next_delim = next.is_none_or(|x| {
                x.kind == Kind::Punct && matches!(x.text.as_str(), ")" | "]" | "," | ";" | "}" | ":" | "?")
                    || x.kind == Kind::Template && !x.text.starts_with('`')
            });
            let statement_level = prev.is_some_and(|p| p.is("=>")) || statement_start;
            let ok_left = (prev_delim && !(statement_level && starts_object))
                || prev_binary.is_some_and(|p| low > precedence(&p.text).expect("binary"))
                || (unary_prev && low == 16 && !next.is_some_and(|x| x.is("**")));
            let ok_right = (next_delim && !(next.is_some_and(|x| x.is("?")) && low < 4))
                || next_binary.is_some_and(|x| {
                    let p = precedence(&x.text).expect("binary");
                    low > p || (low == p && x.text != "**")
                })
                || (low == 16
                    && next.is_some_and(|x| x.is(".") || x.is("[") || x.is("("))
                    && chain_of(inner)
                    && unary_prev);
            // A primary expression: a name or member chain, `new X(…)`, a
            // function in an expression position. Parentheses around it change
            // nothing before `.`, `[`, `(`, except after `new`.
            let one_function = (inner[0].is("function")
                || inner[0].is("class")
                || inner[0].is("async") && inner.get(1).is_some_and(|f| f.is("function")))
                && body_brace(inner).is_some_and(|b| closing(inner, b) + 1 == inner.len());
            let new_call = inner[0].is("new") && inner.last().is_some_and(|l| l.is(")")) && {
                let mut k = 1;
                while k < inner.len() && (inner[k].kind == Kind::Ident || inner[k].is(".")) {
                    k += 1;
                }
                inner.get(k).is_some_and(|x| x.is("(")) && closing(inner, k) + 1 == inner.len()
            };
            let chain = inner.iter().enumerate().all(|(k, x)| {
                x.kind == Kind::Ident && !NOT_ENDING.contains(&x.text.as_str()) && !x.is("async")
                    || x.is(".")
                    || x.kind == Kind::Str && k > 0
                    || x.open()
                    || x.close()
                    || k > 0
                        && inner[..k].iter().filter(|y| y.open()).count()
                            > inner[..k].iter().filter(|y| y.close()).count()
            }) && inner[0].kind == Kind::Ident
                && low == 16;
            let after_new = prev.is_some_and(|p| p.is("new"));
            let primary = (chain || new_call) && !after_new || one_function && !statement_start;
            let expression_position = prev_delim || prev_binary.is_some() || unary_prev || after_new && one_function;
            let before_member = next.is_some_and(|x| x.is(".") || x.is("[") || x.is("(") || x.kind == Kind::Template);
            if primary && expression_position && (before_member || ok_right) {
                drop[i] = true;
                drop[c] = true;
                continue;
            }
            if (ok_left && ok_right) || jsx {
                drop[i] = true;
                drop[c] = true;
            }
        }
    }
    // TypeScript: in braces that are not blocks (type literals, objects), `,`
    // between members is `;`; an object literal with `;` would not parse.
    if d.ts {
        // Open brackets: whether a literal brace, and the depth of `<…>` in it.
        let mut stack: Vec<(bool, usize)> = Vec::new();
        for i in 0..n {
            let t = &toks[i];
            if t.open() {
                let literal = t.is("{")
                    && !is_block(&toks, i)
                    && !(i > 0 && toks[i - 1].is(":") && statement_colon(&toks, i - 1));
                stack.push((literal, 0));
            } else if t.close() {
                stack.pop();
            } else if let Some(top) = stack.last_mut() {
                if t.is("<") {
                    top.1 += 1;
                } else if t.is(">") || t.is(">>") || t.is(">>>") {
                    top.1 = top.1.saturating_sub(t.text.len());
                } else if t.is(",") && top.0 && top.1 == 0 && replace[i].is_none() {
                    replace[i] = Some(tok(Kind::Punct, ";"));
                }
            }
        }
    }
    let mut out = Vec::with_capacity(n);
    for (i, t) in toks.into_iter().enumerate() {
        if !drop[i] {
            out.push(replace[i].take().unwrap_or(t));
        }
        out.append(&mut insert_after[i]);
    }
    out
}

/// Tokens in canonical form for comparing: also no empty statements (`;`
/// after a block, before `}`, repeated).
fn comparable(text: &str, d: Dialect) -> Vec<(Kind, String)> {
    let toks = structural(text, d);
    let n = toks.len();
    let mut opener = vec![usize::MAX; n];
    let mut stack = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if t.open() {
            stack.push(i);
        } else if t.close()
            && let Some(o) = stack.pop()
        {
            opener[i] = o;
        }
    }
    let mut out: Vec<(Kind, String)> = Vec::with_capacity(n);
    for (i, t) in toks.iter().enumerate() {
        if t.is(";") {
            // Not after an arrow function's body: `() => {}(x)` would not parse.
            let after_block = i > 0
                && toks[i - 1].is("}")
                && opener[i - 1] != usize::MAX
                && is_block(&toks, opener[i - 1])
                && !(opener[i - 1] > 0 && toks[opener[i - 1] - 1].is("=>"))
                // Nor after a function expression, which `(…)` would call.
                && !fn_expression_end(&toks, i);
            // `{ a; }` is `{ a }`, in blocks and type literals alike.
            let before_close = toks.get(i + 1).is_some_and(|x| x.is("}"));
            let repeated = out.last().is_some_and(|(k, s)| *k == Kind::Punct && s == ";");
            let at_start = out.is_empty();
            if after_block || before_close || repeated || at_start || toks.get(i + 1).is_none() {
                continue;
            }
        }
        // `Foo<Bar<T>>` is `Foo<Bar<T> >`; split only here, after the
        // parentheses were decided on the real operators.
        if t.kind == Kind::Punct && matches!(t.text.as_str(), ">>" | ">>>") {
            out.extend(t.text.chars().map(|c| (Kind::Punct, c.to_string())));
            continue;
        }
        out.push((t.kind, t.text.clone()));
    }
    out
}

/// Whether two sources are the same code, apart from comments and formatting.
pub fn same_code(left: &str, right: &str, d: Dialect) -> bool {
    comparable(left, d) == comparable(right, d)
}

// ---- outline ----

fn display(t: &Tok) -> String {
    match t.kind {
        Kind::Str => {
            let mut s = String::from("\"");
            for c in t.text.chars() {
                match c {
                    '"' => s.push_str("\\\""),
                    '\\' => s.push_str("\\\\"),
                    '\n' => s.push_str("\\n"),
                    c => s.push(c),
                }
            }
            s.push('"');
            s
        }
        Kind::JsxText => t.text.clone(),
        _ => t.text.clone(),
    }
}

fn render(toks: &[Tok]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Tok> = None;
    let mut angle = 0usize;
    for t in toks {
        if let Some(p) = prev {
            let tight = p.is("(")
                || p.is("[")
                || p.is(".")
                || p.is("?.")
                || p.is("...")
                || p.is("!") && p.kind == Kind::Punct && !ends_expression(p)
                || p.is("@")
                || p.is("#")
                || t.is(")")
                || t.is("]")
                || t.is(",")
                || t.is(";")
                || t.is(".")
                || t.is("?.")
                || (t.is(":") && (p.kind != Kind::Punct || p.is(")") || p.is("]") || p.is("?")))
                || (t.is("(") || t.is("["))
                    && (p.kind == Kind::Ident && !KEYWORDS.contains(&p.text.as_str()) || p.is(")") || p.is("]"))
                || (t.is("<") && p.kind == Kind::Ident && !KEYWORDS.contains(&p.text.as_str()))
                || (t.is(">") && angle > 0)
                || (p.is("<") && angle > 0)
                || (t.is("(") && p.is(">"))
                || (t.is("!") && ends_expression(p));
            if !tight {
                out.push(' ');
            }
        }
        if t.is("<") && prev.is_some_and(|p| p.kind == Kind::Ident && !KEYWORDS.contains(&p.text.as_str())) {
            angle += 1;
        } else if t.is(">") && angle > 0 {
            angle -= 1;
        }
        out.push_str(&display(t));
        prev = Some(t);
    }
    out
}

/// Semicolons that change nothing: before `}`, repeated, at the end.
fn optional_semicolon(toks: &[Tok], i: usize) -> bool {
    toks[i].is(";") && toks.get(i + 1).is_none_or(|n| n.is("}") || n.is(";"))
}

/// The number of tokens that count for [`checksum`].
fn counted(toks: &[Tok]) -> usize {
    (0..toks.len()).filter(|&i| !optional_semicolon(toks, i)).count()
}

/// A checksum of tokens; semicolons before `}`, repeated or at the end do not count.
fn checksum(toks: &[Tok]) -> String {
    let mut h = crc32fast::Hasher::new();
    for (i, t) in toks.iter().enumerate() {
        if optional_semicolon(toks, i) {
            continue;
        }
        h.update(&[t.kind as u8]);
        h.update(t.text.as_bytes());
        h.update(&[0]);
    }
    format!("{:08x}", h.finalize())
}

/// Top-level statements: up to `;` outside brackets, or the `}` of a
/// declaration's body (`function f() {}`, `class A {}`).
fn statements(toks: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if t.is(";") {
            if i > start {
                out.push(&toks[start..i]);
            }
            i += 1;
            start = i;
            continue;
        }
        if t.open() {
            let c = closing(toks, i);
            let block = t.is("{") && is_block(toks, i);
            i = (c + 1).min(toks.len());
            let decl = toks[start..].iter().take(6).any(|w| {
                ["function", "class", "interface", "enum", "namespace", "module", "if", "for", "while", "try", "switch"]
                    .iter()
                    .any(|k| w.is(k))
            });
            let next_continues = toks.get(i).is_some_and(|x| {
                x.is("else") || x.is("catch") || x.is("finally") || x.is("while") || x.is(".") || x.is("(")
            });
            if block && decl && !next_continues {
                out.push(&toks[start..i]);
                start = i;
            }
            continue;
        }
        i += 1;
    }
    if start < toks.len() {
        out.push(&toks[start..]);
    }
    out
}

const LONG: usize = 100;

/// The index of the body brace of a function-like statement: the first `{`
/// at the top level that is a block.
fn body_brace(stmt: &[Tok]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in stmt.iter().enumerate() {
        if t.is("{") && depth == 0 && is_block(stmt, i) {
            return Some(i);
        }
        if t.open() {
            depth += 1;
        } else if t.close() {
            depth = depth.saturating_sub(1);
        }
    }
    None
}

fn members(toks: &[Tok], indent: &str, out: &mut Vec<String>) {
    let mut entries: Vec<(u8, String, Vec<String>)> = Vec::new();
    for stmt in statements(toks) {
        let mut k = 0;
        // Decorators.
        let mut lines = Vec::new();
        while k < stmt.len() && stmt[k].is("@") {
            let mut e = k + 2;
            while e < stmt.len() && stmt[e].is(".") {
                e += 2;
            }
            if stmt.get(e).is_some_and(|t| t.is("(")) {
                e = closing(stmt, e) + 1;
            }
            lines.push(format!("{indent}{}", render(&stmt[k..e.min(stmt.len())])));
            k = e;
        }
        let rest = &stmt[k.min(stmt.len())..];
        if rest.is_empty() {
            continue;
        }
        let name_at = rest
            .iter()
            .position(|t| {
                t.kind == Kind::Ident && !KEYWORDS.contains(&t.text.as_str())
                    || t.kind == Kind::Str
                    || t.is("constructor")
                    || t.is("[")
            })
            .unwrap_or(0);
        let name = rest.get(name_at).map_or(String::new(), |t| t.text.clone());
        let method =
            rest.iter().position(|t| t.is("(")).is_some_and(|p| p <= name_at + 2) && body_brace(rest).is_some();
        if method {
            let b = body_brace(rest).expect("checked");
            lines.push(format!(
                "{indent}{}  // code {}, {} tokens",
                render(&rest[..b]),
                checksum(&rest[b..]),
                counted(&rest[b..])
            ));
        } else {
            let text = render(rest);
            if text.chars().count() > LONG {
                let eq = rest.iter().position(|t| t.is("=")).unwrap_or(rest.len());
                lines.push(format!("{indent}{} = …  // value {}", render(&rest[..eq]), checksum(&rest[eq..])));
            } else {
                lines.push(format!("{indent}{text}"));
            }
        }
        let rank = if rest.iter().any(|t| t.is("constructor")) {
            0
        } else if method {
            2
        } else {
            1
        };
        entries.push((rank, name, lines));
    }
    entries.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    for (_, _, mut l) in entries {
        out.append(&mut l);
    }
}

fn outline_statements(toks: &[Tok], indent: &str, out: &mut Vec<String>) {
    let mut imports = Vec::new();
    let mut entries: Vec<(u8, String, Vec<String>)> = Vec::new();
    let mut other: Vec<Tok> = Vec::new();
    let mut other_count = 0usize;
    for stmt in statements(toks) {
        let mut k = 0;
        while k < stmt.len()
            && (stmt[k].is("export")
                || stmt[k].is("declare")
                || stmt[k].is("default")
                || stmt[k].is("async")
                || stmt[k].is("abstract"))
        {
            k += 1;
        }
        let Some(kw) = stmt.get(k) else { continue };
        let name = stmt.get(k + 1).map_or(String::new(), |t| t.text.clone());
        if stmt[0].is("import")
            || (stmt[0].is("export")
                && stmt.iter().any(|t| t.is("from"))
                && !stmt.iter().any(|t| t.is("function") || t.is("class")))
        {
            imports.push(format!("{indent}{}", render(stmt)));
            continue;
        }
        let head = |end: usize| render(&stmt[..end]);
        let block = body_brace(stmt);
        match kw.text.as_str() {
            "function" if block.is_some() => {
                let b = block.expect("checked");
                let name = stmt
                    .get(k + 1)
                    .filter(|t| !t.is("*"))
                    .or(stmt.get(k + 2))
                    .map_or(String::new(), |t| t.text.clone());
                entries.push((
                    4,
                    name,
                    vec![format!(
                        "{indent}{}  // code {}, {} tokens",
                        head(b),
                        checksum(&stmt[b..]),
                        counted(&stmt[b..])
                    )],
                ));
            }
            "class" | "interface" | "enum" | "namespace" | "module" if block.is_some() => {
                let b = block.expect("checked");
                let c = closing(stmt, b);
                let mut lines = vec![format!("{indent}{} {{", head(b))];
                if kw.is("namespace") || kw.is("module") {
                    outline_statements(&stmt[b + 1..c.min(stmt.len())], &format!("{indent}    "), &mut lines);
                } else if kw.is("enum") {
                    for m in stmt[b + 1..c.min(stmt.len())].split(|t| t.is(",")).filter(|m| !m.is_empty()) {
                        lines.push(format!("{indent}    {},", render(m)));
                    }
                } else {
                    members(&stmt[b + 1..c.min(stmt.len())], &format!("{indent}    "), &mut lines);
                }
                lines.push(format!("{indent}}}"));
                let rank = if kw.is("class") { 3 } else { 2 };
                entries.push((rank, name, lines));
            }
            "type" if name != "=" => {
                let text = render(stmt);
                let line = if text.chars().count() > LONG {
                    let eq = stmt.iter().position(|t| t.is("=")).unwrap_or(stmt.len());
                    format!("{indent}{} = …  // value {}", head(eq), checksum(&stmt[eq..]))
                } else {
                    format!("{indent}{text}")
                };
                entries.push((2, name, vec![line]));
            }
            "const" | "let" | "var" => {
                let eq = stmt.iter().position(|t| t.is("=")).unwrap_or(stmt.len());
                let value = &stmt[(eq + 1).min(stmt.len())..];
                // A function in a variable: `const f = (a) => {…}`.
                let arrow = value.iter().position(|t| t.is("=>"));
                let func = value.first().is_some_and(|t| {
                    t.is("function") || t.is("async") && value.get(1).is_some_and(|f| f.is("function"))
                });
                let line = if let Some(a) = arrow.filter(|_| {
                    value.first().is_some_and(|t| t.is("(") || t.kind == Kind::Ident || t.is("async") || t.is("<"))
                }) {
                    let sig_end = eq + 1 + a + 1;
                    format!(
                        "{indent}{}  // code {}, {} tokens",
                        head(sig_end),
                        checksum(&stmt[sig_end..]),
                        counted(&stmt[sig_end..])
                    )
                } else if func {
                    let b = body_brace(value).map_or(stmt.len(), |b| eq + 1 + b);
                    format!("{indent}{}  // code {}, {} tokens", head(b), checksum(&stmt[b..]), counted(&stmt[b..]))
                } else {
                    let text = render(stmt);
                    if text.chars().count() > LONG {
                        format!("{indent}{} = …  // value {}", head(eq), checksum(&stmt[eq..]))
                    } else {
                        format!("{indent}{text}")
                    }
                };
                let name = stmt.get(k + 1).map_or(String::new(), |t| t.text.clone());
                entries.push((1, name, vec![line]));
            }
            _ if stmt[0].is("export") => {
                let text = render(stmt);
                let line = if text.chars().count() > LONG {
                    format!("{indent}export …  // value {}", checksum(stmt))
                } else {
                    format!("{indent}{text}")
                };
                entries.push((5, name, vec![line]));
            }
            _ => {
                other.extend(stmt.iter().cloned());
                other.push(tok(Kind::Punct, ";"));
                other_count += 1;
            }
        }
    }
    imports.sort();
    imports.dedup();
    let top = indent.is_empty();
    out.append(&mut imports);
    if top && out.last().is_some_and(|l| !l.is_empty()) {
        out.push(String::new());
    }
    entries.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    for (_, _, mut lines) in entries {
        let block = lines.len() > 1;
        if top && block && out.last().is_some_and(|l| !l.is_empty()) {
            out.push(String::new());
        }
        out.append(&mut lines);
        if top && block {
            out.push(String::new());
        }
    }
    if other_count > 0 {
        out.push(format!(
            "{indent}// other code {}, {other_count} statement{}",
            checksum(&other),
            if other_count == 1 { "" } else { "s" }
        ));
    }
}

/// The outline of a JavaScript or TypeScript file.
pub fn outline(text: &str, d: Dialect) -> String {
    let toks = structural(text, d);
    let mut lines = Vec::new();
    outline_statements(&toks, "", &mut lines);
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let mut out = String::new();
    for l in lines {
        let _ = writeln!(out, "{l}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const JS: Dialect = Dialect { ts: false, jsx: true };
    const TS: Dialect = Dialect { ts: true, jsx: false };
    const TSX: Dialect = Dialect { ts: true, jsx: true };

    fn texts(src: &str, d: Dialect) -> Vec<String> {
        tokens(src, d).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn lexes_javascript() {
        assert_eq!(
            texts("a = b / c / d; r = /=[/]x/gi.test(s)", JS),
            ["a", "=", "b", "/", "c", "/", "d", ";", "r", "=", "/=[/]x/gi", ".", "test", "(", "s", ")"]
        );
        assert_eq!(texts("`a${b + `c${d}`}e` // x", JS), ["`a${", "b", "+", "`c${", "d", "}`", "}e`"]);
        assert_eq!(
            texts("x = 'it\\'s' + \"\\u0041\\x42\" + 0x10 + 1_000 + .5e1", JS),
            ["x", "=", "it's", "+", "AB", "+", "16", "+", "1000", "+", "5"]
        );
        assert_eq!(
            texts("const el = <div className=\"a\" id={x}>\n  Hello {name}!\n  <br />\n</div>;", JS),
            [
                "const",
                "el",
                "=",
                "<",
                "div",
                "className",
                "=",
                "a",
                "id",
                "=",
                "{",
                "x",
                "}",
                ">",
                "Hello ",
                "{",
                "name",
                "}",
                "!",
                "<",
                "br",
                "/>",
                "</",
                "div",
                ">",
                ";"
            ]
        );
        assert_eq!(
            texts("const f = <T,>(x: T) => x;", TSX),
            ["const", "f", "=", "<", "T", ",", ">", "(", "x", ":", "T", ")", "=>", "x", ";"]
        );
        assert_eq!(texts("let x = <T>y;", TS), ["let", "x", "=", "<", "T", ">", "y", ";"]);
    }

    #[test]
    fn inserts_semicolons_like_javascript() {
        assert!(same_code("let a = 1\nlet b = 2\n", "let a = 1;\nlet b = 2;\n", JS));
        // `a\n(b)` is a call: no semicolon there.
        assert!(!same_code("let x = a\n(b)\n", "let x = a;\n(b);\n", JS));
        assert!(same_code("return\nx\n", "return;\nx;\n", JS));
        assert!(!same_code("return x\n", "return;\nx\n", JS));
        assert!(same_code("if (a) {\n  b()\n}\nc()\n", "if (a) { b(); } c();", JS));
        assert!(same_code("a\n++b\n", "a;\n++b;\n", JS));
        assert!(!same_code(
            "function f() {\n  return (\n    <div />\n  )\n}",
            "function f() {\n  return\n    <div />\n}",
            JS
        ));
        assert!(!same_code("const f = function g() {}\n;(a).b = 1", "const f = function g() {}(a).b = 1", JS));
        assert!(!same_code("x = !((a && b) || c)", "x = !(a && b) || c", JS));
        assert!(same_code("x = !((a && b) || c)", "x = !(a && b || c)", JS));
        assert!(!same_code("const c = f<A<B>>(null)", "const c = f<A<B>>null", TS));
        assert!(!same_code("type M = { [k in K]?: string }", "type M = { [(k) in K]?: string }", TS));
        assert!(!same_code("x = !!o.sync", "x = !(!o).sync", JS));
        assert_eq!(clean_jsx_text(" "), " ");
        assert_eq!(clean_jsx_text("\n   a\n   b  \n"), "a b");
        assert!(!same_code("if (a) { b() }", "if (a) ({ b() })", JS));
        assert!(!same_code("interface I { a: string }", "interface I { (a: string) }", TS));
        assert!(!same_code("const e = z.custom<number>(f)", "const e = (z.custom<number)>(f)", TS));
    }

    #[test]
    fn ignores_what_prettier_changes() {
        let a = "const f = x => { return (a && b) || c }\nconst o = { 'k': 1, \"j\": [1, 2,], }\nlet p = new Foo\nconst e = (\n  <div>\n    Hi{' '}\n    <b>there</b>\n  </div>\n)\n";
        let b = "const f = (x) => {\n  return a && b || c;\n};\nconst o = { k: 1, j: [1, 2] };\nlet p = new Foo();\nconst e = <div>Hi <b>there</b></div>;\n";
        assert!(same_code(a, b, JS));
        assert!(!same_code("x = (a + b) * c", "x = a + b * c", JS));
        assert!(!same_code("x = c - (a - b)", "x = c - a - b", JS));
        assert!(same_code("x = (a - b) - c", "x = a - b - c", JS));
        assert!(!same_code("x = (a, b)", "x = a, b", JS));
        assert!(!same_code("x = [a,,]", "x = [a,]", JS));
        assert!(!same_code("f = () => ({})", "f = () => {}", JS));
        assert!(!same_code("x = 'a // b'", "x = 'a '", JS));
        assert!(same_code(
            "type A = | 'a' | 'b'\ninterface I { a: string, b: number }",
            "type A = \"a\" | \"b\";\ninterface I {\n  a: string;\n  b: number;\n}\n",
            TS
        ));
        assert!(same_code("x = (y ?? z)\n", "x = y ?? z;\n", TS));
    }

    #[test]
    fn outlines_a_module() {
        let src = "import { a } from './a'\nimport b from 'b'\n\nexport const LIMIT = 5\n\nexport interface Item { id: string; price: number }\n\nexport class Cart extends Base {\n  items: Item[] = []\n  constructor(private name: string) { super() }\n  add(item: Item): this { this.items.push(item); return this }\n  get total(): number { return sum(this.items) }\n}\n\nexport function sum(items: Item[]): number {\n  return items.reduce((s, i) => s + i.price, 0)\n}\n\nconst fmt = (n: number) => n.toFixed(2)\n\nmain()\n";
        let o = outline(src, TS);
        assert!(o.starts_with("import b from \"b\"\nimport { a } from \"./a\"\n\nexport const LIMIT = 5\nconst fmt = (n: number) =>  // code "), "{o}");
        assert!(o.contains("\nexport interface Item {\n    id: string\n    price: number\n}\n"), "{o}");
        assert!(
            o.contains("\nexport class Cart extends Base {\n    constructor(private name: string)  // code "),
            "{o}"
        );
        assert!(o.contains("\n    items: Item[] = []\n    add(item: Item): this  // code "), "{o}");
        assert!(o.contains("\n    get total(): number  // code "), "{o}");
        assert!(o.contains("\nexport function sum(items: Item[]): number  // code "), "{o}");
        assert!(o.ends_with(", 1 statement\n"), "{o}");
    }
}
