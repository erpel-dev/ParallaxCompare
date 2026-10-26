//! Sphinx-Needs requirements, from reStructuredText sources and from the
//! `needs.json` a Sphinx build writes.
//!
//! The "Needs" view lists every need sorted by id, with its options in a fixed
//! order, link and tag lists sorted, and its text reflowed. Moving needs around,
//! reordering options or re-wrapping a description is then no change, while a
//! changed status, a new link or a reworded sentence shows up under its need.
//!
//! In reST, needs are the directives of the default types (`need`, `req`,
//! `spec`, `impl`, `test`) and any other directive with an `:id:` option, since
//! a project can define its own types. Nested needs are listed on their own,
//! with their parent. `needextend` directives are listed at the end.
//!
//! From `needs.json`, the empty defaults and what only says where or when the
//! build ran (line numbers, timestamps, the tool version) are left out.

use std::collections::BTreeMap;

use serde_json::Value;

use super::rst::reflow;
use crate::{Error, Result};

/// Directive names of the default need types.
const DEFAULT_TYPES: &[&str] = &["need", "req", "spec", "impl", "test"];

/// Options listed first, in this order; the others follow by name.
const FIRST: &[&str] = &["status", "tags", "links"];

/// `needs.json` fields that are not shown: shown elsewhere (id, type, title,
/// content), derived from others, or about the build rather than the need.
const JSON_SKIP: &[&str] = &[
    "id",
    "type",
    "title",
    "content",
    "type_name",
    "doctype",
    "lineno",
    "lineno_content",
    "created",
    "external_css",
    "section_name",
    "parent_needs",
    "constraints_passed",
    "full_title",
    "is_need",
    "is_part",
    "id_complete",
    "id_parent",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Need {
    pub id: Option<String>,
    pub kind: String,
    pub title: String,
    /// Options by name, list values already sorted and joined.
    pub fields: BTreeMap<String, String>,
    pub content: String,
}

impl Need {
    fn sort_key(&self) -> (bool, String, String, String) {
        (self.id.is_none(), self.id.clone().unwrap_or_default(), self.kind.clone(), self.title.clone())
    }
}

/// A `needextend` directive: which needs it changes, and how.
#[derive(Debug, Clone, PartialEq)]
pub struct Extension {
    pub filter: String,
    pub fields: BTreeMap<String, String>,
}

/// Values of the fields Sphinx-Needs keeps as lists of ids or tags: sorted, so that
/// their order is no change. Other fields, such as `needs_extra_options`, are text
/// whose order counts, even when it looks like a list.
fn normalize_value(key: &str, value: &str) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let listy =
        matches!(key, "tags" | "constraints" | "parent_needs") || key.ends_with("links") || key.ends_with("_back");
    if listy {
        let mut parts: Vec<&str> = value.split([',', ';', '|']).map(str::trim).filter(|p| !p.is_empty()).collect();
        parts.sort_unstable();
        parts.dedup();
        parts.join(", ")
    } else {
        value
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// `.. name:: argument` → (name, argument).
fn directive(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix(".. ")?;
    let (name, argument) = rest.split_once("::")?;
    let name = name.trim();
    (!name.is_empty() && !name.contains(' ')).then_some((name, argument.trim()))
}

/// Whether a directive is a need: a default type, or anything with an id that
/// is not one of the need query and layout directives (`needtable`, `needflow` …).
fn is_need(name: &str, fields: &BTreeMap<String, String>) -> bool {
    DEFAULT_TYPES.contains(&name) || (fields.contains_key("id") && !name.starts_with("need"))
}

/// Whether a reST text has any needs, to offer the Needs view.
pub fn rst_has_needs(text: &str) -> bool {
    let (needs, extensions) = parse_rst(text);
    !needs.is_empty() || !extensions.is_empty()
}

/// The needs and `needextend`s of a reST text.
pub fn parse_rst(text: &str) -> (Vec<Need>, Vec<Extension>) {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let mut needs = Vec::new();
    let mut extensions = Vec::new();
    collect(&lines, None, &mut needs, &mut extensions);
    needs.sort_by_key(Need::sort_key);
    extensions.sort_by(|a, b| a.filter.cmp(&b.filter));
    (needs, extensions)
}

/// Finds the need directives in `lines`, and those nested in their content.
/// Returns the content of `lines` with each need block replaced by a pointer
/// line, for the parent's description.
fn collect(
    lines: &[&str],
    parent: Option<&str>,
    needs: &mut Vec<Need>,
    extensions: &mut Vec<Extension>,
) -> Vec<String> {
    let mut rest = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let indent = indent_of(line);
        let Some((name, argument)) = directive(line.trim_start()) else {
            rest.push(line.to_string());
            i += 1;
            continue;
        };
        // The directive's block: everything indented deeper, and blank lines.
        let start = i;
        i += 1;
        let mut end = i;
        while end < lines.len() && (lines[end].is_empty() || indent_of(lines[end]) > indent) {
            end += 1;
        }
        // Trailing blank lines belong to what follows.
        while end > i && lines[end - 1].is_empty() {
            end -= 1;
        }
        let block = &lines[i..end];

        // The argument may go on over the next lines, up to the options.
        let mut title = argument.to_string();
        let mut j = 0;
        while j < block.len() && !block[j].is_empty() && !block[j].trim_start().starts_with(':') {
            title.push(' ');
            title.push_str(block[j].trim());
            j += 1;
        }
        // Options: `:key: value`, continued on deeper-indented lines.
        let mut fields = BTreeMap::new();
        let mut current: Option<(String, String)> = None;
        while j < block.len() && !block[j].is_empty() {
            let text = block[j].trim();
            let option = text.strip_prefix(':').and_then(|t| t.split_once(':'));
            match option {
                Some((key, value)) if !key.contains(' ') || current.is_none() => {
                    if let Some((k, v)) = current.take() {
                        fields.insert(k, v);
                    }
                    current = Some((key.trim().to_string(), value.trim().to_string()));
                }
                _ => {
                    if let Some((_, v)) = current.as_mut() {
                        v.push(' ');
                        v.push_str(text);
                    }
                }
            }
            j += 1;
        }
        if let Some((k, v)) = current {
            fields.insert(k, v);
        }
        let fields: BTreeMap<String, String> = fields
            .into_iter()
            .map(|(k, v)| {
                let v = normalize_value(&k, &v);
                (k, v)
            })
            .collect();

        // The content, dedented.
        let body: Vec<&str> = block[j..].iter().copied().skip_while(|l| l.is_empty()).collect();
        let cut = body.iter().filter(|l| !l.is_empty()).map(|l| indent_of(l)).min().unwrap_or(0);
        let body: Vec<&str> = body.iter().map(|l| l.get(cut..).unwrap_or_else(|| l.trim_start())).collect();

        if name == "needextend" {
            extensions.push(Extension { filter: argument.to_string(), fields });
        } else if is_need(name, &fields) {
            let mut fields = fields;
            let id = fields.remove("id");
            if let Some(parent) = parent {
                fields.insert("parent".into(), parent.into());
            }
            let own = collect(&body, id.as_deref().or(Some(&title)), needs, extensions);
            needs.push(Need {
                id: id.clone(),
                kind: name.to_string(),
                title: title.split_whitespace().collect::<Vec<_>>().join(" "),
                fields,
                content: reflow(&own.join("\n")).trim_end().to_string(),
            });
            rest.push(format!("{}↳ {}", " ".repeat(indent), id.as_deref().unwrap_or(&title)));
        } else {
            // Not a need, but needs may be nested in it (a container, an admonition).
            rest.extend(lines[start..end].iter().take(1).map(|l| l.to_string()));
            rest.extend(collect(block, parent, needs, extensions));
        }
        i = end;
    }
    rest
}

/// The needs of each version in a `needs.json`, by version.
pub fn parse_json(data: &[u8]) -> Result<(String, BTreeMap<String, Vec<Need>>)> {
    let root: Value = serde_json::from_slice(data).map_err(|e| Error::Unsupported(format!("needs.json: {e}")))?;
    let project = root.get("project").and_then(Value::as_str).unwrap_or("").to_string();
    let versions = root
        .get("versions")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Unsupported("needs.json without \"versions\"".into()))?;
    let mut out = BTreeMap::new();
    for (version, body) in versions {
        let mut needs = Vec::new();
        for (key, need) in body.get("needs").and_then(Value::as_object).into_iter().flatten() {
            let Some(obj) = need.as_object() else { continue };
            let text = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let mut fields = BTreeMap::new();
            for (k, v) in obj {
                if JSON_SKIP.contains(&k.as_str()) || is_empty(v) {
                    continue;
                }
                let value = match v {
                    Value::String(s) => s.clone(),
                    Value::Array(items) => items
                        .iter()
                        .map(|i| i.as_str().map(str::to_string).unwrap_or_else(|| i.to_string()))
                        .collect::<Vec<_>>()
                        .join(", "),
                    other => other.to_string(),
                };
                let key = if k == "parent_need" { "parent".to_string() } else { k.clone() };
                let value = if k == "sections" { value.replace(", ", " › ") } else { normalize_value(&key, &value) };
                fields.insert(key, value);
            }
            let id = obj.get("id").and_then(Value::as_str).unwrap_or(key).to_string();
            needs.push(Need {
                id: Some(id),
                kind: text("type"),
                title: text("title"),
                fields,
                content: reflow(&without_needs(&text("content"))).trim_end().to_string(),
            });
        }
        needs.sort_by_key(Need::sort_key);
        out.insert(version.clone(), needs);
    }
    Ok((project, out))
}

