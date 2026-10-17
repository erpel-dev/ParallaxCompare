//! DBC files (Vector CAN database): the messages and signals of a CAN bus.
//!
//! A DBC is a text file of statements: `BO_` messages with their `SG_` signals,
//! and, spread over the rest of the file, everything that belongs to them: `CM_`
//! comments, `BA_` attribute values (with `BA_DEF_` definitions and `BA_DEF_DEF_`
//! defaults), `VAL_` value descriptions, `SIG_VALTYPE_` float types, signal groups
//! and extended multiplexing. Tools write these sections in different orders and
//! reformat numbers, so a plain text diff of two exports is noisy.
//!
//! The "CAN database" view gathers each message with everything that refers to it,
//! sorts messages by id and signals by multiplexer value and start bit, resolves
//! enum attributes to their labels, and shows one fact per line. The parser is a
//! hand-written, tolerant tokenizer: real-world files have vendor statements,
//! missing semicolons and odd spacing that strict grammars (such as the `can-dbc`
//! crate) reject as a whole. Statements it does not understand are kept, with
//! normalized spacing, in an "Other statements" section, so the view never hides a
//! difference.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::{Error, Result};

/// Signals and messages are read in full, but a corrupt or generated file with
/// millions of statements is cut off at this many output lines.
const MAX_LINES: usize = 2_000_000;

/// Pseudo message that holds signals not sent by any message.
const INDEPENDENT_SIGNALS: u32 = 0xC000_0000;

/// The "CAN database" view.
pub fn summary(text: &str) -> Result<String> {
    let db = Dbc::parse(text);
    if db.messages.is_empty() && db.nodes.is_empty() && db.version.is_none() {
        return Err(Error::Unsupported("DBC: no messages, nodes or version found".into()));
    }
    Ok(db.render())
}

/// Whether two DBC files hold the same database, only in another order or with
/// other formatting (for the ≈ similar status).
pub fn same_database(left: &str, right: &str) -> bool {
    let (l, r) = (Dbc::parse(left), Dbc::parse(right));
    l.messages.len() == r.messages.len() && l.render() == r.render()
}

// ---- Tokenizer ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tk {
    Word,
    Str,
    Punct,
}

#[derive(Debug, Clone, Copy)]
struct Tok<'a> {
    text: &'a str,
    kind: Tk,
    /// First token on its line.
    line_start: bool,
}

impl Tok<'_> {
    fn is(&self, p: &str) -> bool {
        self.kind != Tk::Str && self.text == p
    }

    /// The text of a string token without quotes and escapes.
    fn string(&self) -> String {
        if self.kind != Tk::Str {
            return self.text.to_string();
        }
        let inner = &self.text[1..self.text.len() - usize::from(self.text.len() > 1 && self.text.ends_with('"'))];
        if !inner.contains('\\') {
            return inner.to_string();
        }
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some(n @ ('"' | '\\')) => out.push(n),
                    Some(n) => {
                        out.push('\\');
                        out.push(n);
                    }
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    }
}

fn tokenize(text: &str) -> Vec<Tok<'_>> {
    let b = text.as_bytes();
    let mut toks = Vec::with_capacity(text.len() / 6);
    let mut i = 0;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        let kind = if c == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' && i + 1 < b.len() { 2 } else { 1 };
            }
            i = (i + 1).min(b.len());
            Tk::Str
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            // Not part of the format, but some tools write comment lines.
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        } else if c.is_ascii_alphanumeric()
            || c == b'_'
            || c == b'.'
            || (matches!(c, b'-' | b'+') && starts_number(b, i + 1))
        {
            i += 1;
            while i < b.len() {
                let d = b[i];
                let exponent_sign =
                    matches!(d, b'-' | b'+') && matches!(b[i - 1], b'e' | b'E') && starts_number(b, start);
                if d.is_ascii_alphanumeric() || d == b'_' || d == b'.' || exponent_sign || d >= 0x80 {
                    i += 1;
                } else {
                    break;
                }
            }
            Tk::Word
        } else if c >= 0x80 {
            // A non-ASCII identifier (not allowed, but seen); take the whole character run.
            while i < b.len() && !b[i].is_ascii_whitespace() && !b"\":;|@()[],".contains(&b[i]) {
                i += 1;
            }
            Tk::Word
        } else {
            i += 1;
            Tk::Punct
        };
        // Multi-byte characters only appear inside the runs above, so the cut is on a boundary.
        toks.push(Tok { text: &text[start..i], kind, line_start });
        line_start = false;
    }
    toks
}

