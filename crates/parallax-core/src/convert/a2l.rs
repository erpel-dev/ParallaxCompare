//! A2L files (ASAM MCD-2 MC): the description of an ECU's calibration and
//! measurement objects.
//!
//! An A2L is a tree of `/begin KIND … /end KIND` blocks: a `PROJECT` with a
//! `MODULE` that holds thousands of `CHARACTERISTIC`s (calibration values, curves,
//! maps), `MEASUREMENT`s, `AXIS_PTS`, `COMPU_METHOD`s, `RECORD_LAYOUT`s,
//! `FUNCTION`s and `GROUP`s. Each block has positional parameters (name, long
//! identifier, address, record layout, conversion, limits …) followed by optional
//! keywords and nested blocks. Generators emit objects in arbitrary order, and a
//! rebuild of the ECU software moves addresses, so a plain text diff mixes
//! reordering with the changes that matter.
//!
//! The "A2L objects" view shows each object as one block, sorted by kind and name,
//! with its positional parameters labelled one per line (addresses normalized to
//! `0x0001A2B4`, so a moved object shows exactly one changed line), its keywords in
//! file order, lists such as `REF_CHARACTERISTIC` one item per line and sorted, and
//! `IF_DATA` summarized. Comments are dropped.
//!
//! Real ECU A2Ls are 50–200 MB, so the parser is a hand-written streaming
//! tokenizer over the text: each top-level object is parsed into a small tree,
//! rendered (or hashed, for the ≈ similar check) and dropped. It knows the
//! positional parameter count of each block kind and the A2L keyword names, and
//! treats anything else as arguments, so vendor extensions and minor syntax errors
//! do not stop it. (The `a2lfile` crate parses fully into typed structures, which
//! is stricter and needs several times the file size in memory.)

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

use rayon::prelude::*;

use crate::{Error, Result};

/// Objects shown at most; the rest are counted.
const MAX_OBJECTS: usize = 500_000;
/// Objects rendered together in parallel.
const BATCH: usize = 4096;
/// Deeper nesting is treated as opaque (no real file comes close).
const MAX_DEPTH: usize = 24;
/// `IF_DATA` with at most this many values is shown inline.
const IF_DATA_INLINE: usize = 24;

/// The "A2L objects" view.
pub fn objects(text: &str) -> Result<String> {
    // Objects are parsed in one pass and rendered in parallel, a batch at a time, so
    // memory stays near the size of the output.
    let mut objs: Vec<Rendered> = Vec::new();
    let mut batch: Vec<(usize, Node)> = Vec::with_capacity(BATCH);
    let mut skipped = 0usize;
    let header = walk(text, &mut |module, node| {
        if objs.len() + batch.len() >= MAX_OBJECTS {
            skipped += 1;
            return;
        }
        batch.push((module, node));
        if batch.len() == BATCH {
            render_batch(&mut batch, &mut objs);
        }
    })?;
    render_batch(&mut batch, &mut objs);
    objs.sort_unstable_by(|a, b| (a.module, a.rank, a.kind, &a.name).cmp(&(b.module, b.rank, b.kind, &b.name)));

    let mut out = String::with_capacity(objs.iter().map(|o| o.text.len() + 2).sum::<usize>() + 4096);
    out.push_str(&header.text);
    let mut counts: BTreeMap<(usize, &str), usize> = BTreeMap::new();
    for o in &objs {
        *counts.entry((o.module, o.kind)).or_default() += 1;
    }
    let mut section = None;
    for o in &objs {
        if section != Some((o.module, o.kind)) {
            section = Some((o.module, o.kind));
            let n = counts[&(o.module, o.kind)];
            match header.modules.get(o.module).filter(|_| header.modules.len() > 1) {
                Some(m) => {
                    let _ = writeln!(out, "\n=== {m}: {} ({n}) ===", o.kind);
                }
                None => {
                    let _ = writeln!(out, "\n=== {} ({n}) ===", o.kind);
                }
            }
        }
        out.push('\n');
        out.push_str(&o.text);
    }
    if skipped > 0 {
        let _ = writeln!(out, "\n! {skipped} more objects are not shown");
    }
    if objs.is_empty() && header.modules.is_empty() {
        return Err(Error::Unsupported("A2L: no PROJECT or MODULE found".into()));
    }
    Ok(out)
}

