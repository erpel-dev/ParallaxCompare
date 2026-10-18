//! Rust source: an outline of the items, and a comparison that ignores
//! comments (doc comments too) and formatting.
//!
//! Both work on tokens. The lexer knows nested block comments, raw strings
//! (`r#"…"#`), byte and C strings, char literals versus lifetimes and number
//! suffixes. What rustfmt adds, removes or reorders does not count: line
//! breaks and spacing, trailing commas (not the one of a 1-tuple), the order of
//! `use`, `mod` and `extern crate` items and of the names inside `use` braces,
//! commas between match arms and the braces it puts around a long arm body,
//! the semicolon after a final `return`, `break` or `continue`, and nested
//! parentheses.
//!
//! The outline lists the items of each module, sorted by kind and name: `use`
//! declarations, constants, types with their fields or variants, traits and
//! `impl` blocks with their members, functions with their signature and a
//! checksum of their body, and macros.

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Ident,
    Lifetime,
    Literal,
    Punct,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
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

const PUNCT: &[&str] = &[
    "<<=", ">>=", "...", "..=", "::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "%=",
    "^=", "&=", "|=", "<<", ">>", "..",
];

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn",
    "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static",
    "struct", "super", "trait", "true", "type", "union", "unsafe", "use", "where", "while", "yield",
];

fn ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}
fn ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// The tokens of Rust source, without comments and layout.
pub fn tokens(text: &str) -> Vec<Tok> {
    let s = text;
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // A shebang line, but not an inner attribute `#![…]`.
    if s.starts_with("#!") && !s[2..].trim_start().starts_with('[') {
        i = s.find('\n').unwrap_or(s.len());
    }
    while i < b.len() {
        let c = s[i..].chars().next().expect("in bounds");
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let rest = &s[i..];
        if rest.starts_with("//") {
            i += rest.find('\n').unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("/*") {
            // Block comments nest.
            let mut depth = 0usize;
            let mut j = i;
            while j < b.len() {
                if s[j..].starts_with("/*") {
                    depth += 1;
                    j += 2;
                } else if s[j..].starts_with("*/") {
                    depth -= 1;
                    j += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    j += s[j..].chars().next().map_or(1, char::len_utf8);
                }
            }
            i = j;
            continue;
        }
        // Raw strings and identifiers: r"…", r#"…"#, br#"…"#, cr"…", r#ident.
        let raw_prefix = ["br", "cr", "r"].into_iter().find(|p| {
            rest.starts_with(p) && {
                let after = &rest[p.len()..];
                after.starts_with('"') || (after.starts_with('#') && after.trim_start_matches('#').starts_with('"'))
            }
        });
        if let Some(p) = raw_prefix {
            let hashes = rest[p.len()..].bytes().take_while(|&c| c == b'#').count();
            let open = p.len() + hashes + 1;
            let close = format!("\"{}", "#".repeat(hashes));
            let end = rest[open..].find(&close).map_or(rest.len(), |n| open + n + close.len());
            // The literal's value: a raw string is the same as an escaped one
            // with the same text only when nothing needs escaping, so keep the kind.
            out.push(Tok::new(
                Kind::Literal,
                format!("{p}\"{}", &rest[open..end.saturating_sub(close.len()).max(open)]),
            ));
            i += end;
            continue;
        }
        if rest.starts_with("r#") && rest[2..].starts_with(ident_start) {
            let n = rest[2..].chars().take_while(|&c| ident_continue(c)).map(char::len_utf8).sum::<usize>();
            out.push(Tok::new(Kind::Ident, &rest[..2 + n]));
            i += 2 + n;
            continue;
        }
        // Strings: "…", b"…", c"…"; byte chars b'…'.
        let str_prefix = ["b\"", "c\"", "\""].into_iter().find(|p| rest.starts_with(p));
        if let Some(p) = str_prefix {
            let mut j = p.len();
            let rb = rest.as_bytes();
            while j < rb.len() && rb[j] != b'"' {
                j += if rb[j] == b'\\' { 2 } else { 1 };
            }
            let end = (j + 1).min(rest.len());
            out.push(Tok::new(Kind::Literal, &rest[..end]));
            i += end;
            continue;
        }
        if rest.starts_with("b'") || rest.starts_with('\'') {
            let start = usize::from(rest.starts_with('b'));
            let body = &rest[start + 1..];
            // A char literal: one character or an escape, then `'`.
            let char_len = if let Some(escaped) = body.strip_prefix('\\') {
                // Skip the escaped character, which may be `'` itself.
                let first = escaped.chars().next().map_or(0, char::len_utf8);
                escaped[first..].find('\'').map(|n| 1 + first + n)
            } else {
                body.chars().next().map(char::len_utf8).filter(|&n| body[n..].starts_with('\''))
            };
            if let Some(n) = char_len {
                let end = start + 1 + n + 1;
                out.push(Tok::new(Kind::Literal, &rest[..end]));
                i += end;
                continue;
            }
            if start == 0 && body.starts_with(ident_start) {
                let n = body.chars().take_while(|&c| ident_continue(c)).map(char::len_utf8).sum::<usize>();
                out.push(Tok::new(Kind::Lifetime, &rest[..1 + n]));
                i += 1 + n;
                continue;
            }
        }
        if c.is_ascii_digit() {
            let rb = rest.as_bytes();
            let mut j = 0;
            let hex = rest.starts_with("0x") || rest.starts_with("0X");
            while j < rb.len() {
                let d = rb[j];
                if d.is_ascii_alphanumeric() || d == b'_' {
                    // An exponent's sign.
                    if !hex && matches!(d, b'e' | b'E') && matches!(rb.get(j + 1), Some(b'+' | b'-')) {
                        j += 1;
                    }
                    j += 1;
                } else if d == b'.' && !hex {
                    // `1.5`, `1.` but not `1..2`, `1.max(2)` or `x.0.1`.
                    let next = rb.get(j + 1).copied();
                    let prev_dot = out.last().is_some_and(|t: &Tok| t.is("."));
                    if prev_dot
                        || rest[..j].contains('.')
                        || next == Some(b'.')
                        || next.is_some_and(|n| n.is_ascii_alphabetic() || n == b'_')
                    {
                        break;
                    }
                    j += 1;
                } else {
                    break;
                }
            }
            out.push(Tok::new(Kind::Literal, rest[..j].replace('_', "")));
            i += j;
            continue;
        }
        if ident_start(c) {
            let n = rest.chars().take_while(|&c| ident_continue(c)).map(char::len_utf8).sum::<usize>();
            out.push(Tok::new(Kind::Ident, &rest[..n]));
            i += n;
            continue;
        }
        let p = PUNCT.iter().find(|p| rest.starts_with(**p)).map_or(&rest[..c.len_utf8()], |p| *p);
        out.push(Tok::new(Kind::Punct, p));
        i += p.len();
    }
    out
}

/// The index after the bracket that closes the one at `open`.
fn after_close(toks: &[Tok], open: usize) -> usize {
    let mut depth = 0usize;
    for (j, t) in toks.iter().enumerate().skip(open) {
        if t.open() {
            depth += 1;
        } else if t.close() {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return j + 1;
            }
        }
    }
    toks.len()
}

