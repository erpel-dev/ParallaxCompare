//! AUTOSAR ECU configuration values (EcucValues ARXML) as simplified JSON
//! that can be edited and written back.
//!
//! Each module becomes an object keyed by its short name. Inside a module or
//! container, parameters and references are keyed by the last segment of their
//! definition, sub-containers are grouped by their definition and keyed by
//! their short name. Keys are sorted, numbers and booleans normalized and the
//! package of the file's modules is left out of references, so files that
//! differ only in order, UUIDs, admin data, number spelling or package names
//! give the same text.
//!
//! Writing back patches the original file: only the values, containers and
//! modules that differ from the edited text are changed, everything else keeps
//! its bytes. Elements that are new are built like the same definition in the
//! file or in the file on the other side; the result is checked by reading it
//! again before anything is saved.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::ops::Range;

use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// Whether the data holds ECUC module configuration values.
pub fn is_ecuc(data: &[u8]) -> bool {
    memchr::memmem::find(data, b"<ECUC-MODULE-CONFIGURATION-VALUES").is_some()
}

/// The simplified JSON of every module configuration in the file.
pub fn render(xml: &str) -> Result<String, String> {
    let root = parse_xml(xml)?;
    let doc = XDoc::read(&root);
    Ok(to_json(&doc.model()))
}

/// The file with the configuration of `edited` (simplified JSON) applied to
/// `original` (ARXML). `donor` is the file on the other side, which tells how
/// to write parameters and containers that `original` does not have yet.
pub fn write_back(original: &str, edited: &str, donor: Option<&str>) -> Result<String, String> {
    let root = parse_xml(original)?;
    let doc = XDoc::read(&root);
    if let Some(name) = &doc.ambiguous {
        return Err(format!("{name} occurs twice in the file, so edits cannot be placed"));
    }
    let wanted = from_json(edited)?;
    if same_model(&doc.model(), &wanted).is_none() {
        return Ok(original.to_string());
    }
    let donor_root = donor.and_then(|d| parse_xml(d).ok());
    let mut templates = Templates::default();
    templates.collect(&doc);
    if let Some(d) = &donor_root {
        templates.collect(&XDoc::read(d));
    }
    let nl = if original.contains("\r\n") { "\r\n" } else { "\n" };
    let mut w =
        Writer { src: original, patches: Vec::new(), unit: indent_unit(original, &root), nl, doc: &doc, templates };
    w.sync_modules(&wanted)?;
    let out = w.apply()?;
    // Read back: the saved file must hold exactly the edited configuration.
    let check_root = parse_xml(&out).map_err(|e| format!("the written file would not be valid XML ({e})"))?;
    let check = XDoc::read(&check_root);
    if let Some(path) = same_model(&check.model(), &wanted) {
        return Err(format!("writing {path} did not give the edited value; nothing was saved"));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// XML tree with byte spans

#[derive(Debug)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    /// From the `<` of the start tag to after the end tag.
    span: Range<usize>,
    /// Between the tags; for a self-closing tag the empty range at its end.
    inner: Range<usize>,
    empty: bool,
    children: Vec<El>,
    text: String,
}

impl El {
    fn child(&self, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.name == name)
    }

    fn named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a El> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    fn text_of(&self, name: &str) -> Option<String> {
        self.child(name).map(|c| c.text.trim().to_string())
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn parse_xml(xml: &str) -> Result<El, String> {
    let mut reader = Reader::from_str(xml);
    let new = |name: String, attrs, start: usize| El {
        name,
        attrs,
        span: start..start,
        inner: start..start,
        empty: false,
        children: Vec::new(),
        text: String::new(),
    };
    let mut stack = vec![new(String::new(), Vec::new(), 0)];
    loop {
        let before = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|e| format!("XML error at byte {before}: {e}"))?;
        let after = reader.buffer_position() as usize;
        let attrs_of = |e: &quick_xml::events::BytesStart<'_>| {
            e.attributes()
                .flatten()
                .map(|a| {
                    let key = a.key.local_name().as_ref().to_string();
                    (key, a.value.into_owned())
                })
                .collect::<Vec<_>>()
        };
        match event {
            Event::Start(e) => {
                let name = e.local_name().as_ref().to_string();
                let mut el = new(name, attrs_of(&e), before);
                el.inner = after..after;
                stack.push(el);
            }
            Event::Empty(e) => {
                let name = e.local_name().as_ref().to_string();
                let mut el = new(name, attrs_of(&e), before);
                el.span = before..after;
                el.inner = after..after;
                el.empty = true;
                stack.last_mut().expect("root").children.push(el);
            }
            Event::End(_) => {
                let mut el = stack.pop().expect("balanced");
                if stack.is_empty() {
                    return Err("unbalanced end tag".into());
                }
                el.inner.end = before;
                el.span.end = after;
                stack.last_mut().expect("root").children.push(el);
            }
            Event::Text(t) => stack.last_mut().expect("root").text.push_str(&t.xml10_content()),
            Event::CData(t) => stack.last_mut().expect("root").text.push_str(&t),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => match &*r {
                        "amp" => "&".into(),
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "quot" => "\"".into(),
                        "apos" => "'".into(),
                        other => format!("&{other};"),
                    },
                };
                stack.last_mut().expect("root").text.push_str(&resolved);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err("the XML ends inside an element".into());
    }
    let mut root = stack.pop().expect("root");
    root.span = 0..xml.len();
    root.inner = 0..xml.len();
    Ok(root)
}

// ---------------------------------------------------------------------------
// The model shown as JSON

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Null,
    Bool(bool),
    /// A normalized number: integers in decimal, floats in their shortest form.
    Num(String),
    Str(String),
    /// A reference path without the arrow; instance references join their
    /// contexts and target with ` ▸ `.
    Ref(String),
}

impl Val {
    /// The value as compared: a number written as text equals the number.
    fn key(&self) -> String {
        match self {
            Val::Null => "\u{0}null".into(),
            Val::Bool(b) => b.to_string(),
            Val::Num(n) | Val::Str(n) => n.clone(),
            Val::Ref(r) => format!("{ARROW}{r}"),
        }
    }
}

const ARROW: &str = "→ ";
const STEP: &str = " ▸ ";

#[derive(Debug, Default, Clone)]
struct Node {
    /// `@def`, `@index`, `@variant` …
    attrs: BTreeMap<String, Val>,
    values: BTreeMap<String, Vec<Val>>,
    groups: BTreeMap<String, BTreeMap<String, Node>>,
}

type Model = BTreeMap<String, Node>;

/// The first path where two models differ, or None when they are the same.
fn same_model(a: &Model, b: &Model) -> Option<String> {
    for name in a.keys().chain(b.keys()) {
        match (a.get(name), b.get(name)) {
            (Some(x), Some(y)) => {
                if let Some(p) = same_node(x, y, name) {
                    return Some(p);
                }
            }
            _ => return Some(name.clone()),
        }
    }
    None
}

fn same_node(a: &Node, b: &Node, path: &str) -> Option<String> {
    let keys = |v: &[Val]| {
        let mut k: Vec<String> = v.iter().map(Val::key).collect();
        k.sort();
        k
    };
    for k in a.attrs.keys().chain(b.attrs.keys()) {
        if a.attrs.get(k).map(Val::key) != b.attrs.get(k).map(Val::key) {
            return Some(format!("{path}/{k}"));
        }
    }
    for k in a.values.keys().chain(b.values.keys()) {
        let empty = Vec::new();
        if keys(a.values.get(k).unwrap_or(&empty)) != keys(b.values.get(k).unwrap_or(&empty)) {
            return Some(format!("{path}/{k}"));
        }
    }
    for g in a.groups.keys().chain(b.groups.keys()) {
        let empty = BTreeMap::new();
        let (x, y) = (a.groups.get(g).unwrap_or(&empty), b.groups.get(g).unwrap_or(&empty));
        for n in x.keys().chain(y.keys()) {
            match (x.get(n), y.get(n)) {
                (Some(p), Some(q)) => {
                    if let Some(d) = same_node(p, q, &format!("{path}/{n}")) {
                        return Some(d);
                    }
                }
                _ => return Some(format!("{path}/{n}")),
            }
        }
    }
    None
}

const ATTR_ORDER: [&str; 6] = ["@def", "@index", "@variant", "@edition", "@implementation", "@postBuildVariantUsed"];

fn to_json(model: &Model) -> String {
    let mut out = String::from("{\n");
    let n = model.len();
    for (i, (name, node)) in model.iter().enumerate() {
        write_node(&mut out, name, node, 1, i + 1 == n);
    }
    out.push_str("}\n");
    out
}