/// Whether two A2L files describe the same objects, in another order or with other
/// formatting and comments. Compares a hash of each object's normalized tokens, so
/// nothing is rendered.
pub fn same_objects(left: &str, right: &str) -> Option<bool> {
    // The hash covers the kind and all parameters (the name among them); equal
    // multisets of object hashes mean equal objects.
    let digest = |text: &str| -> Option<(u64, Vec<(usize, u64)>)> {
        let mut objs = Vec::new();
        let header = walk(text, &mut |module, node| {
            let mut h = std::hash::DefaultHasher::new();
            node.hash_into(&mut h);
            objs.push((module, h.finish()));
        })
        .ok()?;
        objs.sort_unstable();
        let mut h = std::hash::DefaultHasher::new();
        header.text.hash(&mut h);
        Some((h.finish(), objs))
    };
    let (l, r) = rayon::join(|| digest(left), || digest(right));
    let l = l?;
    if l.1.is_empty() {
        return None;
    }
    Some(l == r?)
}

fn render_batch<'a>(batch: &mut Vec<(usize, Node<'a>)>, objs: &mut Vec<Rendered<'a>>) {
    objs.par_extend(batch.par_drain(..).map(|(module, node)| Rendered {
        module,
        rank: kind_rank(node.kind),
        kind: node.kind,
        name: node.name(),
        text: render_object(&node),
    }));
}

struct Rendered<'a> {
    module: usize,
    rank: u8,
    kind: &'a str,
    name: String,
    text: String,
}

