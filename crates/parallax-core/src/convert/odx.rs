//! ODX diagnostic descriptions, rendered without dropping unrecognized data.
//!
//! This is a structural view, not a diagnostic runtime: references retain their
//! ID-REF/DOCREF and all variants, encodings, parameters and vendor extensions
//! remain visible. Only named definition collections are sorted; wire parameter,
//! computation-scale and other sequence orders are retained.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use quick_xml::{Reader, events::Event};

use crate::{Error, Result};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_NODES: usize = 250_000;
const MAX_DEPTH: usize = 128;

/// Collections of definitions keyed by ID/SHORT-NAME, not positional sequences.
const DEFINITION_SETS: &[&str] = &[
    "BASE-VARIANTS",
    "ECU-VARIANTS",
    "ECU-SHARED-DATAS",
    "PROTOCOLS",
    "FUNCTIONAL-GROUPS",
    "REQUESTS",
    "POS-RESPONSES",
    "NEG-RESPONSES",
    "GLOBAL-NEG-RESPONSES",
    "DIAG-COMMS",
    "DATA-OBJECT-PROPS",
    "DTC-DOPS",
    "STRUCTURES",
    "END-OF-PDU-FIELDS",
    "DYNAMIC-LENGTH-FIELDS",
    "TABLES",
    "UNITS",
    "PHYSICAL-DIMENSIONS",
    "COMPARAMS",
    "COMPLEX-COMPARAMS",
    "FUNCT-CLASSS",
];

/// Known element-only containers. In unknown or rich-text elements whitespace
/// is kept: even a space between two inline elements can change their meaning.
const CONTAINERS: &[&str] = &[
    "ODX",
    "DIAG-LAYER-CONTAINER",
    "DIAG-DATA-DICTIONARY-SPEC",
    "COMPARAM-SPEC",
    "COMPARAM-SUBSET",
    "VEHICLE-INFO-SPEC",
    "FLASH",
    "ECU-CONFIG",
    "BASE-VARIANT",
    "ECU-VARIANT",
    "PROTOCOL",
    "FUNCTIONAL-GROUP",
    "ECU-SHARED-DATA",
    "REQUEST",
    "POS-RESPONSE",
    "NEG-RESPONSE",
    "GLOBAL-NEG-RESPONSE",
    "DIAG-SERVICE",
    "SINGLE-ECU-JOB",
    "PARAMS",
    "PARAM",
    "DATA-OBJECT-PROP",
    "DTC-DOP",
    "STRUCTURE",
    "END-OF-PDU-FIELD",
    "DYNAMIC-LENGTH-FIELD",
    "TABLE",
    "TABLE-ROWS",
    "TABLE-ROW",
    "DIAG-CODED-TYPE",
    "PHYSICAL-TYPE",
    "COMPU-METHOD",
    "COMPU-INTERNAL-TO-PHYS",
    "COMPU-PHYS-TO-INTERNAL",
    "COMPU-SCALES",
    "COMPU-SCALE",
    "COMPU-CONST",
    "COMPU-RATIONAL-COEFFS",
    "COMPU-NUMERATOR",
    "COMPU-DENOMINATOR",
    "UNIT-SPEC",
    "UNIT",
    "PHYSICAL-DIMENSION",
    "DTC-S",
    "DTC",
    "COMPARAM-REFS",
    "POS-RESPONSE-REFS",
    "NEG-RESPONSE-REFS",
    "GLOBAL-NEG-RESPONSE-REFS",
    "COMPARAM-SUBSET-REFS",
    "COMPARAM-REF",
    "COMPARAM",
    "COMPLEX-COMPARAM",
    "ADMIN-DATA",
    "COMPANY-DATAS",
    "COMPANY-DATA",
    "COMPANY-DOC-INFOS",
    "COMPANY-DOC-INFO",
    "DOC-REVISIONS",
    "DOC-REVISION",
    "TEAM-MEMBERS",
    "TEAM-MEMBER",
    "SDGS",
    "PARENT-REFS",
    "PARENT-REF",
    "FUNCT-CLASS",
    "STATE-CHARTS",
    "STATE-CHART",
    "STATES",
    "STATE",
    "TRANSITIONS",
    "TRANSITION",
];