/// One member of a module or container, in the order they are shown.
enum Entry<'a> {
    Attr(&'a str, &'a Val),
    Values(&'a str, &'a [Val]),
    Group(&'a str, &'a BTreeMap<String, Node>),
}

fn write_node(out: &mut String, name: &str, node: &Node, depth: usize, last: bool) {
    let pad = "  ".repeat(depth);
    let inner = "  ".repeat(depth + 1);
    let mut attrs: Vec<_> = node.attrs.iter().collect();
    attrs.sort_by_key(|(k, _)| (ATTR_ORDER.iter().position(|a| a == k).unwrap_or(ATTR_ORDER.len()), k.as_str()));
    let entries: Vec<Entry<'_>> = attrs
        .into_iter()
        .map(|(k, v)| Entry::Attr(k, v))
        .chain(node.values.iter().map(|(k, vs)| Entry::Values(k, vs)))
        .chain(node.groups.iter().map(|(g, is)| Entry::Group(g, is)))
        .collect();
    if entries.is_empty() {
        let _ = writeln!(out, "{pad}{}: {{}}{}", json_str(name), comma(last));
        return;
    }
    let _ = writeln!(out, "{pad}{}: {{", json_str(name));
    let n = entries.len();
    for (i, e) in entries.into_iter().enumerate() {
        let last = i + 1 == n;
        match e {
            Entry::Attr(k, v) => {
                let _ = writeln!(out, "{inner}{}: {}{}", json_str(k), json_val(v), comma(last));
            }
            Entry::Values(k, [v]) => {
                let _ = writeln!(out, "{inner}{}: {}{}", json_str(k), json_val(v), comma(last));
            }
            Entry::Values(k, vs) => {
                let mut sorted: Vec<&Val> = vs.iter().collect();
                sorted.sort_by_key(|v| v.key());
                let _ = writeln!(out, "{inner}{}: [", json_str(k));
                for (j, v) in sorted.iter().enumerate() {
                    let _ = writeln!(out, "{inner}  {}{}", json_val(v), comma(j + 1 == sorted.len()));
                }
                let _ = writeln!(out, "{inner}]{}", comma(last));
            }
            Entry::Group(g, instances) => {
                let _ = writeln!(out, "{inner}{}: {{", json_str(g));
                let m = instances.len();
                for (j, (iname, inode)) in instances.iter().enumerate() {
                    write_node(out, iname, inode, depth + 2, j + 1 == m);
                }
                let _ = writeln!(out, "{inner}}}{}", comma(last));
            }
        }
    }
    let _ = writeln!(out, "{pad}}}{}", comma(last));
}

fn comma(last: bool) -> &'static str {
    if last { "" } else { "," }
}

fn json_val(v: &Val) -> String {
    match v {
        Val::Null => "null".into(),
        Val::Bool(b) => b.to_string(),
        Val::Num(n) => n.clone(),
        Val::Str(s) => json_str(s),
        Val::Ref(r) => json_str(&format!("{ARROW}{r}")),
    }
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// AUTOSAR numerical values: decimal, `0x` hex, `0b` binary and octal with a leading zero.
fn canon_int(s: &str) -> Option<String> {
    let s = s.trim();
    let (neg, body) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let value = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i128::from_str_radix(h, 16).ok()?
    } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
        i128::from_str_radix(b, 2).ok()?
    } else if body.len() > 1 && body.starts_with('0') {
        i128::from_str_radix(&body[1..], 8).ok()?
    } else if !body.is_empty() && body.bytes().all(|b| b.is_ascii_digit()) {
        body.parse::<i128>().ok()?
    } else {
        return None;
    };
    Some(if neg && value != 0 { format!("-{value}") } else { value.to_string() })
}

fn canon_float(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')) {
        return None;
    }
    let f: f64 = s.parse().ok()?;
    f.is_finite().then(|| format!("{f:?}"))
}

fn numerical(text: &str, dest: &str) -> Val {
    let t = text.trim();
    if t.is_empty() {
        return Val::Null;
    }
    let boolean = || match t.to_ascii_lowercase().as_str() {
        "true" | "1" => Some(Val::Bool(true)),
        "false" | "0" => Some(Val::Bool(false)),
        _ => None,
    };
    let number = || canon_int(t).or_else(|| canon_float(t)).map(Val::Num);
    let v = match dest {
        "ECUC-BOOLEAN-PARAM-DEF" => boolean(),
        "ECUC-FLOAT-PARAM-DEF" => canon_float(t).or_else(|| canon_int(t).and_then(|i| canon_float(&i))).map(Val::Num),
        "ECUC-INTEGER-PARAM-DEF" => number(),
        _ => number().or_else(|| matches!(t, "true" | "false").then(boolean).flatten()),
    };
    v.unwrap_or_else(|| Val::Str(t.to_string()))
}

// ---------------------------------------------------------------------------
// The file read with the elements behind every value

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Numerical,
    Textual,
    Reference,
    Instance,
}

struct XVal<'a> {
    el: &'a El,
    kind: Kind,
    dest: String,
    val: Val,
}

struct XNode<'a> {
    el: &'a El,
    def: String,
    attrs: BTreeMap<String, (Val, Option<&'a El>)>,
    values: BTreeMap<String, Vec<XVal<'a>>>,
    groups: BTreeMap<String, BTreeMap<String, XNode<'a>>>,
}

struct XModule<'a> {
    /// The ELEMENTS of the package the module is in.
    elements: &'a El,
    package: String,
    node: XNode<'a>,
}

struct XDoc<'a> {
    modules: BTreeMap<String, XModule<'a>>,
    /// Packages holding module configurations, longest first.
    packages: Vec<String>,
    ambiguous: Option<String>,
}

impl<'a> XDoc<'a> {
    fn read(root: &'a El) -> XDoc<'a> {
        let mut found = Vec::new();
        fn walk<'a>(el: &'a El, path: &str, found: &mut Vec<(String, &'a El, &'a El)>) {
            for c in &el.children {
                match c.name.as_str() {
                    "AR-PACKAGE" => {
                        let p = format!("{path}/{}", c.text_of("SHORT-NAME").unwrap_or_default());
                        walk(c, &p, found);
                    }
                    "ELEMENTS" => {
                        for m in c.named("ECUC-MODULE-CONFIGURATION-VALUES") {
                            found.push((path.to_string(), c, m));
                        }
                    }
                    "AUTOSAR" | "AR-PACKAGES" => walk(c, path, found),
                    _ => {}
                }
            }
        }
        walk(root, "", &mut found);
        let mut packages: Vec<String> = found.iter().map(|(p, _, _)| p.clone()).collect();
        packages.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        packages.dedup();
        let mut doc = XDoc { modules: BTreeMap::new(), packages, ambiguous: None };
        for (package, elements, m) in found {
            let name = m.text_of("SHORT-NAME").unwrap_or_default();
            let node = doc.module(m);
            if doc.modules.contains_key(&name) {
                doc.ambiguous.get_or_insert(name.clone());
                continue;
            }
            doc.modules.insert(name, XModule { elements, package, node });
        }
        doc
    }

    fn rel(&self, path: &str) -> String {
        let path = path.trim();
        for p in &self.packages {
            if let Some(rest) = path.strip_prefix(p.as_str()).and_then(|r| r.strip_prefix('/'))
                && !p.is_empty()
            {
                return rest.to_string();
            }
        }
        path.to_string()
    }

    fn abs(&self, path: &str) -> Result<String, String> {
        if path.starts_with('/') {
            return Ok(path.to_string());
        }
        let first = path.split('/').next().unwrap_or_default();
        if let Some(m) = self.modules.get(first) {
            return Ok(format!("{}/{path}", m.package));
        }
        match self.packages.as_slice() {
            [only] => Ok(format!("{only}/{path}")),
            _ => Err(format!(
                "cannot tell which package the reference {ARROW}{path} points into; write it with its full path"
            )),
        }
    }

    fn module(&mut self, m: &'a El) -> XNode<'a> {
        let def = m.text_of("DEFINITION-REF").unwrap_or_default();
        let mut node = self.node_contents(m, def.clone(), m.child("CONTAINERS"));
        node.attrs.insert("@def".into(), (Val::Str(def), m.child("DEFINITION-REF")));
        let field = |node: &mut XNode<'a>, key: &str, tag: &str, f: &dyn Fn(String) -> Val| {
            if let Some(el) = m.child(tag) {
                node.attrs.insert(key.into(), (f(el.text.trim().to_string()), Some(el)));
            }
        };
        field(&mut node, "@edition", "ECUC-DEF-EDITION", &Val::Str);
        field(&mut node, "@variant", "IMPLEMENTATION-CONFIG-VARIANT", &|t| {
            Val::Str(t.strip_prefix("VARIANT-").map(str::to_string).unwrap_or(t))
        });
        field(&mut node, "@implementation", "MODULE-DESCRIPTION-REF", &Val::Str);
        field(&mut node, "@postBuildVariantUsed", "POST-BUILD-VARIANT-USED", &|t| match t.as_str() {
            "true" | "1" => Val::Bool(true),
            "false" | "0" => Val::Bool(false),
            _ => Val::Str(t),
        });
        node
    }

