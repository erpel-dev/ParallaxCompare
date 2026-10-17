//! CDFX files (ASAM CDF 2.x calibration data, XML).
//!
//! A CDFX holds calibration values for an ECU: per `SW-INSTANCE` its short name,
//! category (`VALUE`, `VAL_BLK`, `CURVE`, `MAP`, `COM_AXIS`, `ASCII`, `BOOLEAN` …),
//! unit and physical values (`V` numbers, `VT` texts, `VG` groups for the rows of
//! a map), the axes of curves and maps, and a calibration history. As XML it is
//! verbose and tools order and indent it differently.
//!
//! The "Calibration values" view shows each instance as a block, sorted by name,
//! with one value per line (`[2][3] = 1.5`), so a changed value is one changed
//! line. Numbers are shown as numbers (`1.50` → `1.5`). The document's revision is
//! in a header. Files whose view is equal are ≈ similar ("same values, only order
//! or formatting differ").

use std::fmt::Write as _;

use super::office::{Ev, walk_xml};
use crate::{Error, Result};

/// Values shown per instance; very large maps are cut off.
const MAX_VALUES: usize = 100_000;

/// Whether the start of a file is a CDF document.
pub fn is_cdfx(data: &[u8]) -> bool {
    let head = &data[..data.len().min(4096)];
    head.windows(6).any(|w| w == b"<MSRSW")
}

/// The "Calibration values" view.
pub fn values(text: &str) -> Result<String> {
    let doc = parse(text)?;
    Ok(doc.render())
}

/// Whether two CDFX files hold the same values.
pub fn same_values(left: &str, right: &str) -> Option<bool> {
    let (l, r) = rayon::join(|| parse(left), || parse(right));
    let (l, r) = (l.ok()?, r.ok()?);
    if l.trees.iter().map(|t| t.1.len()).sum::<usize>() == 0 {
        return None;
    }
    Some(l.render() == r.render())
}

#[derive(Default)]
struct Values {
    /// Index path (for `VG` groups) and the value as shown.
    items: Vec<(Vec<usize>, String)>,
    /// Position while reading: one counter per open `VG`.
    position: Vec<usize>,
    truncated: bool,
}

#[derive(Default)]
struct Axis {
    category: String,
    unit: Option<String>,
    reference: Option<String>,
    values: Values,
    other: Vec<(String, String)>,
}

#[derive(Default)]
struct Instance {
    name: String,
    category: String,
    feature: Option<String>,
    unit: Option<String>,
    array_size: Vec<String>,
    values: Values,
    axes: Vec<Axis>,
    history: Vec<String>,
    other: Vec<(String, String)>,
}

#[derive(Default)]
struct Doc {
    header: Vec<(String, String)>,
    /// Instance trees: name and instances.
    trees: Vec<(String, Vec<Instance>)>,
}

/// A value as shown: numbers normalized, texts quoted.
fn value(tag: &str, text: &str) -> String {
    match tag {
        "V" => match text.parse::<f64>() {
            Ok(v) if v.is_finite() => {
                if v == 0.0 {
                    "0".into()
                } else {
                    format!("{v}")
                }
            }
            _ => text.to_string(),
        },
        _ => format!("\"{}\"", text.replace('\n', "\\n")),
    }
}

