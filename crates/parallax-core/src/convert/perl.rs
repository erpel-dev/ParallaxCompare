//! Perl source: an outline of the code, and a comparison that ignores
//! comments, POD and formatting.
//!
//! Like the Python view this works on tokens. The lexer knows Perl's quoting
//! (`'…'`, `"…"`, `q qq qw qx m qr s tr y` with any delimiters, here-documents,
//! `<FH>`), tells a regex from a division and a hash sigil from the modulo
//! operator by what precedes them, and skips comments, POD and the layout
//! between tokens. Strings that interpolate nothing compare equal in either
//! kind of quotes, `qw` lists by their words, and the separators a formatter
//! like perltidy adds or removes, a trailing comma in a list and the last
//! semicolon in a block, do not count. `__END__` and `__DATA__` sections
//! compare as they are.
//!
//! The outline lists per package the modules used, `our` variables and
//! constants, and the subs with their prototype or signature and a checksum of
//! their body, sorted by name; other code is summed up in one checksum.

use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Word,
    Var,
    Number,
    /// A string or other quote-like literal; the text starts with what kind.
    Quote,
    Op,
    /// The text after `__END__` or `__DATA__`.
    Data,
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
        matches!(self.kind, Kind::Op | Kind::Word) && self.text == s
    }
}

const OPERATORS: &[&str] = &[
    "<<>>", "**=", "||=", "&&=", "//=", "<<=", ">>=", "<=>", "...", "->", "++", "--", "**", "=~", "!~", "==", "!=",
    "<=", ">=", "&&", "||", "//", "..", "::", "<<", ">>", "+=", "-=", "*=", "/=", ".=", "%=", "x=", "&=", "|=", "^=",
    "=>",
];

/// Words after which a term (not an operator) follows: `split /,/`, `return %h`.
const TERM_AFTER: &[&str] = &[
    "and",
    "or",
    "not",
    "xor",
    "if",
    "unless",
    "while",
    "until",
    "elsif",
    "return",
    "split",
    "grep",
    "map",
    "join",
    "push",
    "unshift",
    "print",
    "printf",
    "say",
    "die",
    "warn",
    "eq",
    "ne",
    "lt",
    "gt",
    "le",
    "ge",
    "cmp",
    "x",
    "defined",
    "ref",
    "scalar",
    "lc",
    "uc",
    "lcfirst",
    "ucfirst",
    "keys",
    "values",
    "each",
    "exists",
    "delete",
    "when",
    "foreach",
    "for",
    "local",
    "my",
    "our",
    "sort",
    "reverse",
    "wantarray",
    "chomp",
    "chop",
    "exec",
    "system",
    "open",
    "close",
    "unlink",
    "last",
    "next",
    "redo",
    "goto",
    "croak",
    "confess",
    "carp",
    "length",
    "eval",
    "do",
    "lock",
];

/// Quote-like operators and how many parts they have.
fn quote_parts(word: &str) -> Option<usize> {
    match word {
        "q" | "qq" | "qw" | "qx" | "m" | "qr" => Some(1),
        "s" | "tr" | "y" => Some(2),
        _ => None,
    }
}

fn closing_delim(c: char) -> char {
    match c {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        other => other,
    }
}

struct Heredoc {
    terminator: String,
    indented: bool,
    interpolates: bool,
    /// Where the token is, to fill in its text when the body is read.
    at: usize,
}

struct Lexer<'a> {
    s: &'a str,
    i: usize,
    out: Vec<Tok>,
    heredocs: Vec<Heredoc>,
    /// Whether whitespace, or a line break, came before the current token.
    spaced: bool,
    newline: bool,
}