    fn container(&mut self, c: &'a El, parent_def: &str, group: &str) -> XNode<'a> {
        let def = c.text_of("DEFINITION-REF").unwrap_or_default();
        let mut node = self.node_contents(c, def.clone(), c.child("SUB-CONTAINERS"));
        if def != format!("{parent_def}/{group}") {
            node.attrs.insert("@def".into(), (Val::Str(def), c.child("DEFINITION-REF")));
        }
        if let Some(i) = c.child("INDEX") {
            node.attrs.insert("@index".into(), (numerical(&i.text, "ECUC-INTEGER-PARAM-DEF"), Some(i)));
        }
        node
    }

    fn node_contents(&mut self, el: &'a El, def: String, subs: Option<&'a El>) -> XNode<'a> {
        let mut node =
            XNode { el, def: def.clone(), attrs: BTreeMap::new(), values: BTreeMap::new(), groups: BTreeMap::new() };
        let wrappers = [el.child("PARAMETER-VALUES"), el.child("REFERENCE-VALUES")];
        for v in wrappers.into_iter().flatten().flat_map(|w| w.children.iter()) {
            let Some(xv) = self.value(v) else { continue };
            let name = last_segment(&v.text_of("DEFINITION-REF").unwrap_or_default()).to_string();
            node.values.entry(name).or_default().push(xv);
        }
        for c in subs.into_iter().flat_map(|s| s.named("ECUC-CONTAINER-VALUE")) {
            let cdef = c.text_of("DEFINITION-REF").unwrap_or_default();
            let group = last_segment(&cdef).to_string();
            let name = c.text_of("SHORT-NAME").unwrap_or_default();
            let child = self.container(c, &def, &group);
            let instances = node.groups.entry(group).or_default();
            if instances.contains_key(&name) {
                self.ambiguous.get_or_insert(name.clone());
                continue;
            }
            instances.insert(name, child);
        }
        node
    }

    fn value(&self, v: &'a El) -> Option<XVal<'a>> {
        let dest = v.child("DEFINITION-REF").and_then(|d| d.attr("DEST")).unwrap_or_default().to_string();
        let (kind, val) = match v.name.as_str() {
            "ECUC-NUMERICAL-PARAM-VALUE" => {
                (Kind::Numerical, v.child("VALUE").map_or(Val::Null, |x| numerical(&x.text, &dest)))
            }
            "ECUC-TEXTUAL-PARAM-VALUE" => {
                (Kind::Textual, v.child("VALUE").map_or(Val::Null, |x| Val::Str(x.text.clone())))
            }
            "ECUC-REFERENCE-VALUE" => {
                let r = v.child("VALUE-REF").map(|x| x.text.trim()).filter(|t| !t.is_empty());
                (Kind::Reference, r.map_or(Val::Null, |r| Val::Ref(self.rel(r))))
            }
            "ECUC-INSTANCE-REFERENCE-VALUE" => {
                let val = v.child("VALUE-IREF").map_or(Val::Null, |i| {
                    let mut parts: Vec<String> = i.named("CONTEXT-ELEMENT-REF").map(|c| self.rel(&c.text)).collect();
                    parts.extend(i.child("TARGET-REF").map(|t| self.rel(&t.text)));
                    Val::Ref(parts.join(STEP))
                });
                (Kind::Instance, val)
            }
            _ => return None,
        };
        Some(XVal { el: v, kind, dest, val })
    }

    fn model(&self) -> Model {
        self.modules.iter().map(|(k, m)| (k.clone(), m.node.model())).collect()
    }
}

impl XNode<'_> {
    fn model(&self) -> Node {
        Node {
            attrs: self.attrs.iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect(),
            values: self.values.iter().map(|(k, vs)| (k.clone(), vs.iter().map(|v| v.val.clone()).collect())).collect(),
            groups: self
                .groups
                .iter()
                .map(|(g, is)| (g.clone(), is.iter().map(|(n, c)| (n.clone(), c.model())).collect()))
                .collect(),
        }
    }
}

fn last_segment(path: &str) -> &str {
    path.trim().rsplit('/').next().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Lenient JSON: commas between members are optional and may trail, so lines
// copied from the other side never break the syntax.

#[derive(Debug)]
enum J {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, usize, J)>),
}