fn top_level_has(toks: &[Tok], what: &str) -> bool {
    let mut depth = 0usize;
    for t in toks {
        if t.open() {
            depth += 1;
        } else if t.close() {
            depth = depth.saturating_sub(1);
        } else if depth == 0 && t.is(what) {
            return true;
        }
    }
    false
}

/// Whether an opening parenthesis after `prev` is a call or a tuple struct,
/// where a trailing comma does not make a tuple.
fn call_like(prev: Option<&Tok>) -> bool {
    prev.is_some_and(|p| {
        (p.kind == Kind::Ident && !KEYWORDS.contains(&p.text.as_str()) || p.is("self") || p.is("Self"))
            || p.is(">")
            || p.is(")")
            || p.is("]")
            || p.is("!")
            || p.is("fn")
    })
}

/// A `use` tree in a canonical form: the names in braces sorted, `{a}` as `a`.
fn use_tree(toks: &[Tok]) -> Vec<Tok> {
    // Split at the first top-level `{`: prefix `a::b::` then the group.
    let Some(open) = toks.iter().position(|t| t.is("{")) else { return toks.to_vec() };
    let close = after_close(toks, open);
    let inner = &toks[open + 1..close.saturating_sub(1)];
    let mut items: Vec<Vec<Tok>> = Vec::new();
    let mut depth = 0usize;
    let mut cur = Vec::new();
    for t in inner {
        if t.open() {
            depth += 1;
        } else if t.close() {
            depth = depth.saturating_sub(1);
        }
        if depth == 0 && t.is(",") {
            items.push(std::mem::take(&mut cur));
        } else {
            cur.push(t.clone());
        }
    }
    if !cur.is_empty() {
        items.push(cur);
    }
    let mut items: Vec<Vec<Tok>> = items.iter().filter(|i| !i.is_empty()).map(|i| use_tree(i)).collect();
    items.sort();
    items.dedup();
    let mut out = toks[..open].to_vec();
    if items.len() == 1 && !items[0].iter().any(|t| t.is("self")) {
        out.extend(items.pop().expect("one"));
    } else {
        out.push(Tok::new(Kind::Punct, "{"));
        for (n, item) in items.into_iter().enumerate() {
            if n > 0 {
                out.push(Tok::new(Kind::Punct, ","));
            }
            out.extend(item);
        }
        out.push(Tok::new(Kind::Punct, "}"));
    }
    out.extend_from_slice(&toks[close..]);
    out
}

