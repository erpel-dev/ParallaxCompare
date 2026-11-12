//! Copying between the sides of an ECUC values comparison and saving gives a
//! proper ECUC value file, in every kind of difference.
//!
//! Each case under `fixtures/ecuc/copy` is a pair of files. For both directions
//! the test copies every difference on its own, then all of them one after the
//! other with a save in between, then everything at once, and finally every
//! single differing line. A save must either give a well-formed ARXML file whose
//! ECUC structure is valid (element order, required parts, definitions and
//! references with their DEST, unique short names) and that shows exactly the
//! edited text, or be refused with a message; it must never write a broken file.

use std::path::Path;

use parallax_core::convert::{self, View};
use parallax_core::edit::{EditSession, Side};
use parallax_core::text::decode;
use parallax_core::textdiff::{DiffOptions, RowKind};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

fn view(bytes: &[u8]) -> String {
    convert::render(View::EcucValues, "Ecuc.arxml", bytes).expect("ECUC values view")
}

fn session(left: &str, right: &str) -> EditSession {
    let keyed = DiffOptions { keyed: true, ..Default::default() };
    EditSession::new(decode(left.as_bytes()), decode(right.as_bytes()), keyed)
}

/// Rows that differ, other than in a comma.
fn differing(a: &str, b: &str) -> usize {
    let s = session(a, b);
    s.diff().rows.iter().filter(|r| matches!(r.kind, RowKind::Changed | RowKind::Added | RowKind::Removed)).count()
}

/// The configuration a view's text holds, written the one way the view writes it.
fn canonical(text: &str) -> String {
    convert::canonical_text(View::EcucValues, text).expect("the edited text reads")
}

/// Saves `edited` (the view of `to`) into the file `to`; the other file is the donor.
fn save(to: &[u8], edited: &str, from: &[u8], what: &str) -> Vec<u8> {
    let saved = convert::write_back(View::EcucValues, to, edited, Some(from))
        .unwrap_or_else(|e| panic!("{what}: saving failed: {e}"));
    check_ecuc(&saved)
        .unwrap_or_else(|e| panic!("{what}: not a proper ECUC file: {e}\n{}", String::from_utf8_lossy(&saved)));
    // ECUC_COPY_OUT=dir keeps every saved file, for checking with other tools.
    if let Some(dir) = std::env::var_os("ECUC_COPY_OUT") {
        let name: String = what.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        std::fs::write(Path::new(&dir).join(format!("{name}.arxml")), &saved).unwrap();
    }
    // The saved file shows exactly what was edited.
    let shown = view(&saved);
    assert_eq!(shown, canonical(edited), "{what}: the saved file shows something else\n{shown}");
    saved
}