pub fn is_name(name: &str) -> bool {
    let ext = name.rsplit(['/', '\\']).next().unwrap_or(name).rsplit_once('.').map(|(_, e)| e);
    ext.is_some_and(|e| {
        let e = e.to_ascii_lowercase();
        e == "odx" || e.strip_prefix("odx-").is_some_and(|suffix| !suffix.is_empty())
    })
}

pub fn diagnostics(text: &str) -> Result<String> {
    let mut root = parse(text)?;
    root.normalize(false);
    let mut out = String::from("=== ODX diagnostics ===\n");
    root.render(&mut out, 0);
    Ok(out)
}

fn bad(message: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("ODX: {message}"))
}

#[derive(Debug)]
enum Content {
    Element(Node),
    Text(String),
    Instruction(String),
}

#[derive(Debug)]
struct Node {
    name: String,
    attrs: BTreeMap<String, String>,
    content: Vec<Content>,
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("strings can be serialized")
}

fn element(e: &quick_xml::events::BytesStart<'_>) -> Result<Node> {
    let mut attrs = BTreeMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(bad)?;
        let name = attr.key.as_ref().to_string();
        let value = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_err(bad)?.into_owned();
        attrs.insert(name, value);
    }
    Ok(Node { name: e.name().as_ref().to_string(), attrs, content: Vec::new() })
}

fn push_text(node: &mut Node, text: &str) {
    if let Some(Content::Text(previous)) = node.content.last_mut() {
        previous.push_str(text);
    } else if !text.is_empty() {
        node.content.push(Content::Text(text.to_string()));
    }
}

fn parse(text: &str) -> Result<Node> {
    if text.len() > MAX_BYTES {
        return Err(bad("file exceeds the 64 MiB structured-view limit"));
    }
    let mut reader = Reader::from_str(text.trim_start_matches('\u{feff}'));
    let mut stack = vec![Node { name: String::new(), attrs: BTreeMap::new(), content: Vec::new() }];
    let mut nodes = 0usize;
    loop {
        let event = reader.read_event().map_err(bad)?;
        let empty = matches!(&event, Event::Empty(_));
        match event {
            Event::Start(e) | Event::Empty(e) => {
                nodes += 1;
                if nodes > MAX_NODES || stack.len() > MAX_DEPTH {
                    return Err(bad("too many elements or excessive XML nesting"));
                }
                let node = element(&e)?;
                if empty {
                    stack.last_mut().expect("document").content.push(Content::Element(node));
                } else {
                    stack.push(node);
                }
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err(bad("unexpected closing element"));
                }
                let node = stack.pop().expect("element");
                stack.last_mut().expect("document").content.push(Content::Element(node));
            }
            Event::Text(t) => push_text(stack.last_mut().expect("document"), &t.xml10_content()),
            Event::CData(t) => push_text(stack.last_mut().expect("document"), &t),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref().map_err(bad)? {
                    Some(c) => c.to_string(),
                    None => match &*r {
                        "amp" => "&".into(),
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "quot" => "\"".into(),
                        "apos" => "'".into(),
                        _ => return Err(bad(format!("undefined entity &{};", &*r))),
                    },
                };
                push_text(stack.last_mut().expect("document"), &resolved);
            }
            Event::PI(pi) => stack.last_mut().expect("document").content.push(Content::Instruction(pi.to_string())),
            Event::DocType(_) => return Err(bad("DOCTYPE declarations are not supported")),
            Event::Eof => break,
            Event::Decl(d) => {
                if d.version().map_err(bad)?.as_ref() != "1.0" {
                    return Err(bad("only XML 1.0 documents are supported"));
                }
            }
            Event::Comment(_) => {}
        }
    }
    if stack.len() != 1 {
        return Err(bad("file ends inside an element"));
    }
    let document = stack.pop().expect("document");
    let mut root = None;
    for item in document.content {
        match item {
            Content::Element(node) if root.is_none() => root = Some(node),
            Content::Text(s) if s.chars().all(char::is_whitespace) => {}
            // A top-level instruction can affect interpretation too; do not discard it.
            Content::Instruction(_) => {
                return Err(bad("processing instructions outside the ODX root are not supported"));
            }
            _ => return Err(bad("expected a single ODX document root")),
        }
    }
    let root = root.ok_or_else(|| bad("no ODX root element"))?;
    if local(&root.name) != "ODX" {
        return Err(bad("no ODX root element"));
    }
    Ok(root)
}