/// A need's content with the needs nested in it replaced by a pointer: they
/// are listed on their own.
fn without_needs(content: &str) -> String {
    let lines: Vec<&str> = content.lines().map(str::trim_end).collect();
    collect(&lines, None, &mut Vec::new(), &mut Vec::new()).join("\n")
}

/// Defaults that say nothing: null, "", [], {}, false and 0.
fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

fn summary(needs: &[Need]) -> String {
    let mut by_type: BTreeMap<&str, usize> = BTreeMap::new();
    for n in needs {
        *by_type.entry(&n.kind).or_default() += 1;
    }
    let parts: Vec<String> = by_type.iter().map(|(k, n)| format!("{n} {k}")).collect();
    format!("{} need{}: {}", needs.len(), if needs.len() == 1 { "" } else { "s" }, parts.join(", "))
}

fn write_need(out: &mut String, n: &Need) {
    let id = n.id.as_deref().unwrap_or("(no id)");
    out.push_str(&format!("--- {id} [{}] {} ---\n", n.kind, n.title));
    let mut keys: Vec<&String> = n.fields.keys().collect();
    keys.sort_by_key(|k| (FIRST.iter().position(|f| f == k).unwrap_or(FIRST.len()), k.as_str()));
    for key in keys {
        out.push_str(&format!("{key}: {}\n", n.fields[key]));
    }
    for line in n.content.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str("    ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push('\n');
}

/// The Needs view of a reST text.
pub fn render_rst(text: &str) -> String {
    let (needs, extensions) = parse_rst(text);
    let mut out = format!("=== {} ===\n\n", summary(&needs));
    for n in &needs {
        write_need(&mut out, n);
    }
    for e in &extensions {
        out.push_str(&format!("--- needextend {} ---\n", e.filter));
        for (k, v) in &e.fields {
            out.push_str(&format!("{k}: {v}\n"));
        }
        out.push('\n');
    }
    out
}

/// The Needs view of a `needs.json`.
pub fn render_json(data: &[u8]) -> Result<String> {
    let (project, versions) = parse_json(data)?;
    let mut out = String::new();
    for (version, needs) in &versions {
        let name = match (project.is_empty(), version.is_empty()) {
            (false, false) => format!("{project} {version} · "),
            (false, true) => format!("{project} · "),
            (true, false) => format!("version {version} · "),
            (true, true) => String::new(),
        };
        out.push_str(&format!("=== {name}{} ===\n\n", summary(needs)));
        for n in needs {
            write_need(&mut out, n);
        }
    }
    Ok(out)
}

/// Whether a file is a Sphinx-Needs export: named `needs.json`, or JSON whose
/// versions hold needs.
pub fn is_needs_json(name: &str, data: &[u8]) -> bool {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    if file.eq_ignore_ascii_case("needs.json") {
        return true;
    }
    file.to_ascii_lowercase().ends_with(".json") && data.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') && {
        let head = &data[..data.len().min(4096)];
        let head = String::from_utf8_lossy(head);
        head.contains("\"versions\"") && head.contains("\"needs\"")
    }
}

