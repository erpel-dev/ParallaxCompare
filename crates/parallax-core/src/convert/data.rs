//! YAML and TOML as normalized data: keys sorted, one consistent layout.
//!
//! These files are written by hand, so the plain text stays the default view.
//! The normalized view answers "is the data different, or only the way it is
//! written?": mapping keys are sorted recursively, comments are dropped, scalars
//! are written in one canonical form (`0x1F` → `31`, `'a'` → `a`, `~` → `null`),
//! YAML anchors, aliases and merge keys (`<<`) are expanded, and multi-line
//! strings become literal blocks so a changed script line stays one changed line.
//!
//! YAML is read with yaml-rust2's event parser into a small tree of our own, so
//! alias expansion can be bounded (a "billion laughs" file fails instead of
//! exhausting memory) and duplicate keys can be reported (the last one wins, as
//! in most loaders). Scalars resolve by the YAML 1.2 core schema. TOML is read
//! with the `toml` crate and written back with its tables sorted.

use std::collections::HashMap;
use std::fmt::Write as _;

use yaml_rust2::parser::{Event, Parser, Tag};
use yaml_rust2::scanner::TScalarStyle;

use crate::{Error, Result};

/// Nodes a YAML file may expand to through aliases.
const MAX_NODES: usize = 5_000_000;
/// Nesting depth; deeper files are rejected rather than risking the stack.
const MAX_DEPTH: usize = 256;

// ---- YAML ----

/// A scalar in canonical form.
#[derive(Debug, Clone, PartialEq)]
enum Scalar {
    Null,
    Bool(bool),
    /// Decimal digits (or the source text when out of range).
    Int(Num),
    /// Shortest round-trip form, `.inf`, `-.inf` or `.nan`.
    Float(Num),
    Str(String),
    /// A scalar with an application tag (`!secret abc`): the tag and the raw text.
    Tagged(String, String),
}

/// A number in canonical form, with the text it was written as (which does not
/// take part in comparisons: `1.10` is the float `1.1`, but a lockfile reader wants
/// the version string back).
#[derive(Debug, Clone)]
struct Num {
    canonical: String,
    raw: String,
}

impl PartialEq for Num {
    fn eq(&self, other: &Num) -> bool {
        self.canonical == other.canonical
    }
}

fn num(canonical: impl Into<String>, raw: &str) -> Num {
    Num { canonical: canonical.into(), raw: raw.to_string() }
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Scalar(Scalar),
    Seq(Vec<Node>, Option<String>),
    /// Sorted by key, keys unique.
    Map(Vec<(Node, Node)>, Option<String>),
}

impl Node {
    fn count(&self) -> usize {
        match self {
            Node::Scalar(_) => 1,
            Node::Seq(items, _) => 1 + items.iter().map(Node::count).sum::<usize>(),
            Node::Map(entries, _) => 1 + entries.iter().map(|(k, v)| k.count() + v.count()).sum::<usize>(),
        }
    }
}

/// A parsed YAML stream: its documents and any warnings (duplicate keys).
struct Yaml {
    docs: Vec<Node>,
    warnings: Vec<String>,
}

enum Frame {
    Seq { items: Vec<Node>, anchor: usize, tag: Option<String> },
    Map { entries: Vec<(Node, Node)>, key: Option<Node>, anchor: usize, tag: Option<String> },
}

fn tag_name(tag: &Tag) -> String {
    match tag.handle.as_str() {
        "tag:yaml.org,2002:" | "!!" => format!("!!{}", tag.suffix),
        h => format!("{h}{}", tag.suffix),
    }
}

/// The core-schema type of a plain scalar.
fn resolve_plain(v: &str) -> Scalar {
    match v {
        "" | "~" | "null" | "Null" | "NULL" => return Scalar::Null,
        "true" | "True" | "TRUE" => return Scalar::Bool(true),
        "false" | "False" | "FALSE" => return Scalar::Bool(false),
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" => return Scalar::Float(num(".inf", v)),
        "-.inf" | "-.Inf" | "-.INF" => return Scalar::Float(num("-.inf", v)),
        ".nan" | ".NaN" | ".NAN" => return Scalar::Float(num(".nan", v)),
        _ => {}
    }
    if let Some(int) = parse_int(v) {
        return Scalar::Int(num(int, v));
    }
    if is_float(v)
        && let Ok(f) = v.parse::<f64>()
        && f.is_finite()
    {
        return Scalar::Float(num(format!("{f:?}"), v));
    }
    Scalar::Str(v.to_string())
}