fn parse(text: &str) -> Result<Doc> {
    let mut doc = Doc::default();
    let mut path: Vec<String> = Vec::new();
    let mut text_buf = String::new();
    // Instances being read (nested instances are structure members).
    let mut open: Vec<Instance> = Vec::new();
    let mut entry: Vec<String> = Vec::new();
    let mut saw_root = false;
    let result = walk_xml(text, |ev| match ev {
        Ev::Start(tag, _) => {
            text_buf.clear();
            match tag {
                "MSRSW" => saw_root = true,
                "SW-INSTANCE-TREE" => doc.trees.push((String::new(), Vec::new())),
                "SW-INSTANCE" => open.push(Instance::default()),
                "SW-AXIS-CONT" => {
                    if let Some(i) = open.last_mut() {
                        i.axes.push(Axis::default());
                    }
                }
                "SW-VALUES-PHYS" => {
                    if let Some(v) = current_values(&mut open, &path) {
                        v.position = vec![0];
                    }
                }
                "VG" => {
                    if let Some(v) = current_values(&mut open, &path) {
                        v.position.push(0);
                    }
                }
                "CS-ENTRY" => entry.clear(),
                _ => {}
            }
            path.push(tag.to_string());
        }
        Ev::Text(t) => text_buf.push_str(t),
        Ev::End(_) => {
            let tag = path.pop().unwrap_or_default();
            let parent = path.last().map_or("", String::as_str);
            let t = text_buf.trim().to_string();
            text_buf.clear();
            let in_values = path.iter().any(|p| p == "SW-VALUES-PHYS");
            match tag.as_str() {
                "SW-INSTANCE" => {
                    if let Some(mut done) = open.pop() {
                        match open.last_mut() {
                            Some(parent) => {
                                // A structure member: shown with the parent's name as prefix.
                                done.name = format!("{}.{}", parent.name, done.name);
                                if let Some(tree) = doc.trees.last_mut() {
                                    tree.1.push(done);
                                }
                            }
                            None => match doc.trees.last_mut() {
                                Some(tree) => tree.1.push(done),
                                None => doc.trees.push((String::new(), vec![done])),
                            },
                        }
                    }
                }
                "VG" => {
                    if let Some(v) = current_values(&mut open, &path) {
                        v.position.pop();
                        if let Some(last) = v.position.last_mut() {
                            *last += 1;
                        }
                    }
                }
                "V" | "VT" | "VH" | "VF" | "VTF" if in_values => {
                    if let Some(v) = current_values(&mut open, &path) {
                        if v.items.len() < MAX_VALUES {
                            v.items.push((v.position.clone(), value(&tag, &t)));
                        } else {
                            v.truncated = true;
                        }
                        if let Some(last) = v.position.last_mut() {
                            *last += 1;
                        }
                    }
                }
                "V" if parent == "SW-ARRAYSIZE" => {
                    if let Some(i) = open.last_mut() {
                        match i.axes.last_mut().filter(|_| path.iter().any(|p| p == "SW-AXIS-CONT")) {
                            Some(a) => a.other.push(("Size".into(), t)),
                            None => i.array_size.push(t),
                        }
                    }
                }
                _ if t.is_empty() => {}
                "CS-ENTRY" => {}
                _ if path.iter().any(|p| p == "CS-ENTRY") => entry.push(t),
                _ if open.is_empty() => match (tag.as_str(), parent) {
                    ("SHORT-NAME", "SW-INSTANCE-TREE") => {
                        if let Some(tree) = doc.trees.last_mut() {
                            tree.0 = t;
                        }
                    }
                    ("SHORT-NAME" | "CATEGORY", "MSRSW") => doc.header.push((tag.to_string(), t)),
                    ("SHORT-NAME", "SW-SYSTEM") => doc.header.push(("SW-SYSTEM".into(), t)),
                    (_, _) if path.iter().any(|p| p == "ADMIN-DATA") => doc.header.push((tag.to_string(), t)),
                    _ => {}
                },
                _ => {
                    let Some(inst) = open.last_mut() else { return };
                    let in_axis = path.iter().any(|p| p == "SW-AXIS-CONT");
                    match (tag.as_str(), parent) {
                        ("SHORT-NAME", "SW-INSTANCE") => inst.name = t,
                        ("CATEGORY", "SW-INSTANCE") => inst.category = t,
                        ("SW-FEATURE-REF", _) => inst.feature = Some(t),
                        ("CATEGORY", "SW-AXIS-CONT") => {
                            if let Some(a) = inst.axes.last_mut() {
                                a.category = t;
                            }
                        }
                        ("UNIT-DISPLAY-NAME", _) if in_axis => {
                            if let Some(a) = inst.axes.last_mut() {
                                a.unit = Some(t);
                            }
                        }
                        ("UNIT-DISPLAY-NAME", _) => inst.unit = Some(t),
                        ("SW-INSTANCE-REF", _) if in_axis => {
                            if let Some(a) = inst.axes.last_mut() {
                                a.reference = Some(t);
                            }
                        }
                        _ if in_values => {}
                        _ if in_axis => {
                            if let Some(a) = inst.axes.last_mut() {
                                a.other.push((tag.to_string(), t));
                            }
                        }
                        _ => inst.other.push((tag.to_string(), t)),
                    }
                }
            }
            if tag == "CS-ENTRY" && !entry.is_empty() {
                if let Some(i) = open.last_mut() {
                    i.history.push(entry.join(", "));
                }
                entry.clear();
            }
        }
    });
    result.map_err(|e| Error::Unsupported(format!("CDFX: {e}")))?;
    if !saw_root {
        return Err(Error::Unsupported("CDFX: no MSRSW root element".into()));
    }
    for tree in &mut doc.trees {
        tree.1.sort_by(|a, b| a.name.cmp(&b.name));
    }
    doc.trees.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(doc)
}

/// The values being read: of the instance's current axis or of the instance.
fn current_values<'i>(open: &'i mut [Instance], path: &[String]) -> Option<&'i mut Values> {
    let inst = open.last_mut()?;
    if path.iter().any(|p| p == "SW-AXIS-CONT") {
        return inst.axes.last_mut().map(|a| &mut a.values);
    }
    Some(&mut inst.values)
}

fn line(out: &mut String, indent: usize, label: &str, value: impl std::fmt::Display) {
    out.extend(std::iter::repeat_n(' ', indent));
    out.push_str(label);
    out.push(':');
    out.extend(std::iter::repeat_n(' ', 18usize.saturating_sub(label.len() + 1) + 1));
    let _ = writeln!(out, "{value}");
}