struct Parser<'a> {
    s: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn line(&self) -> usize {
        line_of(self.s, self.i)
    }

    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("line {}: {msg}", self.line()))
    }

    fn peek(&self) -> Option<u8> {
        self.s.as_bytes().get(self.i).copied()
    }

    fn skip(&mut self, commas: bool) {
        loop {
            let rest = &self.s[self.i..];
            if let Some(c) = rest.chars().next()
                && (c.is_whitespace() || (commas && c == ','))
            {
                self.i += c.len_utf8();
            } else if rest.starts_with("//") {
                self.i += rest.find('\n').unwrap_or(rest.len());
            } else if let Some(body) = rest.strip_prefix("/*") {
                self.i += body.find("*/").map_or(rest.len(), |n| n + 4);
            } else {
                return;
            }
        }
    }

    fn value(&mut self) -> Result<J, String> {
        self.skip(false);
        match self.peek() {
            Some(b'{') => {
                self.i += 1;
                let mut members: Vec<(String, usize, J)> = Vec::new();
                loop {
                    self.skip(true);
                    match self.peek() {
                        Some(b'}') => {
                            self.i += 1;
                            let mut seen = std::collections::HashSet::new();
                            if let Some((k, at, _)) = members.iter().find(|(k, _, _)| !seen.insert(k.as_str())) {
                                return Err(format!("line {}: {} occurs twice", line_of(self.s, *at), json_str(k)));
                            }
                            return Ok(J::Obj(members));
                        }
                        Some(b'"') => {
                            let line = self.i;
                            let key = self.string()?;
                            self.skip(false);
                            if self.peek() != Some(b':') {
                                return self.err(&format!("expected ':' after {}", json_str(&key)));
                            }
                            self.i += 1;
                            let v = self.value()?;
                            members.push((key, line, v));
                        }
                        None => return self.err("the text ends inside an object; a '}' is missing"),
                        _ => return self.err("expected a quoted name or '}'"),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip(true);
                    match self.peek() {
                        Some(b']') => {
                            self.i += 1;
                            return Ok(J::Arr(items));
                        }
                        None => return self.err("the text ends inside a list; a ']' is missing"),
                        _ => items.push(self.value()?),
                    }
                }
            }
            Some(b'"') => Ok(J::Str(self.string()?)),
            Some(_) => {
                let rest = &self.s[self.i..];
                let len = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_')))
                    .unwrap_or(rest.len());
                let word = &rest[..len];
                if len == 0 {
                    return self.err(&format!("unexpected '{}'", rest.chars().next().unwrap_or(' ')));
                }
                let v = match word {
                    "null" => J::Null,
                    "true" => J::Bool(true),
                    "false" => J::Bool(false),
                    w => match canon_int(w).or_else(|| canon_float(w)) {
                        Some(n) => J::Num(n),
                        None => return self.err(&format!("{w} is not a number; text needs quotes")),
                    },
                };
                self.i += len;
                Ok(v)
            }
            None => self.err("a value is missing"),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        let start = self.i;
        self.i += 1;
        let mut out = String::new();
        let bytes = self.s.as_bytes();
        loop {
            let Some(&b) = bytes.get(self.i) else {
                return Err(format!("line {}: a string is not closed", line_of(self.s, start)));
            };
            match b {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\n' => return Err(format!("line {}: a string is not closed", line_of(self.s, start))),
                b'\\' => {
                    let e = bytes.get(self.i + 1).copied();
                    self.i += 2;
                    match e {
                        Some(b'n') => out.push('\n'),
                        Some(b't') => out.push('\t'),
                        Some(b'r') => out.push('\r'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'u') => {
                            let hex =
                                |p: &Self, at: usize| p.s.get(at..at + 4).and_then(|h| u32::from_str_radix(h, 16).ok());
                            let Some(mut c) = hex(self, self.i) else { return self.err("bad \\u escape") };
                            self.i += 4;
                            if (0xD800..0xDC00).contains(&c)
                                && self.s[self.i..].starts_with("\\u")
                                && let Some(lo) = hex(self, self.i + 2)
                            {
                                c = 0x10000 + ((c - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                                self.i += 6;
                            }
                            out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                        }
                        Some(c) => out.push(c as char),
                        None => return self.err("a string is not closed"),
                    }
                }
                _ => {
                    let c = self.s[self.i..].chars().next().expect("in bounds");
                    out.push(c);
                    self.i += c.len_utf8();
                }
            }
        }
    }
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// The model of edited JSON; `at` in the tree are byte offsets, turned into
/// line numbers for messages.
fn from_json(text: &str) -> Result<Model, String> {
    let mut p = Parser { s: text, i: 0 };
    let top = p.value()?;
    p.skip(true);
    if p.i < text.len() {
        return p.err("text after the end of the configuration");
    }
    let J::Obj(modules) = top else { return Err("line 1: the configuration must be an object of modules".into()) };
    let mut model = Model::new();
    for (name, at, v) in modules {
        model.insert(
            name.clone(),
            node_from_json(v, &name, at, true).map_err(|(at, e)| format!("line {}: {e}", line_of(text, at)))?,
        );
    }
    Ok(model)
}

fn scalar(j: J, what: &str, at: usize) -> Result<Val, (usize, String)> {
    Ok(match j {
        J::Null => Val::Null,
        J::Bool(b) => Val::Bool(b),
        J::Num(n) => Val::Num(n),
        J::Str(s) => match s.strip_prefix(ARROW) {
            Some(r) => Val::Ref(r.trim().to_string()),
            None => Val::Str(s),
        },
        J::Arr(_) | J::Obj(_) => return Err((at, format!("{what} must be a single value"))),
    })
}

fn node_from_json(j: J, name: &str, at: usize, module: bool) -> Result<Node, (usize, String)> {
    let J::Obj(members) = j else {
        return Err((at, format!("{} must be an object", json_str(name))));
    };
    let mut node = Node::default();
    for (key, line, v) in members {
        if key.starts_with('@') {
            if !ATTR_ORDER.contains(&key.as_str()) || (!module && !matches!(key.as_str(), "@def" | "@index")) {
                return Err((line, format!("{key} is not known here")));
            }
            let mut val = scalar(v, &key, line)?;
            if let (Val::Str(s), "@variant") = (&val, key.as_str())
                && let Some(t) = s.strip_prefix("VARIANT-")
            {
                val = Val::Str(t.to_string());
            }
            node.attrs.insert(key, val);
            continue;
        }
        match v {
            J::Obj(instances) => {
                let mut group = BTreeMap::new();
                for (iname, iline, iv) in instances {
                    let child = node_from_json(iv, &iname, iline, false)?;
                    group.insert(iname, child);
                }
                // A group without instances has nothing to write.
                if !group.is_empty() {
                    node.groups.insert(key, group);
                }
            }
            J::Arr(items) => {
                let vals = items.into_iter().map(|i| scalar(i, &key, line)).collect::<Result<Vec<_>, _>>()?;
                node.values.insert(key, vals);
            }
            other => {
                node.values.insert(key.clone(), vec![scalar(other, &key, line)?]);
            }
        }
    }
    if module && !node.attrs.contains_key("@def") {
        return Err((at, format!("module {} needs \"@def\", the path of its definition", json_str(name))));
    }
    Ok(node)
}

// ---------------------------------------------------------------------------
// How new elements are written: taken from the same definition in either file

#[derive(Clone)]
struct ValueTemplate {
    tag: String,
    dest: String,
    value_dest: Option<String>,
    iref_dests: Vec<String>,
    numeric_bools: bool,
}

#[derive(Default)]
struct Templates {
    values: BTreeMap<String, ValueTemplate>,
    containers: BTreeMap<String, String>,
}

impl Templates {
    fn collect(&mut self, doc: &XDoc<'_>) {
        for (name, m) in &doc.modules {
            self.node(&m.node, name);
        }
    }

    fn node(&mut self, node: &XNode<'_>, path: &str) {
        for (name, vs) in &node.values {
            let Some(v) = vs.first() else { continue };
            let value_dest = v.el.child("VALUE-REF").and_then(|r| r.attr("DEST")).map(str::to_string);
            let iref_dests = v
                .el
                .child("VALUE-IREF")
                .map_or(Vec::new(), |i| i.children.iter().filter_map(|c| c.attr("DEST").map(str::to_string)).collect());
            let numeric_bools = v.el.child("VALUE").is_some_and(|x| matches!(x.text.trim(), "0" | "1"));
            self.values.entry(format!("{path}/{name}")).or_insert(ValueTemplate {
                tag: v.el.name.clone(),
                dest: v.dest.clone(),
                value_dest,
                iref_dests,
                numeric_bools,
            });
        }
        for (group, instances) in &node.groups {
            let gpath = format!("{path}/{group}");
            for c in instances.values() {
                if let Some(dest) = c.el.child("DEFINITION-REF").and_then(|d| d.attr("DEST")) {
                    self.containers.entry(gpath.clone()).or_insert_with(|| dest.to_string());
                }
                self.node(c, &gpath);
            }
        }
    }

    fn value(&self, path: &str, name: &str, val: &Val) -> Result<ValueTemplate, String> {
        if let Some(t) = self.values.get(path) {
            return Ok(t.clone());
        }
        let numerical = |dest: &str| ValueTemplate {
            tag: "ECUC-NUMERICAL-PARAM-VALUE".into(),
            dest: dest.into(),
            value_dest: None,
            iref_dests: Vec::new(),
            numeric_bools: false,
        };
        match val {
            Val::Bool(_) => Ok(numerical("ECUC-BOOLEAN-PARAM-DEF")),
            Val::Num(n) if n.contains(['.', 'e', 'E']) => Ok(numerical("ECUC-FLOAT-PARAM-DEF")),
            Val::Num(_) => Ok(numerical("ECUC-INTEGER-PARAM-DEF")),
            Val::Ref(r) if !r.contains(STEP) && !r.starts_with('/') => Ok(ValueTemplate {
                tag: "ECUC-REFERENCE-VALUE".into(),
                dest: "ECUC-REFERENCE-DEF".into(),
                value_dest: Some("ECUC-CONTAINER-VALUE".into()),
                iref_dests: Vec::new(),
                numeric_bools: false,
            }),
            _ => Err(format!(
                "neither file has {name} at {path}, so its kind of parameter is unknown; add it in the Text view"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Writing

/// A piece of XML to insert.
enum X {
    Leaf(String, Vec<(String, String)>, String),
    Node(String, Vec<(String, String)>, Vec<X>),
}

impl X {
    fn leaf(tag: &str, text: &str) -> X {
        X::Leaf(tag.into(), Vec::new(), text.into())
    }

    fn write(&self, out: &mut String, indent: &str, unit: &str, nl: &str) {
        let open = |out: &mut String, tag: &str, attrs: &[(String, String)]| {
            out.push('<');
            out.push_str(tag);
            for (k, v) in attrs {
                let _ = write!(out, " {k}=\"{}\"", escape(v, true));
            }
            out.push('>');
        };
        out.push_str(indent);
        match self {
            X::Leaf(tag, attrs, text) => {
                open(out, tag, attrs);
                out.push_str(&escape(text, false));
                let _ = write!(out, "</{tag}>");
            }
            X::Node(tag, attrs, children) => {
                open(out, tag, attrs);
                out.push_str(nl);
                let deeper = if nl.is_empty() { String::new() } else { format!("{indent}{unit}") };
                for c in children {
                    c.write(out, &deeper, unit, nl);
                    out.push_str(nl);
                }
                let _ = write!(out, "{indent}</{tag}>");
            }
        }
    }
}

fn escape(s: &str, attr: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

fn indent_unit(src: &str, root: &El) -> String {
    // The indentation step between the first element with children and its first child.
    fn find(src: &str, el: &El) -> Option<String> {
        for c in &el.children {
            if let (Some(a), Some(b)) = (indent_at(src, c.span.start), c.children.first())
                && let Some(b) = indent_at(src, b.span.start)
                && b.len() > a.len()
                && b.starts_with(&a)
            {
                return Some(b[a.len()..].to_string());
            }
            if let Some(u) = find(src, c) {
                return Some(u);
            }
        }
        None
    }
    find(src, root).unwrap_or_else(|| "  ".into())
}

/// The whitespace before `pos` on its line, when nothing else precedes it.
fn indent_at(src: &str, pos: usize) -> Option<String> {
    let start = src[..pos].rfind('\n').map_or(0, |i| i + 1);
    let before = &src[start..pos];
    before.chars().all(|c| c == ' ' || c == '\t').then(|| before.to_string())
}

fn line_start(src: &str, pos: usize) -> usize {
    src[..pos].rfind('\n').map_or(0, |i| i + 1)
}

/// After the line break that follows `pos`, when only whitespace is between.
fn after_line(src: &str, pos: usize) -> Option<usize> {
    let rest = &src[pos..];
    let nl = rest.find('\n')?;
    rest[..nl].trim().is_empty().then_some(pos + nl + 1)
}

struct Patch {
    range: Range<usize>,
    text: String,
}

struct Writer<'a> {
    src: &'a str,
    patches: Vec<Patch>,
    unit: String,
    nl: &'static str,
    doc: &'a XDoc<'a>,
    templates: Templates,
}

/// What to add to a module or container once its children are compared.
#[derive(Default)]
struct Pending {
    params: Vec<X>,
    refs: Vec<X>,
    subs: Vec<X>,
}

impl Writer<'_> {
    fn nl(&self) -> &'static str {
        self.nl
    }

    fn replace(&mut self, range: Range<usize>, text: String) {
        self.patches.push(Patch { range, text });
    }

    fn delete(&mut self, el: &El) {
        let range = match (indent_at(self.src, el.span.start), after_line(self.src, el.span.end)) {
            (Some(_), Some(end)) => line_start(self.src, el.span.start)..end,
            _ => el.span.clone(),
        };
        self.replace(range, String::new());
    }

    fn set_text(&mut self, el: &El, text: &str) {
        if el.empty {
            let attrs: Vec<(String, String)> = el.attrs.clone();
            let mut out = String::new();
            X::Leaf(el.name.clone(), attrs, text.into()).write(&mut out, "", "", "");
            self.replace(el.span.clone(), out);
        } else {
            self.replace(el.inner.clone(), escape(text, false));
        }
    }

    /// Inserts `items` as the last children of `parent`.
    fn append(&mut self, parent: &El, items: Vec<X>) {
        if items.is_empty() {
            return;
        }
        let nl = self.nl();
        let indent = indent_at(self.src, parent.span.start);
        if parent.empty {
            let attrs = parent.attrs.clone();
            let mut out = String::new();
            let pretty = indent.is_some();
            X::Node(parent.name.clone(), attrs, items).write(
                &mut out,
                "",
                if pretty { &self.unit } else { "" },
                if pretty { nl } else { "" },
            );
            // The first line has no indentation of its own; nested lines get the parent's.
            let out = match &indent {
                Some(i) if !i.is_empty() => out.replace(nl, &format!("{nl}{i}")),
                _ => out,
            };
            self.replace(parent.span.clone(), out);
            return;
        }
        let end_tag = parent.inner.end;
        let pretty = indent.is_some() && indent_at(self.src, end_tag).is_some();
        let mut out = String::new();
        if pretty {
            let child = format!("{}{}", indent.unwrap_or_default(), self.unit);
            for item in items {
                item.write(&mut out, &child, &self.unit, nl);
                out.push_str(nl);
            }
            self.replace(line_start(self.src, end_tag)..line_start(self.src, end_tag), out);
        } else {
            for item in items {
                item.write(&mut out, "", "", "");
            }
            self.replace(end_tag..end_tag, out);
        }
    }

    /// Inserts `items` right after `sibling`, at its indentation.
    fn insert_after(&mut self, sibling: &El, items: Vec<X>) {
        let nl = self.nl();
        let mut out = String::new();
        match (indent_at(self.src, sibling.span.start), after_line(self.src, sibling.span.end)) {
            (Some(indent), Some(pos)) => {
                for item in items {
                    item.write(&mut out, &indent, &self.unit, nl);
                    out.push_str(nl);
                }
                self.replace(pos..pos, out);
            }
            _ => {
                for item in items {
                    item.write(&mut out, "", "", "");
                }
                self.replace(sibling.span.end..sibling.span.end, out);
            }
        }
    }

    /// Inserts `items` before `sibling`, at its indentation.
    fn insert_before(&mut self, sibling: &El, items: Vec<X>) {
        let nl = self.nl();
        let mut out = String::new();
        match indent_at(self.src, sibling.span.start) {
            Some(indent) => {
                for item in items {
                    item.write(&mut out, "", &self.unit, nl);
                    out.push_str(nl);
                    out.push_str(&indent);
                }
            }
            None => {
                for item in items {
                    item.write(&mut out, "", "", "");
                }
            }
        }
        let pos = sibling.span.start;
        self.replace(pos..pos, out);
    }

    /// Adds `item` after the last of `predecessors` present in `parent`.
    fn insert_in_order(&mut self, parent: &El, predecessors: &[&str], items: Vec<X>) {
        let after = parent.children.iter().rfind(|c| predecessors.contains(&c.name.as_str()));
        match after {
            Some(sibling) => self.insert_after(sibling, items),
            None => match parent.children.first() {
                Some(first) => self.insert_before(first, items),
                None => self.append(parent, items),
            },
        }
    }

    fn apply(mut self) -> Result<String, String> {
        // Inserts before replacements at the same place keep their order.
        self.patches.sort_by_key(|p| (p.range.start, !p.range.is_empty()));
        let mut out = String::with_capacity(self.src.len() + 1024);
        let mut at = 0;
        for p in &self.patches {
            if p.range.start < at {
                return Err("overlapping changes; nothing was saved".into());
            }
            out.push_str(&self.src[at..p.range.start]);
            out.push_str(&p.text);
            at = p.range.end;
        }
        out.push_str(&self.src[at..]);
        Ok(out)
    }

    fn sync_modules(&mut self, wanted: &Model) -> Result<(), String> {
        let doc = self.doc;
        for (name, m) in &doc.modules {
            if !wanted.contains_key(name) {
                self.delete(m.node.el);
            }
        }
        let mut new_modules = Vec::new();
        for (name, node) in wanted {
            match doc.modules.get(name) {
                Some(m) => self.sync_module(name, &m.node, node)?,
                None => new_modules.push(self.new_module(name, node)?),
            }
        }
        if !new_modules.is_empty() {
            // Next to the first module of the file, in its package.
            let Some(first) = doc.modules.values().next() else {
                return Err("the file has no module configuration to place new modules next to".into());
            };
            self.append(first.elements, new_modules);
        }
        Ok(())
    }

    fn sync_module(&mut self, name: &str, x: &XNode<'_>, node: &Node) -> Result<(), String> {
        let el = x.el;
        let order = [
            "SHORT-NAME",
            "ADMIN-DATA",
            "DEFINITION-REF",
            "ECUC-DEF-EDITION",
            "IMPLEMENTATION-CONFIG-VARIANT",
            "MODULE-DESCRIPTION-REF",
            "POST-BUILD-VARIANT-USED",
        ];
        let fields = [
            ("@def", "DEFINITION-REF", Some("ECUC-MODULE-DEF")),
            ("@edition", "ECUC-DEF-EDITION", None),
            ("@variant", "IMPLEMENTATION-CONFIG-VARIANT", None),
            ("@implementation", "MODULE-DESCRIPTION-REF", Some("BSW-IMPLEMENTATION")),
            ("@postBuildVariantUsed", "POST-BUILD-VARIANT-USED", None),
        ];
        for (key, tag, dest) in fields {
            let have = x.attrs.get(key);
            let want = node.attrs.get(key);
            if have.map(|(v, _)| v.key()) == want.map(Val::key) {
                continue;
            }
            match (have.and_then(|(_, e)| *e), want) {
                (Some(e), None) => self.delete(e),
                (Some(e), Some(v)) => self.set_text(e, &attr_text(key, v)),
                (None, Some(v)) => {
                    let pos = order.iter().position(|t| *t == tag).unwrap_or(0);
                    let attrs = dest.map(|d| vec![("DEST".to_string(), d.to_string())]).unwrap_or_default();
                    self.insert_in_order(el, &order[..pos], vec![X::Leaf(tag.into(), attrs, attr_text(key, v))]);
                }
                (None, None) => {}
            }
        }
        let def = match node.attrs.get("@def") {
            Some(Val::Str(d)) => d.clone(),
            _ => x.def.clone(),
        };
        let mut pending = Pending::default();
        self.sync_children(x, node, name, &def, &mut pending)?;
        let subs = std::mem::take(&mut pending.subs);
        self.add_wrapper(el, "CONTAINERS", &order, subs);
        Ok(())
    }

    /// Writes the container wrapper `tag` with `items`, or adds them to it.
    fn add_wrapper(&mut self, el: &El, tag: &str, predecessors: &[&str], items: Vec<X>) {
        if items.is_empty() {
            return;
        }
        match el.child(tag) {
            Some(w) => self.append(w, items),
            None => self.insert_in_order(el, predecessors, vec![X::Node(tag.into(), Vec::new(), items)]),
        }
    }

    fn sync_container(&mut self, x: &XNode<'_>, node: &Node, path: &str, def: &str) -> Result<(), String> {
        let el = x.el;
        // @def: the definition path, when it is not the parent's plus the group name.
        let have_def = x.attrs.get("@def").map(|(v, _)| v.key());
        let want_def = node.attrs.get("@def").map(Val::key);
        if have_def != want_def
            && let Some(d) = el.child("DEFINITION-REF")
        {
            self.set_text(d, def);
        }
        let have_index = x.attrs.get("@index");
        match (have_index, node.attrs.get("@index")) {
            (Some((v, Some(e))), Some(w)) if v.key() != w.key() => self.set_text(e, &w.key()),
            (Some((_, Some(e))), None) => self.delete(e),
            (None, Some(w)) => match el.child("DEFINITION-REF") {
                Some(d) => self.insert_before(d, vec![X::leaf("INDEX", &w.key())]),
                None => self.append(el, vec![X::leaf("INDEX", &w.key())]),
            },
            _ => {}
        }
        let mut pending = Pending::default();
        self.sync_children(x, node, path, def, &mut pending)?;
        let order = ["SHORT-NAME", "ADMIN-DATA", "INDEX", "DEFINITION-REF"];
        let params = std::mem::take(&mut pending.params);
        self.add_wrapper(el, "PARAMETER-VALUES", &order, params);
        let refs = std::mem::take(&mut pending.refs);
        let with_params = [&order[..], &["PARAMETER-VALUES"]].concat();
        self.add_wrapper(el, "REFERENCE-VALUES", &with_params, refs);
        let subs = std::mem::take(&mut pending.subs);
        let with_refs = [&with_params[..], &["REFERENCE-VALUES"]].concat();
        self.add_wrapper(el, "SUB-CONTAINERS", &with_refs, subs);
        Ok(())
    }

    /// Values and sub-containers of a module or container.
    fn sync_children(
        &mut self,
        x: &XNode<'_>,
        node: &Node,
        path: &str,
        def: &str,
        pending: &mut Pending,
    ) -> Result<(), String> {
        let el = x.el;
        // Elements to delete, per wrapper, so a wrapper left empty goes as a whole.
        let mut deletions: Vec<&El> = Vec::new();
        let empty = Vec::new();
        let names: std::collections::BTreeSet<&String> = x.values.keys().chain(node.values.keys()).collect();
        for name in names {
            let have = x.values.get(name).map_or(&empty[..], |v| &v[..]);
            let want = node.values.get(name).map_or(&[][..], |v| &v[..]);
            let vpath = format!("{path}/{name}");
            // Values on both sides stay; the others are changed in place, removed or added.
            let mut used = vec![false; have.len()];
            let mut missing = Vec::new();
            for w in want {
                match have.iter().enumerate().find(|(i, h)| !used[*i] && h.val.key() == w.key()) {
                    Some((i, _)) => used[i] = true,
                    None => missing.push(w),
                }
            }
            let mut stale = have.iter().zip(&used).filter(|(_, u)| !**u).map(|(h, _)| h);
            for w in missing {
                match stale.next() {
                    Some(h) => self.set_value(h, w, &vpath)?,
                    None => {
                        let t = self.templates.value(&vpath, name, w)?;
                        let item = self.new_value(&t, &format!("{def}/{name}"), w, &vpath)?;
                        if t.tag == "ECUC-REFERENCE-VALUE" || t.tag == "ECUC-INSTANCE-REFERENCE-VALUE" {
                            pending.refs.push(item);
                        } else {
                            pending.params.push(item);
                        }
                    }
                }
            }
            deletions.extend(stale.map(|h| h.el));
        }
        let groups: std::collections::BTreeSet<&String> = x.groups.keys().chain(node.groups.keys()).collect();
        let none = BTreeMap::new();
        for g in groups {
            let have = x.groups.get(g).unwrap_or(&none);
            let want = node.groups.get(g);
            let gpath = format!("{path}/{g}");
            for (n, c) in have {
                if !want.is_some_and(|w| w.contains_key(n)) {
                    deletions.push(c.el);
                }
            }
            for (n, wnode) in want.into_iter().flatten() {
                let cdef = match wnode.attrs.get("@def") {
                    Some(Val::Str(d)) => d.clone(),
                    _ => format!("{def}/{g}"),
                };
                match have.get(n) {
                    Some(c) => self.sync_container(c, wnode, &gpath, &cdef)?,
                    None => pending.subs.push(self.new_container(n, wnode, &gpath, &cdef)?),
                }
            }
        }
        // A wrapper all of whose children go, and that gets nothing new, goes too.
        let wrappers = [
            ("PARAMETER-VALUES", pending.params.is_empty()),
            ("REFERENCE-VALUES", pending.refs.is_empty()),
            ("SUB-CONTAINERS", pending.subs.is_empty()),
            ("CONTAINERS", pending.subs.is_empty()),
        ];
        let gone: std::collections::HashSet<*const El> = deletions.iter().map(|d| *d as *const El).collect();
        let mut whole = Vec::new();
        for (tag, nothing_new) in wrappers {
            if let Some(w) = el.child(tag)
                && nothing_new
                && !w.children.is_empty()
                && w.children.iter().all(|c| gone.contains(&(c as *const El)))
            {
                whole.push(w);
            }
        }
        let in_whole: std::collections::HashSet<*const El> =
            whole.iter().flat_map(|w| w.children.iter().map(|c| c as *const El)).collect();
        for d in deletions {
            if !in_whole.contains(&(d as *const El)) {
                self.delete(d);
            }
        }
        for w in whole {
            self.delete(w);
        }
        Ok(())
    }

    fn set_value(&mut self, h: &XVal<'_>, w: &Val, path: &str) -> Result<(), String> {
        let el = h.el;
        match h.kind {
            Kind::Numerical | Kind::Textual => {
                let old = el.child("VALUE");
                let text = match (h.kind, w) {
                    (_, Val::Null) => None,
                    (_, Val::Ref(_)) => return Err(format!("{path} is a parameter, not a reference")),
                    (Kind::Numerical, v) => Some(numeric_text(v, old.map(|o| o.text.trim()).unwrap_or_default())),
                    (_, v) => Some(v.key()),
                };
                match (old, text) {
                    (Some(o), None) => self.delete(o),
                    (Some(o), Some(t)) => self.set_text(o, &t),
                    (None, Some(t)) => self.append(el, vec![X::leaf("VALUE", &t)]),
                    (None, None) => {}
                }
            }
            Kind::Reference => {
                let old = el.child("VALUE-REF");
                let target = match w {
                    Val::Null => None,
                    Val::Ref(r) if !r.contains(STEP) => Some(self.doc.abs(r)?),
                    _ => return Err(format!("{path} is a reference; write it as \"{ARROW}path\"")),
                };
                match (old, target) {
                    (Some(o), None) => self.delete(o),
                    (Some(o), Some(t)) => self.set_text(o, &t),
                    (None, Some(t)) => {
                        let dest = self
                            .templates
                            .values
                            .get(path)
                            .and_then(|t| t.value_dest.clone())
                            .unwrap_or_else(|| "ECUC-CONTAINER-VALUE".into());
                        self.append(el, vec![X::Leaf("VALUE-REF".into(), vec![("DEST".into(), dest)], t)]);
                    }
                    (None, None) => {}
                }
            }
            Kind::Instance => {
                let old = el.child("VALUE-IREF");
                let dests: Vec<String> = old
                    .map(|i| i.children.iter().filter_map(|c| c.attr("DEST").map(str::to_string)).collect())
                    .unwrap_or_default();
                let iref = match w {
                    Val::Null => None,
                    Val::Ref(r) => Some(self.iref(r, &dests, path)?),
                    _ => {
                        return Err(format!(
                            "{path} is an instance reference; write it as \"{ARROW}context{STEP}target\""
                        ));
                    }
                };
                match (old, iref) {
                    (Some(o), None) => self.delete(o),
                    (Some(o), Some(i)) => {
                        let mut out = String::new();
                        let indent = indent_at(self.src, o.span.start);
                        let pretty = indent.is_some();
                        let nl = self.nl();
                        i.write(&mut out, "", if pretty { &self.unit } else { "" }, if pretty { nl } else { "" });
                        let out = match &indent {
                            Some(ind) if !ind.is_empty() => out.replace(nl, &format!("{nl}{ind}")),
                            _ => out,
                        };
                        self.replace(o.span.clone(), out);
                    }
                    (None, Some(i)) => self.append(el, vec![i]),
                    (None, None) => {}
                }
            }
        }
        Ok(())
    }

    /// The VALUE-IREF for `ctx ▸ … ▸ target`, with the DEST of each step from `dests`
    /// or from the same definition in either file.
    fn iref(&self, value: &str, dests: &[String], path: &str) -> Result<X, String> {
        let parts: Vec<&str> = value.split(STEP).map(str::trim).collect();
        let known = self.templates.values.get(path).map(|t| t.iref_dests.clone()).unwrap_or_default();
        let dests = if dests.len() == parts.len() {
            dests.to_vec()
        } else if known.len() == parts.len() {
            known
        } else {
            return Err(format!(
                "{path}: the kinds of the elements of this instance reference are unknown; edit it in the Text view"
            ));
        };
        let mut children = Vec::new();
        for (i, (p, d)) in parts.iter().zip(dests).enumerate() {
            let tag = if i + 1 == parts.len() { "TARGET-REF" } else { "CONTEXT-ELEMENT-REF" };
            children.push(X::Leaf(tag.into(), vec![("DEST".into(), d)], self.doc.abs(p)?));
        }
        Ok(X::Node("VALUE-IREF".into(), Vec::new(), children))
    }

    fn new_value(&self, t: &ValueTemplate, def: &str, w: &Val, path: &str) -> Result<X, String> {
        let mut children = vec![X::Leaf("DEFINITION-REF".into(), vec![("DEST".into(), t.dest.clone())], def.into())];
        match (t.tag.as_str(), w) {
            (_, Val::Null) => {}
            ("ECUC-REFERENCE-VALUE", Val::Ref(r)) if !r.contains(STEP) => {
                let dest = t.value_dest.clone().unwrap_or_else(|| "ECUC-CONTAINER-VALUE".into());
                children.push(X::Leaf("VALUE-REF".into(), vec![("DEST".into(), dest)], self.doc.abs(r)?));
            }
            ("ECUC-INSTANCE-REFERENCE-VALUE", Val::Ref(r)) => children.push(self.iref(r, &t.iref_dests, path)?),
            ("ECUC-REFERENCE-VALUE" | "ECUC-INSTANCE-REFERENCE-VALUE", _) => {
                return Err(format!("{path} is a reference; write it as \"{ARROW}path\""));
            }
            (_, Val::Ref(_)) => return Err(format!("{path} is a parameter, not a reference")),
            ("ECUC-NUMERICAL-PARAM-VALUE", v) => {
                let style = if t.numeric_bools { "1" } else { "" };
                children.push(X::leaf("VALUE", &numeric_text(v, style)));
            }
            (_, v) => children.push(X::leaf("VALUE", &v.key())),
        }
        Ok(X::Node(t.tag.clone(), Vec::new(), children))
    }

    fn new_container(&self, name: &str, node: &Node, path: &str, def: &str) -> Result<X, String> {
        let dest =
            self.templates.containers.get(path).cloned().unwrap_or_else(|| "ECUC-PARAM-CONF-CONTAINER-DEF".into());
        let mut children = vec![X::leaf("SHORT-NAME", name)];
        if let Some(i) = node.attrs.get("@index") {
            children.push(X::leaf("INDEX", &i.key()));
        }
        children.push(X::Leaf("DEFINITION-REF".into(), vec![("DEST".into(), dest)], def.into()));
        children.extend(self.new_contents(node, path, def, "SUB-CONTAINERS")?);
        Ok(X::Node("ECUC-CONTAINER-VALUE".into(), Vec::new(), children))
    }

    /// PARAMETER-VALUES, REFERENCE-VALUES and the sub-containers of a new module or container.
    fn new_contents(&self, node: &Node, path: &str, def: &str, subs_tag: &str) -> Result<Vec<X>, String> {
        let (mut params, mut refs, mut subs) = (Vec::new(), Vec::new(), Vec::new());
        for (name, vals) in &node.values {
            let vpath = format!("{path}/{name}");
            for v in vals {
                let t = self.templates.value(&vpath, name, v)?;
                let item = self.new_value(&t, &format!("{def}/{name}"), v, &vpath)?;
                if t.tag == "ECUC-REFERENCE-VALUE" || t.tag == "ECUC-INSTANCE-REFERENCE-VALUE" {
                    refs.push(item);
                } else {
                    params.push(item);
                }
            }
        }
        for (g, instances) in &node.groups {
            for (n, c) in instances {
                let cdef = match c.attrs.get("@def") {
                    Some(Val::Str(d)) => d.clone(),
                    _ => format!("{def}/{g}"),
                };
                subs.push(self.new_container(n, c, &format!("{path}/{g}"), &cdef)?);
            }
        }
        let mut out = Vec::new();
        if subs_tag == "SUB-CONTAINERS" {
            if !params.is_empty() {
                out.push(X::Node("PARAMETER-VALUES".into(), Vec::new(), params));
            }
            if !refs.is_empty() {
                out.push(X::Node("REFERENCE-VALUES".into(), Vec::new(), refs));
            }
        } else if !params.is_empty() || !refs.is_empty() {
            return Err(format!("{path}: a module holds containers, not parameters"));
        }
        if !subs.is_empty() {
            out.push(X::Node(subs_tag.into(), Vec::new(), subs));
        }
        Ok(out)
    }

    fn new_module(&self, name: &str, node: &Node) -> Result<X, String> {
        let def = match node.attrs.get("@def") {
            Some(Val::Str(d)) => d.clone(),
            _ => return Err(format!("module {name} needs \"@def\", the path of its definition")),
        };
        let mut children = vec![
            X::leaf("SHORT-NAME", name),
            X::Leaf("DEFINITION-REF".into(), vec![("DEST".into(), "ECUC-MODULE-DEF".into())], def.clone()),
        ];
        for (key, tag, dest) in [
            ("@edition", "ECUC-DEF-EDITION", None),
            ("@variant", "IMPLEMENTATION-CONFIG-VARIANT", None),
            ("@implementation", "MODULE-DESCRIPTION-REF", Some("BSW-IMPLEMENTATION")),
            ("@postBuildVariantUsed", "POST-BUILD-VARIANT-USED", None),
        ] {
            if let Some(v) = node.attrs.get(key) {
                let attrs = dest.map(|d| vec![("DEST".to_string(), d.to_string())]).unwrap_or_default();
                children.push(X::Leaf(tag.into(), attrs, attr_text(key, v)));
            }
        }
        children.extend(self.new_contents(node, name, &def, "CONTAINERS")?);
        Ok(X::Node("ECUC-MODULE-CONFIGURATION-VALUES".into(), Vec::new(), children))
    }
}

fn attr_text(key: &str, v: &Val) -> String {
    match (key, v) {
        ("@variant", Val::Str(s)) if !s.starts_with("VARIANT-") => format!("VARIANT-{s}"),
        (_, v) => v.key(),
    }
}

/// A numerical value written like `old`: booleans as 0/1 or false/true,
/// integers in hex when `old` was.
fn numeric_text(v: &Val, old: &str) -> String {
    match v {
        Val::Bool(b) if matches!(old, "0" | "1") => if *b { "1" } else { "0" }.into(),
        Val::Bool(b) => b.to_string(),
        Val::Num(n) => {
            let hex = old.strip_prefix("0x").or_else(|| old.strip_prefix("0X"));
            match (hex, n.parse::<u128>()) {
                (Some(digits), Ok(i)) => {
                    let prefix = &old[..2];
                    let lower = digits.bytes().any(|b| b.is_ascii_lowercase());
                    if lower { format!("{prefix}{i:x}") } else { format!("{prefix}{i:X}") }
                }
                _ => n.clone(),
            }
        }
        other => other.key(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<AUTOSAR xmlns="http://autosar.org/schema/r4.0">
  <AR-PACKAGES>
    <AR-PACKAGE UUID="p1">
      <SHORT-NAME>ActiveEcuC</SHORT-NAME>
      <ELEMENTS>
        <ECUC-MODULE-CONFIGURATION-VALUES UUID="m1">
          <SHORT-NAME>Can</SHORT-NAME>
          <DEFINITION-REF DEST="ECUC-MODULE-DEF">/AUTOSAR/EcucDefs/Can</DEFINITION-REF>
          <IMPLEMENTATION-CONFIG-VARIANT>VARIANT-PRE-COMPILE</IMPLEMENTATION-CONFIG-VARIANT>
          <CONTAINERS>
            <ECUC-CONTAINER-VALUE UUID="c1">
              <SHORT-NAME>CanConfigSet</SHORT-NAME>
              <DEFINITION-REF DEST="ECUC-PARAM-CONF-CONTAINER-DEF">/AUTOSAR/EcucDefs/Can/CanConfigSet</DEFINITION-REF>
              <SUB-CONTAINERS>
                <ECUC-CONTAINER-VALUE UUID="c2">
                  <SHORT-NAME>CanController_0</SHORT-NAME>
                  <DEFINITION-REF DEST="ECUC-PARAM-CONF-CONTAINER-DEF">/AUTOSAR/EcucDefs/Can/CanConfigSet/CanController</DEFINITION-REF>
                  <PARAMETER-VALUES>
                    <ECUC-NUMERICAL-PARAM-VALUE>
                      <DEFINITION-REF DEST="ECUC-INTEGER-PARAM-DEF">/AUTOSAR/EcucDefs/Can/CanConfigSet/CanController/CanControllerId</DEFINITION-REF>
                      <VALUE>0x1F</VALUE>
                    </ECUC-NUMERICAL-PARAM-VALUE>
                    <ECUC-NUMERICAL-PARAM-VALUE>
                      <DEFINITION-REF DEST="ECUC-BOOLEAN-PARAM-DEF">/AUTOSAR/EcucDefs/Can/CanConfigSet/CanController/CanControllerActivation</DEFINITION-REF>
                      <VALUE>1</VALUE>
                    </ECUC-NUMERICAL-PARAM-VALUE>
                    <ECUC-TEXTUAL-PARAM-VALUE>
                      <DEFINITION-REF DEST="ECUC-ENUMERATION-PARAM-DEF">/AUTOSAR/EcucDefs/Can/CanConfigSet/CanController/CanRxProcessing</DEFINITION-REF>
                      <VALUE>INTERRUPT</VALUE>
                    </ECUC-TEXTUAL-PARAM-VALUE>
                  </PARAMETER-VALUES>
                  <REFERENCE-VALUES>
                    <ECUC-REFERENCE-VALUE>
                      <DEFINITION-REF DEST="ECUC-REFERENCE-DEF">/AUTOSAR/EcucDefs/Can/CanConfigSet/CanController/CanCpuClockRef</DEFINITION-REF>
                      <VALUE-REF DEST="ECUC-CONTAINER-VALUE">/ActiveEcuC/Mcu/McuModuleConfiguration/Clk</VALUE-REF>
                    </ECUC-REFERENCE-VALUE>
                  </REFERENCE-VALUES>
                </ECUC-CONTAINER-VALUE>
              </SUB-CONTAINERS>
            </ECUC-CONTAINER-VALUE>
          </CONTAINERS>
        </ECUC-MODULE-CONFIGURATION-VALUES>
      </ELEMENTS>
    </AR-PACKAGE>
  </AR-PACKAGES>
</AUTOSAR>
"#;

    fn canonical(json: &str) -> String {
        to_json(&from_json(json).unwrap())
    }

    #[test]
    fn renders_simplified_json() {
        let json = render(FILE).unwrap();
        assert_eq!(
            json,
            r#"{
  "Can": {
    "@def": "/AUTOSAR/EcucDefs/Can",
    "@variant": "PRE-COMPILE",
    "CanConfigSet": {
      "CanConfigSet": {
        "CanController": {
          "CanController_0": {
            "CanControllerActivation": true,
            "CanControllerId": 31,
            "CanCpuClockRef": "→ Mcu/McuModuleConfiguration/Clk",
            "CanRxProcessing": "INTERRUPT"
          }
        }
      }
    }
  }
}
"#
        );
    }

    #[test]
    fn unchanged_json_keeps_the_file() {
        let json = render(FILE).unwrap();
        assert_eq!(write_back(FILE, &json, None).unwrap(), FILE);
        // Commas do not matter: lines copied from the other side may carry them or not.
        let loose = json.replace(",\n", "\n").replace("31", "31,");
        assert_eq!(write_back(FILE, &loose, None).unwrap(), FILE);
    }

    #[test]
    fn changes_values_in_place() {
        let json = render(FILE).unwrap();
        let edited = json
            .replace("\"CanControllerId\": 31", "\"CanControllerId\": 32")
            .replace("\"CanControllerActivation\": true", "\"CanControllerActivation\": false")
            .replace("Clk\"", "Clk2\"");
        let out = write_back(FILE, &edited, None).unwrap();
        assert!(out.contains("<VALUE>0x20</VALUE>"), "hex stays hex");
        assert!(out.contains("<VALUE>0</VALUE>"), "0/1 booleans stay 0/1");
        assert!(out.contains(">/ActiveEcuC/Mcu/McuModuleConfiguration/Clk2</VALUE-REF>"));
        assert_eq!(out.len(), FILE.len() + 1, "{out}");
        assert_eq!(render(&out).unwrap(), edited);
    }

    #[test]
    fn adds_and_removes_containers_and_values() {
        let json = render(FILE).unwrap();
        let edited = json.replace(
            "            \"CanRxProcessing\": \"INTERRUPT\"\n          }\n",
            "            \"CanRxProcessing\": \"INTERRUPT\"\n          },\n          \"CanController_1\": {\n            \"CanControllerId\": 2,\n            \"CanControllerActivation\": false,\n            \"CanCpuClockRef\": \"→ Mcu/McuModuleConfiguration/Clk\"\n          }\n",
        );
        let out = write_back(FILE, &edited, None).unwrap();
        assert_eq!(render(&out).unwrap(), canonical(&edited));
        assert!(out.contains("UUID=\"c2\""), "the existing container keeps its UUID");
        assert!(
            out.contains(
                "                <ECUC-CONTAINER-VALUE>\n                  <SHORT-NAME>CanController_1</SHORT-NAME>\n"
            ),
            "{out}"
        );
        // And back: removing the controller gives the original file.
        assert_eq!(write_back(&out, &json, None).unwrap(), FILE);

        // A removed parameter; a reference list emptied takes REFERENCE-VALUES with it.
        let edited = json.replace("            \"CanCpuClockRef\": \"→ Mcu/McuModuleConfiguration/Clk\",\n", "");
        let out = write_back(FILE, &edited, None).unwrap();
        assert!(!out.contains("REFERENCE-VALUES"), "{out}");
        assert_eq!(render(&out).unwrap(), edited);
    }

    #[test]
    fn new_parameters_take_their_kind_from_the_other_file() {
        let json = render(FILE).unwrap();
        let edited = json.replace(
            "\"CanRxProcessing\": \"INTERRUPT\"",
            "\"CanRxProcessing\": \"INTERRUPT\",\n            \"CanTxProcessing\": \"POLLING\"",
        );
        let err = write_back(FILE, &edited, None).unwrap_err();
        assert!(err.contains("CanTxProcessing"), "{err}");
        let donor = FILE.replace("CanRxProcessing", "CanTxProcessing");
        let out = write_back(FILE, &edited, Some(&donor)).unwrap();
        assert!(out.contains("<DEFINITION-REF DEST=\"ECUC-ENUMERATION-PARAM-DEF\">/AUTOSAR/EcucDefs/Can/CanConfigSet/CanController/CanTxProcessing</DEFINITION-REF>\n                      <VALUE>POLLING</VALUE>"), "{out}");
        assert_eq!(render(&out).unwrap(), edited);
    }

    #[test]
    fn json_errors_name_the_line() {
        let json = render(FILE).unwrap();
        let err = write_back(FILE, &json.replace("31", "3 1"), None).unwrap_err();
        assert!(err.starts_with("line 10:"), "{err}");
        let err =
            write_back(FILE, &json.replace("\"CanControllerId\": 31", "\"CanRxProcessing\": 31"), None).unwrap_err();
        assert!(err.contains("occurs twice"), "{err}");
        let err = write_back(FILE, &json.replace("31", "xyz"), None).unwrap_err();
        assert!(err.contains("xyz is not a number"), "{err}");
    }

    #[test]
    fn numbers_and_packages_are_normalized() {
        assert_eq!(canon_int("0x10").as_deref(), Some("16"));
        assert_eq!(canon_int("0b101").as_deref(), Some("5"));
        assert_eq!(canon_int("017").as_deref(), Some("15"));
        assert_eq!(canon_int("-0").as_deref(), Some("0"));
        assert_eq!(canon_float("5E2").as_deref(), Some("500.0"));
        assert_eq!(numerical("INF", "ECUC-FLOAT-PARAM-DEF"), Val::Str("INF".into()));
        let other = FILE.replace("ActiveEcuC", "EcucValues").replace("0x1F", "31").replace("<VALUE>1<", "<VALUE>true<");
        assert_eq!(render(&other).unwrap(), render(FILE).unwrap());
    }
}