// ---- Tokenizer ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok<'a> {
    Word(&'a str),
    /// A string with its quotes.
    Str(&'a str),
    Begin(&'a str),
    End(&'a str),
}

struct Tokens<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Tokens<'a> {
    /// The next word, string or comment-free token.
    fn raw(&mut self) -> Option<(&'a str, bool)> {
        let b = self.text.as_bytes();
        loop {
            while self.pos < b.len() && b[self.pos].is_ascii_whitespace() {
                self.pos += 1;
            }
            if self.pos >= b.len() {
                return None;
            }
            match (b[self.pos], b.get(self.pos + 1)) {
                (b'/', Some(b'*')) => {
                    self.pos = match self.text[self.pos + 2..].find("*/") {
                        Some(e) => self.pos + 2 + e + 2,
                        None => b.len(),
                    };
                }
                (b'/', Some(b'/')) => {
                    self.pos = self.text[self.pos..].find('\n').map_or(b.len(), |e| self.pos + e);
                }
                _ => break,
            }
        }
        let start = self.pos;
        if b[start] == b'"' {
            let mut i = start + 1;
            while i < b.len() && b[i] != b'"' {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            self.pos = (i + 1).min(b.len());
            return Some((&self.text[start..self.pos], true));
        }
        let mut i = start;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'"' {
            if b[i] == b'/' && matches!(b.get(i + 1), Some(b'*' | b'/')) && i > start {
                break;
            }
            i += 1;
        }
        // Whitespace and quotes are ASCII, so the cut is on a char boundary; a lone
        // `/` before `*` cannot loop because i > start is required for that stop.
        self.pos = i.max(start + 1);
        while !self.text.is_char_boundary(self.pos) {
            self.pos += 1;
        }
        Some((&self.text[start..self.pos], false))
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Tok<'a>;

    fn next(&mut self) -> Option<Tok<'a>> {
        let (t, is_str) = self.raw()?;
        if is_str {
            return Some(Tok::Str(t));
        }
        if t.eq_ignore_ascii_case("/begin") {
            return Some(Tok::Begin(self.raw().map_or("", |(k, _)| k)));
        }
        if t.eq_ignore_ascii_case("/end") {
            return Some(Tok::End(self.raw().map_or("", |(k, _)| k)));
        }
        Some(Tok::Word(t))
    }
}

// ---- Tree ----

/// A block: its kind, positional parameters and the keywords and blocks after them.
struct Node<'a> {
    kind: &'a str,
    args: Vec<Tok<'a>>,
    items: Vec<Item<'a>>,
}

enum Item<'a> {
    Line(&'a str, Vec<Tok<'a>>),
    Block(Node<'a>),
}

#[derive(Clone, Copy)]
enum Shape {
    /// This many positional parameters, then keywords and blocks.
    Fields(usize),
    /// Only a list of identifiers or numbers (`REF_CHARACTERISTIC`, `FUNCTION_LIST` …).
    List,
    /// Vendor-specific content, kept as tokens (`IF_DATA`, `A2ML`).
    Opaque,
}

fn shape(kind: &str) -> Shape {
    use Shape::*;
    match kind {
        "IF_DATA" | "A2ML" => Opaque,
        "REF_CHARACTERISTIC"
        | "REF_MEASUREMENT"
        | "DEF_CHARACTERISTIC"
        | "IN_MEASUREMENT"
        | "OUT_MEASUREMENT"
        | "LOC_MEASUREMENT"
        | "SUB_FUNCTION"
        | "SUB_GROUP"
        | "FUNCTION_LIST"
        | "MAP_LIST"
        | "VIRTUAL"
        | "REF_GROUP"
        | "FIX_AXIS_PAR_LIST"
        | "VAR_ADDRESS"
        | "TRANSFORMER_IN_OBJECTS"
        | "TRANSFORMER_OUT_OBJECTS"
        | "FRAME_MEASUREMENT"
        | "CALIBRATION_HANDLE"
        | "ANNOTATION_TEXT"
        | "VAR_FORBIDDEN_COMB"
        | "VAR_MEASUREMENT" => List,
        _ => Fields(labels(kind).len()),
    }
}

/// Labels of the positional parameters; the first is the name for named objects.
fn labels(kind: &str) -> &'static [&'static str] {
    match kind {
        "PROJECT" | "MODULE" | "FUNCTION" | "GROUP" | "CALIBRATION_METHOD" => &["Name", "Description"],
        "HEADER" | "MOD_PAR" | "MOD_COMMON" | "BIT_OPERATION" | "ANNOTATION" | "VARIANT_CODING" => &["Description"],
        "CHARACTERISTIC" => &[
            "Name",
            "Description",
            "Type",
            "Address",
            "Record layout",
            "Max diff",
            "Conversion",
            "Lower limit",
            "Upper limit",
        ],
        "MEASUREMENT" => {
            &["Name", "Description", "Data type", "Conversion", "Resolution", "Accuracy", "Lower limit", "Upper limit"]
        }
        "AXIS_PTS" => &[
            "Name",
            "Description",
            "Address",
            "Input quantity",
            "Record layout",
            "Max diff",
            "Conversion",
            "Max axis points",
            "Lower limit",
            "Upper limit",
        ],
        "AXIS_DESCR" => &["Attribute", "Input quantity", "Conversion", "Max axis points", "Lower limit", "Upper limit"],
        "COMPU_METHOD" => &["Name", "Description", "Conversion type", "Format", "Unit"],
        "COMPU_TAB" | "COMPU_VTAB" => &["Name", "Description", "Conversion type", "Entries"],
        "COMPU_VTAB_RANGE" => &["Name", "Description", "Entries"],
        "RECORD_LAYOUT"
        | "VAR_CHARACTERISTIC"
        | "USER_RIGHTS"
        | "DEPENDENT_CHARACTERISTIC"
        | "VIRTUAL_CHARACTERISTIC" => &["Name"],
        "FORMULA" => &["Formula"],
        "UNIT" => &["Name", "Description", "Display", "Type"],
        "FRAME" => &["Name", "Description", "Scaling unit", "Rate"],
        "MEMORY_SEGMENT" => &[
            "Name",
            "Description",
            "Program type",
            "Memory type",
            "Attribute",
            "Address",
            "Size",
            "Offset 1",
            "Offset 2",
            "Offset 3",
            "Offset 4",
            "Offset 5",
        ],
        "MEMORY_LAYOUT" => {
            &["Program type", "Address", "Size", "Offset 1", "Offset 2", "Offset 3", "Offset 4", "Offset 5"]
        }
        "VAR_CRITERION" => &["Name", "Description"],
        "BLOB" => &["Name", "Description", "Address", "Size"],
        "INSTANCE" => &["Name", "Description", "Type", "Address"],
        "TYPEDEF_STRUCTURE" => &["Name", "Description", "Size"],
        "STRUCTURE_COMPONENT" => &["Name", "Type", "Address offset"],
        "TYPEDEF_BLOB" => &["Name", "Description", "Size"],
        "TYPEDEF_MEASUREMENT" => {
            &["Name", "Description", "Data type", "Conversion", "Resolution", "Accuracy", "Lower limit", "Upper limit"]
        }
        "TYPEDEF_CHARACTERISTIC" => {
            &["Name", "Description", "Type", "Record layout", "Max diff", "Conversion", "Lower limit", "Upper limit"]
        }
        "TYPEDEF_AXIS" => &[
            "Name",
            "Description",
            "Input quantity",
            "Record layout",
            "Max diff",
            "Conversion",
            "Max axis points",
            "Lower limit",
            "Upper limit",
        ],
        "TRANSFORMER" => &["Name", "Version", "DLL 32", "DLL 64", "Timeout", "Trigger", "Inverse"],
        _ => &[],
    }
}

/// Whether the first positional parameter names the object.
fn is_named(kind: &str) -> bool {
    labels(kind).first() == Some(&"Name")
}

/// Order of the sections: module settings first, then the objects people look for.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "A2ML" => 0,
        "MOD_PAR" => 1,
        "MOD_COMMON" => 2,
        "IF_DATA" => 3,
        "CHARACTERISTIC" => 4,
        "AXIS_PTS" => 5,
        "MEASUREMENT" => 6,
        "COMPU_METHOD" => 7,
        "COMPU_TAB" => 8,
        "COMPU_VTAB" => 9,
        "COMPU_VTAB_RANGE" => 10,
        "RECORD_LAYOUT" => 11,
        "FUNCTION" => 12,
        "GROUP" => 13,
        "UNIT" => 14,
        _ => 20,
    }
}