/// Item ranges at the top level of a token range: attributes, then up to a
/// `;` or the closing brace of the item's block.
fn items(toks: &[Tok]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let start = i;
        // Outer attributes and doc attributes.
        while i < toks.len() && toks[i].is("#") {
            i += 1;
            if toks.get(i).is_some_and(|t| t.is("!")) {
                i += 1;
            }
            if toks.get(i).is_some_and(|t| t.is("[")) {
                i = after_close(toks, i);
            }
        }
        // Inner attributes stand alone.
        if i > start && toks[start + 1].is("!") {
            out.push((start, i));
            continue;
        }
        // The item's keyword, after visibility and `const`/`unsafe`/`async` before `fn`.
        let mut k = i;
        if toks.get(k).is_some_and(|t| t.is("pub")) {
            k += 1;
            if toks.get(k).is_some_and(|t| t.is("(")) {
                k = after_close(toks, k);
            }
        }
        let kw = toks.get(k);
        let ends_at_semicolon = kw.is_some_and(|t| t.is("use") || t.is("type") || t.is("let") || t.is("static"))
            || (kw.is_some_and(|t| t.is("const"))
                && !toks
                    .get(k + 1)
                    .is_some_and(|t| t.is("fn") || t.is("unsafe") || t.is("async") || t.is("extern") || t.is("{")));
        while i < toks.len() {
            let t = &toks[i];
            if t.is(";") {
                i += 1;
                break;
            }
            if t.open() {
                let end = after_close(toks, i);
                let brace = t.is("{");
                i = end;
                if brace && !ends_at_semicolon {
                    // `macro_rules! m { … }` and blocks end the item; `struct A {…}` too.
                    if toks.get(i).is_some_and(|n| n.is(";")) {
                        i += 1;
                    }
                    break;
                }
                continue;
            }
            i += 1;
        }
        out.push((start, i));
    }
    out
}

/// Whether an item (after attributes) is a `use`, `mod x;` or `extern crate`
/// declaration, whose order does not matter.
fn reorderable(item: &[Tok]) -> Option<u8> {
    let mut j = 0;
    while j < item.len() && item[j].is("#") {
        j += 1;
        if item.get(j).is_some_and(|t| t.is("[")) {
            j = after_close(item, j);
        }
    }
    if item.get(j).is_some_and(|t| t.is("pub")) {
        j += 1;
        if item.get(j).is_some_and(|t| t.is("(")) {
            j = after_close(item, j);
        }
    }
    let k = item.get(j)?;
    let last = item.last()?;
    if k.is("use") {
        Some(0)
    } else if k.is("extern") && item.get(j + 1).is_some_and(|t| t.is("crate")) {
        Some(1)
    } else if k.is("mod") && last.is(";") {
        Some(2)
    } else {
        None
    }
}

/// Sorts runs of `use`, `mod x;` and `extern crate` items, in every block.
fn sort_declarations(toks: Vec<Tok>) -> Vec<Tok> {
    let ranges = items(&toks);
    let mut out = Vec::with_capacity(toks.len());
    let mut run: Vec<(u8, Vec<Tok>)> = Vec::new();
    let flush = |out: &mut Vec<Tok>, run: &mut Vec<(u8, Vec<Tok>)>| {
        run.sort();
        for (_, item) in run.drain(..) {
            out.extend(item);
        }
    };
    for (s, e) in ranges {
        let item = &toks[s..e];
        match reorderable(item) {
            Some(kind) => {
                let item = if kind == 0 { use_tree(item) } else { item.to_vec() };
                run.push((kind, item));
            }
            None => {
                flush(&mut out, &mut run);
                // Blocks inside the item: module bodies, function bodies.
                let mut j = 0;
                while j < item.len() {
                    if item[j].is("{") {
                        let end = after_close(item, j);
                        out.push(item[j].clone());
                        out.extend(sort_declarations(item[j + 1..end - 1].to_vec()));
                        out.push(item[end - 1].clone());
                        j = end;
                    } else {
                        out.push(item[j].clone());
                        j += 1;
                    }
                }
            }
        }
    }
    flush(&mut out, &mut run);
    out
}

/// Which tokens are inside a `macro_rules!` definition, where any token of a
/// matcher can be matched literally.
fn macro_rules_ranges(toks: &[Tok]) -> Vec<bool> {
    let n = toks.len();
    let mut literal = vec![false; n];
    for i in 0..n {
        if toks[i].is("macro_rules")
            && toks.get(i + 1).is_some_and(|b| b.is("!"))
            && toks.get(i + 3).is_some_and(Tok::open)
        {
            let end = after_close(toks, i + 3);
            literal[i..end].iter_mut().for_each(|l| *l = true);
        }
    }
    literal
}