fn starts_number(b: &[u8], i: usize) -> bool {
    match b.get(i) {
        Some(c) if c.is_ascii_digit() => true,
        Some(b'.') => b.get(i + 1).is_some_and(u8::is_ascii_digit),
        Some(b'-' | b'+') => b.get(i + 1).is_some_and(|c| c.is_ascii_digit() || *c == b'.'),
        _ => false,
    }
}

/// Statements that start a new section of the file when they begin a line.
const KEYWORDS: &[&str] = &[
    "VERSION",
    "NS_",
    "BS_",
    "BU_",
    "VAL_TABLE_",
    "BO_",
    "SG_",
    "BO_TX_BU_",
    "EV_",
    "ENVVAR_DATA_",
    "SGTYPE_",
    "SGTYPE_VAL_",
    "SIG_TYPE_REF_",
    "CM_",
    "BA_DEF_",
    "BA_DEF_DEF_",
    "BA_DEF_REL_",
    "BA_DEF_DEF_REL_",
    "BA_DEF_SGTYPE_",
    "BA_SGTYPE_",
    "BA_",
    "BA_REL_",
    "VAL_",
    "CAT_DEF_",
    "CAT_",
    "FILTER",
    "SIG_GROUP_",
    "SIG_VALTYPE_",
    "SIGTYPE_VALTYPE_",
    "SG_MUL_VAL_",
];

fn is_keyword(t: &Tok) -> bool {
    t.kind == Tk::Word && t.line_start && KEYWORDS.contains(&t.text)
}

/// Splits the tokens into statements: each starts with a keyword at the start of a
/// line and ends with `;` or the next statement. `NS_` lists keyword names on
/// their own lines, so it runs up to `BS_`.
fn statements<'t, 'a>(toks: &'t [Tok<'a>]) -> Vec<&'t [Tok<'a>]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if !is_keyword(&toks[i]) {
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        if toks[start].text == "NS_" {
            while i < toks.len() && !(toks[i].is("BS_") && toks[i].line_start) {
                i += 1;
            }
            out.push(&toks[start..i]);
            continue;
        }
        while i < toks.len() && !is_keyword(&toks[i]) {
            if toks[i].is(";") {
                i += 1;
                break;
            }
            i += 1;
        }
        out.push(&toks[start..i]);
    }
    out
}

// ---- Model ----

#[derive(Default)]
struct Dbc {
    version: Option<String>,
    baudrate: Option<String>,
    nodes: Vec<String>,
    node_info: BTreeMap<String, Info>,
    value_tables: BTreeMap<String, Vec<(String, String)>>,
    messages: BTreeMap<u32, Message>,
    env_vars: BTreeMap<String, EnvVar>,
    network: Info,
    attr_defs: BTreeMap<String, AttrDef>,
    /// Statements kept as normalized text.
    other: Vec<String>,
}

/// Comment and attribute values of an object.
#[derive(Default, Clone)]
struct Info {
    comment: Option<String>,
    /// Attribute values as written, resolved when rendered.
    attrs: BTreeMap<String, String>,
}

#[derive(Default)]
struct Message {
    name: String,
    dlc: String,
    sender: String,
    transmitters: Vec<String>,
    signals: Vec<Signal>,
    signal_groups: Vec<String>,
    info: Info,
}

#[derive(Default)]
struct Signal {
    name: String,
    /// `M`, `m3` or `m3M` as written.
    mux: String,
    start: u32,
    length: String,
    little_endian: bool,
    signed: bool,
    factor: String,
    offset: String,
    min: String,
    max: String,
    unit: String,
    receivers: Vec<String>,
    float_type: Option<&'static str>,
    values: Vec<(String, String)>,
    /// Extended multiplexing: (multiplexer, value ranges).
    mux_ranges: Vec<String>,
    info: Info,
}

impl Signal {
    /// Sort key: plain signals and multiplexers first, then multiplexed ones by value.
    fn order(&self) -> (u8, i64, u32) {
        let value = self.mux.strip_prefix('m').map(|r| r.trim_end_matches('M')).and_then(|v| v.parse().ok());
        match (self.mux.as_str(), value) {
            ("M", _) => (0, 0, self.start),
            (_, Some(v)) => (1, v, self.start),
            _ => (0, 0, self.start),
        }
    }
}

