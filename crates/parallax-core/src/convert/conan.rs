//! Conan package manager files, besides `conan.lock` (see `lockfile`).
//!
//! - The JSON that `conan graph info`, `conan install` and `conan create` write
//!   with `--format=json`: the "Conan graph" view lists every node by reference
//!   with its package id, package revision, binary status, settings, options and
//!   direct requirements, sorted, without the paths, timestamps, recipe metadata
//!   and node numbers that make up most of the file. Two graphs that differ only
//!   in those, in binary status (cache, download, build) or remotes are similar.
//! - `conanfile.txt`, profiles, `conaninfo.txt` and `conanbuildinfo.txt`: the
//!   "Normalized" view drops comments, spacing around `=` and blank lines, puts
//!   the sections in alphabetical order (merging repeated ones) and sorts the
//!   generators. The order of other entries is kept: later settings, conf and
//!   environment entries override or append to earlier ones. Profiles using
//!   Jinja keep their section order too.
//! - `conanmanifest.txt`: two manifests listing the same files and checksums are
//!   similar when only the time on the first line differs.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::Value;

use crate::{Error, Result};

fn base_name(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase()
}

// ---- graph JSON ----

/// Whether a JSON file is a Conan 2 dependency graph.
pub fn is_graph_json(name: &str, data: &[u8]) -> bool {
    if !base_name(name).ends_with(".json") || data.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return false;
    }
    let head = String::from_utf8_lossy(&data[..data.len().min(512)]);
    head.contains("\"graph\"") && head.contains("\"nodes\"")
}