/// Tokens with what rustfmt adds, removes or reorders taken out.
pub fn canonical(text: &str) -> Vec<Tok> {
    let toks = normalize_forms(sort_declarations(tokens(text)));
    let n = toks.len();
    let mut drop = vec![false; n];
    let literal = macro_rules_ranges(&toks);
    let mut unwrap = vec![false; n];
    // Where each match body starts and ends.
    let mut match_bodies: Vec<(usize, usize)> = Vec::new();
    for i in 0..n {
        let t = &toks[i];
        if t.is("match") {
            let mut depth = 0usize;
            for (j, u) in toks.iter().enumerate().skip(i + 1) {
                if u.is("{") && depth == 0 {
                    match_bodies.push((j, after_close(&toks, j) - 1));
                    break;
                }
                if u.open() {
                    depth += 1;
                } else if u.close() {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                } else if u.is(";") && depth == 0 {
                    break;
                }
            }
        }
        if !t.open() || literal[i] {
            continue;
        }
        let end = after_close(&toks, i) - 1;
        if end <= i {
            continue;
        }
        let prev = i.checked_sub(1).map(|p| &toks[p]);
        // A trailing comma, unless it makes a 1-tuple.
        if end > i + 1 && toks[end - 1].is(",") {
            let keep = t.is("(") && !call_like(prev) && !top_level_has(&toks[i + 1..end - 1], ",");
            if !keep {
                drop[end - 1] = true;
            }
        }
        // Nested parentheses: `((a))` is `(a)` when the outer pair is not a call.
        if t.is("(") && toks[i + 1].is("(") && after_close(&toks, i + 1) == end && !call_like(prev) {
            drop[i + 1] = true;
            drop[end - 1] = true;
        }
    }
    // Generic arguments: a trailing comma before `>`.
    for i in 1..n {
        if toks[i].is(">") && toks[i - 1].is(",") && !literal[i] {
            drop[i - 1] = true;
        }
    }
    for &(open, close) in &match_bodies {
        if literal[open] {
            continue;
        }
        let mut depth = 0usize;
        let mut j = open + 1;
        while j < close {
            let t = &toks[j];
            if t.open() {
                depth += 1;
            } else if t.close() {
                depth = depth.saturating_sub(1);
            }
            if depth == 0 && t.is(",") {
                drop[j] = true;
            }
            // `=> { expr }`: a block with one expression is the expression.
            if depth == 0 && t.is("=>") && toks.get(j + 1).is_some_and(|b| b.is("{")) {
                let b_end = after_close(&toks, j + 1) - 1;
                let inner = &toks[j + 2..b_end];
                let simple = !inner.is_empty()
                    && !top_level_has(inner, ";")
                    && !inner[0].is("let")
                    && !inner[0].is("#")
                    && !inner.iter().any(|t| t.kind == Kind::Lifetime);
                if simple {
                    unwrap[j + 1] = true;
                    unwrap[b_end] = true;
                }
            }
            j += 1;
        }
    }
    // `return x }` is `return x; }`; also `break` and `continue`.
    for i in 1..n {
        if !(toks[i].is("}") && toks[i - 1].is(";")) || literal[i] {
            continue;
        }
        let mut depth = 0usize;
        let mut k = i - 1;
        let mut start = 0;
        while k > 0 {
            k -= 1;
            let t = &toks[k];
            if t.close() {
                depth += 1;
            } else if t.open() {
                if depth == 0 {
                    start = k + 1;
                    break;
                }
                depth -= 1;
            } else if depth == 0 && t.is(";") {
                start = k + 1;
                break;
            }
        }
        if toks.get(start).is_some_and(|t| t.is("return") || t.is("break") || t.is("continue")) {
            drop[i - 1] = true;
        }
    }
    toks.into_iter().enumerate().filter(|(i, _)| !drop[*i] && !unwrap[*i]).map(|(_, t)| t).collect()
}

/// Whether the token before `|` makes it the start of a closure rather than `|` the operator.
fn closure_start(prev: Option<&Tok>) -> bool {
    prev.is_none_or(|p| match p.kind {
        Kind::Ident => KEYWORDS.contains(&p.text.as_str()) && !p.is("self") && !p.is("Self"),
        Kind::Literal | Kind::Lifetime => false,
        Kind::Punct => !matches!(p.text.as_str(), ")" | "]" | "}" | "?"),
    })
}