fn parse_int(v: &str) -> Option<String> {
    let digits = |s: &str, radix: u32| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
    let (radix, body) = if let Some(h) = v.strip_prefix("0x") {
        (16, h)
    } else if let Some(o) = v.strip_prefix("0o") {
        (8, o)
    } else {
        let body = v.strip_prefix(['-', '+']).unwrap_or(v);
        if !digits(body, 10) {
            return None;
        }
        return Some(match v.parse::<i128>() {
            Ok(n) => n.to_string(),
            Err(_) => v.to_string(),
        });
    };
    digits(body, radix).then(|| u128::from_str_radix(body, radix).map_or_else(|_| v.to_string(), |n| n.to_string()))
}

/// `[-+]? ( \. [0-9]+ | [0-9]+ ( \. [0-9]* )? ) ( [eE] [-+]? [0-9]+ )?`
fn is_float(v: &str) -> bool {
    let b = v.strip_prefix(['-', '+']).unwrap_or(v).as_bytes();
    let mut i = 0;
    let int_digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    i += int_digits;
    let mut frac_digits = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        frac_digits = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        i += frac_digits;
    }
    if int_digits == 0 && frac_digits == 0 {
        return false;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'-' | b'+')) {
            i += 1;
        }
        let exp = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        if exp == 0 {
            return false;
        }
        i += exp;
    }
    i == b.len()
}

fn scalar(value: String, style: TScalarStyle, tag: Option<&Tag>) -> Scalar {
    let plain = style == TScalarStyle::Plain;
    match tag.map(tag_name).as_deref() {
        None | Some("!") if plain && tag.is_none() => resolve_plain(&value),
        None | Some("!") => Scalar::Str(value),
        Some("!!str") => Scalar::Str(value),
        Some("!!int" | "!!float" | "!!bool" | "!!null") => match resolve_plain(&value) {
            Scalar::Str(s) => Scalar::Tagged(tag_name(tag.unwrap()), s),
            other => other,
        },
        Some(t) => Scalar::Tagged(t.to_string(), value),
    }
}

/// The text a key sorts by: strings as they are, other scalars as written.
fn key_text(key: &Node) -> String {
    match key {
        Node::Scalar(Scalar::Str(s)) => s.clone(),
        Node::Scalar(s) => scalar_text(s, true),
        other => {
            let mut out = String::new();
            flow(other, &mut out);
            out
        }
    }
}

/// A one-line form of a collection used as a mapping key (rare in practice).
fn flow(node: &Node, out: &mut String) {
    match node {
        Node::Scalar(s) => out.push_str(&scalar_text(s, false)),
        Node::Seq(items, _) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                flow(item, out);
            }
            out.push(']');
        }
        Node::Map(entries, _) => {
            out.push('{');
            for (i, (k, v)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                flow(k, out);
                out.push_str(": ");
                flow(v, out);
            }
            out.push('}');
        }
    }
}

/// Finishes a mapping: applies merge keys, keeps the last of duplicate keys, sorts.
fn finish_map(entries: Vec<(Node, Node)>, warnings: &mut Vec<String>) -> Vec<(Node, Node)> {
    // (key text, priority: merged 0 / explicit 1, order, key, value)
    let mut all: Vec<(String, u8, usize, Node, Node)> = Vec::with_capacity(entries.len());
    for (i, (k, v)) in entries.into_iter().enumerate() {
        if k == Node::Scalar(Scalar::Str("<<".into())) {
            let sources = match v {
                Node::Map(..) => vec![v],
                Node::Seq(items, _) if items.iter().all(|n| matches!(n, Node::Map(..))) => items,
                other => {
                    all.push(("<<".into(), 1, i, k, other));
                    continue;
                }
            };
            // Earlier sources win over later ones: number them so the first sorts last.
            let n = sources.len();
            for (j, source) in sources.into_iter().enumerate() {
                if let Node::Map(merged, _) = source {
                    for (mk, mv) in merged {
                        all.push((key_text(&mk), 0, n - j, mk, mv));
                    }
                }
            }
            continue;
        }
        all.push((key_text(&k), 1, i, k, v));
    }
    all.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
    let mut out: Vec<(Node, Node)> = Vec::with_capacity(all.len());
    let mut last_key: Option<(String, u8)> = None;
    for (text, prio, _, k, v) in all {
        match &last_key {
            Some((t, p)) if *t == text => {
                if *p == 1 && prio == 1 && warnings.len() < 100 {
                    warnings.push(format!("duplicate key {text}, the last one is used"));
                }
                *out.last_mut().unwrap() = (k, v);
            }
            _ => out.push((k, v)),
        }
        last_key = Some((text, prio));
    }
    out
}