#[derive(Default)]
struct EnvVar {
    fields: Vec<(&'static str, String)>,
    values: Vec<(String, String)>,
    info: Info,
}

struct AttrDef {
    object: &'static str,
    kind: String,
    /// Enum labels, for resolving values.
    labels: Vec<String>,
    default: Option<String>,
}

/// A number as a person writes it: `1.000` and `1E+000` are both `1`.
fn num(s: &str) -> String {
    match s.parse::<f64>() {
        Ok(v) if v.is_finite() && !s.starts_with("0x") => {
            let r = format!("{v}");
            if r == "-0" { "0".into() } else { r }
        }
        _ => s.to_string(),
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => {}
            '\t' => out.push_str("\\t"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Value descriptions: `n "text"` pairs up to the end of the statement.
fn value_pairs(toks: &[Tok]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < toks.len() {
        if toks[i].kind == Tk::Word && toks[i + 1].kind == Tk::Str {
            out.push((num(toks[i].text), toks[i + 1].string()));
            i += 2;
        } else {
            i += 1;
        }
    }
    out.sort_by(|a, b| {
        let (x, y) = (a.0.parse::<f64>().ok(), b.0.parse::<f64>().ok());
        x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0))
    });
    out
}

fn normalized(stmt: &[Tok]) -> String {
    let mut s = String::new();
    for (i, t) in stmt.iter().enumerate() {
        if i > 0 && !t.is(";") && !t.is(",") {
            s.push(' ');
        }
        s.push_str(t.text);
    }
    s.replace('\n', "\\n")
}

impl Dbc {
    fn parse(text: &str) -> Dbc {
        let toks = tokenize(text);
        let mut db = Dbc::default();
        let mut current: Option<u32> = None;
        // Attribute values refer to definitions that may come later in the file.
        let mut attr_values = Vec::new();
        for stmt in statements(&toks) {
            let w = |i: usize| stmt.get(i).map_or("", |t| t.text);
            let ok = match stmt[0].text {
                "VERSION" => {
                    db.version = stmt.get(1).map(Tok::string);
                    true
                }
                "NS_" => true,
                "BS_" => {
                    let rest: Vec<&str> = stmt[1..].iter().filter(|t| !t.is(":")).map(|t| t.text).collect();
                    if !rest.is_empty() {
                        db.baudrate = Some(rest.join(" "));
                    }
                    true
                }
                "BU_" => {
                    db.nodes.extend(stmt[1..].iter().filter(|t| t.kind == Tk::Word).map(|t| t.text.to_string()));
                    true
                }
                "VAL_TABLE_" if stmt.len() > 1 => {
                    db.value_tables.insert(w(1).to_string(), value_pairs(&stmt[2..]));
                    true
                }
                "BO_" => {
                    current = db.message(stmt);
                    current.is_some()
                }
                "SG_" => match current.and_then(|id| db.messages.get_mut(&id)) {
                    Some(m) => parse_signal(stmt).map(|s| m.signals.push(s)).is_some(),
                    None => false,
                },
                "BO_TX_BU_" => match w(1).parse().ok().and_then(|id: u32| db.messages.get_mut(&id)) {
                    Some(m) => {
                        let names = stmt[2..].iter().filter(|t| t.kind == Tk::Word).map(|t| t.text.to_string());
                        m.transmitters.extend(names);
                        true
                    }
                    None => false,
                },
                "CM_" => db.comment(stmt),
                "BA_DEF_" => db.attr_def(stmt),
                "BA_DEF_DEF_" => match (stmt.get(1), stmt.get(2)) {
                    (Some(name), Some(value)) => match db.attr_defs.get_mut(&name.string()) {
                        Some(def) => {
                            def.default =
                                Some(if value.kind == Tk::Str { quote(&value.string()) } else { num(value.text) });
                            true
                        }
                        None => false,
                    },
                    _ => false,
                },
                "BA_" => {
                    attr_values.push(stmt);
                    true
                }
                "VAL_" => db.values(stmt),
                "SIG_VALTYPE_" => {
                    let kind = match stmt.iter().rev().find(|t| t.kind == Tk::Word).map(|t| t.text) {
                        Some("1") => Some("float (32 bit)"),
                        Some("2") => Some("double (64 bit)"),
                        _ => None,
                    };
                    match db.signal_mut(w(1), w(2)) {
                        Some(s) => {
                            s.float_type = kind;
                            true
                        }
                        None => false,
                    }
                }
                "SG_MUL_VAL_" => {
                    let ranges: Vec<&str> = stmt[4..].iter().filter(|t| !t.is(";")).map(|t| t.text).collect();
                    let text = format!("{} = {}", w(3), ranges.join(" ").replace(" , ", ", ").replace(" ,", ","));
                    match db.signal_mut(w(1), w(2)) {
                        Some(s) => {
                            s.mux_ranges.push(text);
                            true
                        }
                        None => false,
                    }
                }
                "SIG_GROUP_" => match w(1).parse().ok().and_then(|id: u32| db.messages.get_mut(&id)) {
                    Some(m) => {
                        let sigs: Vec<&str> =
                            stmt.iter().skip(5).filter(|t| t.kind == Tk::Word).map(|t| t.text).collect();
                        m.signal_groups.push(format!("{} (repetitions {}): {}", w(2), w(3), sigs.join(", ")));
                        true
                    }
                    None => false,
                },
                "EV_" => db.env_var(stmt),
                "ENVVAR_DATA_" => match db.env_vars.get_mut(w(1)) {
                    Some(e) => {
                        e.fields.push(("Data size", w(3).to_string()));
                        true
                    }
                    None => false,
                },
                _ => false,
            };
            if !ok {
                db.other.push(normalized(stmt));
            }
        }
        for stmt in attr_values {
            if !db.attr_value(stmt) {
                db.other.push(normalized(stmt));
            }
        }
        db.nodes.sort();
        db.nodes.dedup();
        for m in db.messages.values_mut() {
            m.signals.sort_by(|a, b| a.order().cmp(&b.order()).then_with(|| a.name.cmp(&b.name)));
            m.signal_groups.sort();
            m.transmitters.sort();
        }
        db.other.sort();
        db
    }

    /// `BO_ 100 Name: 8 Sender`
    fn message(&mut self, stmt: &[Tok]) -> Option<u32> {
        let id: u32 = stmt.get(1)?.text.parse().ok()?;
        let name = stmt.get(2)?.text.to_string();
        let rest: Vec<&str> = stmt[3..].iter().filter(|t| !t.is(":")).map(|t| t.text).collect();
        let m = self.messages.entry(id).or_default();
        m.name = name;
        m.dlc = rest.first().map(|s| s.to_string()).unwrap_or_default();
        m.sender = rest.get(1).map(|s| s.to_string()).unwrap_or_default();
        Some(id)
    }

    fn signal_mut(&mut self, id: &str, name: &str) -> Option<&mut Signal> {
        let id: u32 = id.parse().ok()?;
        self.messages.get_mut(&id)?.signals.iter_mut().find(|s| s.name == name)
    }

    /// `CM_ [BU_ node | BO_ id | SG_ id sig | EV_ name] "text";`
    fn comment(&mut self, stmt: &[Tok]) -> bool {
        let Some(text) = stmt.iter().find(|t| t.kind == Tk::Str).map(Tok::string) else { return false };
        let w = |i: usize| stmt.get(i).map_or("", |t| t.text);
        let info = match w(1) {
            "BU_" => Some(self.node_info.entry(w(2).to_string()).or_default()),
            "BO_" => w(2).parse().ok().and_then(|id: u32| self.messages.get_mut(&id)).map(|m| &mut m.info),
            "SG_" => self.signal_mut(w(2), w(3)).map(|s| &mut s.info),
            "EV_" => self.env_vars.get_mut(w(2)).map(|e| &mut e.info),
            _ if stmt[1].kind == Tk::Str => Some(&mut self.network),
            _ => None,
        };
        match info {
            Some(info) => {
                info.comment = Some(text);
                true
            }
            None => false,
        }
    }

    /// `BA_DEF_ [BU_|BO_|SG_|EV_] "Name" INT 0 100;`
    fn attr_def(&mut self, stmt: &[Tok]) -> bool {
        let (object, rest) = match stmt.get(1).map(|t| t.text) {
            Some("BU_") => ("node", &stmt[2..]),
            Some("BO_") => ("message", &stmt[2..]),
            Some("SG_") => ("signal", &stmt[2..]),
            Some("EV_") => ("environment variable", &stmt[2..]),
            _ => ("network", &stmt[1..]),
        };
        let Some(name) = rest.first().filter(|t| t.kind == Tk::Str) else { return false };
        let ty = rest.get(1).map_or("", |t| t.text);
        let args: Vec<String> = rest
            .iter()
            .skip(2)
            .filter(|t| !t.is(";") && !t.is(","))
            .map(|t| if ty == "ENUM" { t.string() } else { num(t.text) })
            .collect();
        let kind = match ty {
            "ENUM" => format!("ENUM {}", args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(", ")),
            _ if args.is_empty() => ty.to_string(),
            _ => format!("{ty} {}", args.join(" .. ")),
        };
        let labels = if ty == "ENUM" { args } else { Vec::new() };
        self.attr_defs.insert(name.string(), AttrDef { object, kind, labels, default: None });
        true
    }

    /// `BA_ "Name" [BU_ node | BO_ id | SG_ id sig | EV_ name] value;`
    fn attr_value(&mut self, stmt: &[Tok]) -> bool {
        let Some(name) = stmt.get(1).filter(|t| t.kind == Tk::Str).map(Tok::string) else { return false };
        let w = |i: usize| stmt.get(i).map_or("", |t| t.text);
        let (info, value) = match w(2) {
            "BU_" => (Some(self.node_info.entry(w(3).to_string()).or_default()), stmt.get(4)),
            "BO_" => {
                (w(3).parse().ok().and_then(|id: u32| self.messages.get_mut(&id)).map(|m| &mut m.info), stmt.get(4))
            }
            "SG_" => (self.signal_mut(w(3), w(4)).map(|s| &mut s.info), stmt.get(5)),
            "EV_" => (self.env_vars.get_mut(w(3)).map(|e| &mut e.info), stmt.get(4)),
            _ => (Some(&mut self.network), stmt.get(2)),
        };
        match (info, value.filter(|v| !v.is(";"))) {
            (Some(info), Some(v)) => {
                info.attrs.insert(name, if v.kind == Tk::Str { quote(&v.string()) } else { num(v.text) });
                true
            }
            _ => false,
        }
    }

    /// `VAL_ id sig n "text" …;` or `VAL_ envvar n "text" …;`
    fn values(&mut self, stmt: &[Tok]) -> bool {
        let w = |i: usize| stmt.get(i).map_or("", |t| t.text);
        if let Ok(id) = w(1).parse::<u32>()
            && stmt.get(2).is_some_and(|t| t.kind == Tk::Word && t.text.parse::<f64>().is_err())
        {
            let pairs = value_pairs(&stmt[3..]);
            return self.signal_mut(&id.to_string(), w(2)).map(|s| s.values = pairs).is_some();
        }
        let pairs = value_pairs(&stmt[2..]);
        self.env_vars.get_mut(w(1)).map(|e| e.values = pairs).is_some()
    }

    /// `EV_ name : type [min|max] "unit" initial id DUMMY_NODE_VECTOR0 nodes;`
    fn env_var(&mut self, stmt: &[Tok]) -> bool {
        let words: Vec<&Tok> =
            stmt[1..].iter().filter(|t| !matches!(t.text, ":" | "[" | "]" | "|" | ";" | ",")).collect();
        if words.len() < 8 {
            return false;
        }
        let kind = match words[1].text {
            "0" => "integer",
            "1" => "float",
            "2" => "string",
            _ => words[1].text,
        };
        let e = self.env_vars.entry(words[0].text.to_string()).or_default();
        e.fields = vec![
            ("Type", kind.to_string()),
            ("Range", format!("{} .. {}", num(words[2].text), num(words[3].text))),
            ("Unit", quote(&words[4].string())),
            ("Initial value", num(words[5].text)),
            ("Access", words[7].text.to_string()),
        ];
        let nodes: Vec<&str> = words[8..].iter().map(|t| t.text).filter(|n| *n != "Vector__XXX").collect();
        if !nodes.is_empty() {
            e.fields.push(("Access nodes", nodes.join(", ")));
        }
        true
    }

    /// An attribute value as a person reads it: enum indexes become their label.
    fn attr_text(&self, name: &str, value: &str) -> String {
        match self.attr_defs.get(name) {
            Some(def) if !def.labels.is_empty() => match value.parse::<usize>().ok().and_then(|i| def.labels.get(i)) {
                Some(label) => quote(label),
                None => value.to_string(),
            },
            _ => value.to_string(),
        }
    }

    fn render(&self) -> String {
        let mut out = String::with_capacity(self.messages.len() * 600 + 1024);
        if let Some(v) = &self.version {
            line(&mut out, 0, "Version", quote(v));
        }
        if let Some(b) = &self.baudrate {
            line(&mut out, 0, "Bit timing", b);
        }
        line(&mut out, 0, "Messages", self.messages.len());
        let signals: usize = self.messages.values().map(|m| m.signals.len()).sum();
        line(&mut out, 0, "Signals", signals);
        self.info(&mut out, 0, &self.network);

        let _ = writeln!(out, "\n=== Nodes ({}) ===", self.nodes.len());
        for node in &self.nodes {
            let _ = writeln!(out, "{node}");
            if let Some(info) = self.node_info.get(node) {
                self.info(&mut out, 2, info);
            }
        }
        for (node, info) in self.node_info.iter().filter(|(n, _)| !self.nodes.contains(n)) {
            let _ = writeln!(out, "{node}  // not in BU_");
            self.info(&mut out, 2, info);
        }

        if !self.value_tables.is_empty() {
            let _ = writeln!(out, "\n=== Value tables ({}) ===", self.value_tables.len());
            for (name, values) in &self.value_tables {
                let _ = writeln!(out, "{name}");
                for (v, text) in values {
                    let _ = writeln!(out, "  {v} = {}", quote(text));
                }
            }
        }

        let _ = writeln!(out, "\n=== Messages ({}) ===", self.messages.len());
        for (&id, m) in &self.messages {
            if out.len() / 40 > MAX_LINES {
                let _ = writeln!(out, "\n! Output cut off: the file has more messages than can be shown");
                break;
            }
            self.render_message(&mut out, id, m);
        }

        if !self.env_vars.is_empty() {
            let _ = writeln!(out, "\n=== Environment variables ({}) ===", self.env_vars.len());
            for (name, e) in &self.env_vars {
                let _ = writeln!(out, "\n--- {name} ---");
                for (label, value) in &e.fields {
                    line(&mut out, 0, label, value);
                }
                for (v, text) in &e.values {
                    line(&mut out, 0, &format!("Value {v}"), quote(text));
                }
                self.info(&mut out, 0, &e.info);
            }
        }

        if !self.attr_defs.is_empty() {
            let _ = writeln!(out, "\n=== Attribute definitions ({}) ===", self.attr_defs.len());
            for (name, def) in &self.attr_defs {
                let default = def
                    .default
                    .as_deref()
                    .map(|d| format!(", default {}", self.attr_text(name, d)))
                    .unwrap_or_default();
                let _ = writeln!(out, "{name}: {} {}{default}", def.object, def.kind);
            }
        }

        if !self.other.is_empty() {
            let _ = writeln!(out, "\n=== Other statements ({}) ===", self.other.len());
            for s in &self.other {
                let _ = writeln!(out, "{s}");
            }
        }
        out
    }

    fn render_message(&self, out: &mut String, id: u32, m: &Message) {
        let (raw, extended) = (id & 0x1FFF_FFFF, id & 0x8000_0000 != 0);
        if id == INDEPENDENT_SIGNALS {
            let _ = writeln!(out, "\n--- {} (signals not in any message) ---", m.name);
        } else if extended {
            let _ = writeln!(out, "\n--- 0x{raw:08X} {} ---", m.name);
        } else {
            let _ = writeln!(out, "\n--- 0x{raw:03X} {} ---", m.name);
        }
        if id != INDEPENDENT_SIGNALS {
            let kind = if extended { "extended (29 bit)" } else { "standard (11 bit)" };
            line(out, 0, "Id", format!("{raw}, {kind}"));
        }
        line(out, 0, "DLC", &m.dlc);
        if m.sender != "Vector__XXX" {
            line(out, 0, "Sender", &m.sender);
        }
        if !m.transmitters.is_empty() {
            line(out, 0, "Transmitters", m.transmitters.join(", "));
        }
        let cycle = m.info.attrs.get("GenMsgCycleTime").map(|v| (v.clone(), ""));
        let cycle = cycle.or_else(|| {
            let d = self.attr_defs.get("GenMsgCycleTime")?.default.clone()?;
            (d != "0").then_some((d, " (default)"))
        });
        if let Some((ms, how)) = cycle {
            line(out, 0, "Cycle time", format!("{ms} ms{how}"));
        }
        let mut info = m.info.clone();
        info.attrs.remove("GenMsgCycleTime");
        self.info(out, 0, &info);
        for g in &m.signal_groups {
            line(out, 0, "Signal group", g);
        }
        for s in &m.signals {
            self.render_signal(out, s);
        }
    }

    fn render_signal(&self, out: &mut String, s: &Signal) {
        let _ = writeln!(out, "  Signal {}", s.name);
        let order = if s.little_endian { "little-endian (Intel)" } else { "big-endian (Motorola)" };
        let sign = s.float_type.unwrap_or(if s.signed { "signed" } else { "unsigned" });
        let bits = if s.length == "1" { "bit" } else { "bits" };
        line(out, 4, "Layout", format!("start bit {}, {} {bits}, {order}, {sign}", s.start, s.length));
        match s.mux.as_str() {
            "" => {}
            "M" => line(out, 4, "Multiplexer", "yes (multiplexor switch)"),
            m => {
                let value = m.trim_start_matches('m').trim_end_matches('M');
                line(out, 4, "Multiplexed", format!("when multiplexer = {value}"));
                if m.ends_with('M') {
                    line(out, 4, "Multiplexer", "yes (extended multiplexing)");
                }
            }
        }
        for r in &s.mux_ranges {
            line(out, 4, "Multiplexed by", r);
        }
        line(out, 4, "Scaling", format!("factor {}, offset {}", s.factor, s.offset));
        line(out, 4, "Range", format!("{} .. {}", s.min, s.max));
        if !s.unit.is_empty() {
            line(out, 4, "Unit", quote(&s.unit));
        }
        let receivers: Vec<&str> = s.receivers.iter().map(String::as_str).filter(|r| *r != "Vector__XXX").collect();
        if !receivers.is_empty() {
            line(out, 4, "Receivers", receivers.join(", "));
        }
        self.info(out, 4, &s.info);
        for (v, text) in &s.values {
            line(out, 4, &format!("Value {v}"), quote(text));
        }
    }

    fn info(&self, out: &mut String, indent: usize, info: &Info) {
        if let Some(c) = &info.comment {
            line(out, indent, "Comment", quote(c));
        }
        for (name, value) in &info.attrs {
            line(out, indent, &format!("Attribute {name}"), self.attr_text(name, value));
        }
    }
}

/// `SG_ Name [M|m3] : 0|8@1+ (1,0) [0|255] "unit" Rx1,Rx2`
fn parse_signal(stmt: &[Tok]) -> Option<Signal> {
    let mut s = Signal { name: stmt.get(1)?.text.to_string(), ..Signal::default() };
    let mut i = 2;
    if !stmt.get(i)?.is(":") {
        s.mux = stmt[i].text.to_string();
        i += 1;
    }
    if !stmt.get(i)?.is(":") {
        return None;
    }
    // Number tokens include a trailing sign only as an exponent, so `@1+` is `@`, `1`, `+`.
    let t = |k: usize| stmt.get(i + k).map_or("", |t| t.text);
    s.start = t(1).parse().ok()?;
    if t(2) != "|" || t(4) != "@" {
        return None;
    }
    s.length = t(3).to_string();
    // `@1+` may also come as the word `1` and the punctuation `+`, or as `1+` when followed by a digit.
    s.little_endian = t(5).starts_with('1');
    let sign = if t(5).len() > 1 { &t(5)[1..] } else { t(6) };
    s.signed = sign == "-";
    i += if t(5).len() > 1 { 6 } else { 7 };
    let rest = &stmt[i..];
    let nums: Vec<&str> =
        rest.iter().take_while(|t| t.kind != Tk::Str).filter(|t| t.kind == Tk::Word).map(|t| t.text).collect();
    let n = |k: usize| nums.get(k).map(|v| num(v)).unwrap_or_default();
    s.factor = n(0);
    s.offset = n(1);
    s.min = n(2);
    s.max = n(3);
    let unit_at = rest.iter().position(|t| t.kind == Tk::Str);
    if let Some(u) = unit_at {
        s.unit = rest[u].string();
        s.receivers = rest[u + 1..].iter().filter(|t| t.kind == Tk::Word).map(|t| t.text.to_string()).collect();
    }
    Some(s)
}

fn line(out: &mut String, indent: usize, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:indent$}{:<18} {value}", "", format!("{label}:"));
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dbc/", $name))
        };
    }
    const V1: &str = fixture!("powertrain-v1.dbc");
    const V1_REORDERED: &str = fixture!("powertrain-v1-reordered.dbc");
    const V2: &str = fixture!("powertrain-v2.dbc");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn messages_and_signals() {
        let s = summary(V1).unwrap();
        for expected in [
            "Version:           \"1.4\"",
            "\n--- 0x0C8 EngineData ---\nId:                200, standard (11 bit)\nDLC:               8\nSender:            Engine\nCycle time:        10 ms\nComment:           \"Engine state, sent every 10 ms\"\nAttribute GenMsgSendType: \"Cyclic\"\n",
            "--- 0x18FEF100 CruiseControl ---\nId:                419361024, extended (29 bit)",
            "  Signal EngineSpeed\n    Layout:            start bit 0, 16 bits, little-endian (Intel), unsigned\n    Scaling:           factor 0.25, offset 0\n    Range:             0 .. 16383.75\n    Unit:              \"rpm\"\n    Receivers:         Dashboard, Gateway\n    Comment:           \"Engine speed from crank sensor\"\n",
            "    Layout:            start bit 48, 1 bit, little-endian (Intel), unsigned",
            "    Value 0:           \"Off\"",
            "Attribute GenMsgSendType: \"Cyclic\"",
            "Multiplexer:       yes (multiplexor switch)",
            "Multiplexed:       when multiplexer = 1",
            "Signal group:      Temps (repetitions 1): CoolantTemp, OilTemp",
            "GenMsgCycleTime: message INT 0 .. 10000, default 0",
            "GenMsgSendType: message ENUM \"Cyclic\", \"Event\", \"IfActive\", \"NoMsgSendType\", default \"NoMsgSendType\"",
            "Transmitters:      Dashboard, Gateway",
            "  Signal BatteryVoltage\n    Layout:            start bit 8, 16 bits, little-endian (Intel), unsigned\n    Multiplexed:       when multiplexer = 1\n",
            "Cycle time:        100 ms",
            "    Layout:            start bit 39, 8 bits, big-endian (Motorola), signed",
            "    Layout:            start bit 32, 32 bits, little-endian (Intel), float (32 bit)",
        ] {
            assert!(s.contains(expected), "{expected}\n---\n{s}");
        }
        assert!(!s.contains("Other statements"), "{s}");
    }

    #[test]
    fn a_diff_shows_only_what_changed() {
        let (a, b) = (summary(V1).unwrap(), summary(V2).unwrap());
        // The moved message does not show; the changed factor and the new signal do.
        assert_eq!(lines_only_in(&a, &b), ["Signals:           14", "    Scaling:           factor 0.25, offset 0"]);
        assert_eq!(
            lines_only_in(&b, &a),
            [
                "Signals:           15",
                "    Scaling:           factor 0.125, offset 0",
                "  Signal FuelRate",
                "    Layout:            start bit 56, 8 bits, little-endian (Intel), unsigned",
                "    Scaling:           factor 0.2, offset 0",
                "    Range:             0 .. 51",
                "    Unit:              \"l/h\"",
                "    Comment:           \"Injected fuel per hour\"",
            ]
        );
    }

    #[test]
    fn same_database_ignores_order_and_formatting() {
        assert_eq!(summary(V1).unwrap(), summary(V1_REORDERED).unwrap());
        assert!(same_database(V1, V1_REORDERED));
        assert!(!same_database(V1, V2));
    }

    #[test]
    fn tolerant_of_garbage() {
        assert!(summary("").is_err());
        assert!(summary("hello world").is_err());
        for cut in [10, 100, 500, 1000, 2000, V1.len() - 3] {
            let mut end = cut.min(V1.len());
            while !V1.is_char_boundary(end) {
                end -= 1;
            }
            let _ = summary(&V1[..end]);
        }
        let odd = "VERSION \"\"\nBO_ 1 M: 8 X\n SG_ S : 0|8@1+ (1,0) [0|1] \"\" X\n SG_ broken : 7\nBA_ \"Nope\" BO_ 99 1;\nCM_ \"unterminated";
        let s = summary(odd).unwrap();
        assert!(s.contains("=== Other statements (2) ===\nBA_ \"Nope\" BO_ 99 1;\nSG_ broken : 7"), "{s}");
    }
}