const SUFFIXED: &[&str] = &[
    "AXIS_PTS_",
    "AXIS_RESCALE_",
    "DIST_OP_",
    "FIX_NO_AXIS_PTS_",
    "NO_AXIS_PTS_",
    "NO_RESCALE_",
    "OFFSET_",
    "RIP_ADDR_",
    "SHIFT_OP_",
    "SRC_ADDR_",
];

/// Keywords of ASAP2 1.5–1.7 that are not blocks.
fn is_keyword(w: &str) -> bool {
    if !w.as_bytes().first().is_some_and(u8::is_ascii_uppercase) {
        return false;
    }
    if SUFFIXED.iter().any(|p| w.strip_prefix(p).is_some_and(|s| matches!(s, "X" | "Y" | "Z" | "4" | "5" | "W"))) {
        return true;
    }
    matches!(
        w,
        "A2ML_VERSION"
            | "ADDR_EPK"
            | "ADDRESS_TYPE"
            | "ALIGNMENT_BYTE"
            | "ALIGNMENT_FLOAT16_IEEE"
            | "ALIGNMENT_FLOAT32_IEEE"
            | "ALIGNMENT_FLOAT64_IEEE"
            | "ALIGNMENT_INT64"
            | "ALIGNMENT_LONG"
            | "ALIGNMENT_WORD"
            | "ANNOTATION_LABEL"
            | "ANNOTATION_ORIGIN"
            | "AR_COMPONENT"
            | "AR_PROTOTYPE_OF"
            | "ARRAY_SIZE"
            | "ASAP2_VERSION"
            | "AXIS_PTS_REF"
            | "BIT_MASK"
            | "BYTE_ORDER"
            | "CALIBRATION_ACCESS"
            | "CALIBRATION_HANDLE_TEXT"
            | "COEFFS"
            | "COEFFS_LINEAR"
            | "COMPARISON_QUANTITY"
            | "COMPU_TAB_REF"
            | "CONSISTENT_EXCHANGE"
            | "CONVERSION"
            | "CPU_TYPE"
            | "CURVE_AXIS_REF"
            | "CUSTOMER"
            | "CUSTOMER_NO"
            | "DATA_SIZE"
            | "DEFAULT_VALUE"
            | "DEFAULT_VALUE_NUMERIC"
            | "DEPOSIT"
            | "DISCRETE"
            | "DISPLAY_IDENTIFIER"
            | "ECU"
            | "ECU_ADDRESS"
            | "ECU_ADDRESS_EXTENSION"
            | "ECU_CALIBRATION_OFFSET"
            | "ENCODING"
            | "EPK"
            | "ERROR_MASK"
            | "EXTENDED_LIMITS"
            | "FIX_AXIS_PAR"
            | "FIX_AXIS_PAR_DIST"
            | "FNC_VALUES"
            | "FORMAT"
            | "FORMULA_INV"
            | "FUNCTION_VERSION"
            | "GUARD_RAILS"
            | "IDENTIFICATION"
            | "LAYOUT"
            | "LEFT_SHIFT"
            | "LIMITS"
            | "MATRIX_DIM"
            | "MAX_GRAD"
            | "MAX_REFRESH"
            | "MODEL_LINK"
            | "MONOTONY"
            | "NO_OF_INTERFACES"
            | "NUMBER"
            | "PHONE_NO"
            | "PHYS_UNIT"
            | "PROJECT_NO"
            | "READ_ONLY"
            | "READ_WRITE"
            | "REF_MEMORY_SEGMENT"
            | "REF_UNIT"
            | "RESERVED"
            | "RIGHT_SHIFT"
            | "ROOT"
            | "S_REC_LAYOUT"
            | "SI_EXPONENTS"
            | "SIGN_EXTEND"
            | "STATIC_ADDRESS_OFFSETS"
            | "STATIC_RECORD_LAYOUT"
            | "STATUS_STRING_REF"
            | "STEP_SIZE"
            | "SUPPLIER"
            | "SYMBOL_LINK"
            | "SYMBOL_TYPE_LINK"
            | "SYSTEM_CONSTANT"
            | "UNIT_CONVERSION"
            | "USER"
            | "VAR_NAMING"
            | "VAR_SELECTION_CHARACTERISTIC"
            | "VAR_SEPARATOR"
            | "VERSION"
    )
}