fn parse_yaml(text: &str) -> Result<Yaml> {
    let bad = |e: String| Error::Unsupported(format!("not readable as YAML: {e}"));
    let mut parser = Parser::new_from_str(text);
    let mut docs = Vec::new();
    let mut warnings = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut anchors: HashMap<usize, Node> = HashMap::new();
    let mut nodes = 0usize;
    loop {
        let (event, mark) = parser.next_token().map_err(|e| bad(e.to_string()))?;
        let (node, anchor) = match event {
            Event::StreamEnd => break,
            Event::Nothing | Event::StreamStart | Event::DocumentStart | Event::DocumentEnd => continue,
            Event::SequenceStart(anchor, tag) | Event::MappingStart(anchor, tag) if stack.len() >= MAX_DEPTH => {
                let _ = (anchor, tag);
                return Err(bad(format!("nested deeper than {MAX_DEPTH} levels at line {}", mark.line())));
            }
            Event::SequenceStart(anchor, tag) => {
                stack.push(Frame::Seq { items: Vec::new(), anchor, tag: tag.as_ref().map(tag_name) });
                continue;
            }
            Event::MappingStart(anchor, tag) => {
                stack.push(Frame::Map { entries: Vec::new(), key: None, anchor, tag: tag.as_ref().map(tag_name) });
                continue;
            }
            Event::SequenceEnd => match stack.pop() {
                Some(Frame::Seq { items, anchor, tag }) => (Node::Seq(items, tag), anchor),
                _ => return Err(bad("unbalanced sequence".into())),
            },
            Event::MappingEnd => match stack.pop() {
                Some(Frame::Map { entries, key: None, anchor, tag }) => {
                    (Node::Map(finish_map(entries, &mut warnings), tag), anchor)
                }
                _ => return Err(bad("unbalanced mapping".into())),
            },
            Event::Scalar(value, style, anchor, tag) => (Node::Scalar(scalar(value, style, tag.as_ref())), anchor),
            Event::Alias(id) => {
                let node = anchors.get(&id).ok_or_else(|| bad(format!("unknown alias at line {}", mark.line())))?;
                nodes += node.count();
                if nodes > MAX_NODES {
                    return Err(bad(format!("aliases expand to more than {MAX_NODES} nodes")));
                }
                (node.clone(), 0)
            }
        };
        nodes += 1;
        if anchor != 0 {
            anchors.insert(anchor, node.clone());
        }
        match stack.last_mut() {
            None => docs.push(node),
            Some(Frame::Seq { items, .. }) => items.push(node),
            Some(Frame::Map { entries, key, .. }) => match key.take() {
                None => *key = Some(node),
                Some(k) => entries.push((k, node)),
            },
        }
    }
    Ok(Yaml { docs, warnings })
}