/// Whether two `needs.json` files hold the same needs, apart from build time,
/// line numbers and order.
pub fn same_needs_json(left: &[u8], right: &[u8]) -> Option<bool> {
    Some(parse_json(left).ok()? == parse_json(right).ok()?)
}

/// Whether two reST texts have the same needs, though the text around them may differ.
pub fn same_rst_needs(left: &str, right: &str) -> bool {
    let (l, r) = (parse_rst(left), parse_rst(right));
    (!l.0.is_empty() || !l.1.is_empty()) && l == r
}

#[cfg(test)]
mod tests {
    use super::*;

    const RST: &str = "\
Requirements
============

.. req:: Login with
   password
   :id: REQ_001
   :status: open
   :tags: security, auth

   The user can log in with a user name
   and a password.

   .. spec:: Password rules
      :id: SPEC_001
      :links: REQ_001

      At least 12 characters.

.. impl:: Login form
   :id: IMPL_001
   :links: SPEC_001, REQ_001

   Implemented in ``login.py``.

.. needtable::
   :types: req

.. story:: Without a default type
   :id: ST_9

.. needextend:: REQ_001
   :status: closed
";

    /// Only the list fields of Sphinx-Needs ignore order; an extra option is text.
    #[test]
    fn only_list_fields_ignore_order() {
        let rst = |code: &str, tags: &str| {
            format!(".. req:: R\n   :id: R_1\n   :product_code: {code}\n   :tags: {tags}\n   :links: A_1; B_1\n")
        };
        assert!(same_rst_needs(&rst("alpha,beta", "x, y"), &rst("alpha,beta", "y | x")));
        for (a, b) in [("alpha,beta", "beta,alpha"), ("a;b", "b;a"), ("a|b", "b|a")] {
            assert!(!same_rst_needs(&rst(a, "x"), &rst(b, "x")), "{a} / {b}");
        }
        let json = |code: &str, links: &str| {
            format!(
                r#"{{"versions": {{"1": {{"needs": {{"R_1": {{"id": "R_1", "type": "req", "product_code": "{code}", "links": {links}}}}}}}}}}}"#
            )
        };
        let same = |a: String, b: String| same_needs_json(a.as_bytes(), b.as_bytes()).unwrap();
        assert!(same(json("a,b", r#"["A_1", "B_1"]"#), json("a,b", r#"["B_1", "A_1"]"#)));
        assert!(!same(json("a,b", "[]"), json("b,a", "[]")));
    }

    #[test]
    fn needs_from_rst() {
        let (needs, ext) = parse_rst(RST);
        let ids: Vec<_> = needs.iter().map(|n| n.id.clone().unwrap()).collect();
        assert_eq!(ids, ["IMPL_001", "REQ_001", "SPEC_001", "ST_9"]);
        let req = &needs[1];
        assert_eq!(req.title, "Login with password");
        assert_eq!(req.fields["tags"], "auth, security");
        assert_eq!(req.content, "The user can log in with a user name and a password.\n\n↳ SPEC_001");
        let spec = &needs[2];
        assert_eq!(spec.fields["parent"], "REQ_001");
        assert_eq!(spec.content, "At least 12 characters.");
        assert_eq!(needs[0].fields["links"], "REQ_001, SPEC_001");
        assert_eq!(
            ext,
            [Extension { filter: "REQ_001".into(), fields: BTreeMap::from([("status".into(), "closed".into())]) }]
        );
        let view = render_rst(RST);
        assert!(view.starts_with("=== 4 needs: 1 impl, 1 req, 1 spec, 1 story ===\n\n--- IMPL_001 [impl] Login form ---\nlinks: REQ_001, SPEC_001\n"));
        assert!(view.contains(
            "--- REQ_001 [req] Login with password ---\nstatus: open\ntags: auth, security\n    The user can"
        ));
        assert!(view.ends_with("--- needextend REQ_001 ---\nstatus: closed\n\n"));
    }

    #[test]
    fn moved_and_rewrapped_needs_are_the_same() {
        let moved = "\
.. impl:: Login form
   :links: REQ_001,SPEC_001
   :id: IMPL_001

   Implemented in
   ``login.py``.

Requirements
=================

.. story:: Without a default type
   :id: ST_9

.. req:: Login with password
   :status: open
   :id: REQ_001
   :tags: auth, security

   The user can log in with a user name and a password.

   .. spec:: Password rules
      :links: REQ_001
      :id: SPEC_001

      At least 12
      characters.

.. needextend:: REQ_001
   :status: closed
";
        assert!(same_rst_needs(RST, moved));
        let changed = moved.replace(":status: open", ":status: done");
        assert!(!same_rst_needs(RST, &changed));
        assert!(!rst_has_needs("Just text.\n\n.. note:: Hi\n"));
    }
}
