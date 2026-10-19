//! Jupyter notebooks (`.ipynb`): the cells as source text, outputs in short form.
//!
//! A notebook is JSON with every source line as a string, base64 images inline,
//! and volatile data everywhere (execution counts, cell ids, execution timestamps
//! in the metadata). The views keep what a person reads in Jupyter: each cell's
//! type and source, then its outputs — stream text, the `text/plain` form of
//! results, errors as one line, and every other MIME bundle (images, HTML) as a
//! placeholder with its size and a checksum, so a changed plot shows as one
//! changed line. Cells are not numbered: inserting a cell would renumber every
//! header below it.
//!
//! Outputs are parsed without keeping large payloads: an image is hashed while
//! the JSON parser walks over it, so a 100 MB notebook full of plots costs one
//! pass over the bytes. nbformat 4 is read, and nbformat 3 (cells in
//! `worksheets`, code in `input`) as far as the fields carry over.

use std::fmt::{self, Write as _};

use serde::Deserialize;
use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::{Error, Result};

/// Output lines shown per output before "… N more lines".
const MAX_OUTPUT_LINES: usize = 100;
/// MIME payloads up to this size are kept as text (for `text/plain`); larger ones only hashed.
const MAX_KEPT_TEXT: usize = 256 * 1024;

/// Whether the file is a notebook: the `.ipynb` extension and a JSON object with
/// the `nbformat` key (written last by Jupyter, so it is searched from the end).
pub fn is_notebook(ext: &str, data: &[u8]) -> bool {
    ext == "ipynb"
        && data.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{')
        && memchr::memmem::rfind(data, b"\"nbformat\"").is_some()
}

/// The notebook with outputs; `outputs = false` gives the code-only view.
pub fn render(data: &[u8], outputs: bool) -> Result<String> {
    let nb: Notebook<Vec<Output>> = parse(data)?;
    let cells = nb.cells();
    let mut out = String::with_capacity(data.len().min(1 << 20) / 2);
    line(&mut out, "Notebook", format_args!("nbformat {}.{}", nb.nbformat, nb.nbformat_minor));
    if let Some(k) = &nb.metadata.kernelspec {
        let kernel = k.display_name.as_deref().or(k.name.as_deref()).unwrap_or("?");
        line(&mut out, "Kernel", kernel);
    }
    let language = nb.metadata.language_info.as_ref().and_then(|l| l.name.as_deref());
    if let Some(language) = language.or(nb.metadata.kernelspec.as_ref().and_then(|k| k.language.as_deref())) {
        line(&mut out, "Language", language);
    }
    let count = |t: &str| cells.iter().filter(|c| c.cell_type == t).count();
    let mut kinds: Vec<String> = ["code", "markdown", "raw"]
        .iter()
        .filter_map(|t| Some(count(t)).filter(|&n| n > 0).map(|n| format!("{n} {t}")))
        .collect();
    let other = cells.iter().filter(|c| !matches!(c.cell_type.as_str(), "code" | "markdown" | "raw")).count();
    if other > 0 {
        kinds.push(format!("{other} other"));
    }
    line(&mut out, "Cells", format_args!("{} ({})", cells.len(), kinds.join(", ")));
    for cell in cells {
        let _ = writeln!(out, "\n=== {} ===", cell.cell_type);
        push_text(&mut out, cell.source(), usize::MAX);
        if outputs {
            for o in &cell.outputs {
                o.render(&mut out);
            }
        }
    }
    Ok(out)
}

/// Whether two notebooks have the same cells (types and sources), so that only
/// outputs, execution counts, ids or metadata differ. `None` when either is not
/// a readable notebook.
pub fn same_cells(left: &[u8], right: &[u8]) -> Option<bool> {
    let l: Notebook<IgnoredAny> = parse(left).ok()?;
    let r: Notebook<IgnoredAny> = parse(right).ok()?;
    let (lc, rc) = (l.cells(), r.cells());
    Some(
        lc.len() == rc.len() && lc.iter().zip(&rc).all(|(a, b)| a.cell_type == b.cell_type && a.source() == b.source()),
    )
}

fn parse<'de, O: Deserialize<'de> + Default>(data: &'de [u8]) -> Result<Notebook<O>> {
    let nb: Notebook<O> =
        serde_json::from_slice(data).map_err(|e| Error::Unsupported(format!("not a readable notebook: {e}")))?;
    if nb.nbformat == 0 {
        return Err(Error::Unsupported("not a notebook: no nbformat version".into()));
    }
    Ok(nb)
}

fn line(out: &mut String, label: &str, value: impl fmt::Display) {
    let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
}

/// Appends text line by line, without terminal colour codes, at most `max` lines.
fn push_text(out: &mut String, text: &str, max: usize) {
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() {
        return;
    }
    let mut lines = text.split('\n');
    for l in lines.by_ref().take(max) {
        let l = l.strip_suffix('\r').unwrap_or(l);
        if l.contains('\x1b') {
            out.push_str(&strip_ansi(l));
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    let rest = lines.count();
    if rest > 0 {
        let _ = writeln!(out, "… {rest} more lines");
    }
}

/// Removes ANSI escape sequences (`ESC [ … letter`), as IPython colours tracebacks.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if chars.clone().next() == Some('[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

fn size(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MB", bytes as f64 / 1_048_576.0),
    }
}

// ---- The JSON structure ----

#[derive(Deserialize)]
struct Notebook<O> {
    #[serde(default)]
    cells: Vec<Cell<O>>,
    /// nbformat 3: cells live in worksheets.
    #[serde(default)]
    worksheets: Vec<Worksheet<O>>,
    #[serde(default)]
    metadata: Metadata,
    #[serde(default)]
    nbformat: u32,
    #[serde(default)]
    nbformat_minor: u32,
}

impl<O> Notebook<O> {
    /// The cells in order, those of all nbformat 3 worksheets one after the other.
    fn cells(&self) -> Vec<&Cell<O>> {
        if self.cells.is_empty() {
            self.worksheets.iter().flat_map(|w| &w.cells).collect()
        } else {
            self.cells.iter().collect()
        }
    }
}

#[derive(Deserialize)]
struct Worksheet<O> {
    #[serde(default)]
    cells: Vec<Cell<O>>,
}

#[derive(Deserialize, Default)]
struct Metadata {
    kernelspec: Option<KernelSpec>,
    language_info: Option<LanguageInfo>,
}

#[derive(Deserialize)]
struct KernelSpec {
    name: Option<String>,
    display_name: Option<String>,
    language: Option<String>,
}

#[derive(Deserialize)]
struct LanguageInfo {
    name: Option<String>,
}

#[derive(Deserialize)]
struct Cell<O> {
    cell_type: String,
    #[serde(default)]
    source: Multiline,
    /// nbformat 3 code cells.
    #[serde(default)]
    input: Multiline,
    #[serde(default)]
    outputs: O,
}

impl<O> Cell<O> {
    fn source(&self) -> &str {
        if self.source.0.is_empty() { &self.input.0 } else { &self.source.0 }
    }
}

#[derive(Deserialize)]
struct Output {
    output_type: String,
    /// Stream name: stdout, stderr.
    name: Option<String>,
    #[serde(default)]
    text: Multiline,
    data: Option<DataMap>,
    ename: Option<String>,
    evalue: Option<String>,
}

impl Output {
    fn render(&self, out: &mut String) {
        match self.output_type.as_str() {
            "stream" => {
                let _ = writeln!(out, "--- {} ---", self.name.as_deref().unwrap_or("stream"));
                push_text(out, &self.text.0, MAX_OUTPUT_LINES);
            }
            "error" | "pyerr" => {
                let _ = writeln!(out, "--- error ---");
                let name = self.ename.as_deref().unwrap_or("Error");
                let value = strip_ansi(self.evalue.as_deref().unwrap_or_default());
                let mut first = value.lines();
                let _ = writeln!(out, "! {name}: {}", first.next().unwrap_or_default());
                push_text(out, &first.collect::<Vec<_>>().join("\n"), MAX_OUTPUT_LINES);
            }
            kind => {
                let tag = match kind {
                    "execute_result" | "pyout" => "result",
                    "display_data" => "display",
                    other => other,
                };
                let _ = writeln!(out, "--- {tag} ---");
                // nbformat 3 put text results in `text`.
                if !self.text.0.is_empty() {
                    push_text(out, &self.text.0, MAX_OUTPUT_LINES);
                }
                let mut data: Vec<&(String, Payload)> = self.data.iter().flat_map(|d| &d.0).collect();
                data.sort_by(|a, b| a.0.cmp(&b.0));
                if let Some((_, plain)) = data.iter().find(|(m, _)| m == "text/plain") {
                    match &plain.text {
                        Some(t) => push_text(out, t, MAX_OUTPUT_LINES),
                        None => plain.placeholder(out, "text/plain"),
                    }
                }
                for (mime, payload) in data.iter().filter(|(m, _)| m != "text/plain") {
                    payload.placeholder(out, mime);
                }
            }
        }
    }
}

/// A source or text field: one string or a list of lines.
#[derive(Default)]
struct Multiline(String);

impl<'de> Deserialize<'de> for Multiline {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Multiline;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a string or a list of strings")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Multiline, E> {
                Ok(Multiline(v.to_string()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Multiline, E> {
                Ok(Multiline(v))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Multiline, E> {
                Ok(Multiline::default())
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Multiline, A::Error> {
                let mut s = String::new();
                while let Some(part) = seq.next_element::<std::borrow::Cow<'de, str>>()? {
                    s.push_str(&part);
                }
                Ok(Multiline(s))
            }
        }
        d.deserialize_any(V)
    }
}

/// One MIME bundle entry: its size and checksum, and the text when it is small.
struct Payload {
    text: Option<String>,
    /// Bytes without line breaks (base64 payloads are wrapped differently by different writers).
    len: usize,
    crc: u32,
    /// Not a string (e.g. `application/json`): the size is that of the JSON text.
    structured: bool,
}

impl Payload {
    fn placeholder(&self, out: &mut String, mime: &str) {
        let base64 = !self.structured
            && !(mime.starts_with("text/")
                || mime.contains("json")
                || mime.contains("xml")
                || mime.contains("javascript"));
        let bytes = if base64 { self.len / 4 * 3 } else { self.len };
        let _ = writeln!(out, "[{mime} {}, crc {:08x}]", size(bytes), self.crc);
    }
}

#[derive(Default)]
struct PayloadBuilder {
    text: String,
    keep: bool,
    len: usize,
    crc: crc32fast::Hasher,
}

impl PayloadBuilder {
    fn new() -> Self {
        PayloadBuilder { keep: true, ..Default::default() }
    }

    fn push(&mut self, s: &str) {
        for part in s.split(['\n', '\r']) {
            self.crc.update(part.as_bytes());
            self.len += part.len();
        }
        if self.keep && self.text.len() + s.len() <= MAX_KEPT_TEXT {
            self.text.push_str(s);
        } else {
            self.keep = false;
            self.text = String::new();
        }
    }

    fn finish(self, structured: bool) -> Payload {
        let text = (self.keep && !structured).then_some(self.text);
        Payload { text, len: self.len, crc: self.crc.finalize(), structured }
    }
}

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Payload;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a MIME payload")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Payload, E> {
                let mut b = PayloadBuilder::new();
                b.push(v);
                Ok(b.finish(false))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Payload, A::Error> {
                let mut b = PayloadBuilder::new();
                while let Some(part) = seq.next_element::<serde_json::Value>()? {
                    match part {
                        serde_json::Value::String(s) => b.push(&s),
                        other => b.push(&other.to_string()),
                    }
                }
                Ok(b.finish(false))
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> std::result::Result<Payload, A::Error> {
                let value = serde_json::Value::deserialize(de::value::MapAccessDeserializer::new(map))?;
                let mut b = PayloadBuilder::new();
                b.push(&value.to_string());
                Ok(b.finish(true))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Payload, E> {
                self.visit_str(&v.to_string())
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Payload, E> {
                self.visit_str(&v.to_string())
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Payload, E> {
                self.visit_str(&v.to_string())
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Payload, E> {
                self.visit_str(&v.to_string())
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Payload, E> {
                self.visit_str("null")
            }
        }
        d.deserialize_any(V)
    }
}

/// The `data` object of an output as a list, in file order.
struct DataMap(Vec<(String, Payload)>);

impl<'de> Deserialize<'de> for DataMap {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = DataMap;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a MIME bundle")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<DataMap, A::Error> {
                let mut entries = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Payload>()? {
                    entries.push((k, v));
                }
                Ok(DataMap(entries))
            }
        }
        d.deserialize_map(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/notebook/", $name))
        };
    }
    const V1: &[u8] = fixture!("analysis-v1.ipynb");
    const RERUN: &[u8] = fixture!("analysis-v1-rerun.ipynb");
    const V2: &[u8] = fixture!("analysis-v2.ipynb");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn renders_cells_and_outputs() {
        let v = render(V1, true).unwrap();
        for expected in [
            "Kernel:            Python 3 (ipykernel)",
            "Language:          python",
            "Cells:             7 (5 code, 1 markdown, 1 raw)",
            "=== markdown ===\n# Sales analysis\n\nTotals per *region*.\n",
            "--- stdout ---\nloaded 3 regions\n",
            "--- result ---\n[('east', 18), ('north', 27), ('south', 82)]\n",
            "--- display ---\n<IPython.core.display.Image object>\n[image/png ",
            "--- result ---\n<IPython.core.display.HTML object>\n[text/html 17 B, crc ",
            "--- error ---\n! ZeroDivisionError: division by zero\n",
            "=== raw ===\nraw text, not executed\n",
        ] {
            assert!(v.contains(expected), "{expected}\n---\n{v}");
        }
        assert!(!v.contains("execution_count") && !v.contains("2026-") && !v.contains('\x1b'), "{v}");
        let code = render(V1, false).unwrap();
        assert!(!code.contains("--- ") && code.contains("sorted(sales.items())"), "{code}");
    }

    #[test]
    fn a_diff_shows_only_what_changed() {
        let (a, b) = (render(V1, false).unwrap(), render(V2, false).unwrap());
        assert_eq!(
            lines_only_in(&a, &b),
            ["Cells:             7 (5 code, 1 markdown, 1 raw)", "# Sales analysis", "1 / 0"]
        );
        // Re-running changes outputs: the random numbers and the image, nothing else.
        let (a, b) = (render(V1, true).unwrap(), render(RERUN, true).unwrap());
        let changed = lines_only_in(&a, &b);
        assert_eq!(changed.len(), 3, "{changed:?}");
        assert!(changed[1].starts_with("[image/png "), "{changed:?}");
        assert_eq!(render(V1, false).unwrap(), render(RERUN, false).unwrap());
    }

    #[test]
    fn same_cells_ignores_outputs_and_ids() {
        assert_eq!(same_cells(V1, RERUN), Some(true));
        assert_eq!(same_cells(V1, V2), Some(false));
        assert_eq!(same_cells(V1, b"{\"cells\": []}"), None);
    }

    #[test]
    fn detection_and_damaged_input() {
        assert!(is_notebook("ipynb", V1));
        assert!(!is_notebook("json", V1));
        assert!(!is_notebook("ipynb", b"{\"cells\": []}"));
        for n in [0, 1, 100, V1.len() / 2, V1.len() - 2] {
            assert!(render(&V1[..n], true).is_err());
        }
        let v3 = br#"{"nbformat": 3, "nbformat_minor": 0, "metadata": {}, "worksheets": [{"cells": [
            {"cell_type": "code", "input": ["x = 1\n", "x"], "outputs": [{"output_type": "pyout", "text": ["1"]}]}]}]}"#;
        assert_eq!(
            render(v3, true).unwrap(),
            "Notebook:          nbformat 3.0\nCells:             1 (1 code)\n\n=== code ===\nx = 1\nx\n--- result ---\n1\n"
        );
        let odd = br#"{"nbformat": 4, "nbformat_minor": 5, "metadata": {"kernelspec": null}, "cells": [
            {"cell_type": "code", "source": "", "outputs": [{"output_type": "display_data", "data": {"application/json": {"a": [1, 2]}, "text/plain": 7}}]}]}"#;
        let v = render(odd, true).unwrap();
        assert!(v.contains("--- display ---\n7\n[application/json 11 B, crc "), "{v}");
    }

    #[test]
    fn all_nbformat3_worksheets() {
        let nb = |code: &str| {
            format!(
                r#"{{"nbformat": 3, "nbformat_minor": 0, "worksheets": [{{"cells": [{{"cell_type": "code", "input": ["shared\n"], "outputs": []}}]}}, {{"cells": [{{"cell_type": "code", "input": ["{code}\n"], "outputs": []}}]}}]}}"#
            )
        };
        let (a, b) = (nb("x=1"), nb("x=999"));
        assert_eq!(same_cells(a.as_bytes(), b.as_bytes()), Some(false));
        assert!(render(b.as_bytes(), true).unwrap().contains("x=999"));
    }

    #[test]
    fn strips_ansi_codes() {
        assert_eq!(strip_ansi("\x1b[31mZeroDivisionError\x1b[39m: x"), "ZeroDivisionError: x");
    }
}