#[test]
fn copying_ecuc_values_gives_proper_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ecuc/copy");
    let mut cases: Vec<_> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).collect();
    cases.sort();
    assert!(cases.len() >= 10, "cases in {}", root.display());
    let (mut saves, mut refused) = (0, 0);
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().to_string();
        let files = [std::fs::read(case.join("left.arxml")).unwrap(), std::fs::read(case.join("right.arxml")).unwrap()];
        for file in &files {
            check_ecuc(file).unwrap_or_else(|e| panic!("{name}: the case file itself is not valid: {e}"));
        }
        let views = [view(&files[0]), view(&files[1])];
        assert!(differing(&views[0], &views[1]) > 0, "{name}: the sides should differ");
        for to in [Side::Left, Side::Right] {
            let (t, f) = if to == Side::Left { (0, 1) } else { (1, 0) };
            let dir = if to == Side::Left { "to the left" } else { "to the right" };

            // Each difference on its own.
            let base = session(&views[0], &views[1]);
            let sections = base.diff().sections.clone();
            for (i, sec) in sections.iter().enumerate() {
                let what = format!("{name}: difference {} of {} copied {dir}", i + 1, sections.len());
                let mut s = session(&views[0], &views[1]);
                s.copy_rows(sec.start..sec.end, to).expect("copy");
                let edited = s.doc(to).text.clone();
                let saved = save(&files[t], &edited, &files[f], &what);
                saves += 1;
                assert_ne!(view(&saved), views[t], "{what}: the saved configuration did not change");
            }

            // One after the other, saving each time, until the sides are equal.
            let mut current = files[t].clone();
            for step in 0..=sections.len() {
                let mine = view(&current);
                let (l, r) = if to == Side::Left { (&mine, &views[1]) } else { (&views[0], &mine) };
                let mut s = session(l, r);
                let Some(sec) = s.diff().sections.first().cloned() else { break };
                assert!(step < sections.len(), "{name}: copying {dir} one by one does not end");
                s.copy_rows(sec.start..sec.end, to).expect("copy");
                let edited = s.doc(to).text.clone();
                current = save(&current, &edited, &files[f], &format!("{name}: step {} copying {dir}", step + 1));
                saves += 1;
            }
            assert_eq!(view(&current), views[f], "{name}: copying {dir} one by one ends elsewhere");

            // Everything at once.
            let mut s = session(&views[0], &views[1]);
            let n = s.diff().rows.len() as u32;
            s.copy_rows(0..n, to).expect("copy");
            let edited = s.doc(to).text.clone();
            let saved = save(&files[t], &edited, &files[f], &format!("{name}: everything copied {dir}"));
            saves += 1;
            assert_eq!(view(&saved), views[f], "{name}: everything copied {dir} gives another configuration");

            // Every single differing line. Half a container is not valid JSON: such a
            // save must be refused with a message, never written.
            for (row, r) in base.diff().rows.iter().enumerate() {
                if !matches!(r.kind, RowKind::Changed | RowKind::Added | RowKind::Removed) {
                    continue;
                }
                let mut s = session(&views[0], &views[1]);
                s.copy_rows(row as u32..row as u32 + 1, to).expect("copy");
                let edited = s.doc(to).text.clone();
                let what = format!("{name}: row {} copied {dir}", row + 1);
                match convert::write_back(View::EcucValues, &files[t], &edited, Some(&files[f])) {
                    Ok(saved) => {
                        check_ecuc(&saved).unwrap_or_else(|e| panic!("{what}: not a proper ECUC file: {e}"));
                        assert_eq!(view(&saved), canonical(&edited), "{what}: the saved file shows something else");
                        saves += 1;
                    }
                    Err(e) => {
                        assert!(!e.trim().is_empty(), "{what}: refused without a message");
                        refused += 1;
                    }
                }
            }
        }
    }
    eprintln!("{} cases: {saves} saves checked, {refused} single-line copies refused as not valid", cases.len());
}

// ---------------------------------------------------------------------------
// ECUC structure, as the AUTOSAR 4 schema defines it for the parts these files use

struct El {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<El>,
}

impl El {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
    fn child(&self, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.name == name)
    }
    fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|c| c.text.trim())
    }
}

fn parse(bytes: &[u8]) -> Result<El, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))?;
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    let mut stack = vec![El { name: String::new(), attrs: Vec::new(), text: String::new(), children: Vec::new() }];
    let open = |e: &quick_xml::events::BytesStart<'_>| -> Result<El, String> {
        let attrs =
            e.attributes().flatten().map(|a| (a.key.local_name().as_ref().to_string(), a.value.into_owned())).collect();
        Ok(El { name: e.local_name().as_ref().to_string(), attrs, text: String::new(), children: Vec::new() })
    };
    loop {
        match reader.read_event().map_err(|e| format!("XML error at byte {}: {e}", reader.buffer_position()))? {
            Event::Start(e) => stack.push(open(&e)?),
            Event::Empty(e) => {
                let el = open(&e)?;
                stack.last_mut().unwrap().children.push(el);
            }
            Event::End(_) => {
                let el = stack.pop().unwrap();
                stack.last_mut().ok_or("end tag without start")?.children.push(el);
            }
            Event::Text(t) => stack.last_mut().unwrap().text.push_str(&t.xml10_content()),
            Event::CData(t) => stack.last_mut().unwrap().text.push_str(&t),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => match &*r {
                        "amp" => "&".into(),
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "quot" => "\"".into(),
                        "apos" => "'".into(),
                        other => return Err(format!("unknown entity &{other};")),
                    },
                };
                stack.last_mut().unwrap().text.push_str(&resolved);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err("unclosed elements at the end".into());
    }
    let mut doc = stack.pop().unwrap();
    if doc.children.len() != 1 {
        return Err(format!("{} root elements", doc.children.len()));
    }
    Ok(doc.children.pop().unwrap())
}