/// Whether a string can be written without quotes and still read back as this string.
fn plain_ok(s: &str, key: bool) -> bool {
    let Some(first) = s.chars().next() else { return false };
    let second = s.chars().nth(1);
    if first.is_whitespace() || s.ends_with(char::is_whitespace) || s.ends_with(':') {
        return false;
    }
    if "[]{},#&*!|>'\"%@`".contains(first) {
        return false;
    }
    if matches!(first, '-' | '?' | ':') && second.is_none_or(char::is_whitespace) {
        return false;
    }
    // `12:30` is a base-60 number to YAML 1.1 readers.
    if first.is_ascii_digit() && s.contains(':') {
        return false;
    }
    if s.contains(": ") || s.contains(" #") || s.chars().any(|c| c.is_control()) {
        return false;
    }
    if key && (s.contains(['[', ']', '{', '}', ','])) {
        return false;
    }
    // Strings that would read back as another type, also by YAML 1.1 rules (yes/no/on/off).
    let yaml11 = matches!(s.to_ascii_lowercase().as_str(), "y" | "n" | "yes" | "no" | "on" | "off" | "<<" | "=");
    !yaml11 && matches!(resolve_plain(s), Scalar::Str(_)) && s != "---" && s != "..."
}

fn quoted(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// A scalar on one line.
fn scalar_text(s: &Scalar, key: bool) -> String {
    match s {
        Scalar::Null => "null".into(),
        Scalar::Bool(b) => b.to_string(),
        Scalar::Int(v) | Scalar::Float(v) => v.canonical.clone(),
        Scalar::Str(v) if plain_ok(v, key) => v.clone(),
        Scalar::Str(v) => quoted(v),
        Scalar::Tagged(t, v) => format!("{t} {}", if plain_ok(v, key) { v.clone() } else { quoted(v) }),
    }
}

/// Multi-line strings as a literal block: the header (`|`, `|-`, `|+`) and the lines.
fn literal_block(s: &str) -> Option<(&'static str, Vec<&str>)> {
    if !s.contains('\n')
        || s.starts_with([' ', '\t', '\n'])
        || s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return None;
    }
    let body = s.trim_end_matches('\n');
    let trailing = s.len() - body.len();
    let header = match trailing {
        0 => "|-",
        1 => "|",
        _ => "|+",
    };
    let mut lines: Vec<&str> = body.split('\n').collect();
    if trailing > 1 {
        lines.extend(std::iter::repeat_n("", trailing - 1));
    }
    Some((header, lines))
}

/// Writes `node` after `prefix` (`key:`, `-`, or nothing for a document root) at `indent` spaces.
fn emit(out: &mut String, node: &Node, indent: usize, prefix: &str) {
    let pad = " ".repeat(indent);
    let lead = if prefix.is_empty() { pad.clone() } else { format!("{pad}{prefix} ") };
    let tag = |t: &Option<String>| t.as_ref().map(|t| format!("{t} ")).unwrap_or_default();
    match node {
        Node::Scalar(Scalar::Str(s)) if literal_block(s).is_some() => {
            let (header, lines) = literal_block(s).unwrap();
            let _ = writeln!(out, "{lead}{header}");
            for l in lines {
                if l.is_empty() {
                    out.push('\n');
                } else {
                    let _ = writeln!(out, "{pad}  {l}");
                }
            }
        }
        Node::Scalar(s) => {
            let _ = writeln!(out, "{lead}{}", scalar_text(s, false));
        }
        Node::Seq(items, t) if items.is_empty() => {
            let _ = writeln!(out, "{lead}{}[]", tag(t));
        }
        Node::Map(entries, t) if entries.is_empty() => {
            let _ = writeln!(out, "{lead}{}{{}}", tag(t));
        }
        Node::Seq(_, t) | Node::Map(_, t) if prefix == "-" && t.is_none() => {
            // `- a: 1` / `- - x`: the first line of the child joins the dash.
            let start = out.len();
            emit_children(out, node, indent + 2);
            out.replace_range(start..start + indent + 2, &format!("{pad}- "));
        }
        Node::Seq(_, t) | Node::Map(_, t) => {
            let child_indent = if prefix.is_empty() { indent } else { indent + 2 };
            if let Some(t) = t {
                let _ = writeln!(out, "{}", format!("{lead}{t}").trim_end());
            } else if !prefix.is_empty() {
                let _ = writeln!(out, "{}", lead.trim_end());
            }
            emit_children(out, node, child_indent);
        }
    }
}