fn render_values(out: &mut String, indent: usize, v: &Values, scalar: bool) {
    match v.items.as_slice() {
        [] => {}
        [(pos, x)] if scalar && pos.len() == 1 => line(out, indent, "Value", x),
        items => {
            line(out, indent, "Values", items.len());
            for (pos, x) in items {
                out.extend(std::iter::repeat_n(' ', indent + 2));
                for p in pos {
                    let _ = write!(out, "[{p}]");
                }
                let _ = writeln!(out, " = {x}");
            }
        }
    }
    if v.truncated {
        let _ = writeln!(out, "! More values are not shown");
    }
}

impl Doc {
    fn render(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.header {
            let label = match k.as_str() {
                "SHORT-NAME" => "Name",
                "CATEGORY" => "Category",
                "SW-SYSTEM" => "System",
                "LANGUAGE" => "Language",
                "REVISION-LABEL" => "Revision",
                "STATE" => "State",
                "DATE" => "Date",
                k => k,
            };
            line(&mut out, 0, label, v);
        }
        for (name, instances) in &self.trees {
            let title = if name.is_empty() { "Instances" } else { name };
            let _ = writeln!(out, "\n=== {title} ({}) ===", instances.len());
            for i in instances {
                let _ = writeln!(out, "\n--- {} ---", i.name);
                line(&mut out, 0, "Category", &i.category);
                if let Some(f) = &i.feature {
                    line(&mut out, 0, "Feature", f);
                }
                if let Some(u) = &i.unit {
                    line(&mut out, 0, "Unit", format!("\"{u}\""));
                }
                if !i.array_size.is_empty() {
                    line(&mut out, 0, "Size", i.array_size.join(" x "));
                }
                for (k, v) in &i.other {
                    line(&mut out, 0, k, v);
                }
                let scalar =
                    matches!(i.category.as_str(), "VALUE" | "BOOLEAN" | "ASCII" | "DEPENDENT_VALUE" | "TEXT" | "");
                render_values(&mut out, 0, &i.values, scalar);
                for (n, a) in i.axes.iter().enumerate() {
                    let _ = writeln!(out, "Axis {} ({})", n + 1, if a.category.is_empty() { "?" } else { &a.category });
                    if let Some(u) = &a.unit {
                        line(&mut out, 2, "Unit", format!("\"{u}\""));
                    }
                    if let Some(r) = &a.reference {
                        line(&mut out, 2, "Reference", r);
                    }
                    for (k, v) in &a.other {
                        line(&mut out, 2, k, v);
                    }
                    render_values(&mut out, 2, &a.values, false);
                }
                for h in &i.history {
                    line(&mut out, 0, "History", h);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cdfx/", $name))
        };
    }
    const V1: &str = fixture!("engine-v1.cdfx");
    const V1_REFORMATTED: &str = fixture!("engine-v1-reformatted.cdfx");
    const V2: &str = fixture!("engine-v2.cdfx");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn values_per_instance() {
        let s = values(V1).unwrap();
        for expected in [
            "Revision:          1.4\nState:             released\nDate:              2026-10-01\nSystem:            EDC42\n",
            "=== EngineCalibration (6) ===",
            "--- K_IdleSpeed ---\nCategory:          VALUE\nFeature:           F_Idle\nUnit:              \"rpm\"\nValue:             800\nHistory:           checked, 2026-09-30, J. Doe, raised for cold start\n",
            "--- KL_IgnitionAdvance ---\nCategory:          CURVE\nUnit:              \"deg\"\nSize:              4\nValues:            4\n  [0] = 5\n  [1] = 10.5\n  [2] = 18\n  [3] = 24\nAxis 1 (STD_AXIS)\n  Unit:              \"rpm\"\n  Size:              4\n  Values:            4\n    [0] = 1000\n",
            "--- KF_FuelMap ---\nCategory:          MAP\n",
            "  [1][2] = 3.25",
            "Axis 1 (COM_AXIS)\n  Reference:         AX_EngineSpeed",
            "--- K_Mode ---\nCategory:          VALUE\nValue:             \"Limp home\"",
            "--- T_Variant ---\nCategory:          ASCII\nValue:             \"EU6 diesel\"",
            "--- KA_Offsets ---\nCategory:          VAL_BLK\nSize:              3\nValues:            3\n",
        ] {
            assert!(s.contains(expected), "{expected}\n---\n{s}");
        }
    }

    #[test]
    fn a_diff_shows_the_changed_values() {
        let (a, b) = (values(V1).unwrap(), values(V2).unwrap());
        assert_eq!(lines_only_in(&a, &b), ["  [1][2] = 3.25", "Value:             800"]);
        assert_eq!(lines_only_in(&b, &a), ["  [1][2] = 3.5", "Value:             850"]);
    }

    #[test]
    fn similar_ignores_order_and_formatting() {
        assert_eq!(same_values(V1, V1_REFORMATTED), Some(true));
        assert_eq!(same_values(V1, V2), Some(false));
        assert!(is_cdfx(V1.as_bytes()));
        assert!(!is_cdfx(b"<?xml version=\"1.0\"?><AUTOSAR/>"));
        assert!(values("<x/>").is_err());
        for end in (0..V1.len()).step_by(29) {
            if V1.is_char_boundary(end) {
                let _ = values(&V1[..end]);
            }
        }
    }
}