const IDENTIFIABLE: [&str; 8] = [
    "SHORT-NAME",
    "SHORT-NAME-FRAGMENTS",
    "LONG-NAME",
    "DESC",
    "CATEGORY",
    "ADMIN-DATA",
    "INTRODUCTION",
    "ANNOTATIONS",
];

/// The children of `el` appear in `order`, each at most once, and `required` ones are there.
fn sequence(el: &El, order: &[&str], required: &[&str], at: &str) -> Result<(), String> {
    let mut last = 0;
    for c in &el.children {
        let Some(pos) = order.iter().position(|o| *o == c.name) else {
            return Err(format!("{at}: <{}> is not allowed in <{}>", c.name, el.name));
        };
        if pos < last || (pos == last && last > 0 && el.children.iter().filter(|x| x.name == c.name).count() > 1) {
            return Err(format!("{at}: <{}> is out of order or repeated in <{}>", c.name, el.name));
        }
        last = pos;
    }
    for r in required {
        if el.child(r).is_none() {
            return Err(format!("{at}: <{}> lacks <{r}>", el.name));
        }
    }
    Ok(())
}

fn short_name(el: &El, at: &str) -> Result<String, String> {
    let name = el.text_of("SHORT-NAME").unwrap_or_default().to_string();
    let ok = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 128;
    if ok { Ok(name) } else { Err(format!("{at}: {name:?} is not a valid short name")) }
}

fn definition(el: &El, dests: &[&str], parent_def: &str, at: &str) -> Result<String, String> {
    let d = el.child("DEFINITION-REF").ok_or(format!("{at}: no DEFINITION-REF"))?;
    let dest = d.attr("DEST").ok_or(format!("{at}: DEFINITION-REF without DEST"))?;
    if !dests.contains(&dest) {
        return Err(format!("{at}: DEFINITION-REF DEST={dest} is not one of {dests:?}"));
    }
    let def = d.text.trim().to_string();
    if !def.starts_with('/') || (!parent_def.is_empty() && !def.starts_with(&format!("{parent_def}/"))) {
        return Err(format!("{at}: definition {def} is not below {parent_def}"));
    }
    Ok(def)
}

fn check_ecuc(bytes: &[u8]) -> Result<(), String> {
    let root = parse(bytes)?;
    if root.name != "AUTOSAR" {
        return Err(format!("the root is <{}>", root.name));
    }
    let mut modules = 0;
    let mut stack = vec![(&root, String::new())];
    while let Some((el, at)) = stack.pop() {
        for c in &el.children {
            match c.name.as_str() {
                "AR-PACKAGE" => {
                    let name = short_name(c, &at)?;
                    stack.push((c, format!("{at}/{name}")));
                }
                "AR-PACKAGES" | "ELEMENTS" => stack.push((c, at.clone())),
                "ECUC-MODULE-CONFIGURATION-VALUES" => {
                    modules += 1;
                    check_module(c, &at)?;
                }
                _ => {}
            }
        }
    }
    if modules == 0 { Err("no ECUC module configuration".into()) } else { Ok(()) }
}