impl<'a> Lexer<'a> {
    fn rest(&self) -> &'a str {
        &self.s[self.i..]
    }
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    /// Whether a term is expected next, from the token before.
    fn expect_term(&self) -> bool {
        match self.out.last() {
            None => true,
            Some(t) => match t.kind {
                Kind::Op => !matches!(t.text.as_str(), ")" | "]" | "}"),
                Kind::Word => TERM_AFTER.contains(&t.text.as_str()),
                _ => false,
            },
        }
    }

    /// The body of a delimited part starting at the opening delimiter; returns
    /// it with escaped delimiters unescaped.
    fn delimited(&mut self) -> Option<(String, char)> {
        let open = self.peek()?;
        let close = closing_delim(open);
        self.i += open.len_utf8();
        let mut depth = 0usize;
        let mut body = String::new();
        let mut chars = self.rest().char_indices();
        while let Some((at, c)) = chars.next() {
            if c == '\\' {
                if let Some((_, e)) = chars.next() {
                    if e != open && e != close {
                        body.push('\\');
                    }
                    body.push(e);
                }
                continue;
            }
            if c == close && depth == 0 {
                self.i += at + c.len_utf8();
                return Some((body, open));
            }
            if open != close {
                if c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                }
            }
            body.push(c);
        }
        self.i = self.s.len();
        Some((body, open))
    }

    fn skip_space_and_comments(&mut self) {
        loop {
            let r = self.rest();
            let t = r.trim_start();
            self.i += r.len() - t.len();
            if t.starts_with('#') {
                self.i += t.find('\n').unwrap_or(t.len());
            } else {
                break;
            }
        }
    }

    /// A quote-like operator whose word has just been read.
    fn quote_like(&mut self, word: &str, parts: usize) -> Tok {
        // The delimiter may follow after whitespace, but not `#` then.
        self.i += self.rest().len() - self.rest().trim_start().len();
        let Some((first, open)) = self.delimited() else { return Tok::new(Kind::Quote, format!("{word}:")) };
        let mut text = format!("{word}:");
        if word == "qw" {
            text.push_str(&first.split_whitespace().collect::<Vec<_>>().join(" "));
        } else {
            text.push_str(&first);
        }
        if parts == 2 {
            text.push('\0');
            if open != closing_delim(open) {
                // `s{…}{…}`: the second part has its own delimiters.
                self.skip_space_and_comments();
                if let Some((second, _)) = self.delimited() {
                    text.push_str(&second);
                }
            } else {
                // `s/…/…/`: the closing delimiter of the first part opens the second.
                self.i -= open.len_utf8();
                if let Some((second, _)) = self.delimited() {
                    text.push_str(&second);
                }
            }
        }
        let flags: String = self.rest().chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        self.i += flags.len();
        if matches!(word, "m" | "qr" | "s" | "tr" | "y") {
            let mut f: Vec<char> = flags.chars().collect();
            f.sort_unstable();
            text.push('\0');
            text.extend(f);
        }
        // A `"…"`-like string that interpolates nothing is a `'…'` string.
        if word == "qq" && !text.contains(['$', '@', '\\']) {
            text.replace_range(..2, "q");
        }
        Tok::new(Kind::Quote, text)
    }

    /// Reads the bodies of pending here-documents at the start of a line.
    fn heredoc_bodies(&mut self) {
        for h in std::mem::take(&mut self.heredocs) {
            let mut lines: Vec<&str> = Vec::new();
            loop {
                if self.i >= self.s.len() {
                    break;
                }
                let line_end = self.rest().find('\n').map_or(self.s.len(), |n| self.i + n);
                let line = self.s[self.i..line_end].trim_end_matches('\r');
                self.i = (line_end + 1).min(self.s.len());
                let check = if h.indented { line.trim_start() } else { line };
                if check == h.terminator {
                    break;
                }
                lines.push(line);
            }
            if h.indented {
                let indent = lines
                    .iter()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| l.len() - l.trim_start().len())
                    .min()
                    .unwrap_or(0);
                for l in &mut lines {
                    *l = l.get(indent..).unwrap_or("");
                }
            }
            let body = lines.join("\n");
            let kind = if h.interpolates && body.contains(['$', '@', '\\']) { "qq" } else { "q" };
            self.out[h.at].text = format!("{kind}:{body}\n");
        }
    }

    fn run(mut self) -> Vec<Tok> {
        let mut line_start = true;
        while self.i < self.s.len() {
            let c = self.peek().expect("in bounds");
            // POD: from a line starting with `=word` to `=cut`.
            if line_start && c == '=' && self.rest()[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
                // The first line starts POD even if it is `=cut`, as in perl.
                let mut first = true;
                loop {
                    let line_end = self.rest().find('\n').map_or(self.s.len(), |n| self.i + n);
                    let is_cut = !first && self.s[self.i..line_end].starts_with("=cut");
                    first = false;
                    self.i = (line_end + 1).min(self.s.len());
                    if is_cut || self.i >= self.s.len() {
                        break;
                    }
                }
                continue;
            }
            line_start = false;
            if c == '\n' {
                self.i += 1;
                line_start = true;
                self.spaced = true;
                self.newline = true;
                if !self.heredocs.is_empty() {
                    self.heredoc_bodies();
                }
                continue;
            }
            if c.is_whitespace() {
                self.i += c.len_utf8();
                self.spaced = true;
                continue;
            }
            if c == '#' {
                self.i += self.rest().find('\n').unwrap_or(self.rest().len());
                continue;
            }
            let spaced = std::mem::replace(&mut self.spaced, false);
            let newline = std::mem::replace(&mut self.newline, false);
            let expect_term = self.expect_term();
            let rest = self.rest();
            match c {
                '\'' | '"' | '`' => {
                    let word = match c {
                        '\'' => "q",
                        '"' => "qq",
                        _ => "qx",
                    };
                    let t = self.quote_like(word, 1);
                    self.out.push(t);
                }
                '$' | '@' => self.variable(c),
                '%' | '&' | '*'
                    if expect_term
                        && rest[1..].starts_with(|n: char| {
                            n == '$'
                                || n == '{'
                                || n == ':'
                                || n == '_'
                                || n.is_alphabetic()
                                || (c == '%' && matches!(n, '+' | '-' | '!'))
                                || (c == '&' && n == '$')
                        }) =>
                {
                    self.variable(c)
                }
                '/' if expect_term => {
                    let t = self.quote_like("m", 1);
                    self.out.push(t);
                }
                '<' if expect_term
                    && rest.starts_with("<<")
                    && !rest.starts_with("<<=")
                    && !rest.starts_with("<<>>") =>
                {
                    if !self.heredoc_start() {
                        self.i += 2;
                        self.out.push(Tok::new(Kind::Op, "<<"));
                    }
                }
                // `<<"EOF"`, `<<~EOF` and `<<EOF` are here-documents after anything.
                '<' if !expect_term
                    && rest.starts_with("<<")
                    && rest[2..]
                        .starts_with(|n: char| matches!(n, '"' | '\'' | '~' | '_') || n.is_ascii_alphabetic()) =>
                {
                    if !self.heredoc_start() {
                        self.i += 2;
                        self.out.push(Tok::new(Kind::Op, "<<"));
                    }
                }
                '<' if expect_term && {
                    let inner: String = rest[1..].chars().take_while(|&c| c != '>' && c != '\n' && c != ' ').collect();
                    rest[1 + inner.len()..].starts_with('>')
                        && inner.chars().all(|c| c.is_alphanumeric() || "$_:*.?/~-".contains(c))
                } =>
                {
                    // `<FH>`, `<$fh>`, `<*.c>`.
                    let end = rest.find('>').expect("checked") + 1;
                    self.out.push(Tok::new(Kind::Quote, format!("<>:{}", &rest[1..end - 1])));
                    self.i += end;
                }
                '0'..='9' => self.number(),
                '.' if rest[1..].starts_with(|c: char| c.is_ascii_digit()) && expect_term => self.number(),
                _ if c == '_' || c.is_alphabetic() => self.word(spaced, newline),
                _ => {
                    let op = OPERATORS.iter().find(|o| rest.starts_with(**o)).map_or(&rest[..c.len_utf8()], |o| *o);
                    self.i += op.len();
                    self.out.push(Tok::new(Kind::Op, op));
                }
            }
        }
        self.out
    }

    /// `<<"EOF"`, `<<'EOF'`, `<<~EOF`, `<<EOF`. Returns false for a shift.
    fn heredoc_start(&mut self) -> bool {
        let mut j = self.i + 2;
        let b = self.s.as_bytes();
        let indented = b.get(j) == Some(&b'~');
        if indented {
            j += 1;
        }
        while b.get(j) == Some(&b' ') {
            j += 1;
        }
        let (terminator, interpolates, end) = match b.get(j) {
            Some(q @ (b'"' | b'\'')) => {
                let Some(n) = self.s[j + 1..].find(*q as char) else { return false };
                (self.s[j + 1..j + 1 + n].to_string(), *q == b'"', j + 2 + n)
            }
            Some(c) if (c.is_ascii_alphabetic() || *c == b'_') && j == self.i + 2 + usize::from(indented) => {
                let name: String = self.s[j..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                let end = j + name.len();
                (name, true, end)
            }
            _ => return false,
        };
        self.i = end;
        self.heredocs.push(Heredoc { terminator, indented, interpolates, at: self.out.len() });
        self.out.push(Tok::new(Kind::Quote, "q:"));
        true
    }

    fn variable(&mut self, sigil: char) {
        let start = self.i;
        self.i += 1;
        let rest = self.rest();
        let name_len = |r: &str| {
            let mut n = 0;
            let b = r.as_bytes();
            loop {
                let len =
                    r[n..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum::<usize>();
                n += len;
                if b.get(n) == Some(&b':') && b.get(n + 1) == Some(&b':') {
                    n += 2;
                } else if b.get(n) == Some(&b'\'') && len > 0 && r[n + 1..].starts_with(|c: char| c.is_alphabetic()) {
                    // The old package separator: `$main'x`.
                    n += 1;
                } else {
                    break;
                }
            }
            n
        };
        if sigil == '$' && rest.starts_with('#') {
            // `$#array`, `$#{…}`, `$#$ref`, or the variable `$#`.
            self.i += 1 + name_len(&rest[1..]);
        } else if rest.starts_with("{^") {
            // `${^WARNING_BITS}`.
            self.i += rest.find('}').map_or(rest.len(), |n| n + 1);
        } else if rest.starts_with('^') && rest[1..].starts_with(|c: char| c.is_ascii_uppercase() || c == '_') {
            self.i += 2;
        } else {
            let n = name_len(rest);
            if n > 0 {
                self.i += n;
            } else if rest.starts_with(|c: char| c.is_ascii_digit()) {
                self.i += rest.chars().take_while(char::is_ascii_digit).count();
            } else if rest.starts_with(['{', '$']) || rest.is_empty() || rest.starts_with(char::is_whitespace) {
                // A dereference or block: the sigil alone.
            } else if sigil == '$' || (sigil == '@' && rest.starts_with(['-', '+', '_'])) {
                // Punctuation variables: `$_`, `$@`, `$/`, `$'`, `@-`.
                self.i += rest.chars().next().map_or(0, char::len_utf8);
            }
        }
        let text = &self.s[start..self.i];
        self.out.push(Tok::new(Kind::Var, text));
    }

    fn number(&mut self) {
        let start = self.i;
        let b = self.s.as_bytes();
        while self.i < b.len() {
            let c = b[self.i];
            // Digits, a decimal point (not `..`), an exponent's sign.
            let part = c.is_ascii_alphanumeric()
                || c == b'_'
                || (c == b'.' && b.get(self.i + 1).is_some_and(u8::is_ascii_digit))
                || (matches!(c, b'+' | b'-')
                    && matches!(b[self.i - 1], b'e' | b'E')
                    && !self.s[start..self.i].starts_with("0x"));
            if !part {
                break;
            }
            self.i += 1;
        }
        let text = self.s[start..self.i].replace('_', "").to_ascii_lowercase();
        self.out.push(Tok::new(Kind::Number, text));
    }

    fn word(&mut self, spaced: bool, newline: bool) {
        let start = self.i;
        let b = self.s.as_bytes();
        loop {
            let len =
                self.rest().chars().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum::<usize>();
            self.i += len;
            if b.get(self.i) == Some(&b':') && b.get(self.i + 1) == Some(&b':') {
                self.i += 2;
            } else {
                break;
            }
        }
        let word = &self.s[start..self.i];
        if matches!(word, "__END__" | "__DATA__") {
            let data = self.rest().split_once('\n').map_or("", |(_, d)| d);
            self.out.push(Tok::new(Kind::Word, word));
            self.out.push(Tok::new(Kind::Data, data));
            self.i = self.s.len();
            return;
        }
        if let Some(parts) = quote_parts(word) {
            let after = self.rest();
            let next = after.trim_start_matches([' ', '\t']).chars().next();
            let prev = self.out.last();
            // Not a quote: a hash key (`{s}`, `s =>`), a method (`->s`), a sub
            // name (`sub s`), a file test (`-s $f`).
            let not_quote = next.is_none_or(|n| n.is_alphanumeric() || n == '_' || n.is_whitespace())
                || after.trim_start().starts_with("=>")
                || (after.trim_start().starts_with('}') && prev.is_some_and(|p| p.is("{")))
                || prev.is_some_and(|p| p.is("->") || p.is("sub") || (p.is("-") && !spaced))
                || (after.starts_with([' ', '\t']) && next == Some('#'))
                || (next == Some('=') && after.trim_start_matches([' ', '\t']).starts_with("=="));
            if !not_quote {
                let t = self.quote_like(word, parts);
                self.out.push(t);
                return;
            }
        }
        // `format NAME =` … `.`
        if word == "format" && {
            let line = self.rest().split('\n').next().unwrap_or("");
            line.trim_end().ends_with('=')
                && line[..line.trim_end().len() - 1].trim().chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':')
        } {
            let header_end = self.rest().find('\n').map_or(self.s.len(), |n| self.i + n + 1);
            let name = self.s[self.i..header_end].trim().trim_end_matches('=').trim().to_string();
            self.i = header_end;
            let mut body = String::new();
            while self.i < self.s.len() {
                let line_end = self.rest().find('\n').map_or(self.s.len(), |n| self.i + n);
                let line = &self.s[self.i..line_end];
                self.i = (line_end + 1).min(self.s.len());
                if line.trim_end() == "." {
                    break;
                }
                body.push_str(line);
                body.push('\n');
            }
            self.out.push(Tok::new(Kind::Word, "format"));
            self.out.push(Tok::new(Kind::Word, name));
            self.out.push(Tok::new(Kind::Quote, format!("format:{body}")));
            return;
        }
        // `{word}` is the string "word", but not with a line break after the
        // brace: then perl calls the sub.
        if newline && self.out.last().is_some_and(|p| p.is("{")) && self.rest().trim_start().starts_with('}') {
            self.out.push(Tok::new(Kind::Word, format!("{word}()")));
            return;
        }
        self.out.push(Tok::new(Kind::Word, word));
    }
}