fn parse_block<'a>(toks: &mut Tokens<'a>, kind: &'a str, depth: usize) -> Node<'a> {
    let shape = if depth > MAX_DEPTH { Shape::Opaque } else { shape(kind) };
    let mut node = Node { kind, args: Vec::new(), items: Vec::new() };
    if let Shape::Opaque = shape {
        // Kept flat, nested blocks as their markers, up to the matching end.
        let mut open = 0usize;
        for t in toks.by_ref() {
            match t {
                Tok::Begin(_) => open += 1,
                Tok::End(_) if open == 0 => break,
                Tok::End(_) => open -= 1,
                _ => {}
            }
            node.args.push(t);
        }
        return node;
    }
    while let Some(t) = toks.next() {
        match t {
            // A mismatched end closes this block too: better than swallowing the file.
            Tok::End(_) => break,
            Tok::Begin(k) => node.items.push(Item::Block(parse_block(toks, k, depth + 1))),
            Tok::Word(w) | Tok::Str(w) => match shape {
                Shape::Fields(n) if node.args.len() < n && node.items.is_empty() => node.args.push(t),
                Shape::Fields(_) if matches!(t, Tok::Word(_)) && is_keyword(w) => {
                    node.items.push(Item::Line(w, Vec::new()))
                }
                Shape::Fields(_) => match node.items.last_mut() {
                    Some(Item::Line(_, args)) => args.push(t),
                    _ => node.args.push(t),
                },
                _ => node.args.push(t),
            },
        }
    }
    node
}

impl Node<'_> {
    fn name(&self) -> String {
        match self.kind {
            "IF_DATA" => self.args.first().map(|t| tok_text(t, false).into_owned()).unwrap_or_default(),
            k if is_named(k) => self.args.first().map(|t| tok_text(t, false).into_owned()).unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn hash_into(&self, h: &mut impl Hasher) {
        self.kind.hash(h);
        for a in &self.args {
            hash_tok(a, h);
        }
        for item in &self.items {
            match item {
                Item::Line(k, args) => {
                    1u8.hash(h);
                    k.hash(h);
                    for a in args {
                        hash_tok(a, h);
                    }
                }
                Item::Block(b) => {
                    2u8.hash(h);
                    b.hash_into(h);
                }
            }
        }
        3u8.hash(h);
    }
}

/// The text of a token as shown: numbers normalized (`0x00FF` → `0xFF`, `1.000` →
/// `1`), addresses as eight hex digits, strings with their quotes.
fn tok_text<'a>(t: &Tok<'a>, address: bool) -> Cow<'a, str> {
    match *t {
        Tok::Word(w) => number(w, address).map_or(Cow::Borrowed(w), Cow::Owned),
        Tok::Str(s) => Cow::Borrowed(s),
        Tok::Begin(k) => Cow::Owned(format!("/begin {k}")),
        Tok::End(k) => Cow::Owned(format!("/end {k}")),
    }
}

/// Hashes a token as [`tok_text`] shows it, without building the text.
fn hash_tok(t: &Tok, h: &mut impl Hasher) {
    match *t {
        Tok::Word(w) if w.as_bytes().first().is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.')) => {
            if let Some(hex) = w.strip_prefix("0x").or_else(|| w.strip_prefix("0X"))
                && let Ok(v) = u64::from_str_radix(hex, 16)
            {
                (0u8, v).hash(h);
            } else if let Ok(v) = w.parse::<i64>() {
                (1u8, v).hash(h);
            } else if let Ok(v) = w.parse::<f64>().map(|v| if v == 0.0 { 0.0 } else { v }) {
                if v.fract() == 0.0 && v.abs() < 9e15 {
                    (1u8, v as i64).hash(h);
                } else {
                    (2u8, v.to_bits()).hash(h);
                }
            } else {
                (3u8, w).hash(h);
            }
        }
        Tok::Word(w) | Tok::Str(w) => (3u8, w).hash(h),
        Tok::Begin(k) => (4u8, k).hash(h),
        Tok::End(k) => (5u8, k).hash(h),
    }
}