fn emit_children(out: &mut String, node: &Node, indent: usize) {
    match node {
        Node::Seq(items, _) => {
            for item in items {
                emit(out, item, indent, "-");
            }
        }
        Node::Map(entries, _) => {
            for (k, v) in entries {
                let key = match k {
                    Node::Scalar(s) => scalar_text(s, true),
                    other => quoted(&key_text(other)),
                };
                emit(out, v, indent, &format!("{key}:"));
            }
        }
        Node::Scalar(_) => {}
    }
}

/// YAML with keys sorted and one layout; documents separated by `---`.
pub fn normalized_yaml(data: &[u8]) -> Result<String> {
    let text = crate::text::decode(data).text;
    let yaml = parse_yaml(&text)?;
    let mut out = String::with_capacity(text.len());
    for w in &yaml.warnings {
        let _ = writeln!(out, "# ! {w}");
    }
    let multi = yaml.docs.len() > 1;
    for doc in &yaml.docs {
        if multi {
            out.push_str("---\n");
        }
        emit(&mut out, doc, 0, "");
    }
    Ok(out)
}

/// TOML with tables and keys sorted, in the `toml` crate's layout.
pub fn normalized_toml(data: &[u8]) -> Result<String> {
    let table = parse_toml(data)?;
    toml::to_string_pretty(&table).map_err(|e| Error::Unsupported(format!("TOML could not be written: {e}")))
}

fn parse_toml(data: &[u8]) -> Result<toml::Table> {
    let text = crate::text::decode(data).text;
    text.parse::<toml::Table>().map_err(|e| Error::Unsupported(format!("not readable as TOML: {}", e.message())))
}

/// Whether two YAML files hold the same data. `None` when either does not parse.
pub fn same_yaml(left: &[u8], right: &[u8]) -> Option<bool> {
    let l = parse_yaml(&crate::text::decode(left).text).ok()?;
    let r = parse_yaml(&crate::text::decode(right).text).ok()?;
    Some(l.docs == r.docs)
}

/// Whether two TOML files hold the same data. `None` when either does not parse.
pub fn same_toml(left: &[u8], right: &[u8]) -> Option<bool> {
    Some(parse_toml(left).ok()? == parse_toml(right).ok()?)
}

/// A parsed YAML document for other modules (pnpm and yarn lockfiles).
pub(crate) struct YamlDoc(Node);

/// The first document of a YAML file, `None` when it does not parse.
pub(crate) fn yaml_document(data: &[u8]) -> Option<YamlDoc> {
    let yaml = parse_yaml(&crate::text::decode(data).text).ok()?;
    yaml.docs.into_iter().next().map(YamlDoc)
}

impl YamlDoc {
    pub fn root(&self) -> YamlRef<'_> {
        YamlRef(&self.0)
    }
}

/// A read-only handle on a node of a [`YamlDoc`].
#[derive(Clone, Copy)]
pub(crate) struct YamlRef<'a>(&'a Node);

impl<'a> YamlRef<'a> {
    /// The entries of a mapping, keys as text; empty for other nodes.
    pub fn entries(self) -> impl Iterator<Item = (String, YamlRef<'a>)> {
        let entries: &'a [(Node, Node)] = match self.0 {
            Node::Map(entries, _) => entries,
            _ => &[],
        };
        entries.iter().map(|(k, v)| (scalar_str(k).unwrap_or_else(|| key_text(k)), YamlRef(v)))
    }

    pub fn get(self, key: &str) -> Option<YamlRef<'a>> {
        match self.0 {
            Node::Map(entries, _) => entries.iter().find(|(k, _)| scalar_is(k, key)).map(|(_, v)| YamlRef(v)),
            _ => None,
        }
    }

    pub fn as_str(self) -> Option<String> {
        scalar_str(self.0)
    }
}

fn scalar_is(node: &Node, key: &str) -> bool {
    matches!(node, Node::Scalar(Scalar::Str(s)) if s == key)
}