/// The tokens of Perl source, without comments, POD and layout.
pub fn tokens(text: &str) -> Vec<Tok> {
    Lexer { s: text, i: 0, out: Vec::new(), heredocs: Vec::new(), spaced: true, newline: true }.run()
}

/// Tokens without the separators a formatter adds or removes: a comma before
/// a closing bracket, a semicolon before a closing brace, repeated semicolons.
pub fn canonical(text: &str) -> Vec<Tok> {
    let toks = tokens(text);
    let mut out: Vec<Tok> = Vec::with_capacity(toks.len());
    for t in toks {
        if t.kind == Kind::Op && matches!(t.text.as_str(), ")" | "]" | "}") {
            while out.last().is_some_and(|p| p.is(",") || (t.text == "}" && p.is(";"))) {
                out.pop();
            }
        }
        if t.is(";") && out.last().is_some_and(|p| p.is(";")) {
            continue;
        }
        out.push(t);
    }
    out
}

/// Whether two Perl sources are the same code, apart from comments, POD and formatting.
pub fn same_code(left: &str, right: &str) -> bool {
    canonical(left) == canonical(right)
}

// ---- outline ----

fn display(t: &Tok) -> String {
    match t.kind {
        Kind::Quote => {
            let (kind, body) = t.text.split_once(':').unwrap_or(("", &t.text));
            let body = body.split('\0').next().unwrap_or("");
            let body: String = match body.split_once('\n') {
                Some((first, _)) => format!("{first}…"),
                None => body.to_string(),
            };
            match kind {
                "q" => format!("'{body}'"),
                "qq" => format!("\"{body}\""),
                "qw" => format!("qw({body})"),
                "<>" => format!("<{body}>"),
                _ => format!("{kind}{{{body}}}"),
            }
        }
        Kind::Data => String::new(),
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
                || p.is("->")
                || p.is("::")
                || p.is("\\")
                || (p.kind == Kind::Op && matches!(p.text.as_str(), "$" | "@" | "%"))
                || t.is(")")
                || t.is("]")
                || t.is("}")
                || t.is(",")
                || t.is(";")
                || t.is("->")
                || t.is("::")
                || ((t.is("(") || t.is("[") || t.is("{")) && matches!(p.kind, Kind::Word | Kind::Var))
                || ((t.is("[") || t.is("{")) && (p.is("}") || p.is("]")));
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

/// The index after the token that closes the bracket at `open`.
fn after_block(toks: &[Tok], open: usize) -> usize {
    let mut depth = 0usize;
    for (j, t) in toks.iter().enumerate().skip(open) {
        if t.kind != Kind::Op {
            continue;
        }
        match t.text.as_str() {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return j + 1;
                }
            }
            _ => {}
        }
    }
    toks.len()
}