fn number(w: &str, address: bool) -> Option<String> {
    let first = *w.as_bytes().first()?;
    if !(first.is_ascii_digit() || matches!(first, b'-' | b'+' | b'.')) {
        return None;
    }
    if let Some(hex) = w.strip_prefix("0x").or_else(|| w.strip_prefix("0X")) {
        let v = u64::from_str_radix(hex, 16).ok()?;
        return Some(if address { format!("0x{v:08X}") } else { format!("0x{v:X}") });
    }
    if let Ok(v) = w.parse::<i64>() {
        return Some(if address && v >= 0 { format!("0x{v:08X}") } else { v.to_string() });
    }
    let v = w.parse::<f64>().ok().filter(|v| v.is_finite())?;
    Some(if v == 0.0 { "0".into() } else { format!("{v}") })
}

fn join(toks: &[Tok], address: bool) -> String {
    let mut s = String::new();
    for (i, t) in toks.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&tok_text(t, address && i == 0));
    }
    s
}

/// `Label:` padded to 18 columns, then the value (written without `format!`: this
/// runs for every field of every object).
fn line(out: &mut String, indent: usize, label: &str, value: impl std::fmt::Display) {
    out.extend(std::iter::repeat_n(' ', indent));
    out.push_str(label);
    out.push(':');
    out.extend(std::iter::repeat_n(' ', 18usize.saturating_sub(label.len() + 1) + 1));
    let _ = writeln!(out, "{value}");
}

fn render_object(node: &Node) -> String {
    let mut out = String::with_capacity(256);
    let name = node.name();
    let title = match node.kind {
        "MOD_PAR" | "MOD_COMMON" => node.args.first().map(|t| tok_text(t, false).into_owned()).unwrap_or_default(),
        _ => name,
    };
    if title.is_empty() {
        let _ = writeln!(out, "--- {} ---", node.kind);
    } else {
        let _ = writeln!(out, "--- {title} ---");
    }
    match node.kind {
        "IF_DATA" => if_data(&mut out, 0, node, true),
        "A2ML" => {
            let mut h = std::hash::DefaultHasher::new();
            node.hash_into(&mut h);
            line(&mut out, 0, "Definition", format!("{} tokens, hash {:016x}", node.args.len(), h.finish()));
        }
        _ => body(&mut out, 0, node, matches!(node.kind, "MOD_PAR" | "MOD_COMMON") || is_named(node.kind)),
    }
    out
}

/// The positional parameters, keywords and nested blocks of a node.
fn body(out: &mut String, indent: usize, node: &Node, skip_first: bool) {
    let labels = labels(node.kind);
    let fields = labels.len().min(node.args.len());
    for (i, (label, t)) in labels.iter().zip(&node.args).enumerate() {
        if i == 0 && skip_first {
            continue;
        }
        let address = label.contains("Address") || label.starts_with("Offset");
        line(out, indent, label, tok_text(t, address));
    }
    let extra = &node.args[fields..];
    match (node.kind, shape(node.kind)) {
        (_, Shape::List) => {
            let mut items: Vec<Cow<str>> = extra.iter().map(|t| tok_text(t, false)).collect();
            if node.kind != "ANNOTATION_TEXT" && node.kind != "CALIBRATION_HANDLE" {
                items.sort_unstable();
            }
            for item in items {
                let _ = writeln!(out, "{:indent$}{item}", "");
            }
        }
        ("COMPU_VTAB" | "COMPU_TAB", _) => {
            for pair in extra.chunks(2) {
                let _ = writeln!(out, "{:indent$}{}", "", join(pair, false).replacen(' ', " = ", 1));
            }
        }
        ("COMPU_VTAB_RANGE", _) => {
            for triple in extra.chunks(3) {
                let t: Vec<Cow<str>> = triple.iter().map(|t| tok_text(t, false)).collect();
                let _ = writeln!(
                    out,
                    "{:indent$}{}",
                    "",
                    match t.as_slice() {
                        [a, b, c] => format!("{a} .. {b} = {c}"),
                        other => other.join(" "),
                    }
                );
            }
        }
        _ if !extra.is_empty() => line(out, indent, "Values", join(extra, false)),
        _ => {}
    }
    for item in &node.items {
        match item {
            Item::Line(k, args) => {
                let address = matches!(*k, "ECU_ADDRESS" | "ADDR_EPK");
                if args.is_empty() {
                    let _ = writeln!(out, "{:indent$}{k}", "");
                } else {
                    let _ = writeln!(out, "{:indent$}{k} {}", "", join(args, address));
                }
            }
            Item::Block(b) if b.kind == "IF_DATA" => if_data(out, indent, b, false),
            Item::Block(b) => {
                // The first positional parameter (name, axis kind …) goes in the heading.
                let fields = matches!(shape(b.kind), Shape::Fields(n) if n > 0);
                let title = if fields { b.args.first().map(|t| tok_text(t, false)) } else { None };
                let n = if let Shape::List = shape(b.kind) { format!(" ({})", b.args.len()) } else { String::new() };
                match title {
                    Some(t) => {
                        let _ = writeln!(out, "{:indent$}{} {t}{n}", "", b.kind);
                    }
                    None => {
                        let _ = writeln!(out, "{:indent$}{}{n}", "", b.kind);
                    }
                }
                body(out, indent + 2, b, fields);
            }
        }
    }
}

