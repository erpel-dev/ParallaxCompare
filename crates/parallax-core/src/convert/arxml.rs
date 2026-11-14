//! Read-only AUTOSAR tree with parent-scoped named siblings and ordered unnamed content.
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
#[derive(Default)]
struct Node {
    tag: String,
    attrs: BTreeMap<String, String>,
    content: Vec<Item>,
    preserve_space: bool,
    namespaces: Arc<BTreeMap<String, Arc<str>>>,
    namespace: Arc<str>,
}
enum Item {
    Element(Node),
    Text(String),
    Comment(String),
    Pi(String),
}
fn local(tag: &str) -> &str {
    tag.rsplit(':').next().unwrap_or(tag)
}
impl Node {
    fn text(&mut self, text: String) {
        if let Some(Item::Text(t)) = self.content.last_mut() {
            t.push_str(&text)
        } else {
            self.content.push(Item::Text(text))
        }
    }
    fn start(e: &BytesStart<'_>, parent: &Node) -> Result<Self, String> {
        let mut n = Self { namespaces: parent.namespaces.clone(), ..Default::default() };
        let attributes = e
            .attributes()
            .map(|a| {
                let a = a.map_err(|e| e.to_string())?;
                Ok((
                    a.key.as_ref().to_owned(),
                    quick_xml::escape::unescape(&a.value).map_err(|e| e.to_string())?.into_owned(),
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        for (key, value) in &attributes {
            if key == "xmlns" {
                Arc::make_mut(&mut n.namespaces).insert(String::new(), Arc::from(value.as_str()));
            } else if let Some(prefix) = key.strip_prefix("xmlns:") {
                Arc::make_mut(&mut n.namespaces).insert(prefix.to_owned(), Arc::from(value.as_str()));
            }
        }
        let qname = e.name();
        let name = qname.as_ref();
        let (prefix, name) = name.split_once(':').unwrap_or(("", name));
        n.namespace = n.namespaces.get(prefix).cloned().unwrap_or_default();
        if !prefix.is_empty() && n.namespace.is_empty() {
            return Err(format!("ARXML: unbound namespace prefix {prefix}"));
        }
        n.tag = name.to_owned();
        if n.namespace != parent.namespace {
            n.attrs.insert("@namespace".into(), n.namespace.to_string());
        }
        for (key, value) in attributes {
            if key == "xmlns" || key.starts_with("xmlns:") {
                continue;
            }
            let key = if let Some((prefix, name)) = key.split_once(':') {
                if prefix == "xml" {
                    key.clone()
                } else {
                    let uri =
                        n.namespaces.get(prefix).ok_or_else(|| format!("ARXML: unbound attribute prefix {prefix}"))?;
                    format!("{{{uri}}}{name}")
                }
            } else {
                key
            };
            // xsi:type holds a QName too; retaining its spelling alone could hide
            // a changed binding or report equivalent namespace aliases as different.
            let value = if key == "{http://www.w3.org/2001/XMLSchema-instance}type" {
                let (prefix, name) = value.split_once(':').unwrap_or(("", &value));
                let uri = n.namespaces.get(prefix).map(|s| s.as_ref()).unwrap_or("");
                if !prefix.is_empty() && uri.is_empty() {
                    return Err(format!("ARXML: unbound type prefix {prefix}"));
                }
                format!("{{{uri}}}{name}")
            } else {
                value
            };
            if n.attrs.insert(key.clone(), value).is_some() {
                return Err(format!("ARXML: duplicate attribute {key}"));
            }
        }
        n.preserve_space = match n.attrs.get("xml:space").map(String::as_str) {
            Some("preserve") => true,
            Some("default") => false,
            _ => parent.preserve_space,
        };
        Ok(n)
    }
    fn short_name(&self) -> Option<String> {
        let mut names = self.content.iter().filter_map(|c| match c {
            Item::Element(n) if n.tag == "SHORT-NAME" && n.namespace == self.namespace => Some(n),
            _ => None,
        });
        let n = names.next()?;
        if names.next().is_some() {
            return None;
        }
        let mut text = String::new();
        for c in &n.content {
            match c {
                Item::Text(t) => text.push_str(t),
                _ => return None,
            }
        }
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }
    fn value(mut self) -> Value {
        if self.attrs.is_empty() && self.content.iter().all(|c| matches!(c, Item::Text(_))) {
            return Value::String(
                self.content.into_iter().filter_map(|c| if let Item::Text(t) = c { Some(t) } else { None }).collect(),
            );
        }
        let mut out = BTreeMap::<String, Value>::new();
        if !self.attrs.is_empty() {
            out.insert(
                "@attributes".into(),
                serde_json::to_value(std::mem::take(&mut self.attrs)).expect("string attributes"),
            );
        }
        let mixed =
            self.preserve_space || self.content.iter().any(|c| matches!(c,Item::Text(t) if !t.trim().is_empty()));
        let mut names = BTreeMap::<&str, BTreeSet<String>>::new();
        let mut ambiguous = BTreeSet::new();
        for c in &self.content {
            if let Item::Element(n) = c {
                match n.short_name() {
                    Some(name) if names.entry(&n.tag).or_default().insert(name.clone()) => {}
                    _ => {
                        ambiguous.insert(n.tag.clone());
                    }
                }
            }
        }
        let mut named = BTreeMap::<String, BTreeMap<String, Value>>::new();
        let mut ordered = Vec::new();
        drop(names);
        for c in self.content {
            match c {
                Item::Element(mut n) if !mixed && !ambiguous.contains(n.tag.as_str()) => {
                    let name = n.short_name().expect("unique name");
                    let tag = std::mem::take(&mut n.tag);
                    named.entry(tag).or_default().insert(name, n.value());
                }
                Item::Element(mut n) => {
                    let tag = std::mem::take(&mut n.tag);
                    ordered.push(element(&tag, n.value()));
                }
                Item::Text(t) if mixed => ordered.push(json!({"@text":t})),
                Item::Text(_) => {}
                Item::Comment(t) => ordered.push(json!({"@comment":t})),
                Item::Pi(t) => ordered.push(json!({"@processingInstruction":t})),
            }
        }
        if !named.is_empty() {
            out.insert(
                "@named".into(),
                Value::Object(
                    named
                        .into_iter()
                        .map(|(tag, children)| (tag.to_owned(), Value::Object(children.into_iter().collect())))
                        .collect(),
                ),
            );
        }
        if !ordered.is_empty() {
            out.insert("@content".into(), Value::Array(ordered));
        }
        Value::Object(out.into_iter().collect())
    }
}
fn element(tag: &str, value: Value) -> Value {
    Value::Object([(tag.to_owned(), value)].into_iter().collect())
}
pub fn is_arxml(text: &str) -> bool {
    let mut reader = Reader::from_str(text);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => return local(e.name().as_ref()) == "AUTOSAR",
            Ok(Event::Decl(_) | Event::Comment(_) | Event::PI(_)) => {}
            Ok(Event::Text(t)) if t.trim().is_empty() => {}
            _ => return false,
        }
    }
}
pub fn render(text: &str) -> Result<String, String> {
    let mut reader = Reader::from_str(text);
    let mut stack = vec![Node::default()];
    loop {
        match reader.read_event().map_err(|e| format!("ARXML: {e}"))? {
            Event::Start(e) => {
                if stack.len() >= 128 {
                    return Err("ARXML nesting exceeds 127 levels".into());
                }
                let node = Node::start(&e, stack.last().unwrap())?;
                stack.push(node);
            }
            Event::Empty(e) => {
                if stack.len() >= 128 {
                    return Err("ARXML nesting exceeds 127 levels".into());
                }
                let node = Node::start(&e, stack.last().unwrap())?;
                stack.last_mut().unwrap().content.push(Item::Element(node));
            }
            Event::End(_) => {
                if stack.len() <= 1 {
                    return Err("ARXML: unexpected closing element".into());
                }
                let n = stack.pop().unwrap();
                stack.last_mut().unwrap().content.push(Item::Element(n));
            }
            Event::Text(t) => stack.last_mut().unwrap().text(t.xml10_content().into_owned()),
            Event::CData(t) => stack.last_mut().unwrap().text(t.to_string()),
            Event::GeneralRef(r) => {
                let name = r.to_string();
                let decoded =
                    quick_xml::escape::unescape(&format!("&{name};")).map_err(|e| e.to_string())?.into_owned();
                stack.last_mut().unwrap().text(decoded);
            }
            Event::Comment(t) => stack.last_mut().unwrap().content.push(Item::Comment(t.to_string())),
            Event::PI(t) => stack.last_mut().unwrap().content.push(Item::Pi(t.as_ref().to_owned())),
            Event::DocType(_) => return Err("ARXML: document type declarations are unsupported".into()),
            Event::Decl(_) => {}
            Event::Eof => break,
        }
    }
    if stack.len() != 1 {
        return Err("ARXML: unclosed element".into());
    }
    let d = stack.pop().unwrap();
    let roots: Vec<_> =
        d.content.iter().filter_map(|c| if let Item::Element(n) = c { Some(n) } else { None }).collect();
    if roots.len() != 1
        || local(&roots[0].tag) != "AUTOSAR"
        || d.content.iter().any(|c| matches!(c,Item::Text(t) if !t.trim().is_empty()))
    {
        return Err("ARXML: expected one AUTOSAR root element".into());
    }
    drop(roots);
    let mut root = None;
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut seen_root = false;
    for item in d.content {
        let entry = match item {
            Item::Element(mut n) => {
                seen_root = true;
                let tag = std::mem::take(&mut n.tag);
                root = Some(element(&tag, n.value()));
                continue;
            }
            Item::Comment(t) => json!({"@comment": t}),
            Item::Pi(t) => json!({"@processingInstruction": t}),
            Item::Text(_) => continue,
        };
        if seen_root {
            after.push(entry);
        } else {
            before.push(entry);
        }
    }
    let mut value = root.expect("validated root element");
    let object = value.as_object_mut().expect("document object");
    if !before.is_empty() {
        object.insert("@before".into(), Value::Array(before));
    }
    if !after.is_empty() {
        object.insert("@after".into(), Value::Array(after));
    }
    serde_json::to_string_pretty(&value).map(|s| s + "\n").map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn doc(s: &str) -> String {
        format!("<AUTOSAR><ELEMENTS>{s}</ELEMENTS></AUTOSAR>")
    }
    fn named(tag: &str, n: &str, v: &str) -> String {
        format!("<{tag}><SHORT-NAME>{n}</SHORT-NAME><VALUE>{v}</VALUE></{tag}>")
    }
    #[test]
    fn reordering_named_siblings_preserves_comparison() {
        let a = named("SW-COMPONENT-TYPE", "A", "1");
        let b = named("SW-COMPONENT-TYPE", "B", "2");
        assert_eq!(render(&doc(&(a.clone() + &b))).unwrap(), render(&doc(&(b + &a))).unwrap());
        assert_ne!(render(&doc(&a)).unwrap(), render(&doc(&named("SW-COMPONENT-TYPE", "A", "3"))).unwrap());
    }
    #[test]
    fn ambiguous_and_unnamed_elements_keep_order() {
        for (a, b) in [(named("X", "A", "1"), named("X", "A", "2")), ("<REF>A</REF>".into(), "<REF>B</REF>".into())] {
            assert_ne!(render(&doc(&(a.clone() + &b))).unwrap(), render(&doc(&(b + &a))).unwrap());
        }
    }
    #[test]
    fn identity_is_scoped_by_parent_and_tag() {
        let text = render(&doc(&(named("X", "Same", "1") + &named("Y", "Same", "2")))).unwrap();
        assert!(text.contains("\"X\"") && text.contains("\"Y\""));
        let a = doc(
            "<P><SHORT-NAME>P1</SHORT-NAME><X><SHORT-NAME>A</SHORT-NAME><V>1</V></X></P><P><SHORT-NAME>P2</SHORT-NAME><X><SHORT-NAME>A</SHORT-NAME><V>2</V></X></P>",
        );
        assert_ne!(render(&a).unwrap(), render(&a.replace("<V>1</V>", "<V>3</V>")).unwrap());
    }
    #[test]
    fn keeps_attributes_entities_and_mixed_content() {
        let s=render("<a:AUTOSAR xmlns:a=\"urn:autosar\"><X DEST=\"T\"> a &amp; b<![CDATA[ c ]]><Y/> end</X><!--note--></a:AUTOSAR>").unwrap();
        for t in ["urn:autosar", "DEST", "a & b", " c ", " end", "note"] {
            assert!(s.contains(t), "{s}");
        }
        assert_ne!(render(&doc("<X>one<Y/>two</X>")).unwrap(), render(&doc("<X>two<Y/>one</X>")).unwrap());
    }
    #[test]
    fn document_comments_and_processing_instructions_stay_visible() {
        let text = render("<!--before--><AUTOSAR/><?tool after?>").unwrap();
        assert!(text.contains("before") && text.contains("tool after"));
        assert_ne!(text, render("<!--changed--><AUTOSAR/><?tool after?>").unwrap());
    }
    #[test]
    fn rejects_malformed_or_excessively_deep_documents() {
        for s in
            ["<AUTOSAR>", "<AUTOSAR/><AUTOSAR/>", "<OTHER/>", "<!DOCTYPE AUTOSAR><AUTOSAR/>", "<AUTOSAR><X></AUTOSAR>"]
        {
            assert!(render(s).is_err(), "{s}");
        }
        assert!(render(&format!("<AUTOSAR>{}{}</AUTOSAR>", "<X>".repeat(130), "</X>".repeat(130))).is_err());
    }
    #[test]
    fn namespace_aliases_match_but_namespace_uris_differ() {
        let a = "<a:AUTOSAR xmlns:a='urn:autosar' xmlns:x='urn:attrs'><a:X x:DEST='T'><a:SHORT-NAME>A</a:SHORT-NAME></a:X></a:AUTOSAR>";
        let b = a.replace("a:", "b:").replace("xmlns:a", "xmlns:b").replace("x:", "y:").replace("xmlns:x", "xmlns:y");
        assert_eq!(render(a).unwrap(), render(&b).unwrap());
        assert_ne!(render(a).unwrap(), render(&a.replace("urn:autosar", "urn:other")).unwrap());
        assert!(render("<a:AUTOSAR/>").is_err());
    }

    #[test]
    fn xml_space_preserve_is_inherited_and_can_be_reset() {
        let a = "<AUTOSAR><X xml:space='preserve'> <Y/> </X></AUTOSAR>";
        assert_ne!(render(a).unwrap(), render(&a.replace("> <Y/>", "><Y/>")).unwrap());
        let a = "<AUTOSAR xml:space='preserve'><X> <Y/> </X></AUTOSAR>";
        assert_ne!(render(a).unwrap(), render(&a.replace("> <Y/>", "><Y/>")).unwrap());
        let a = "<AUTOSAR xml:space='preserve'><X xml:space='default'> <Y/> </X></AUTOSAR>";
        assert_eq!(render(a).unwrap(), render(&a.replace("> <Y/>", "><Y/>")).unwrap());
    }
    #[test]
    fn type_qnames_retain_namespace_semantics() {
        let a = "<AUTOSAR xmlns:xsi='http://www.w3.org/2001/XMLSchema-instance' xmlns:t='urn:types'><X xsi:type='t:Thing'/></AUTOSAR>";
        assert_eq!(render(a).unwrap(), render(&a.replace("xmlns:t", "xmlns:u").replace("t:Thing", "u:Thing")).unwrap());
        assert_ne!(render(a).unwrap(), render(&a.replace("urn:types", "urn:other")).unwrap());
        assert!(render("<AUTOSAR xmlns:a='urn:a' xmlns:b='urn:a' a:x='1' b:x='2'/>").is_err());
    }
}
