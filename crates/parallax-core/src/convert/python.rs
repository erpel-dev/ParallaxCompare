//! Python source: an outline of the code, and a comparison that ignores
//! comments and formatting.
//!
//! Both work on tokens rather than text. Comments, blank lines, line breaks
//! inside brackets and backslash continuations are dropped; indentation counts
//! as levels, not as columns; strings are compared by what they say (`'a'` and
//! `"a"` are the same, as are re-indented docstrings), number literals ignore
//! case, and what a formatter like Black adds or removes is ignored: a trailing
//! comma before a closing bracket and the parentheses around a right-hand side
//! or condition that it splits over several lines.
//!
//! The outline lists imports, module and class level assignments, classes and
//! functions with their signatures, sorted by name within each class, and a
//! checksum of each function's body. Code that is none of these (a main block,
//! module level calls) is summed up in one checksum, so a change anywhere
//! changes some line of the outline.

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Name,
    Number,
    Str,
    Op,
    Newline,
    Indent,
    Dedent,
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
    fn is(&self, op: &str) -> bool {
        matches!(self.kind, Kind::Op | Kind::Name) && self.text == op
    }
}

const OPERATORS: &[&str] = &[
    "**=", "//=", ">>=", "<<=", "...", "->", ":=", "**", "//", "<<", ">>", "<=", ">=", "==", "!=", "+=", "-=", "*=",
    "/=", "%=", "&=", "|=", "^=", "@=",
];

/// Python keywords that cannot be followed by a call's parenthesis.
const KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except",
    "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
    "return", "try", "while", "with", "yield",
];

struct Lexer<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
    out: Vec<Tok>,
}