/// The statements of a token range: up to a `;` outside brackets, or a block
/// that ends a compound statement (`sub f {…}`, `if (…) {…} else {…}`).
fn statements(toks: &[Tok]) -> Vec<&[Tok]> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if t.is(";") {
            out.push(&toks[start..=i]);
            i += 1;
            start = i;
            continue;
        }
        if t.is("(") || t.is("[") || t.is("{") {
            let end = after_block(toks, i);
            let first = &toks[start];
            let compound = t.is("{")
                && ([
                    "sub",
                    "BEGIN",
                    "END",
                    "INIT",
                    "CHECK",
                    "UNITCHECK",
                    "if",
                    "unless",
                    "while",
                    "until",
                    "for",
                    "foreach",
                    "package",
                    "else",
                    "elsif",
                    "do",
                    "eval",
                ]
                .iter()
                .any(|k| first.is(k))
                    || start == i);
            i = end;
            if compound
                && !first.is("do")
                && !first.is("eval")
                && toks.get(i).is_none_or(|n| !n.is("else") && !n.is("elsif") && !n.is("continue"))
            {
                out.push(&toks[start..i]);
                start = i;
            }
            continue;
        }
        if t.kind == Kind::Data {
            if i > start {
                out.push(&toks[start..i]);
            }
            out.push(&toks[i..=i]);
            i += 1;
            start = i;
            continue;
        }
        i += 1;
    }
    if start < toks.len() {
        out.push(&toks[start..]);
    }
    out
}