/// `IF_DATA` holds vendor data (XCP, ETK, CCP …): short ones inline, long ones as
/// the names of their sub-blocks with a hash.
fn if_data(out: &mut String, indent: usize, node: &Node, top: bool) {
    let tag = node.args.first().map(|t| tok_text(t, false)).unwrap_or_default();
    let rest = node.args.get(1..).unwrap_or_default();
    if rest.len() <= IF_DATA_INLINE {
        if top {
            line(out, indent, "Content", join(rest, false));
        } else {
            let _ = writeln!(out, "{:indent$}IF_DATA {tag} {}", "", join(rest, false));
        }
        return;
    }
    let mut depth = 0usize;
    let mut blocks = Vec::new();
    for t in rest {
        match t {
            Tok::Begin(k) => {
                if depth == 0 {
                    blocks.push(*k);
                }
                depth += 1;
            }
            Tok::End(_) => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    blocks.dedup();
    let mut h = std::hash::DefaultHasher::new();
    node.hash_into(&mut h);
    let summary = format!("{} values, hash {:016x}", rest.len(), h.finish());
    let summary = if blocks.is_empty() { summary } else { format!("{} ({summary})", blocks.join(", ")) };
    if top {
        line(out, indent, "Content", summary);
    } else {
        let _ = writeln!(out, "{:indent$}IF_DATA {tag} {summary}", "");
    }
}

struct Header {
    /// Version lines, project and header, as text.
    text: String,
    modules: Vec<String>,
}

/// Walks the file: renders the top-level lines, `PROJECT` and `HEADER` into the
/// header, and calls `object` with each object in a `MODULE` (or directly in the
/// project, as some generators write).
fn walk<'a>(text: &'a str, object: &mut dyn FnMut(usize, Node<'a>)) -> Result<Header> {
    let mut toks = Tokens { text, pos: 0 };
    let mut header = Header { text: String::new(), modules: Vec::new() };
    // Containers: 0 = file, 1 = PROJECT, 2 = MODULE.
    let mut level = 0u8;
    let mut module = 0usize;
    let mut saw_project = false;
    let mut pending_line: Option<(&str, Vec<Tok>)> = None;
    let flush = |pending: &mut Option<(&str, Vec<Tok>)>, out: &mut String| {
        if let Some((k, args)) = pending.take() {
            line(out, 0, k, join(&args, false));
        }
    };
    while let Some(t) = toks.next() {
        match t {
            Tok::Begin("PROJECT") if level == 0 => {
                flush(&mut pending_line, &mut header.text);
                level = 1;
                saw_project = true;
                let words: Vec<Tok> = toks.by_ref().take(2).collect();
                if let Some(Tok::Begin(_) | Tok::End(_)) =
                    words.iter().find(|t| matches!(t, Tok::Begin(_) | Tok::End(_)))
                {
                    return Err(Error::Unsupported("A2L: PROJECT has no name".into()));
                }
                line(&mut header.text, 0, "Project", join(&words, false));
            }
            Tok::Begin("MODULE") if level <= 1 => {
                flush(&mut pending_line, &mut header.text);
                level = 2;
                let words: Vec<Tok> = toks.by_ref().take(2).collect();
                let name = words.first().map(|t| tok_text(t, false).into_owned()).unwrap_or_default();
                line(&mut header.text, 0, "Module", join(&words, false));
                module = header.modules.len();
                header.modules.push(name);
            }
            Tok::Begin("HEADER") if level == 1 => {
                flush(&mut pending_line, &mut header.text);
                let node = parse_block(&mut toks, "HEADER", 1);
                let _ = writeln!(header.text, "\n=== HEADER ===");
                body(&mut header.text, 0, &node, false);
                header.text.push('\n');
            }
            Tok::Begin(kind) => {
                flush(&mut pending_line, &mut header.text);
                if header.modules.is_empty() {
                    header.modules.push(String::new());
                }
                object(module, parse_block(&mut toks, kind, 1));
            }
            Tok::End(_) => {
                flush(&mut pending_line, &mut header.text);
                level = level.saturating_sub(1);
            }
            Tok::Word(w) if is_keyword(w) => {
                flush(&mut pending_line, &mut header.text);
                pending_line = Some((w, Vec::new()));
            }
            Tok::Word(_) | Tok::Str(_) => match &mut pending_line {
                Some((_, args)) => args.push(t),
                None if !saw_project && toks.pos > 4096 => {
                    return Err(Error::Unsupported("A2L: no PROJECT found at the start".into()));
                }
                None => {}
            },
        }
    }
    flush(&mut pending_line, &mut header.text);
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/a2l/", $name))
        };
    }
    const V1: &str = fixture!("engine-v1.a2l");
    const V1_REFORMATTED: &str = fixture!("engine-v1-reformatted.a2l");
    const V2: &str = fixture!("engine-v2.a2l");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn objects_are_sorted_and_labelled() {
        let o = objects(V1).unwrap();
        for expected in [
            "ASAP2_VERSION:     1 71",
            "Project:           EngineDemo \"Engine ECU demo project\"",
            "Module:            ENGINE_ECU \"Engine control\"",
            "=== MOD_PAR (1) ===",
            "ADDR_EPK 0x80000000",
            "=== CHARACTERISTIC (4) ===",
            "--- KL_IgnitionAdvance ---\nDescription:       \"Ignition advance over speed\"\nType:              CURVE\nAddress:           0x80010100\nRecord layout:     RL_CURVE_UW\n",
            "AXIS_DESCR STD_AXIS\n  Input quantity:    nEngine\n",
            "=== MEASUREMENT (3) ===",
            "ECU_ADDRESS 0xD0001000",
            "IF_DATA XCP LINK_MAP \"nEngine\" 0xD0001000 0x0 0 0 1 0xCF 0x0",
            "--- CM_Temp ---\nDescription:       \"Temperature\"\nConversion type:   RAT_FUNC\nFormat:            \"%6.1\"\nUnit:              \"degC\"\nCOEFFS 0 1 40 0 0 1",
            "DEF_CHARACTERISTIC (2)\n  KL_IgnitionAdvance\n  K_IdleSpeed\n",
            "0 = \"Off\"\n1 = \"On\"",
            "FNC_VALUES 1 UWORD COLUMN_DIR DIRECT",
            "Content:           PROTOCOL_LAYER, DAQ",
        ] {
            assert!(o.contains(expected), "{expected}\n---\n{o}");
        }
        // Sorted by name within a kind.
        let a = o.find("--- K_IdleSpeed ---").unwrap();
        assert!(o.find("--- KL_IgnitionAdvance ---").unwrap() < a);
    }

    #[test]
    fn a_rebuild_shows_as_changed_addresses() {
        let (a, b) = (objects(V1).unwrap(), objects(V2).unwrap());
        let removed = lines_only_in(&a, &b);
        let moved =
            |l: &&str| l.trim_start().starts_with("Address:") || l.starts_with("ECU_ADDRESS") || l.contains("LINK_MAP");
        assert!(removed.iter().all(moved), "{removed:?}");
        // Four characteristics, the calibration memory segment, one measurement and two IF_DATA.
        assert_eq!(removed.len(), 8, "{removed:?}");
        let added = lines_only_in(&b, &a);
        assert!(added.contains(&"Upper limit:       8000"), "{added:?}");
    }

    #[test]
    fn same_objects_ignores_order_formatting_and_comments() {
        assert_eq!(objects(V1).unwrap(), objects(V1_REFORMATTED).unwrap());
        assert_eq!(same_objects(V1, V1_REFORMATTED), Some(true));
        assert_eq!(same_objects(V1, V2), Some(false));
        assert_eq!(same_objects("garbage", "garbage"), None);
    }

    #[test]
    fn tolerant_of_garbage() {
        assert!(objects("").is_err());
        assert!(objects("hello").is_err());
        for end in (0..V1.len()).step_by(97) {
            if V1.is_char_boundary(end) {
                let _ = objects(&V1[..end]);
            }
        }
        let nested = "/begin PROJECT p \"\" /begin MODULE m \"\" ".to_string() + &"/begin X ".repeat(10_000);
        assert!(objects(&nested).is_ok());
        let _ = objects("/begin PROJECT /end PROJECT /* unterminated");
        let _ = objects("/begin PROJECT p \"unterminated");
    }
}
