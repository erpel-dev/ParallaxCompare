//! Read-only AUTOSAR tree with parent-scoped named siblings and ordered unnamed content.
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Default)]
struct Node {
    tag: String,
    attrs: BTreeMap<String, String>,
    content: Vec<Item>,
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
    fn start(e: &BytesStart<'_>) -> Result<Self, String> {
        let mut n = Self { tag: e.name().as_ref().to_owned(), ..Default::default() };
        for a in e.attributes() {
            let a = a.map_err(|e| e.to_string())?;
            n.attrs.insert(
                a.key.as_ref().to_owned(),
                quick_xml::escape::unescape(&a.value).map_err(|e| e.to_string())?.into_owned(),
            );
        }
        Ok(n)
    }
    fn short_name(&self) -> Option<String> {
        let mut names = self.content.iter().filter_map(|c| match c {
            Item::Element(n) if local(&n.tag) == "SHORT-NAME" => Some(n),
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
    fn value(&self) -> Value {
        if self.attrs.is_empty() && self.content.iter().all(|c| matches!(c, Item::Text(_))) {
            return Value::String(
                self.content
                    .iter()
                    .filter_map(|c| if let Item::Text(t) = c { Some(t.as_str()) } else { None })
                    .collect(),
            );
        }
        let mut out = BTreeMap::<String, Value>::new();
        if !self.attrs.is_empty() {
            out.insert("@attributes".into(), json!(self.attrs));
        }
        let mixed = self.content.iter().any(|c| matches!(c,Item::Text(t) if !t.trim().is_empty()));
        let mut names = BTreeMap::<&str, BTreeSet<String>>::new();
        let mut ambiguous = BTreeSet::new();
        for c in &self.content {
            if let Item::Element(n) = c {
                match n.short_name() {
                    Some(name) if names.entry(&n.tag).or_default().insert(name.clone()) => {}
                    _ => {
                        ambiguous.insert(n.tag.as_str());
                    }
                }
            }
        }
        let mut named = BTreeMap::<&str, BTreeMap<String, Value>>::new();
        let mut ordered = Vec::new();
        for c in &self.content {
            match c {
                Item::Element(n) if !mixed && !ambiguous.contains(n.tag.as_str()) => {
                    named.entry(&n.tag).or_default().insert(n.short_name().expect("unique name"), n.value());
                }
                Item::Element(n) => ordered.push(element(&n.tag, n.value())),
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
                stack.push(Node::start(&e)?);
            }
            Event::Empty(e) => stack.last_mut().unwrap().content.push(Item::Element(Node::start(&e)?)),
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
    let mut value = element(&roots[0].tag, roots[0].value());
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut seen_root = false;
    for item in &d.content {
        let entry = match item {
            Item::Element(_) => {
                seen_root = true;
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
}