fn scalar_str(node: &Node) -> Option<String> {
    match node {
        Node::Scalar(Scalar::Str(s) | Scalar::Tagged(_, s)) => Some(s.clone()),
        Node::Scalar(Scalar::Int(n) | Scalar::Float(n)) => Some(n.raw.clone()),
        Node::Scalar(Scalar::Bool(b)) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/data/", $name))
        };
    }

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn yaml_is_sorted_and_canonical() {
        let y = normalized_yaml(
            b"b: 0x1F\na: 'x'\nc: ~\nd: [1, 'two', 3.50]\ne: {z: yes, y: \"no\"}\nf: |\n  line 1\n  line 2\n",
        )
        .unwrap();
        assert_eq!(
            y,
            "a: x\nb: 31\nc: null\nd:\n  - 1\n  - two\n  - 3.5\ne:\n  \"y\": \"no\"\n  z: \"yes\"\nf: |\n  line 1\n  line 2\n"
        );
        // The output reads back as the same data.
        assert_eq!(
            same_yaml(
                y.as_bytes(),
                b"b: 31\na: x\nc:\nd: [1, two, 3.5]\ne: {z: 'yes', y: 'no'}\nf: \"line 1\\nline 2\\n\""
            ),
            Some(true)
        );
    }

    #[test]
    fn anchors_merges_tags_and_documents() {
        let y = normalized_yaml(
            b"base: &b {image: app, env: {A: 1}}\nweb:\n  <<: *b\n  image: web\nlist:\n  - {x: 1, y: [a, b]}\n  - [p, q]\nsecret: !vault abc\n---\n- !!str 12\n- !!int 7\n",
        )
        .unwrap();
        assert_eq!(
            y,
            "---\nbase:\n  env:\n    A: 1\n  image: app\nlist:\n  - x: 1\n    \"y\":\n      - a\n      - b\n  - - p\n    - q\nsecret: !vault abc\nweb:\n  env:\n    A: 1\n  image: web\n---\n- \"12\"\n- 7\n"
        );
    }

    #[test]
    fn duplicate_keys_and_hostile_input() {
        let y = normalized_yaml(b"a: 1\na: 2\n").unwrap();
        assert_eq!(y, "# ! duplicate key a, the last one is used\na: 2\n");
        let laughs = b"a: &a [x, x, x, x, x, x, x, x, x, x]\nb: &b [*a, *a, *a, *a, *a, *a, *a, *a, *a, *a]\nc: &c [*b, *b, *b, *b, *b, *b, *b, *b, *b, *b]\nd: &d [*c, *c, *c, *c, *c, *c, *c, *c, *c, *c]\ne: &e [*d, *d, *d, *d, *d, *d, *d, *d, *d, *d]\nf: &f [*e, *e, *e, *e, *e, *e, *e, *e, *e, *e]\ng: &g [*f, *f, *f, *f, *f, *f, *f, *f, *f, *f]\n";
        assert!(normalized_yaml(laughs).is_err());
        let deep = "[".repeat(10_000);
        assert!(normalized_yaml(deep.as_bytes()).is_err());
        assert!(normalized_yaml(b"a: [1, 2\nb: {").is_err());
        assert!(normalized_yaml(b"*unknown").is_err());
        assert_eq!(normalized_yaml(b"").unwrap(), "");
        assert_eq!(normalized_yaml(b"just text").unwrap(), "just text\n");
    }

    #[test]
    fn real_files() {
        let (a, b) = (fixture!("compose.yaml"), fixture!("compose-reformatted.yaml"));
        assert_eq!(same_yaml(a, b), Some(true));
        assert_eq!(normalized_yaml(a).unwrap(), normalized_yaml(b).unwrap());
        let c = fixture!("compose-changed.yaml");
        assert_eq!(same_yaml(a, c), Some(false));
        let (na, nc) = (normalized_yaml(a).unwrap(), normalized_yaml(c).unwrap());
        assert_eq!(lines_only_in(&na, &nc), ["      - \"8080:80\""], "{na}");

        let (t, t2) = (fixture!("Cargo.toml"), fixture!("Cargo-reordered.toml"));
        assert_eq!(same_toml(t, t2), Some(true));
        let n = normalized_toml(t).unwrap();
        assert!(n.starts_with("[dependencies]\n"), "{n}");
        assert_eq!(n, normalized_toml(t2).unwrap());
        assert_eq!(same_toml(t, b"[package]\nname = \"x\"\n"), Some(false));
        assert_eq!(same_toml(t, b"[package\n"), None);
    }
}