impl Lexer<'_> {
    fn peek(&self, at: usize) -> u8 {
        self.b.get(self.i + at).copied().unwrap_or(0)
    }

    /// A string literal at `self.i` whose prefix has `prefix_len` letters.
    /// Returns the normalized text.
    fn string(&mut self, prefix_len: usize) -> String {
        let prefix: String = self.s[self.i..self.i + prefix_len].to_ascii_lowercase();
        let raw = prefix.contains('r');
        let formatted = prefix.contains('f') || prefix.contains('t');
        self.i += prefix_len;
        let q = self.b[self.i];
        let triple = self.peek(1) == q && self.peek(2) == q;
        let qlen = if triple { 3 } else { 1 };
        self.i += qlen;
        let start = self.i;
        let mut end = self.b.len();
        let mut braces = 0usize;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c == b'\\' {
                self.i += 2;
                continue;
            }
            if formatted {
                if c == b'{' {
                    if braces == 0 && self.peek(1) == b'{' {
                        self.i += 2;
                        continue;
                    }
                    braces += 1;
                } else if c == b'}' && braces > 0 {
                    braces -= 1;
                } else if braces > 0 && (c == b'"' || c == b'\'') {
                    // A string inside a replacement field (Python 3.12 allows the same quote).
                    self.string(0);
                    continue;
                }
            }
            if braces == 0 && c == q && (!triple || (self.peek(1) == q && self.peek(2) == q)) {
                end = self.i;
                self.i += qlen;
                break;
            }
            if !triple && c == b'\n' {
                end = self.i;
                break;
            }
            self.i += 1;
        }
        let body = &self.s[start..end.min(self.b.len())];
        // The same text in either kind of quotes: escaped quotes count as plain ones.
        let mut text = String::with_capacity(body.len() + 4);
        let mut flags: Vec<char> = prefix.chars().filter(|&c| c != 'u').collect();
        flags.sort_unstable();
        text.extend(flags);
        text.push('"');
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' && !raw && matches!(chars.peek(), Some('\'' | '"')) {
                continue;
            }
            text.push(c);
            if c == '\\' {
                let Some(e) = chars.next() else { break };
                text.push(e);
                // `\xFF` and `\xff` are the same character.
                let digits = match e {
                    'x' if !raw => 2,
                    'u' | 'U' if !raw && !prefix.contains('b') => {
                        if e == 'u' {
                            4
                        } else {
                            8
                        }
                    }
                    _ => 0,
                };
                for _ in 0..digits {
                    match chars.peek() {
                        Some(h) if h.is_ascii_hexdigit() => {
                            text.push(h.to_ascii_lowercase());
                            chars.next();
                        }
                        _ => break,
                    }
                }
            }
        }
        text.push('"');
        text
    }

    fn run(mut self) -> Vec<Tok> {
        let mut depth = 0usize;
        let mut indents: Vec<usize> = vec![0];
        let mut line_start = true;
        while self.i < self.b.len() {
            if line_start {
                line_start = false;
                // Measure the indentation; blank and comment lines do not count.
                let mut col = 0;
                let mut j = self.i;
                while j < self.b.len() {
                    match self.b[j] {
                        b' ' => col += 1,
                        b'\t' => col = (col / 8 + 1) * 8,
                        b'\x0c' => col = 0,
                        _ => break,
                    }
                    j += 1;
                }
                let next = self.b.get(j).copied().unwrap_or(b'\n');
                self.i = j;
                if matches!(next, b'\n' | b'\r' | b'#') || (next == b'\\' && self.b.get(j + 1) == Some(&b'\n')) {
                    // Skip the rest of the line below.
                } else if depth == 0 {
                    if col > *indents.last().expect("never empty") {
                        indents.push(col);
                        self.out.push(Tok::new(Kind::Indent, ""));
                    } else {
                        while col < *indents.last().expect("never empty") && indents.len() > 1 {
                            indents.pop();
                            self.out.push(Tok::new(Kind::Dedent, ""));
                        }
                    }
                }
            }
            let c = self.b[self.i];
            match c {
                b'\n' => {
                    self.i += 1;
                    if depth == 0 {
                        if self
                            .out
                            .last()
                            .is_some_and(|t| !matches!(t.kind, Kind::Newline | Kind::Indent | Kind::Dedent))
                        {
                            self.out.push(Tok::new(Kind::Newline, ""));
                        }
                        line_start = true;
                    }
                }
                b' ' | b'\t' | b'\r' | b'\x0c' => self.i += 1,
                b'#' => {
                    while self.i < self.b.len() && self.b[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                b'\\' if matches!(self.peek(1), b'\n' | b'\r') => {
                    self.i += if self.peek(1) == b'\r' && self.peek(2) == b'\n' { 3 } else { 2 };
                }
                b'"' | b'\'' => {
                    let t = self.string(0);
                    self.out.push(Tok::new(Kind::Str, t));
                }
                b'0'..=b'9' => self.number(),
                b'.' if self.peek(1).is_ascii_digit() => self.number(),
                _ if c == b'_' || c.is_ascii_alphabetic() || c >= 0x80 => {
                    let start = self.i;
                    while self.i < self.b.len() {
                        let ch = self.s[self.i..].chars().next().expect("in bounds");
                        if ch == '_' || ch.is_alphanumeric() {
                            self.i += ch.len_utf8();
                        } else {
                            break;
                        }
                    }
                    if self.i == start {
                        // A character that is not part of Python's syntax.
                        let ch = self.s[self.i..].chars().next().expect("in bounds");
                        self.i += ch.len_utf8();
                        self.out.push(Tok::new(Kind::Op, ch.to_string()));
                        continue;
                    }
                    let word = &self.s[start..self.i];
                    let is_prefix = word.len() <= 2
                        && word.chars().all(|c| "rRbBuUfFtT".contains(c))
                        && matches!(self.peek(0), b'"' | b'\'');
                    if is_prefix {
                        self.i = start;
                        let t = self.string(word.len());
                        self.out.push(Tok::new(Kind::Str, t));
                    } else {
                        self.out.push(Tok::new(Kind::Name, word));
                    }
                }
                _ => {
                    let rest = &self.s[self.i..];
                    let op = OPERATORS.iter().find(|o| rest.starts_with(**o)).map_or(&rest[..1], |o| *o);
                    match op {
                        "(" | "[" | "{" => depth += 1,
                        ")" | "]" | "}" => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                    self.i += op.len();
                    self.out.push(Tok::new(Kind::Op, op));
                }
            }
        }
        if self.out.last().is_some_and(|t| !matches!(t.kind, Kind::Newline | Kind::Indent | Kind::Dedent)) {
            self.out.push(Tok::new(Kind::Newline, ""));
        }
        for _ in 1..indents.len() {
            self.out.push(Tok::new(Kind::Dedent, ""));
        }
        self.out
    }

    fn number(&mut self) {
        let start = self.i;
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' {
                self.i += 1;
                // An exponent's sign belongs to the number, not in hex.
                let hex = self.s[start..self.i].len() > 1 && matches!(&self.b[start..start + 2], b"0x" | b"0X");
                if matches!(c, b'e' | b'E') && !hex && matches!(self.peek(0), b'+' | b'-') {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
        // `.5` and `0.5`, `1e+3` and `1e3`, `1.` and `1.0` are the same number.
        let mut text = self.s[start..self.i].to_ascii_lowercase();
        if !text.starts_with("0x") {
            text = text.replace("e+", "e");
            if text.starts_with('.') {
                text.insert(0, '0');
            }
            if let Some(dot) = text.find('.')
                && !text[dot + 1..].starts_with(|c: char| c.is_ascii_digit())
            {
                text.insert(dot + 1, '0');
            }
        }
        self.out.push(Tok::new(Kind::Number, text));
    }
}

/// The tokens of Python source, without comments and layout.
pub fn tokens(text: &str) -> Vec<Tok> {
    let mut toks = Lexer { s: text, b: text.as_bytes(), i: 0, out: Vec::new() }.run();
    // Docstrings and other strings standing alone: re-indenting does not count.
    for i in 0..toks.len() {
        let alone = toks[i].kind == Kind::Str
            && (i == 0 || matches!(toks[i - 1].kind, Kind::Newline | Kind::Indent | Kind::Dedent))
            && toks.get(i + 1).is_none_or(|t| t.kind == Kind::Newline);
        if alone {
            let t = &toks[i].text;
            let open = t.find('"').expect("normalized strings are quoted") + 1;
            let body: Vec<&str> = t[open..t.len() - 1].lines().map(str::trim).collect();
            let body = body.join("\n");
            toks[i].text = format!("{}{}\"", &t[..open], body.trim());
        }
    }
    toks
}

/// The index of the bracket that closes the one at `open`.
fn closing(toks: &[Tok], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (j, t) in toks.iter().enumerate().skip(open) {
        if t.kind != Kind::Op {
            continue;
        }
        match t.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether the tokens between two brackets contain `what` at their own level.
/// A lambda's parameters (`lambda a, b: …`) are not at that level.
fn has_top_level(toks: &[Tok], open: usize, close: usize, what: &[&str]) -> bool {
    let mut depth = 0usize;
    let mut in_lambda_params = false;
    for t in &toks[open + 1..close] {
        if t.kind == Kind::Op {
            match t.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                ":" if depth == 0 && in_lambda_params => {
                    in_lambda_params = false;
                    continue;
                }
                _ => {}
            }
        }
        if depth == 0 && what.iter().any(|w| t.is(w)) && !in_lambda_params {
            return true;
        }
        if depth == 0 && t.is("lambda") {
            in_lambda_params = true;
        }
    }
    false
}

/// Statements that start a block.
const COMPOUND: &[&str] = &["if", "elif", "else", "for", "while", "try", "except", "finally", "with", "def", "class"];

/// One-line compound statements (`if x: y`) and statements joined by `;` as
/// separate lines, the way a formatter writes them.
fn split_statements(toks: Vec<Tok>) -> Vec<Tok> {
    let mut out = Vec::with_capacity(toks.len());
    let mut line: Vec<Tok> = Vec::new();
    let simple = |out: &mut Vec<Tok>, stmt: &[Tok]| {
        let mut depth = 0usize;
        let mut start = 0;
        for (i, t) in stmt.iter().enumerate() {
            if t.kind == Kind::Op {
                match t.text.as_str() {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth = depth.saturating_sub(1),
                    ";" if depth == 0 => {
                        if i > start {
                            out.extend(stmt[start..i].iter().cloned());
                            out.push(Tok::new(Kind::Newline, ""));
                        }
                        start = i + 1;
                    }
                    _ => {}
                }
            }
        }
        if stmt.len() > start {
            out.extend(stmt[start..].iter().cloned());
            out.push(Tok::new(Kind::Newline, ""));
        }
    };
    for t in toks {
        match t.kind {
            Kind::Indent | Kind::Dedent => out.push(t),
            Kind::Newline => {
                let first = line.iter().position(|t| !t.is("async")).unwrap_or(0);
                let compound = line.get(first).is_some_and(|t| COMPOUND.iter().any(|k| t.is(k)));
                match header_end(&line).filter(|&h| compound && h + 1 < line.len()) {
                    Some(h) => {
                        out.extend(line[..=h].iter().cloned());
                        out.push(Tok::new(Kind::Newline, ""));
                        out.push(Tok::new(Kind::Indent, ""));
                        simple(&mut out, &line[h + 1..]);
                        out.push(Tok::new(Kind::Dedent, ""));
                    }
                    None => simple(&mut out, &line),
                }
                line.clear();
            }
            _ => line.push(t),
        }
    }
    simple(&mut out, &line);
    out
}

/// Tokens with what a formatter adds or removes taken out: trailing commas
/// that do not make a tuple, and parentheses around a whole expression where
/// the surrounding tokens delimit it anyway (a right-hand side, a return
/// value, a condition, an annotation, an assignment or loop target).
pub fn canonical(text: &str) -> Vec<Tok> {
    let mut toks = split_statements(tokens(text));
    let mut drop = vec![false; toks.len()];
    let statement_start = |i: usize| i == 0 || matches!(toks[i - 1].kind, Kind::Newline | Kind::Indent | Kind::Dedent);
    // Where each statement starts, to know a `for` loop's `in` from the operator.
    let mut starts = vec![0usize; toks.len()];
    for i in 1..toks.len() {
        starts[i] = if statement_start(i) { i } else { starts[i - 1] };
    }
    for open in 0..toks.len() {
        let t = &toks[open];
        if t.kind != Kind::Op || !matches!(t.text.as_str(), "(" | "[" | "{") {
            continue;
        }
        let Some(close) = closing(&toks, open) else { continue };
        let prev = open.checked_sub(1).map(|p| &toks[p]);
        let next = toks.get(close + 1);
        // A trailing comma.
        if close > open + 1 && toks[close - 1].is(",") {
            let several = has_top_level(&toks[..close - 1], open, close - 1, &[","]);
            let callable = prev.is_some_and(|p| {
                (p.kind == Kind::Name && !KEYWORDS.contains(&p.text.as_str())) || p.is(")") || p.is("]")
            });
            let subscript = t.text == "[" && callable;
            let keeps_meaning = match t.text.as_str() {
                "(" => several || callable,
                "[" => several || !subscript,
                _ => true,
            };
            if keeps_meaning {
                drop[close - 1] = true;
            }
        }
        // The trailing comma of an import list.
        if t.text == "(" && prev.is_some_and(|p| p.is("import")) && toks[close - 1].is(",") {
            drop[close - 1] = true;
        }
        if t.text != "(" || (close == open + 1 && !(open >= 2 && toks[open - 2].is("class"))) {
            continue;
        }
        let first = &toks[starts[open]];
        let loop_head =
            first.is("for") || (first.is("async") && toks.get(starts[open] + 1).is_some_and(|t| t.is("for")));
        let header = first.is("if") || first.is("elif") || first.is("while") || loop_head;
        let at_start = statement_start(open);
        let assign_op = |t: &Tok| {
            t.is("=")
                || (t.kind == Kind::Op
                    && t.text.ends_with('=')
                    && !matches!(t.text.as_str(), "==" | "!=" | "<=" | ">="))
        };
        let ends = |n: Option<&Tok>| n.is_none_or(|n| n.kind == Kind::Newline);
        let next_is = |ops: &[&str]| next.is_some_and(|n| ops.iter().any(|o| n.is(o)));
        let prev_is = |ops: &[&str]| prev.is_some_and(|p| ops.iter().any(|o| p.is(o)));
        let comma = has_top_level(&toks, open, close, &[","]);
        let generator = has_top_level(&toks, open, close, &["for"]);
        let special = has_top_level(&toks, open, close, &["yield", ":="]);
        let walrus_only = has_top_level(&toks, open, close, &[":="]) && !has_top_level(&toks, open, close, &["yield"]);
        let lambda = has_top_level(&toks, open, close, &["lambda"]);
        // Tuples where the parentheses are optional: targets, import lists, a whole right-hand side.
        let target = (at_start && next.is_some_and(assign_op)) || (prev_is(&["for"]) && next_is(&["in"]));
        let imports = prev_is(&["import"]) && ends(next);
        let tuple = !generator
            && !special
            && ((prev.is_some_and(assign_op) && (ends(next) || next.is_some_and(assign_op)))
                || (prev_is(&["return"]) && ends(next)));
        // One expression where the tokens around delimit it anyway.
        let delimited = [",", ")", "]", "}"];
        let value = !comma
            && !generator
            && !special
            && (prev.is_some_and(|p| assign_op(p) && (ends(next) || next.is_some_and(assign_op) || next_is(&[",", ")"])))
                || (prev_is(&["return", "del", "assert"]) && ends(next))
                || (prev_is(&["assert"]) && next_is(&[","]))
                || (prev_is(&[":"]) && ends(next))
                // The element of a comprehension: `[(a if b else c) for …]`.
                || (prev_is(&["(", "[", "{", ":"]) && next_is(&["for", "async"]) && !lambda)
                || (prev_is(&["if", "elif", "while"]) && header && open == starts[open] + 1 && next_is(&[":"]))
                || (prev_is(&["in"]) && loop_head && next_is(&[":"]))
                || (prev_is(&[":", "->"]) && (next_is(&["="]) || next_is(&delimited) || (prev_is(&["->"]) && next_is(&[":"]))))
                || (prev_is(&["(", "[", "{", ","]) && next_is(&delimited))
                || (prev_is(&[","]) && ends(next))
                || (prev_is(&["except"]) && next_is(&["as", ":"]))
                || (prev_is(&["else"]) && (ends(next) || next_is(&delimited) || (next_is(&["for", "async"]) && !lambda))));
        // The only argument of a call, a generator: `f((x for x in y))`.
        let call_open = prev_is(&["("]) && open >= 2 && {
            let p = &toks[open - 2];
            (p.kind == Kind::Name && !KEYWORDS.contains(&p.text.as_str())) || p.is(")") || p.is("]")
        };
        let lone_generator = generator && !comma && !special && call_open && next_is(&[")"]);
        // A condition with an assignment expression: `if (m := f()):`.
        let walrus = walrus_only
            && !comma
            && prev_is(&["if", "elif", "while"])
            && header
            && open == starts[open] + 1
            && next_is(&[":"]);
        // A power after a unary minus: `-(2 ** 31)` is `-2 ** 31`.
        let unary = open >= 1
            && toks[open - 1].kind == Kind::Op
            && matches!(toks[open - 1].text.as_str(), "-" | "+" | "~")
            && {
                let before = open.checked_sub(2).map(|p| &toks[p]);
                before.is_none_or(|b| {
                    (b.kind == Kind::Op && !matches!(b.text.as_str(), ")" | "]" | "}"))
                        || (b.kind == Kind::Name && KEYWORDS.contains(&b.text.as_str()))
                        || matches!(b.kind, Kind::Newline | Kind::Indent | Kind::Dedent)
                })
            };
        let power = unary && has_top_level(&toks, open, close, &["**"]) && {
            let mut depth = 0usize;
            toks[open + 1..close].iter().all(|t| {
                if t.kind == Kind::Op {
                    match t.text.as_str() {
                        "(" | "[" | "{" => depth += 1,
                        ")" | "]" | "}" => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                }
                depth > 0
                    || t.kind != Kind::Op
                    || matches!(t.text.as_str(), "**" | "." | "(" | "[" | "{" | ")" | "]" | "}")
            })
        };
        // `class A():` is `class A:`.
        let empty_bases = close == open + 1 && open >= 2 && toks[open - 2].is("class") && next_is(&[":"]);
        if empty_bases {
            drop[open] = true;
            drop[close] = true;
        }
        let value = value || target || imports || tuple || lone_generator || walrus || power;
        if value {
            drop[open] = true;
            drop[close] = true;
        }
    }
    // The trailing comma of a bare tuple of two or more: `a, b, = x`, `x = a, b,`.
    let mut depth = 0usize;
    let mut commas = 0usize;
    for i in 0..toks.len() {
        let t = &toks[i];
        if statement_start(i) || t.is("=") {
            commas = 0;
        }
        if t.kind == Kind::Op {
            match t.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                "," if depth == 0 => {
                    commas += 1;
                    let ends = toks.get(i + 1).is_none_or(|n| n.kind == Kind::Newline || n.is("="));
                    if ends && commas >= 2 {
                        drop[i] = true;
                    }
                }
                _ => {}
            }
        }
    }
    let mut i = 0;
    toks.retain(|_| {
        i += 1;
        !drop[i - 1]
    });
    toks
}

/// Whether two Python sources are the same code, apart from comments and formatting.
pub fn same_code(left: &str, right: &str) -> bool {
    canonical(left) == canonical(right)
}

// ---- outline ----

/// Tokens as source text with conventional spacing (PEP 8).
fn render(toks: &[Tok]) -> String {
    let mut out = String::new();
    // Open brackets, and whether the current item inside a parenthesis has an annotation.
    let mut stack: Vec<(char, bool)> = Vec::new();
    let mut prev: Option<&Tok> = None;
    let mut unary_prev = false;
    for t in toks {
        let op = |s: &str| t.kind == Kind::Op && t.text == s;
        let in_parens = stack.last().is_some_and(|(c, _)| *c == '(');
        let annotated = stack.last().is_some_and(|(_, a)| *a);
        // `*args`, `-x`: an operator where a value is expected.
        let starts_value = prev.is_none_or(|p| {
            (p.kind == Kind::Op && !matches!(p.text.as_str(), ")" | "]" | "}"))
                || (p.kind == Kind::Name && KEYWORDS.contains(&p.text.as_str()))
        });
        let unary = t.kind == Kind::Op && matches!(t.text.as_str(), "*" | "**" | "-" | "+" | "~") && starts_value;
        if let Some(p) = prev {
            let tight = (p.kind == Kind::Op && matches!(p.text.as_str(), "(" | "[" | "{" | "." | "@"))
                || unary_prev
                || (t.kind == Kind::Op && matches!(t.text.as_str(), ")" | "]" | "}" | "," | ":" | "." | ";"))
                || ((op("(") || op("["))
                    && ((p.kind == Kind::Name && !KEYWORDS.contains(&p.text.as_str()))
                        || p.is(")")
                        || p.is("]")
                        || p.kind == Kind::Str))
                // Keyword arguments and unannotated defaults: `f(a=1)`.
                || ((op("=") || p.is("=")) && in_parens && !annotated);
            if !tight {
                out.push(' ');
            }
        }
        out.push_str(&display(t));
        if t.kind == Kind::Op {
            match t.text.as_str() {
                "(" | "[" | "{" => stack.push((t.text.chars().next().expect("one char"), false)),
                ")" | "]" | "}" => {
                    stack.pop();
                }
                "," => {
                    if let Some(top) = stack.last_mut() {
                        top.1 = false;
                    }
                }
                ":" => {
                    if let Some(top) = stack.last_mut() {
                        top.1 = top.0 == '(';
                    }
                }
                _ => {}
            }
        }
        unary_prev = unary;
        prev = Some(t);
    }
    out
}

fn display(t: &Tok) -> String {
    if t.kind != Kind::Str {
        return t.text.clone();
    }
    // Normalized strings are shown in double quotes, or single ones when they
    // contain double quotes; multi-line ones by their first line.
    let open = t.text.find('"').expect("normalized strings are quoted");
    let (prefix, body) = (&t.text[..open], &t.text[open + 1..t.text.len() - 1]);
    let (body, more) = match body.split_once('\n') {
        Some((first, _)) => (first, "…"),
        None => (body, ""),
    };
    let q = if body.contains('"') && !body.contains('\'') { '\'' } else { '"' };
    format!("{prefix}{q}{body}{more}{q}")
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

/// One logical line: its tokens (without the newline) and nesting level.
struct Line<'a> {
    level: usize,
    toks: &'a [Tok],
}

fn lines(toks: &[Tok]) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut level = 0usize;
    let mut start = 0;
    for (i, t) in toks.iter().enumerate() {
        match t.kind {
            Kind::Indent => {
                level += 1;
                start = i + 1;
            }
            Kind::Dedent => {
                level = level.saturating_sub(1);
                start = i + 1;
            }
            Kind::Newline => {
                if i > start {
                    out.push(Line { level, toks: &toks[start..i] });
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < toks.len() {
        out.push(Line { level, toks: &toks[start..] });
    }
    out
}

/// The position of the colon that ends a compound statement's header.
fn header_end(toks: &[Tok]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, t) in toks.iter().enumerate() {
        if t.kind == Kind::Op {
            match t.text.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                ":" if depth == 0 => return Some(i),
                _ => {}
            }
        }
        if depth == 0 && t.is("lambda") {
            return None;
        }
    }
    None
}

#[derive(Default)]
struct Scope {
    docstring: Option<String>,
    imports: Vec<String>,
    assignments: Vec<String>,
    /// Classes and functions: sort key and lines.
    members: Vec<(String, Vec<String>)>,
    other: Vec<Tok>,
    other_count: usize,
}

const LONG: usize = 100;

fn outline_scope(lines: &[Line], level: usize, indent: &str, out: &mut Vec<String>, top: bool) {
    let mut scope = Scope::default();
    let mut decorators: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        // The lines nested in this one.
        let mut end = i + 1;
        while end < lines.len() && lines[end].level > level {
            end += 1;
        }
        let body_lines = &lines[i + 1..end];
        let toks = line.toks;
        let first = &toks[0];
        let body_toks = || -> Vec<Tok> {
            let mut v: Vec<Tok> = header_end(toks).map(|h| toks[h + 1..].to_vec()).unwrap_or_default();
            for l in body_lines {
                v.push(Tok::new(Kind::Indent, l.level.to_string()));
                v.extend(l.toks.iter().cloned());
            }
            v
        };
        if first.is("@") {
            decorators.push(format!("{indent}{}", render(toks)));
            i = end;
            continue;
        }
        let def_at = if first.is("async") && toks.get(1).is_some_and(|t| t.is("def")) { 1 } else { 0 };
        if toks[def_at].is("def") || first.is("class") {
            let name = toks.get(def_at + 1).map_or(String::new(), |t| t.text.clone());
            let head = header_end(toks).unwrap_or(toks.len());
            let mut entry = std::mem::take(&mut decorators);
            if first.is("class") {
                entry.push(format!("{indent}{}:", render(&toks[..head])));
                let inner = &lines[i + 1..end];
                let start = entry.len();
                if head + 1 < toks.len() {
                    // `class A: pass` on one line.
                    entry.push(format!("{indent}    {}", render(&toks[head + 1..])));
                }
                outline_scope(inner, level + 1, &format!("{indent}    "), &mut entry, false);
                if entry.len() == start {
                    entry.push(format!("{indent}    ..."));
                }
            } else {
                let body = body_toks();
                let count = body.iter().filter(|t| !matches!(t.kind, Kind::Indent | Kind::Dedent)).count();
                entry.push(format!("{indent}{}:  # code {}, {count} tokens", render(&toks[..head]), checksum(&body)));
            }
            // Constructors first, then by name.
            let rank = match name.as_str() {
                "__new__" => "0",
                "__init__" => "1",
                _ => "2",
            };
            scope.members.push((format!("{rank}{name}"), entry));
            i = end;
            continue;
        }
        if !decorators.is_empty() {
            // A decorator before something else; keep it with the other code.
            decorators.clear();
        }
        if i == 0 && toks.len() == 1 && first.kind == Kind::Str && body_lines.is_empty() {
            // The docstring of the module or class.
            scope.docstring = Some(format!("{indent}{}  # docstring {}", display(first), checksum(toks)));
        } else if level == 0 && (first.is("import") || first.is("from")) && body_lines.is_empty() {
            scope.imports.push(format!("{indent}{}", render(toks)));
        } else if first.kind == Kind::Name
            && !KEYWORDS.contains(&first.text.as_str())
            && toks.get(1).is_some_and(|t| t.is("=") || t.is(":") || t.is(","))
            && toks.iter().any(|t| t.is("=") || t.is(":"))
            && body_lines.is_empty()
        {
            // An assignment or annotation at module or class level.
            let text = render(toks);
            let text = if text.chars().count() > LONG {
                let eq = toks.iter().position(|t| t.is("=")).unwrap_or(toks.len());
                format!("{} = …  # value {}", render(&toks[..eq]), checksum(&toks[eq..]))
            } else {
                text
            };
            scope.assignments.push(format!("{indent}{text}"));
        } else if top && first.is("if") && toks.iter().any(|t| t.text.contains("__main__")) {
            let body = body_toks();
            let head = header_end(toks).unwrap_or(toks.len());
            scope.members.push((
                "3".into(),
                vec![format!("{indent}{}:  # code {}, {} tokens", render(&toks[..head]), checksum(&body), body.len())],
            ));
        } else {
            scope.other.extend(toks.iter().cloned());
            scope.other.push(Tok::new(Kind::Newline, ""));
            for l in body_lines {
                scope.other.push(Tok::new(Kind::Indent, l.level.to_string()));
                scope.other.extend(l.toks.iter().cloned());
                scope.other.push(Tok::new(Kind::Newline, ""));
            }
            scope.other_count += 1;
        }
        i = end;
    }

    scope.imports.sort();
    scope.imports.dedup();
    out.extend(scope.docstring.take());
    let blank = |out: &mut Vec<String>| {
        if top && out.last().is_some_and(|l| !l.is_empty()) {
            out.push(String::new());
        }
    };
    out.append(&mut scope.imports);
    blank(out);
    out.append(&mut scope.assignments);
    if scope.other_count > 0 {
        let what = if top { "module" } else { "class" };
        let n = scope.other_count;
        out.push(format!(
            "{indent}# other {what} code {}, {n} statement{}",
            checksum(&scope.other),
            if n == 1 { "" } else { "s" }
        ));
    }
    blank(out);
    scope.members.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, mut entry) in scope.members {
        let class = entry.iter().any(|l| l.trim_start().starts_with("class "));
        if class && top {
            blank(out);
        }
        out.append(&mut entry);
        if class && top {
            out.push(String::new());
        }
    }
}

/// The outline of a Python module.
pub fn outline(text: &str) -> String {
    let toks = canonical(text);
    let lines = lines(&toks);
    let mut out = Vec::new();
    outline_scope(&lines, 0, "", &mut out, true);
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    let mut text = String::new();
    for l in out {
        let _ = writeln!(text, "{l}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_comments_and_formatting() {
        let a = "import os  # system\n\ndef f(a, b):\n    '''Sum.\n\n    Of two.\n    '''\n    return (a +\n            b)\n\nx = {'k': [1, 2,],}\n";
        let b = "import os\n\n\ndef f(a, b):\n  \"\"\"Sum.\n\n  Of two.\"\"\"\n  # add\n  return a + b\n\nx = {\"k\": [1, 2]}\n";
        assert!(same_code(a, b));
        // A different value, a tuple that loses its comma, a subscript.
        assert!(!same_code("x = 1\n", "x = 2\n"));
        assert!(!same_code("x = (1,)\n", "x = (1)\n"));
        assert!(!same_code("y = x[1,]\n", "y = x[1]\n"));
        assert!(same_code("f(a,)\n", "f(a)\n"));
        assert!(!same_code("x = {2: (lambda *a, **k: 1,)}\n", "x = {2: (lambda *a, **k: 1)}\n"));
        // Precedence: these parentheses matter.
        assert!(!same_code("x = (a + b) * c\n", "x = a + b * c\n"));
        assert!(!same_code("x = y in (a or b)\n", "x = y in a or b\n"));
        assert!(!same_code("f((a, b))\n", "f(a, b)\n"));
        assert!(!same_code("x = (a for a in b)\n", "x = a for a in b\n"));
        // What Black writes.
        assert!(same_code("from os import (path,\n    sep,)\n", "from os import path, sep\n"));
        assert!(same_code("x = -(2 ** 31)\n", "x = -2 ** 31\n"));
        assert!(same_code("if (m := f()):\n    pass\n", "if m := f():\n    pass\n"));
        assert!(same_code("class A():\n    pass\n", "class A:\n    pass\n"));
        assert!(same_code("if x: return 1\n", "if x:\n    return 1\n"));
        assert!(same_code("a = 1; b = 2\n", "a = 1\nb = 2\n"));
        assert!(same_code("y = [(a if b else c) for a in d]\n", "y = [a if b else c for a in d]\n"));
        assert!(same_code("t = .5 + 1e+3 + 2.\n", "t = 0.5 + 1e3 + 2.0\n"));
        assert!(same_code("s = b'\\xFF'\n", "s = b'\\xff'\n"));
        // Indentation is structure.
        assert!(!same_code("if a:\n    b()\nc()\n", "if a:\n    b()\n    c()\n"));
        assert!(same_code("if a:\n    b()\n", "if a:\n\tb()\n"));
        assert!(same_code("x = 'it\\'s'\n", "x = \"it's\"\n"));
        assert!(!same_code("x = r'a\\'b'\n", "x = r\"a'b\"\n"));
        assert!(same_code("if (a and\n    b):\n    pass\n", "if a and b:\n    pass\n"));
        assert!(same_code("n = 0XFF\n", "n = 0xff\n"));
    }

    #[test]
    fn strings_and_f_strings() {
        let t = tokens("s = f\"{x[\"a\"]} {{y}}\" + rb'\\'' + '''a\n# not a comment\n'''\n");
        let texts: Vec<&str> = t.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts[2], "f\"{x[\"a\"]} {{y}}\"");
        assert_eq!(texts[4], "br\"\\'\"");
        assert!(texts[6].contains("# not a comment"));
    }

    #[test]
    fn outlines_a_module() {
        let src = r#"
"""Shop."""
import sys
from dataclasses import dataclass

TAX = 0.19

@dataclass
class Item:
    name: str
    price: float = 0.0

    def total(self, qty: int = 1) -> float:
        return self.price * qty * (1 + TAX)

    def __init__(self, name):
        self.name = name

async def fetch(url, *, timeout=10, **kw):
    return await get(url)

def main(argv=None):
    print(Item("x").total(2))

if __name__ == "__main__":
    main(sys.argv)
"#;
        let o = outline(src);
        assert!(
            o.starts_with("\"Shop.\"  # docstring 600a4ad4\nfrom dataclasses import dataclass\nimport sys\n\nTAX = 0.19\n\n@dataclass\n"),
            "{o}"
        );
        assert!(o.contains("\n@dataclass\nclass Item:\n    name: str\n    price: float = 0.0\n    def __init__(self, name):  # code "), "{o}");
        assert!(o.contains("\n    def total(self, qty: int = 1) -> float:  # code "), "{o}");
        assert!(o.contains("\nasync def fetch(url, *, timeout=10, **kw):  # code "), "{o}");
        assert!(o.contains("\ndef main(argv=None):  # code "), "{o}");
        assert!(o.contains("\nif __name__ == \"__main__\":  # code "), "{o}");
        // Moving a function does not change the outline; changing one does.
        let moved = src.replace("async def fetch(url, *, timeout=10, **kw):\n    return await get(url)\n", "")
            + "\nasync def fetch(url, *, timeout=10, **kw):\n    return await get(url)\n";
        assert_eq!(outline(&moved), o);
        assert_ne!(outline(&src.replace("qty * (1 + TAX)", "qty")), o);
    }
}