fn check_module(m: &El, pkg: &str) -> Result<(), String> {
    let name = short_name(m, pkg)?;
    let at = format!("{pkg}/{name}");
    let order = [
        &IDENTIFIABLE[..],
        &[
            "DEFINITION-REF",
            "ECUC-DEF-EDITION",
            "IMPLEMENTATION-CONFIG-VARIANT",
            "MODULE-DESCRIPTION-REF",
            "POST-BUILD-VARIANT-USED",
            "CONTAINERS",
        ],
    ]
    .concat();
    sequence(m, &order, &["SHORT-NAME", "DEFINITION-REF"], &at)?;
    let def = definition(m, &["ECUC-MODULE-DEF"], "", &at)?;
    if let Some(v) = m.text_of("IMPLEMENTATION-CONFIG-VARIANT") {
        let variants = [
            "PRECONFIGURED-CONFIGURATION",
            "RECOMMENDED-CONFIGURATION",
            "VARIANT-LINK-TIME",
            "VARIANT-POST-BUILD",
            "VARIANT-PRE-COMPILE",
            "VARIANT-POST-BUILD-LOADABLE",
            "VARIANT-POST-BUILD-SELECTABLE",
        ];
        if !variants.contains(&v) {
            return Err(format!("{at}: {v} is not a configuration variant"));
        }
    }
    if let Some(v) = m.text_of("POST-BUILD-VARIANT-USED")
        && !matches!(v, "true" | "false" | "1" | "0")
    {
        return Err(format!("{at}: POST-BUILD-VARIANT-USED {v}"));
    }
    if let Some(r) = m.child("MODULE-DESCRIPTION-REF")
        && r.attr("DEST") != Some("BSW-IMPLEMENTATION")
    {
        return Err(format!("{at}: MODULE-DESCRIPTION-REF without DEST=BSW-IMPLEMENTATION"));
    }
    if let Some(cs) = m.child("CONTAINERS") {
        check_containers(cs, &def, &at)?;
    }
    Ok(())
}

fn check_containers(wrapper: &El, def: &str, at: &str) -> Result<(), String> {
    if wrapper.children.is_empty() {
        return Err(format!("{at}: empty <{}>", wrapper.name));
    }
    let mut names = std::collections::BTreeSet::new();
    for c in &wrapper.children {
        if c.name != "ECUC-CONTAINER-VALUE" {
            return Err(format!("{at}: <{}> in <{}>", c.name, wrapper.name));
        }
        let name = short_name(c, at)?;
        if !names.insert(name.clone()) {
            return Err(format!("{at}: two containers named {name}"));
        }
        check_container(c, def, &format!("{at}/{name}"))?;
    }
    Ok(())
}

fn check_container(c: &El, parent_def: &str, at: &str) -> Result<(), String> {
    let order = [
        &IDENTIFIABLE[..],
        &["INDEX", "DEFINITION-REF", "PARAMETER-VALUES", "REFERENCE-VALUES", "SUB-CONTAINERS", "VARIATION-POINT"],
    ]
    .concat();
    sequence(c, &order, &["SHORT-NAME", "DEFINITION-REF"], at)?;
    if let Some(i) = c.text_of("INDEX")
        && i.parse::<u64>().is_err()
    {
        return Err(format!("{at}: INDEX {i}"));
    }
    let def = definition(c, &["ECUC-PARAM-CONF-CONTAINER-DEF", "ECUC-CHOICE-CONTAINER-DEF"], parent_def, at)?;
    if let Some(ps) = c.child("PARAMETER-VALUES") {
        if ps.children.is_empty() {
            return Err(format!("{at}: empty PARAMETER-VALUES"));
        }
        for p in &ps.children {
            check_parameter(p, &def, at)?;
        }
    }
    if let Some(rs) = c.child("REFERENCE-VALUES") {
        if rs.children.is_empty() {
            return Err(format!("{at}: empty REFERENCE-VALUES"));
        }
        for r in &rs.children {
            check_reference(r, &def, at)?;
        }
    }
    if let Some(subs) = c.child("SUB-CONTAINERS") {
        check_containers(subs, &def, at)?;
    }
    Ok(())
}

const VALUE_ORDER: [&str; 6] = ["INDEX", "DEFINITION-REF", "ANNOTATIONS", "IS-AUTO-VALUE", "VARIATION-POINT", "VALUE"];