/// File names the folder compare reads for the graph rule.
pub fn is_graph_name(name: &str) -> bool {
    let base = base_name(name);
    matches!(base.as_str(), "graph.json" | "install.json" | "create.json" | "build.json" | "deps.json")
        || (base.starts_with("conan") && base.ends_with(".json"))
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

/// `key=value` lines of a settings or options object, sorted by key.
fn pairs(v: Option<&Value>) -> Vec<String> {
    let Some(Value::Object(map)) = v else { return Vec::new() };
    let mut list: Vec<(&String, String)> = map.iter().filter_map(|(k, v)| text_of(v).map(|v| (k, v))).collect();
    list.sort();
    list.into_iter().map(|(k, v)| format!("{k}={v}")).collect()
}

/// The graph as text; `volatile` adds what does not count for similarity
/// (binary status and remotes).
fn graph_text(data: &[u8], volatile: bool) -> Result<String> {
    let root: Value =
        serde_json::from_slice(data).map_err(|e| Error::Unsupported(format!("not a Conan graph: {e}")))?;
    let graph = root.get("graph").ok_or_else(|| Error::Unsupported("not a Conan graph: no \"graph\"".into()))?;
    let Some(Value::Object(nodes)) = graph.get("nodes") else {
        return Err(Error::Unsupported("not a Conan graph: no nodes".into()));
    };
    let str_of = |n: &Value, key: &str| n.get(key).and_then(text_of);
    let is_consumer = |n: &Value| str_of(n, "recipe").as_deref() == Some("Consumer");
    let heading = |n: &Value| {
        let r = str_of(n, "ref").unwrap_or_else(|| "?".into());
        match str_of(n, "context").as_deref() {
            _ if is_consumer(n) => format!("Consumer {r}"),
            Some("build") => format!("{r} (build context)"),
            _ => r,
        }
    };

    let mut out = String::new();
    let line = |out: &mut String, label: &str, value: &str| {
        let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
    };
    if let Some(e) = graph.get("error").and_then(text_of) {
        line(&mut out, "Error", &e);
    }
    let mut contexts: BTreeMap<String, usize> = BTreeMap::new();
    let mut binaries: BTreeMap<String, usize> = BTreeMap::new();
    for n in nodes.values().filter(|n| !is_consumer(n)) {
        *contexts.entry(str_of(n, "context").unwrap_or_else(|| "host".into())).or_default() += 1;
        if let Some(b) = str_of(n, "binary") {
            *binaries.entry(b).or_default() += 1;
        }
    }
    let counts = |m: &BTreeMap<String, usize>| m.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ");
    line(&mut out, "Packages", &format!("{} ({})", contexts.values().sum::<usize>(), counts(&contexts)));
    if volatile && !binaries.is_empty() {
        line(&mut out, "Binaries", &counts(&binaries));
    }
    for (key, title) in [
        ("resolved_ranges", "Resolved version ranges"),
        ("overrides", "Overrides"),
        ("replaced_requires", "Replaced requirements"),
    ] {
        if let Some(Value::Object(m)) = graph.get(key)
            && !m.is_empty()
        {
            let _ = writeln!(out, "\n=== {title} ===");
            for (k, v) in m {
                let _ = writeln!(out, "{k} → {}", text_of(v).unwrap_or_default());
            }
        }
    }

    let mut order: Vec<&Value> = nodes.values().collect();
    order.sort_by_cached_key(|n| (!is_consumer(n), heading(n)));
    for n in order {
        let _ = writeln!(out, "\n=== {} ===", heading(n));
        for (key, label) in
            [("package_type", "Package type"), ("package_id", "Package ID"), ("prev", "Package revision")]
        {
            if let Some(v) = str_of(n, key).filter(|v| v != "unknown") {
                line(&mut out, label, &v);
            }
        }
        if volatile {
            if let Some(v) = str_of(n, "binary") {
                line(&mut out, "Binary", &v);
            }
            if let Some(v) = str_of(n, "binary_remote").or_else(|| str_of(n, "remote")) {
                line(&mut out, "Remote", &v);
            }
        }
        for (key, label) in [("settings", "Settings"), ("options", "Options")] {
            let list = pairs(n.get(key));
            if !list.is_empty() {
                let _ = writeln!(out, "{label}:");
                for p in list {
                    let _ = writeln!(out, "  {p}");
                }
            }
        }
        // Direct requirements by kind, named by the full reference of their node.
        let mut deps: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        if let Some(Value::Object(d)) = n.get("dependencies") {
            for (id, dep) in d {
                if dep.get("direct") != Some(&Value::Bool(true)) {
                    continue;
                }
                let flag = |k: &str| dep.get(k) == Some(&Value::Bool(true));
                let kind = if flag("build") {
                    "Tool requires"
                } else if flag("test") {
                    "Test requires"
                } else {
                    "Requires"
                };
                let r = nodes.get(id).and_then(|n| str_of(n, "ref")).or_else(|| str_of(dep, "ref"));
                deps.entry(kind).or_default().push(r.unwrap_or_else(|| "?".into()));
            }
        }
        for (kind, mut refs) in deps {
            refs.sort();
            let _ = writeln!(out, "{kind}:");
            for r in refs {
                let _ = writeln!(out, "  {r}");
            }
        }
    }
    Ok(out)
}

pub fn graph(data: &[u8]) -> Result<String> {
    graph_text(data, true)
}

/// Whether two graphs have the same packages, binaries and requirements.
pub fn same_graph(left: &[u8], right: &[u8]) -> Option<bool> {
    Some(graph_text(left, false).ok()? == graph_text(right, false).ok()?)
}

// ---- conanfile.txt, profiles, conaninfo.txt ----

/// Section names of conanfile.txt, profiles and conaninfo.txt.
const SECTIONS: &[&str] = &[
    "settings",
    "options",
    "conf",
    "buildenv",
    "runenv",
    "tool_requires",
    "build_requires",
    "test_requires",
    "requires",
    "python_requires",
    "platform_requires",
    "platform_tool_requires",
    "replace_requires",
    "replace_tool_requires",
    "system_tools",
    "generators",
    "layout",
    "imports",
    "env",
];

/// Sections whose entries are a set.
const UNORDERED: &[&str] = &["generators"];

/// Whether a file is one of Conan's INI-like files: by name, or a profile (no
/// extension) by its first entry.
pub fn is_ini(name: &str, data: &[u8]) -> bool {
    let base = base_name(name);
    if matches!(base.as_str(), "conanfile.txt" | "conaninfo.txt" | "conanbuildinfo.txt") {
        return true;
    }
    if base.contains('.') || data.len() > 256 * 1024 {
        return false;
    }
    let Ok(text) = std::str::from_utf8(&data[..data.len().min(4096)]) else { return false };
    let first = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("{%"));
    first.is_some_and(|l| {
        l.starts_with("include(")
            || l.strip_prefix('[').and_then(|l| l.strip_suffix(']')).is_some_and(|s| SECTIONS.contains(&s))
    })
}

/// One entry with the spacing around its operator removed.
fn entry(line: &str) -> String {
    let Some((key, value)) = line.split_once('=') else { return line.to_string() };
    let key = key.trim_end();
    // `+=` and `?=` keep their first character with the key.
    let (key, op) = match key.char_indices().last() {
        Some((i, c @ ('+' | '?'))) => (key[..i].trim_end(), format!("{c}=")),
        _ => (key, "=".to_string()),
    };
    format!("{key}{op}{}", value.trim())
}