impl Node {
    fn short_name(&self) -> Option<String> {
        self.content.iter().find_map(|item| match item {
            Content::Element(node) if local(&node.name) == "SHORT-NAME" => {
                let mut text = String::new();
                for item in &node.content {
                    if let Content::Text(value) = item {
                        text.push_str(value);
                    } else {
                        return None;
                    }
                }
                Some(text)
            }
            _ => None,
        })
    }

    fn key(&self) -> Option<(String, String)> {
        self.attrs.get("ID").cloned().or_else(|| self.short_name()).map(|key| (self.name.clone(), key))
    }

    fn normalize(&mut self, preserve: bool) {
        let preserve = match self.attrs.get("xml:space").map(String::as_str) {
            Some("preserve") => true,
            Some("default") => false,
            _ => preserve,
        };
        let name = local(&self.name);
        let container = CONTAINERS.contains(&name) || DEFINITION_SETS.contains(&name);
        let element_only =
            !self.content.iter().any(|item| matches!(item, Content::Text(t) if !t.chars().all(char::is_whitespace)));
        if !preserve && container && element_only {
            self.content.retain(|item| !matches!(item, Content::Text(t) if t.chars().all(char::is_whitespace)));
        }
        for item in &mut self.content {
            if let Content::Element(node) = item {
                // Unknown and rich-text containers retain even whitespace-only text.
                node.normalize(preserve || !container);
            }
        }
        if !preserve && DEFINITION_SETS.contains(&name) {
            let keys: Option<Vec<_>> = self
                .content
                .iter()
                .map(|item| match item {
                    Content::Element(node) => node.key(),
                    _ => None,
                })
                .collect();
            if let Some(keys) = keys {
                // Duplicate keys are left in their original order, never merged.
                if keys.iter().collect::<BTreeSet<_>>().len() == keys.len() {
                    self.content.sort_by_cached_key(|item| match item {
                        Content::Element(node) => node.key(),
                        _ => None,
                    });
                }
            }
        }
    }