#[derive(Default)]
struct Package {
    name: String,
    imports: Vec<String>,
    variables: Vec<String>,
    subs: Vec<(String, String)>,
    other: Vec<Tok>,
    other_count: usize,
    data: Option<String>,
}

const LONG: usize = 100;

fn collect(toks: &[Tok], packages: &mut Vec<Package>, mut current: usize) {
    for stmt in statements(toks) {
        let first = &stmt[0];
        let pkg = |packages: &mut Vec<Package>, name: &str| -> usize {
            match packages.iter().position(|p| p.name == name) {
                Some(i) => i,
                None => {
                    packages.push(Package { name: name.to_string(), ..Default::default() });
                    packages.len() - 1
                }
            }
        };
        if first.kind == Kind::Data {
            let lines = first.text.lines().count();
            packages[current].data =
                Some(format!("# data {}, {lines} line{}", checksum(stmt), if lines == 1 { "" } else { "s" }));
            continue;
        }
        if first.is("__END__") || first.is("__DATA__") {
            continue;
        }
        if first.is("package") && stmt.len() >= 2 {
            let name = stmt[1].text.clone();
            match stmt.iter().position(|t| t.is("{")) {
                // `package Name { … }`
                Some(open) => {
                    let inner = pkg(packages, &name);
                    let close = after_block(stmt, open);
                    collect(&stmt[open + 1..close.saturating_sub(1)], packages, inner);
                }
                None => current = pkg(packages, &name),
            }
            continue;
        }
        let p = &mut packages[current];
        if first.is("use") || first.is("no") || first.is("require") {
            let text = render(stmt);
            let constant = stmt.get(1).is_some_and(|t| t.is("constant") || t.is("vars"));
            if constant {
                p.variables.push(if text.chars().count() > LONG {
                    format!("{} …  # value {}", render(&stmt[..2]), checksum(stmt))
                } else {
                    text
                });
            } else {
                p.imports.push(text);
            }
        } else if (first.is("sub") || ["BEGIN", "END", "INIT", "CHECK", "UNITCHECK"].iter().any(|k| first.is(k)))
            && stmt.last().is_some_and(|t| t.is("}"))
        {
            let open = stmt.iter().position(|t| t.is("{")).unwrap_or(stmt.len() - 1);
            let body = &stmt[open..];
            let (name, head) = if first.is("sub") {
                (stmt.get(1).map_or(String::new(), |t| t.text.clone()), render(&stmt[..open]))
            } else {
                (format!(" {}", first.text), first.text.clone())
            };
            p.subs.push((name, format!("{head}  # code {}, {} tokens", checksum(body), body.len())));
        } else if first.is("our")
            || (first.kind == Kind::Var
                && stmt.get(1).is_some_and(|t| t.is("="))
                && ["$VERSION", "@ISA", "@EXPORT", "@EXPORT_OK", "%EXPORT_TAGS"].contains(&first.text.as_str()))
        {
            let text = render(stmt);
            let eq = stmt.iter().position(|t| t.is("=")).unwrap_or(stmt.len());
            p.variables.push(if text.chars().count() > LONG {
                format!("{} = …  # value {}", render(&stmt[..eq]), checksum(&stmt[eq..]))
            } else {
                text
            });
        } else {
            p.other.extend(stmt.iter().cloned());
            p.other_count += 1;
        }
    }
}

