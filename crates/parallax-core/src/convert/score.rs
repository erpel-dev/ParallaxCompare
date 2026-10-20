//! Conservative, lossless S-CORE JSON configuration comparisons.
//!
//! Only recognized collections are keyed by identity. Arbitrary arrays, process
//! arguments, parameter matrices, dependency lists and vendor data retain order.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::{Error, Result};

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_NODES: usize = 1_000_000;
const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Json {
    Atom(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

fn bad(message: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("S-CORE JSON: {message}"))
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
    nodes: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.text.as_bytes().get(self.at).is_some_and(|c| matches!(c, b' ' | b'\n' | b'\r' | b'\t')) {
            self.at += 1;
        }
    }

    fn take(&mut self, c: u8) -> bool {
        self.space();
        if self.text.as_bytes().get(self.at) == Some(&c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn string(&mut self) -> Result<String> {
        self.space();
        let start = self.at;
        if !self.take(b'"') {
            return Err(bad("expected a quoted string"));
        }
        while let Some(&c) = self.text.as_bytes().get(self.at) {
            self.at += 1;
            if c == b'\\' {
                if self.at == self.text.len() {
                    return Err(bad("unfinished string escape"));
                }
                self.at += 1;
            } else if c == b'"' {
                return serde_json::from_str(&self.text[start..self.at]).map_err(bad);
            }
        }
        Err(bad("unterminated string"))
    }

    fn value(&mut self, depth: usize) -> Result<Json> {
        self.space();
        self.nodes += 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return Err(bad("too much JSON nesting or too many values"));
        }
        match self.text.as_bytes().get(self.at) {
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b'{') => {
                self.at += 1;
                let mut out = BTreeMap::new();
                if self.take(b'}') {
                    return Ok(Json::Object(out));
                }
                loop {
                    let key = self.string()?;
                    if !self.take(b':') {
                        return Err(bad("expected ':' after an object key"));
                    }
                    let value = self.value(depth + 1)?;
                    if out.insert(key.clone(), value).is_some() {
                        return Err(bad(format!("duplicate object key {key:?}")));
                    }
                    if self.take(b'}') {
                        break;
                    }
                    if !self.take(b',') {
                        return Err(bad("expected ',' or '}'"));
                    }
                }
                Ok(Json::Object(out))
            }
            Some(b'[') => {
                self.at += 1;
                let mut out = Vec::new();
                if self.take(b']') {
                    return Ok(Json::Array(out));
                }
                loop {
                    out.push(self.value(depth + 1)?);
                    if self.take(b']') {
                        break;
                    }
                    if !self.take(b',') {
                        return Err(bad("expected ',' or ']'"));
                    }
                }
                Ok(Json::Array(out))
            }
            _ => {
                let start = self.at;
                while self.text.as_bytes().get(self.at).is_some_and(|c| !c.is_ascii_whitespace() && !b",]}".contains(c))
                {
                    self.at += 1;
                }
                let token = &self.text[start..self.at];
                if matches!(token, "true" | "false" | "null") {
                    return Ok(Json::Atom(token.into()));
                }
                // Validate the JSON number grammar without rounding via f64.
                let bytes = token.as_bytes();
                let mut i = usize::from(bytes.first() == Some(&b'-'));
                if bytes.get(i) == Some(&b'0') {
                    i += 1;
                } else {
                    let begin = i;
                    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    if i == begin {
                        return Err(bad("expected a JSON value"));
                    }
                }
                if bytes.get(i) == Some(&b'.') {
                    i += 1;
                    let begin = i;
                    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    if i == begin {
                        return Err(bad("missing fraction digits"));
                    }
                }
                if bytes.get(i).is_some_and(|c| matches!(c, b'e' | b'E')) {
                    i += 1;
                    if bytes.get(i).is_some_and(|c| matches!(c, b'+' | b'-')) {
                        i += 1;
                    }
                    let begin = i;
                    while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                        i += 1;
                    }
                    if i == begin {
                        return Err(bad("missing exponent digits"));
                    }
                }
                if i != bytes.len() {
                    return Err(bad(format!("invalid JSON number {token:?}")));
                }
                Ok(Json::Atom(super::dcm::number(token).unwrap_or_else(|| token.into())))
            }
        }
    }
}

pub(super) fn parse(data: &[u8]) -> Result<Json> {
    if data.len() > MAX_BYTES {
        return Err(bad("file exceeds the 64 MiB view limit"));
    }
    let decoded = crate::text::decode(data);
    if !decoded.encode().is_ok_and(|bytes| bytes == data) {
        return Err(bad("the text encoding is damaged"));
    }
    let text = decoded.text.trim_start_matches('\u{feff}');
    let mut parser = Parser { text, at: 0, nodes: 0 };
    let value = parser.value(0)?;
    parser.space();
    if parser.at != text.len() {
        return Err(bad("trailing data after JSON"));
    }
    Ok(value)
}