fn check_parameter(p: &El, def: &str, at: &str) -> Result<(), String> {
    let dests: &[&str] = match p.name.as_str() {
        "ECUC-NUMERICAL-PARAM-VALUE" => &["ECUC-INTEGER-PARAM-DEF", "ECUC-FLOAT-PARAM-DEF", "ECUC-BOOLEAN-PARAM-DEF"],
        "ECUC-TEXTUAL-PARAM-VALUE" => &[
            "ECUC-ENUMERATION-PARAM-DEF",
            "ECUC-STRING-PARAM-DEF",
            "ECUC-FUNCTION-NAME-DEF",
            "ECUC-LINKER-SYMBOL-DEF",
            "ECUC-MULTILINE-STRING-PARAM-DEF",
        ],
        other => return Err(format!("{at}: <{other}> in PARAMETER-VALUES")),
    };
    sequence(p, &VALUE_ORDER, &["DEFINITION-REF", "VALUE"], at)?;
    let pdef = definition(p, dests, def, at)?;
    let value = p.text_of("VALUE").unwrap_or_default();
    let dest = p.child("DEFINITION-REF").and_then(|d| d.attr("DEST")).unwrap_or_default();
    let int = |v: &str| {
        let v = v.strip_prefix(['-', '+']).unwrap_or(v);
        let radix = |p: &str, r| v.strip_prefix(p).is_some_and(|d| !d.is_empty() && u128::from_str_radix(d, r).is_ok());
        radix("0x", 16) || radix("0X", 16) || radix("0b", 2) || radix("0B", 2) || v.parse::<u128>().is_ok()
    };
    let ok = match dest {
        "ECUC-INTEGER-PARAM-DEF" => int(value),
        "ECUC-FLOAT-PARAM-DEF" => value.parse::<f64>().is_ok() || matches!(value, "INF" | "-INF" | "NaN"),
        "ECUC-BOOLEAN-PARAM-DEF" => matches!(value, "true" | "false" | "1" | "0"),
        "ECUC-ENUMERATION-PARAM-DEF" | "ECUC-FUNCTION-NAME-DEF" | "ECUC-LINKER-SYMBOL-DEF" => !value.is_empty(),
        _ => true,
    };
    if !ok {
        return Err(format!("{at}: {pdef} has the value {value:?}, not valid for {dest}"));
    }
    Ok(())
}

fn check_reference(r: &El, def: &str, at: &str) -> Result<(), String> {
    match r.name.as_str() {
        "ECUC-REFERENCE-VALUE" => {
            let order = [&VALUE_ORDER[..5], &["VALUE-REF"]].concat();
            sequence(r, &order, &["DEFINITION-REF", "VALUE-REF"], at)?;
            definition(
                r,
                &[
                    "ECUC-REFERENCE-DEF",
                    "ECUC-CHOICE-REFERENCE-DEF",
                    "ECUC-FOREIGN-REFERENCE-DEF",
                    "ECUC-SYMBOLIC-NAME-REFERENCE-DEF",
                    "ECUC-URI-REFERENCE-DEF",
                ],
                def,
                at,
            )?;
            let v = r.child("VALUE-REF").unwrap();
            if v.attr("DEST").is_none_or(str::is_empty) || !v.text.trim().starts_with('/') {
                return Err(format!("{at}: VALUE-REF needs a DEST and an absolute path"));
            }
        }
        "ECUC-INSTANCE-REFERENCE-VALUE" => {
            let order = [&VALUE_ORDER[..5], &["VALUE-IREF"]].concat();
            sequence(r, &order, &["DEFINITION-REF", "VALUE-IREF"], at)?;
            definition(r, &["ECUC-INSTANCE-REFERENCE-DEF"], def, at)?;
            let iref = r.child("VALUE-IREF").unwrap();
            sequence(iref, &["CONTEXT-ELEMENT-REF", "TARGET-REF"], &["TARGET-REF"], at).or_else(|e| {
                // Several context references are allowed, one after the other.
                let contexts = iref.children.iter().take_while(|c| c.name == "CONTEXT-ELEMENT-REF").count();
                let rest: Vec<_> = iref.children[contexts..].iter().map(|c| c.name.as_str()).collect();
                if rest == ["TARGET-REF"] { Ok(()) } else { Err(e) }
            })?;
            for c in &iref.children {
                if c.attr("DEST").is_none_or(str::is_empty) {
                    return Err(format!("{at}: <{}> without DEST", c.name));
                }
            }
        }
        other => return Err(format!("{at}: <{other}> in REFERENCE-VALUES")),
    }
    Ok(())
}