/// Forms rustfmt rewrites that mean the same:
/// - `>>`, `>=`, `>>=`, `<<` split into single characters (generic brackets),
/// - `extern {` is `extern "C" {`,
/// - a closure body `{ expr }` with one expression is the expression,
/// - a trailing comma in a `where` clause,
/// - the delimiters of `macro_rules!` matchers and transcribers, and the `;` between rules.
fn normalize_forms(toks: Vec<Tok>) -> Vec<Tok> {
    let mut split: Vec<Tok> = Vec::with_capacity(toks.len());
    for t in toks {
        if t.kind == Kind::Punct && matches!(t.text.as_str(), ">>" | ">=" | ">>=" | "<<" | "<<=") {
            split.extend(t.text.chars().map(|c| Tok::new(Kind::Punct, c.to_string())));
            continue;
        }
        let abi = t.is("{") || t.is("fn");
        if abi && split.last().is_some_and(|p| p.is("extern")) {
            split.push(Tok::new(Kind::Literal, "\"C\""));
        }
        split.push(t);
    }
    let toks = split;
    let n = toks.len();
    let mut drop = vec![false; n];
    let mut replace: Vec<Option<&'static str>> = vec![None; n];
    let literal = macro_rules_ranges(&toks);
    let mut i = 0;
    while i < n {
        let t = &toks[i];
        if literal[i] && !t.is("macro_rules") {
            i += 1;
            continue;
        }
        // Closures: `|a, b| body`, `|| body`, `move |x| body`.
        if (t.is("|") || t.is("||")) && closure_start(i.checked_sub(1).map(|p| &toks[p])) {
            let mut j = i + 1;
            if t.is("|") {
                let mut depth = 0usize;
                while j < n && !(depth == 0 && toks[j].is("|")) {
                    if toks[j].open() {
                        depth += 1;
                    } else if toks[j].close() {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    j += 1;
                }
                j += 1;
            }
            if toks.get(j).is_some_and(|b| b.is("{")) {
                let end = after_close(&toks, j) - 1;
                let inner = &toks[j + 1..end];
                let simple = !inner.is_empty()
                    && !top_level_has(inner, ";")
                    && !inner[0].is("let")
                    && !inner[0].is("#")
                    && !inner
                        .iter()
                        .any(|t| t.is("use") || t.is("fn") || t.is("struct") || t.is("const") || t.is("static"));
                if simple {
                    drop[j] = true;
                    drop[end] = true;
                }
            }
        }
        // A `where` clause ends at `{` or `;`; its trailing comma does not count.
        if t.is("where") {
            let mut depth = 0usize;
            let mut j = i + 1;
            while j < n {
                let u = &toks[j];
                if depth == 0 && (u.is("{") || u.is(";")) {
                    if toks[j - 1].is(",") {
                        drop[j - 1] = true;
                    }
                    break;
                }
                if u.open() || u.is("<") {
                    depth += 1;
                } else if u.close() || (u.is(">") && depth > 0 && !toks[j - 1].is("-")) {
                    depth = depth.saturating_sub(1);
                }
                j += 1;
            }
        }
        // `macro_rules! name { (…) => {…}; … }`
        if t.is("macro_rules") && toks.get(i + 1).is_some_and(|b| b.is("!")) && toks.get(i + 3).is_some_and(Tok::open) {
            let open = i + 3;
            let end = after_close(&toks, open) - 1;
            let mut j = open + 1;
            while j < end {
                if toks[j].open() {
                    let close = after_close(&toks, j) - 1;
                    let matcher = toks.get(close + 1).is_some_and(|a| a.is("=>"));
                    let transcriber = toks[j - 1].is("=>");
                    if matcher || transcriber {
                        replace[j] = Some("{");
                        replace[close] = Some("}");
                    }
                    j = close + 1;
                    continue;
                }
                if toks[j].is(";") {
                    drop[j] = true;
                }
                j += 1;
            }
        }
        i += 1;
    }
    toks.into_iter()
        .enumerate()
        .filter(|(i, _)| !drop[*i])
        .map(|(i, t)| match replace[i] {
            Some(r) => Tok::new(Kind::Punct, r),
            None => t,
        })
        .collect()
}

/// The canonical tokens for comparing: also `loop { … };` is `loop { … }`
/// where it is not the block's last statement. (Two valid programs can only
/// differ there in a block-like statement: `if`, `match`, `loop`, `while`,
/// `for`, `unsafe` or a bare block.)
fn comparable(text: &str) -> Vec<Tok> {
    let toks = canonical(text);
    let n = toks.len();
    let literal = macro_rules_ranges(&toks);
    // The opening bracket of each closing one.
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
    let block_like = |close: usize| -> bool {
        let open = opener[close];
        if open == usize::MAX {
            return false;
        }
        // Back to the start of the statement.
        let mut depth = 0usize;
        let mut k = open;
        while k > 0 {
            let t = &toks[k - 1];
            // A block before at this level ends the statement before it,
            // unless this is its `else`.
            if depth == 0 && t.is("}") && !toks[k].is("else") {
                break;
            }
            if t.close() {
                depth += 1;
            } else if t.open() {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            } else if depth == 0 && t.is(";") {
                break;
            }
            k -= 1;
        }
        let first = &toks[k];
        k == open
            || ["if", "match", "loop", "while", "for", "unsafe"].iter().any(|w| first.is(w))
            || first.kind == Kind::Lifetime
    };
    let mut out = Vec::with_capacity(n);
    for (i, t) in toks.iter().enumerate() {
        let after_block = i > 0 && toks[i - 1].is("}") && block_like(i - 1);
        if t.is(";") && !literal[i] && after_block && toks.get(i + 1).is_some_and(|n| !n.is("}")) {
            continue;
        }
        out.push(t.clone());
    }
    out
}

/// Whether two Rust sources are the same code, apart from comments and formatting.
pub fn same_code(left: &str, right: &str) -> bool {
    comparable(left) == comparable(right)
}

// ---- outline ----

fn render(toks: &[Tok]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Tok> = None;
    let mut angle = 0usize;
    for (k, t) in toks.iter().enumerate() {
        if let Some(p) = prev {
            // `&` as a reference, not the operator: no operand right before it.
            let reference = (p.is("&") || p.is("&&"))
                && (k < 2
                    || !(matches!(toks[k - 2].kind, Kind::Ident | Kind::Literal)
                        && !KEYWORDS.contains(&toks[k - 2].text.as_str())
                        || toks[k - 2].is(")")
                        || toks[k - 2].is("]")));
            let tight = p.is("(")
                || p.is("[")
                || p.is("::")
                || p.is(".")
                || reference
                || p.is("#")
                || p.is("!") && !t.is("{")
                || p.is("<") && angle > 0
                || p.is("'")
                || t.is(")")
                || t.is("]")
                || t.is(",")
                || t.is(";")
                || t.is("::")
                || t.is(".")
                || t.is("?")
                || t.is(":") && p.kind != Kind::Punct
                || (t.is("(") || t.is("["))
                    && (p.kind == Kind::Ident && !KEYWORDS.contains(&p.text.as_str())
                        || p.is(">")
                        || p.is("!")
                        || p.is(")"))
                || t.is("!") && p.kind == Kind::Ident
                || (t.is("<") || t.is(">")) && angle_context(p, t, angle);
            if !tight {
                out.push(' ');
            }
        }
        if t.is("<") && prev.is_some_and(|p| p.kind == Kind::Ident || p.is("::") || p.is("&") || p.is("impl")) {
            angle += 1;
        } else if t.is(">") && angle > 0 {
            angle -= 1;
        }
        out.push_str(&t.text);
        prev = Some(t);
    }
    out
}

/// Generic brackets are written without spaces: `Vec<u8>`, `impl<T>`.
fn angle_context(prev: &Tok, t: &Tok, angle: usize) -> bool {
    if t.is("<") { prev.kind == Kind::Ident || prev.is("::") } else { angle > 0 }
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

/// Splits a brace body at top-level commas (fields, variants).
fn comma_list(toks: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, t) in toks.iter().enumerate() {
        if t.open() || (t.is("<") && depth_angle(toks, i)) {
            depth += 1;
        } else if t.close() || (t.is(">") && depth > 0 && !toks[i - 1].is("-") && !toks[i - 1].is("=")) {
            depth = depth.saturating_sub(1);
        } else if depth == 0 && t.is(",") {
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

fn depth_angle(toks: &[Tok], i: usize) -> bool {
    i > 0 && (toks[i - 1].kind == Kind::Ident || toks[i - 1].is("::"))
}

/// The item's keyword (after attributes and visibility) and name.
fn head(item: &[Tok]) -> (usize, String, String) {
    let mut j = 0;
    while j < item.len() && item[j].is("#") {
        j += 1;
        if item.get(j).is_some_and(|t| t.is("!")) {
            j += 1;
        }
        if item.get(j).is_some_and(|t| t.is("[")) {
            j = after_close(item, j);
        }
    }
    let attrs_end = j;
    let mut k = j;
    if item.get(k).is_some_and(|t| t.is("pub")) {
        k += 1;
        if item.get(k).is_some_and(|t| t.is("(")) {
            k = after_close(item, k);
        }
    }
    while item.get(k).is_some_and(|t| ["const", "async", "unsafe", "extern", "default"].iter().any(|m| t.is(m)))
        && !item.get(k + 1).is_some_and(|t| {
            t.is(":")
                || t.is("=")
                || (t.kind == Kind::Ident && item.get(k + 2).is_some_and(|u| u.is(":"))) && item[k].is("const")
        })
    {
        k += 1;
        // `extern "C"`
        if item.get(k).is_some_and(|t| t.kind == Kind::Literal) {
            k += 1;
        }
    }
    let kw = item.get(k).map_or(String::new(), |t| t.text.clone());
    let name = match kw.as_str() {
        "impl" => render(&item[k..item[k..].iter().position(|t| t.is("{")).map_or(item.len(), |p| p + k)]),
        "macro_rules" => item.get(k + 2).map_or(String::new(), |t| t.text.clone()),
        _ => item.get(k + 1).map_or(String::new(), |t| t.text.clone()),
    };
    (attrs_end, kw, name)
}

/// Attributes worth showing: not doc comments, which the lexer drops anyway.
fn attributes(item: &[Tok], attrs_end: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut j = 0;
    while j < attrs_end {
        let end = if item.get(j + 1).is_some_and(|t| t.is("[")) {
            after_close(item, j + 1)
        } else {
            after_close(item, j + 2)
        };
        out.push(render(&item[j..end]));
        j = end;
    }
    out
}

const LONG: usize = 100;

fn outline_items(toks: &[Tok], indent: &str, out: &mut Vec<String>) {
    let mut entries: Vec<(u8, String, Vec<String>)> = Vec::new();
    for (s, e) in items(toks) {
        let item = &toks[s..e];
        if item.is_empty() {
            continue;
        }
        let (attrs_end, kw, name) = head(item);
        let mut lines: Vec<String> = attributes(item, attrs_end).into_iter().map(|a| format!("{indent}{a}")).collect();
        let body_open = item[attrs_end..].iter().position(|t| t.is("{")).map(|p| p + attrs_end);
        let sig = |end: usize| render(&item[attrs_end..end]);
        let rank = match kw.as_str() {
            _ if item.get(attrs_end).is_some_and(|t| t.is("!") || t.is("[")) => 0,
            "use" => 1,
            "extern" if item.iter().any(|t| t.is("crate")) => 1,
            "mod" if item.last().is_some_and(|t| t.is(";")) => 2,
            "const" | "static" => 3,
            "type" | "struct" | "enum" | "union" | "trait" => 4,
            "impl" => 5,
            "fn" => 6,
            "macro_rules" => 7,
            "mod" => 8,
            _ => 9,
        };
        match kw.as_str() {
            "fn" => {
                let (head_end, body) = match body_open {
                    Some(o) if item.last().is_some_and(|t| t.is("}")) => (o, &item[o..]),
                    _ => (item.len(), &item[item.len()..]),
                };
                let mut line = format!("{indent}{}", sig(head_end));
                if body.is_empty() {
                    line = line.trim_end_matches(';').to_string() + ";";
                } else {
                    let _ = write!(line, "  // code {}, {} tokens", checksum(body), body.len());
                }
                lines.push(line);
            }
            "struct" | "enum" | "union" if body_open.is_some() && item.last().is_some_and(|t| t.is("}")) => {
                let o = body_open.expect("checked");
                lines.push(format!("{indent}{} {{", sig(o)));
                let close = after_close(item, o) - 1;
                for member in comma_list(&item[o + 1..close]) {
                    lines.push(format!("{indent}    {},", render(member)));
                }
                lines.push(format!("{indent}}}"));
            }
            "trait" | "impl" | "mod" | "extern" if body_open.is_some() && item.last().is_some_and(|t| t.is("}")) => {
                let o = body_open.expect("checked");
                lines.push(format!("{indent}{} {{", sig(o)));
                let close = after_close(item, o) - 1;
                outline_items(&item[o + 1..close], &format!("{indent}    "), &mut lines);
                lines.push(format!("{indent}}}"));
            }
            "macro_rules" => {
                lines.push(format!("{indent}macro_rules! {name}  // code {}, {} tokens", checksum(item), item.len()));
            }
            "use" => {
                lines.push(format!("{indent}{}", sig(item.len()).replace("{ ", "{").replace(" }", "}")));
            }
            _ => {
                let text = sig(item.len());
                if text.chars().count() > LONG {
                    let eq = item.iter().position(|t| t.is("=")).unwrap_or(item.len());
                    lines.push(format!("{indent}{} = …;  // value {}", sig(eq.max(attrs_end)), checksum(&item[eq..])));
                } else {
                    lines.push(format!("{indent}{text}"));
                }
            }
        }
        entries.push((rank, name, lines));
    }
    entries.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let mut last_rank = None;
    for (rank, _, mut lines) in entries {
        let block = lines.len() > 1 && lines.last().is_some_and(|l| l.trim_end() == "}");
        if indent.is_empty()
            && (block || last_rank.is_some_and(|r| r != rank))
            && out.last().is_some_and(|l| !l.is_empty())
        {
            out.push(String::new());
        }
        out.append(&mut lines);
        if indent.is_empty() && block {
            out.push(String::new());
        }
        last_rank = Some(rank);
    }
}

/// The outline of a Rust source file.
pub fn outline(text: &str) -> String {
    let toks = canonical(text);
    let mut lines = Vec::new();
    outline_items(&toks, "", &mut lines);
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

    fn texts(src: &str) -> Vec<String> {
        tokens(src).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn lexes_rust() {
        assert_eq!(
            texts("let s = r#\"a \"// b\"#; // c\n/* x /* y */ z */ 'a'"),
            ["let", "s", "=", "r\"a \"// b", ";", "'a'"]
        );
        assert_eq!(
            texts("fn f<'a>(x: &'a str) -> char { '\\n' }"),
            ["fn", "f", "<", "'a", ">", "(", "x", ":", "&", "'a", "str", ")", "->", "char", "{", "'\\n'", "}"]
        );
        assert_eq!(
            texts("x.0.1 + 1..2 + 1.5e-3 + 1_000u32 + b'x' + c\"s\""),
            ["x", ".", "0", ".", "1", "+", "1", "..", "2", "+", "1.5e-3", "+", "1000u32", "+", "b'x'", "+", "c\"s\""]
        );
        assert_eq!(texts("r#type a&&b a& &b"), ["r#type", "a", "&&", "b", "a", "&", "&", "b"]);
        assert_eq!(
            texts("['\\'', '\\\\', '\\u{1F600}', 'x']"),
            ["[", "'\\''", ",", "'\\\\'", ",", "'\\u{1F600}'", ",", "'x'", "]"]
        );
    }

    #[test]
    fn ignores_what_rustfmt_changes() {
        let a = "use std::io;\nuse std::fmt::{Write, Debug};\nmod b;\nmod a;\n/// Docs.\nfn f(x: u8,) -> u8 {\n    match x {\n        0 => { 1 }\n        _ => 2,\n    }\n}\nfn g() { return }\n";
        let b = "mod a;\nmod b;\nuse std::fmt::{Debug, Write};\nuse std::io;\n\nfn f(x: u8) -> u8 {\n    match x {\n        0 => 1,\n        _ => 2,\n    }\n}\nfn g() {\n    return;\n}\n";
        assert!(same_code(a, b));
        assert!(!same_code("let t = (1,);", "let t = (1);"));
        assert!(same_code("f((a));", "f((a));"));
        assert!(same_code("let x = ((a + b));", "let x = (a + b);"));
        assert!(!same_code("f((a, b));", "f(a, b);"));
        assert!(!same_code("let x = a && b;", "let x = a & &b;"));
        assert!(!same_code("let s = \"a // b\";", "let s = \"a \";"));
        assert!(same_code("use a::{b};", "use a::b;"));
        assert!(!same_code("fn f() { let x = 1; x }", "fn f() { let x = 1; x; }"));
        assert!(!same_code("match x { 0 => { a(); b } }", "match x { 0 => a(); b }"));
        assert!(same_code("let v: Vec<Vec<u8> > = f(|x| { x + 1 });", "let v: Vec<Vec<u8>> = f(|x| x + 1);"));
        assert!(!same_code("let v = a | b;", "let v = a | { b };"));
        assert!(same_code("fn f<T>() where T: X, {}", "fn f<T>() where T: X {}"));
        assert!(same_code("macro_rules! m { ($a:expr) => ($a + 1); }", "macro_rules! m { ($a:expr) => { $a + 1 } }"));
        assert!(same_code("extern { fn f(); }", "extern \"C\" { fn f(); }"));
        assert!(!same_code("const X: u8 = a | (1,);", "const X: u8 = a | (1);"));
        assert!(same_code(
            "fn f() { loop { a() }; b(); if x { y } else { z }; c() }",
            "fn f() { loop { a() } b(); if x { y } else { z } c() }"
        ));
        assert!(!same_code("m! { use a::{b}; c }", "m! { use a::{b} c }"));
        assert!(!same_code("m! { if a { b } let x = S { c }; d }", "m! { if a { b } let x = S { c } d }"));
        assert!(!same_code("macro_rules! m { ($(<$t:tt>,)?) => {} }", "macro_rules! m { ($(<$t:tt>)?) => {} }"));
    }

    #[test]
    fn outlines_items() {
        let src = "use std::fmt;\n\n/// A point.\n#[derive(Debug, Clone)]\npub struct Point {\n    pub x: f64,\n    pub y: f64,\n}\n\nimpl Point {\n    pub fn new(x: f64, y: f64) -> Self { Point { x, y } }\n    fn norm(&self) -> f64 { (self.x * self.x + self.y * self.y).sqrt() }\n}\n\nimpl fmt::Display for Point {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, \"({}, {})\", self.x, self.y) }\n}\n\nconst ORIGIN: Point = Point { x: 0.0, y: 0.0 };\n\npub enum Shape { Circle(Point, f64), Square { at: Point, side: f64 } }\n\npub trait Area { fn area(&self) -> f64; }\n\nfn main() { println!(\"{}\", ORIGIN); }\n";
        let o = outline(src);
        assert!(
            o.starts_with("use std::fmt;\n\nconst ORIGIN: Point = Point { x: 0.0, y: 0.0 };\n\npub trait Area {"),
            "{o}"
        );
        assert!(
            o.contains("\n#[derive(Debug, Clone)]\npub struct Point {\n    pub x: f64,\n    pub y: f64,\n}\n"),
            "{o}"
        );
        assert!(
            o.contains("\npub enum Shape {\n    Circle(Point, f64),\n    Square { at: Point, side: f64 },\n}\n"),
            "{o}"
        );
        assert!(o.contains("\npub trait Area {\n    fn area(&self) -> f64;\n}\n"), "{o}");
        assert!(o.contains("\nimpl Point {\n    pub fn new(x: f64, y: f64) -> Self  // code "), "{o}");
        assert!(o.contains("\nimpl fmt::Display for Point {\n    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result  // code "), "{o}");
        assert!(o.contains("\nfn main()  // code "), "{o}");
    }
}