/// The file with comments, blank lines and spacing dropped, sections sorted.
pub fn normalized(text: &str) -> String {
    let jinja = text.contains("{%") || text.contains("{{");
    let mut head: Vec<String> = Vec::new();
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim().to_string();
            match sections.iter().position(|(n, _)| *n == name).filter(|_| !jinja) {
                // A repeated section continues the first one.
                Some(at) => {
                    let s = sections.remove(at);
                    sections.push(s);
                }
                None => sections.push((name, Vec::new())),
            }
            continue;
        }
        let item = if line.starts_with("{%") || line.starts_with("{#") { line.to_string() } else { entry(line) };
        match sections.last_mut() {
            Some((_, items)) => items.push(item),
            None => head.push(item),
        }
    }
    if !jinja {
        // Stable: repeated sections were merged above, so the order inside stays.
        sections.sort_by(|a, b| a.0.cmp(&b.0));
    }
    let mut out = String::new();
    for h in &head {
        out.push_str(h);
        out.push('\n');
    }
    for (name, mut items) in sections {
        if UNORDERED.contains(&name.as_str()) {
            items.sort();
            items.dedup();
        }
        if !out.is_empty() {
            out.push('\n');
        }
        let _ = writeln!(out, "[{name}]");
        for i in items {
            out.push_str(&i);
            out.push('\n');
        }
    }
    out
}

// ---- conanmanifest.txt ----

pub fn is_manifest(name: &str) -> bool {
    base_name(name) == "conanmanifest.txt"
}

/// Whether two manifests list the same files and checksums; the first line is
/// the time the package was made.
pub fn same_manifest(left: &str, right: &str) -> bool {
    fn files(t: &str) -> Vec<&str> {
        t.lines().skip(1).map(str::trim_end).filter(|l| !l.is_empty()).collect()
    }
    files(left) == files(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_profiles_and_conanfiles() {
        assert!(is_ini("proj/conanfile.txt", b""));
        assert!(is_ini("profiles/linux-gcc", b"# gcc\ninclude(default)\n[settings]\n"));
        assert!(is_ini("default", b"[settings]\narch=x86_64\n"));
        assert!(!is_ini("Makefile", b"all:\n\tcc main.c\n"));
        assert!(!is_ini("notes.ini", b"[settings]\n"));
        assert!(!is_ini("README", b"[link]\n"));
    }

    #[test]
    fn normalizes_ini_files() {
        let a = "# deps\n[requires]\nzlib/1.3\nfmt/10.2.1\n\n[generators]\nCMakeToolchain\nCMakeDeps\n[options]\nzlib/*:shared = True\n[conf]\ntools.build:cxxflags += [\"-O2\"]\n";
        let b = "[options]\nzlib/*:shared=True\n\n[generators]\nCMakeDeps\nCMakeToolchain\n\n[conf]\ntools.build:cxxflags+=[\"-O2\"]\n[requires]\nzlib/1.3\nfmt/10.2.1\n";
        assert_eq!(normalized(a), normalized(b));
        assert_eq!(
            normalized(a),
            "[conf]\ntools.build:cxxflags+=[\"-O2\"]\n\n[generators]\nCMakeDeps\nCMakeToolchain\n\n[options]\nzlib/*:shared=True\n\n[requires]\nzlib/1.3\nfmt/10.2.1\n"
        );
        // The order of requirements counts.
        assert_ne!(normalized("[requires]\na/1\nb/1\n"), normalized("[requires]\nb/1\na/1\n"));
        // A repeated section continues the first, keeping overrides in order.
        assert_eq!(
            normalized("[conf]\na=1\n[settings]\nos=Linux\n[conf]\na=2\n"),
            "[conf]\na=1\na=2\n\n[settings]\nos=Linux\n"
        );
        // Includes stay on top; profiles with Jinja keep their section order.
        assert_eq!(
            normalized("include(default)\n[settings]\nos=Linux\n"),
            "include(default)\n\n[settings]\nos=Linux\n"
        );
        let jinja = "{% set v = 12 %}\n[settings]\ncompiler.version={{ v }}\n[conf]\nx=1\n";
        assert_eq!(normalized(jinja), "{% set v = 12 %}\n\n[settings]\ncompiler.version={{ v }}\n\n[conf]\nx=1\n");
    }

    #[test]
    fn manifests_differ_in_time_only() {
        let a = "1792328320\nconaninfo.txt: 7e03\nlib/libgreet.so: c118\n";
        assert!(same_manifest(a, "1792329999\nconaninfo.txt: 7e03\nlib/libgreet.so: c118\n"));
        assert!(!same_manifest(a, "1792328320\nconaninfo.txt: 7e03\nlib/libgreet.so: ffff\n"));
    }
}