/// The outline of a Perl script or module.
pub fn outline(text: &str) -> String {
    let toks = canonical(text);
    let mut packages = vec![Package { name: "main".into(), ..Default::default() }];
    collect(&toks, &mut packages, 0);
    let mut out = String::new();
    for p in packages {
        if p.name == "main"
            && p.imports.is_empty()
            && p.variables.is_empty()
            && p.subs.is_empty()
            && p.other_count == 0
            && p.data.is_none()
        {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = writeln!(out, "package {};", p.name);
        let mut imports = p.imports;
        imports.sort();
        imports.dedup();
        for l in imports.iter().chain(&p.variables) {
            let _ = writeln!(out, "{l}");
        }
        if p.other_count > 0 {
            let n = p.other_count;
            let _ =
                writeln!(out, "# other code {}, {n} statement{}", checksum(&p.other), if n == 1 { "" } else { "s" });
        }
        let mut subs = p.subs;
        subs.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, line) in subs {
            let _ = writeln!(out, "{line}");
        }
        if let Some(d) = p.data {
            let _ = writeln!(out, "{d}");
        }
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
    fn lexes_perl_quoting() {
        assert_eq!(texts("my $x = $y / 2; # half\n"), ["my", "$x", "=", "$y", "/", "2", ";"]);
        assert_eq!(texts("split /#/, $s;"), ["split", "m:#\0", ",", "$s", ";"]);
        assert_eq!(texts("$x =~ m,a / $,x; s;a;b;;"), ["$x", "=~", "m:a / $\0x", ";", "s:a\0b\0", ";"]);
        assert_eq!(texts("eval <<TRY;\nmy %h; # x\nTRY\n"), ["eval", "q:my %h; # x\n", ";"]);
        // A stray `=cut` starts POD that runs to the next one.
        assert_eq!(texts("a;\n=cut\nb;\n=cut\nc;\n"), ["a", ";", "c", ";"]);
        assert_eq!(texts("$s =~ s{a#b}\n {c}gx;"), ["$s", "=~", "s:a#b\0c\0gx", ";"]);
        assert_eq!(
            texts("tr/a-z/A-Z/;"),
            ["tr:a-z\0A-Z\0"].into_iter().map(String::from).chain([";".to_string()]).collect::<Vec<_>>()
        );
        assert_eq!(texts("print qw( a  b\n c );"), ["print", "qw:a b c", ";"]);
        assert_eq!(
            texts("my %h = (s => 1, y => 2); $h{s}; -s $f;"),
            [
                "my", "%h", "=", "(", "s", "=>", "1", ",", "y", "=>", "2", ")", ";", "$h", "{", "s", "}", ";", "-",
                "s", "$f", ";"
            ]
        );
        assert_eq!(
            texts("$#a; $#{$r}; $'; $x % 2; %$r;"),
            ["$#a", ";", "$#", "{", "$r", "}", ";", "$'", ";", "$x", "%", "2", ";", "%", "$r", ";"]
        );
        // Here-documents, also two on a line, and `<<` as a shift.
        let h = texts("print <<EOT, <<~'X';\nline # not a comment\nEOT\n    raw $x\n    X\n$a << 2;\n");
        assert_eq!(h, ["print", "q:line # not a comment\n", ",", "q:raw $x\n", ";", "$a", "<<", "2", ";"]);
        // POD and __END__.
        let p = texts("my $a;\n\n=head1 NAME\n\nx # y\n\n=cut\n\nmy $b;\n__END__\nfree text\n");
        assert_eq!(p, ["my", "$a", ";", "my", "$b", ";", "__END__", "free text\n"]);
        assert_eq!(texts("while (<$fh>) { }"), ["while", "(", "<>:$fh", ")", "{", "}"]);
    }

    #[test]
    fn ignores_comments_pod_and_formatting() {
        let a = "use strict;\n# setup\nmy %h = ( a => 1, b => 2, );\nsub f {\n    my ($x) = @_;\n    return \"x\";\n}\n\n=pod\n\nDocs.\n\n=cut\n";
        let b = "use strict;\nmy %h = (a=>1,b=>2);\nsub f { my ( $x ) = @_; return 'x' }\n";
        assert!(same_code(a, b));
        assert!(!same_code("my $x = \"$y\";", "my $x = '$y';"));
        assert!(!same_code("my $x = 'a#b';", "my $x = 'a';"));
        assert!(!same_code("$x = $a - -$b;", "$x = $a --$b;"));
        assert!(!same_code("s/a/b/g;", "s/a/b/;"));
        assert!(!same_code("exists $h->{key};", "exists $h->{\nkey};"));
        assert!(same_code("sub f { return 1 }", "sub f {\n    return 1;\n}"));
    }

    #[test]
    fn outlines_a_module() {
        let src = "package Shop::Cart;\nuse strict;\nuse warnings;\nuse parent 'Shop::Base';\nour $VERSION = '1.02';\nuse constant TAX => 0.19;\n\nsub new {\n    my ($class, %args) = @_;\n    return bless {%args}, $class;\n}\n\nsub total ($self, $qty = 1) {\n    return $self->{price} * $qty;\n}\n\n1;\n__END__\n\n=head1 NAME\n";
        let o = outline(src);
        assert!(o.starts_with("package Shop::Cart;\nuse parent 'Shop::Base';\nuse strict;\nuse warnings;\nour $VERSION = '1.02';\nuse constant TAX => 0.19;\n# other code "), "{o}");
        assert!(o.contains("\nsub new  # code "), "{o}");
        assert!(o.contains("\nsub total($self, $qty = 1)  # code "), "{o}");
        assert!(o.contains("\n# data "), "{o}");
    }
}