impl Json {
    fn object(&self) -> Option<&BTreeMap<String, Json>> {
        if let Self::Object(o) = self { Some(o) } else { None }
    }
    fn get(&self, key: &str) -> Option<&Self> {
        self.object()?.get(key)
    }
    fn key(&self) -> Option<String> {
        match self {
            Self::String(v) => Some(format!("s:{v}")),
            Self::Atom(v) => Some(format!("v:{v}")),
            Self::Object(o) if o.keys().all(|k| matches!(k.as_str(), "major" | "minor")) => Some(self.compact()),
            _ => None,
        }
    }
    pub(super) fn compact(&self) -> String {
        let mut out = String::new();
        self.render(&mut out, 0);
        out.trim_end().into()
    }
    fn render(&self, out: &mut String, depth: usize) {
        let indent = |n| "  ".repeat(n);
        match self {
            Self::Atom(s) => out.push_str(s),
            Self::String(s) => out.push_str(&serde_json::to_string(s).expect("string")),
            Self::Array(a) => {
                out.push('[');
                for (i, value) in a.iter().enumerate() {
                    let _ = write!(out, "\n{}", indent(depth + 1));
                    value.render(out, depth + 1);
                    if i + 1 < a.len() {
                        out.push(',');
                    }
                }
                if !a.is_empty() {
                    let _ = write!(out, "\n{}", indent(depth));
                }
                out.push(']');
            }
            Self::Object(o) => {
                out.push('{');
                for (i, (key, value)) in o.iter().enumerate() {
                    let _ = write!(out, "\n{}{}: ", indent(depth + 1), serde_json::to_string(key).expect("key"));
                    value.render(out, depth + 1);
                    if i + 1 < o.len() {
                        out.push(',');
                    }
                }
                if !o.is_empty() {
                    let _ = write!(out, "\n{}", indent(depth));
                }
                out.push('}');
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Format {
    Communication,
    Lifecycle,
    Logging,
    Parameters,
    Schema,
    Feo,
}

fn format(name: &str, value: &Json) -> Option<Format> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    if value.get("serviceTypes").is_some() && value.get("serviceInstances").is_some() {
        Some(Format::Communication)
    } else if value.get("components").is_some()
        && (value.get("runTargets").is_some() || value.get("run_targets").is_some())
    {
        Some(Format::Lifecycle)
    } else if value.get("parameterSets").is_some() {
        Some(Format::Parameters)
    } else if (name.contains("logging") || name == "log-channels.json")
        && (value.get("appId").is_some()
            || value.get("contextConfigs").is_some()
            || (value.get("channels").is_some() && value.get("channelAssignments").is_some()))
    {
        Some(Format::Logging)
    } else if value.get("$schema").is_some()
        && value.get("title").is_some_and(|v| matches!(v, Json::String(s) if s.contains("S-CORE")))
    {
        Some(Format::Schema)
    } else if value.get("agent_assignments").is_some() && value.get("activity_deps").is_some() {
        Some(Format::Feo)
    } else {
        None
    }
}

pub fn is_config(name: &str, data: &[u8]) -> bool {
    name.to_ascii_lowercase().ends_with(".json") && parse(data).is_ok_and(|v| format(name, &v).is_some())
}

fn normalize(value: &mut Json, format: Format, path: &mut Vec<String>) -> Result<()> {
    if let Json::Object(object) = value {
        for (key, value) in object {
            path.push(key.clone());
            normalize(value, format, path)?;
            path.pop();
        }
    } else if let Json::Array(array) = value {
        // Match complete paths: an identically named vendor field is not a set.
        let p = path.iter().map(String::as_str).collect::<Vec<_>>();
        let keys: &[&str] = match (format, p.as_slice()) {
            (Format::Communication, ["serviceTypes"]) => &["serviceTypeName", "version"],
            (Format::Communication, ["serviceInstances"]) => &["instanceSpecifier"],
            (Format::Communication, ["serviceTypes", "[]", "bindings"]) => &["binding"],
            (Format::Communication, ["serviceTypes", "[]", "bindings", "[]", "events"]) => &["eventName"],
            (Format::Communication, ["serviceTypes", "[]", "bindings", "[]", "fields"]) => &["fieldName"],
            (Format::Communication, ["serviceTypes", "[]", "bindings", "[]", "methods"]) => &["methodName"],
            (Format::Communication, ["serviceInstances", "[]", "instances"]) => &["binding", "instanceId"],
            (Format::Communication, ["serviceInstances", "[]", "instances", "[]", "events"]) => &["eventName"],
            (Format::Communication, ["serviceInstances", "[]", "instances", "[]", "fields"]) => &["fieldName"],
            (Format::Communication, ["serviceInstances", "[]", "instances", "[]", "methods"]) => &["methodName"],
            (Format::Lifecycle, ["components"] | ["run_targets"] | ["runTargets"]) => &["name"],
            (Format::Logging, ["contextConfigs"]) => &["name"],
            _ => &[],
        };
        path.push("[]".into());
        for value in array.iter_mut() {
            normalize(value, format, path)?;
        }
        path.pop();
        if !keys.is_empty() {
            let identities: Option<Vec<Vec<String>>> =
                array.iter().map(|v| keys.iter().map(|k| v.get(k)?.key()).collect()).collect();
            if let Some(identities) = identities {
                if identities.iter().collect::<BTreeSet<_>>().len() != identities.len() {
                    return Err(bad(format!("duplicate identity in {}", path.join("/"))));
                }
                let mut entries = identities.into_iter().zip(std::mem::take(array)).collect::<Vec<_>>();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                *array = entries.into_iter().map(|(_, value)| value).collect();
            }
        }
    }
    Ok(())
}

pub fn configuration(name: &str, data: &[u8]) -> Result<String> {
    let mut value = parse(data)?;
    let format = format(name, &value).ok_or_else(|| bad("not a recognized S-CORE configuration"))?;
    normalize(&mut value, format, &mut Vec::new())?;
    Ok(format!("{}\n", value.compact()))
}

pub(super) fn decoded_configuration(mut value: Json) -> Result<String> {
    if let Some(format) = format("decoded.json", &value) {
        normalize(&mut value, format, &mut Vec::new())?;
    }
    Ok(format!("{}\n", value.compact()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/score").join(name)).unwrap()
    }

    #[test]
    fn named_configurations_align_without_discarding_changes() {
        for name in ["communication", "launch", "logging", "parameters"] {
            let file = format!("{name}.json");
            let a = fixture(&format!("{name}-v1.json"));
            let b = fixture(&format!("{name}-reordered.json"));
            let c = fixture(&format!("{name}-v2.json"));
            assert!(is_config(&file, &a));
            assert_eq!(configuration(&file, &a).unwrap(), configuration(&file, &b).unwrap(), "{name}");
            assert_ne!(configuration(&file, &a).unwrap(), configuration(&file, &c).unwrap(), "{name}");
        }
    }

    #[test]
    fn unknown_arrays_arguments_and_parameter_matrices_keep_order() {
        let a = fixture("communication-v1.json");
        let mut b: serde_json::Value = serde_json::from_slice(&a).unwrap();
        b["vendor"]["ordered"] = serde_json::json!([3, 2, 1]);
        assert_ne!(
            configuration("mw_com_config.json", &a).unwrap(),
            configuration("mw_com_config.json", &serde_json::to_vec(&b).unwrap()).unwrap()
        );
        let a = br#"{"components":[{"name":"A","process_arguments":["first","second"]}],"run_targets":[]}"#;
        let b = String::from_utf8_lossy(a).replace("\"first\",\"second\"", "\"second\",\"first\"");
        assert_ne!(configuration("lm_config.json", a).unwrap(), configuration("lm_config.json", b.as_bytes()).unwrap());
        let a = br#"{"parameterSets":{"P":{"parameters":{"M":{"initValue":[[1,2],[3,4]]}}}}}"#;
        assert_ne!(
            configuration("params.json", a).unwrap(),
            configuration(
                "params.json",
                String::from_utf8_lossy(a).replace("[[1,2],[3,4]]", "[[3,4],[1,2]]").as_bytes()
            )
            .unwrap()
        );
    }

    #[test]
    fn exact_numbers_and_duplicate_names_are_safe() {
        let a = br#"{"parameterSets":{"P":{"parameters":{"V":{"initValue":9007199254740993.0}}}}}"#;
        let original = configuration("params.json", a).unwrap();
        assert_eq!(
            original,
            configuration(
                "params.json",
                String::from_utf8_lossy(a).replace("9007199254740993.0", "9.007199254740993e15").as_bytes()
            )
            .unwrap()
        );
        assert_ne!(
            original,
            configuration("params.json", String::from_utf8_lossy(a).replace("993.0", "994.0").as_bytes()).unwrap()
        );
        let duplicate = br#"{"serviceTypes":[{"serviceTypeName":"A","version":{"major":1}},{"serviceTypeName":"A","version":{"major":1}}],"serviceInstances":[]}"#;
        assert!(configuration("mw_com_config.json", duplicate).is_err());
        for invalid in [
            br#"{"parameterSets":{},"parameterSets":{}}"#.as_slice(),
            b"[1,]",
            b"{\"a\":01}",
            b"\x0c{}",
            b"{} trailing",
            b"[true,false",
            b"\"unterminated",
        ] {
            assert!(parse(invalid).is_err(), "{invalid:?}");
        }
        assert!(parse(&vec![b' '; MAX_BYTES + 1]).is_err());
    }
}