    fn render(&self, out: &mut String, depth: usize) {
        out.extend(std::iter::repeat_n(' ', depth * 2));
        out.push_str(&self.name);
        if let Some(name) = self.short_name() {
            let _ = write!(out, " {}", quote(&name));
        }
        for (name, value) in &self.attrs {
            let _ = write!(out, " [{name}={}]", quote(value));
        }
        if let [Content::Text(value)] = self.content.as_slice() {
            let _ = writeln!(out, " = {}", quote(value));
        } else {
            out.push('\n');
            for item in &self.content {
                match item {
                    Content::Element(node) => node.render(out, depth + 1),
                    Content::Text(text) => {
                        let _ = writeln!(out, "{}#text = {}", " ".repeat((depth + 1) * 2), quote(text));
                    }
                    Content::Instruction(pi) => {
                        let _ = writeln!(out, "{}#instruction = {}", " ".repeat((depth + 1) * 2), quote(pi));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &str = include_str!("../../tests/fixtures/automotive/engine-v1.odx-d");
    const FORMATTED: &str = include_str!("../../tests/fixtures/automotive/engine-v1-reformatted.odx-d");
    const V2: &str = include_str!("../../tests/fixtures/automotive/engine-v2.odx-d");

    #[test]
    fn definitions_normalize_without_losing_diagnostic_data() {
        let out = diagnostics(V1).unwrap();
        for expected in [
            "DIAG-LAYER-CONTAINER \"Engine\" [ID=\"DLC.Engine\"]",
            "DIAG-SERVICE \"ReadSerialNumber\"",
            "REQUEST-REF [ID-REF=\"Request.ReadSerialNumber\"]",
            "DIAG-CODED-TYPE [BASE-DATA-TYPE=\"A_UINT32\"] [xsi:type=\"STANDARD-LENGTH-TYPE\"]",
            "CODED-VALUE = \"61840\"",
            "BIT-LENGTH = \"128\"",
        ] {
            assert!(out.contains(expected), "missing {expected}\n{out}");
        }
        assert_eq!(out, diagnostics(FORMATTED).unwrap());
        assert_ne!(out, diagnostics(V2).unwrap());
        for changed in [
            V1.replace("61840", "61841"),
            V1.replace("ID-REF=\"DOP.SerialNumber\"", "ID-REF=\"DOP.Other\""),
            V1.replace("A_ASCIISTRING", "A_BYTEFIELD"),
            V1.replace("IDENTIFICATION", "PROGRAMMING"),
        ] {
            assert_ne!(out, diagnostics(&changed).unwrap());
        }
    }

    #[test]
    fn ordered_parameters_and_unknown_fields_are_preserved() {
        let a = "<ODX><REQUESTS><REQUEST ID='R'><SHORT-NAME>R</SHORT-NAME><PARAMS><PARAM ID='A'/><PARAM ID='B'/></PARAMS></REQUEST></REQUESTS></ODX>";
        assert_ne!(
            diagnostics(a).unwrap(),
            diagnostics(&a.replace("ID='A'/><PARAM ID='B'", "ID='B'/><PARAM ID='A'")).unwrap()
        );
        let a = "<ODX xmlns:v='urn:vendor'><v:EXTRA v:mode='A'><v:VALUE>custom</v:VALUE></v:EXTRA></ODX>";
        let out = diagnostics(a).unwrap();
        assert!(out.contains("v:EXTRA [v:mode=\"A\"]"));
        for changed in [a.replace("custom", "changed"), a.replace("mode='A'", "mode='B'")] {
            assert_ne!(out, diagnostics(&changed).unwrap());
        }
        let a = "<ODX><REQUESTS><REQUEST ID='same'><SHORT-NAME>A</SHORT-NAME></REQUEST><REQUEST ID='same'><SHORT-NAME>B</SHORT-NAME></REQUEST></REQUESTS></ODX>";
        let b = a.replace(">A<", ">temporary<").replace(">B<", ">A<").replace(">temporary<", ">B<");
        assert_ne!(diagnostics(a).unwrap(), diagnostics(&b).unwrap());
    }

    #[test]
    fn entities_cdata_attributes_and_rich_text_are_lossless() {
        let a = "<ODX><SHORT-NAME>A &amp; B</SHORT-NAME><X a='&quot;&amp;' b='2'/></ODX>";
        let b = "<ODX><SHORT-NAME><![CDATA[A & B]]></SHORT-NAME><X b='2' a='&#34;&#38;'></X></ODX>";
        assert_eq!(diagnostics(a).unwrap(), diagnostics(b).unwrap());
        let a = "<ODX><DESC><p><b>one</b> <b>two</b></p></DESC></ODX>";
        assert_ne!(diagnostics(a).unwrap(), diagnostics(&a.replace("</b> <b>", "</b><b>")).unwrap());
        let a = "<ODX><REQUEST xml:space='preserve'>  <SHORT-NAME>X</SHORT-NAME> </REQUEST></ODX>";
        assert_ne!(diagnostics(a).unwrap(), diagnostics(&a.replace("  <", " <")).unwrap());
        assert_ne!(diagnostics("<ODX><V>  x</V></ODX>").unwrap(), diagnostics("<ODX><V>x</V></ODX>").unwrap());
    }

    #[test]
    fn malformed_xml_and_resource_limits_fail_cleanly() {
        for input in [
            "",
            "<NOT-ODX/>",
            "<ODX>",
            "<ODX><A></ODX>",
            "<ODX/><ODX/>",
            "junk<ODX/>",
            "<ODX>&missing;</ODX>",
            "<!DOCTYPE ODX [<!ENTITY x 'hidden'>]><ODX>&x;</ODX>",
            "<ODX ID='A' ID='B'/>",
        ] {
            assert!(diagnostics(input).is_err(), "{input}");
        }
        let deep = format!("<ODX>{}{}</ODX>", "<A>".repeat(MAX_DEPTH + 1), "</A>".repeat(MAX_DEPTH + 1));
        assert!(diagnostics(&deep).is_err());
        let wide = format!("<ODX>{}</ODX>", "<A/>".repeat(MAX_NODES));
        assert!(diagnostics(&wide).is_err());
        assert!(is_name("folder/ecu.ODX-D"));
        assert!(is_name("ecu.odx"));
        assert!(is_name("ecu.odx-cs"));
        assert!(!is_name("ecu.xml"));
    }
}
