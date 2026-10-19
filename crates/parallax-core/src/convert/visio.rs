//! Microsoft Visio drawings: .vsdx and its siblings (drawing, template, stencil,
//! with or without macros) are read directly; the binary .vsd/.vss/.vst go through
//! LibreOffice, which converts them to an OpenDocument drawing.
//!
//! The **Diagram** view lists every page with its shapes: name, master, text,
//! position and size, colours set on the shape, shape data, nested shapes of
//! groups, embedded images, and which shapes each connector joins. Each property
//! is a line of its own, so a diff shows what changed. **Document text** keeps the
//! text alone.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use parking_lot::Mutex;

use super::office::{Ev, Zip, attr, walk_xml};
use crate::{Error, Result};

/// Extensions of the XML formats (since Visio 2013).
pub const XML_EXTENSIONS: [&str; 6] = ["vsdx", "vsdm", "vstx", "vstm", "vssx", "vssm"];
/// Extensions of the binary formats (Visio 2010 and older).
pub const BINARY_EXTENSIONS: [&str; 3] = ["vsd", "vss", "vst"];

#[derive(Default)]
struct Page {
    name: String,
    background: bool,
    /// Width and height with their unit.
    size: Option<(String, String)>,
    shapes: Vec<Shape>,
}

#[derive(Default)]
struct Shape {
    /// The shape's id within its page; empty where the format has none.
    id: String,
    kind: String,
    name: String,
    master: Option<String>,
    text: String,
    /// Shape cells and attributes worth showing: position, size, angle, colours.
    cells: BTreeMap<String, String>,
    /// Shape data (label, value) and user-defined cells.
    data: Vec<(String, String)>,
    image: Option<String>,
    /// For connectors: the ids of the shapes at the begin and the end.
    begin: Option<String>,
    end: Option<String>,
    children: Vec<Shape>,
}

/// The Diagram view of a .vsdx (or .vsdm, .vstx, .vssx …) file.
pub fn diagram(data: &[u8]) -> Result<String> {
    Ok(render_pages(&read_vsdx(data)?, true))
}

/// The text of the shapes, page by page.
pub fn diagram_text(data: &[u8]) -> Result<String> {
    Ok(render_pages(&read_vsdx(data)?, false))
}

/// The Diagram view of a binary .vsd (or .vss, .vst) file, through LibreOffice.
pub fn legacy_diagram(ext: &str, data: &[u8]) -> Result<String> {
    Ok(render_pages(&read_odg(&converted(ext, data)?)?, true))
}

pub fn legacy_text(ext: &str, data: &[u8]) -> Result<String> {
    Ok(render_pages(&read_odg(&converted(ext, data)?)?, false))
}

/// The file converted to an OpenDocument drawing. A conversion takes seconds, and
/// both views and the similar check need it, so the last few are kept.
fn converted(ext: &str, data: &[u8]) -> Result<Vec<u8>> {
    /// (CRC32 and length of the input, converted drawing)
    type Entry = ((u32, usize), Vec<u8>);
    static CACHE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
    let key = (crc32fast::hash(data), data.len());
    if let Some((_, odg)) = CACHE.lock().iter().find(|(k, _)| *k == key) {
        return Ok(odg.clone());
    }
    let odg = super::office::convert(ext, data, "odg")?;
    let mut cache = CACHE.lock();
    cache.push((key, odg.clone()));
    if cache.len() > 8 {
        cache.remove(0);
    }
    Ok(odg)
}

// ---- .vsdx ----

fn read_vsdx(data: &[u8]) -> Result<Vec<Page>> {
    let mut zip = Zip::open(data)?;
    let masters = read_masters(&mut zip)?;
    let rels = |zip: &mut Zip, part: &str| -> HashMap<String, String> {
        let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
        let mut out = HashMap::new();
        if let Some(xml) = zip.read(&format!("{dir}/_rels/{file}.rels")) {
            let _ = walk_xml(&xml, |ev| {
                if let Ev::Start("Relationship", e) = ev
                    && let (Some(id), Some(target)) = (attr(e, "Id"), attr(e, "Target"))
                {
                    out.insert(id, resolve(dir, &target));
                }
            });
        }
        out
    };
    let index =
        zip.read("visio/pages/pages.xml").ok_or_else(|| Error::Unsupported("no pages in this Visio file".into()))?;
    let page_rels = rels(&mut zip, "visio/pages/pages.xml");
    // Pages in their order, with the part that holds each one's shapes.
    let mut pages: Vec<(Page, Option<String>)> = Vec::new();
    let mut in_sheet = false;
    let mut section = 0usize;
    let mut size: (Option<String>, Option<String>) = (None, None);
    walk_xml(&index, |ev| match ev {
        Ev::Start("Page", e) => {
            let name = attr(e, "Name").or_else(|| attr(e, "NameU")).unwrap_or_default();
            let background = attr(e, "Background").is_some_and(|b| b == "1");
            pages.push((Page { name, background, ..Default::default() }, None));
            size = (None, None);
        }
        Ev::Start("PageSheet", _) => in_sheet = true,
        Ev::End("PageSheet") => in_sheet = false,
        Ev::Start("Section", _) => section += 1,
        Ev::End("Section") => section = section.saturating_sub(1),
        Ev::Start("Cell", e) if in_sheet && section == 0 => match attr(e, "N").as_deref() {
            Some("PageWidth") => size.0 = attr(e, "V"),
            Some("PageHeight") => size.1 = attr(e, "V"),
            _ => {}
        },
        Ev::Start("Rel", e) => {
            if let (Some(last), Some(id)) = (pages.last_mut(), attr(e, "id")) {
                last.1 = page_rels.get(&id).cloned();
            }
        }
        Ev::End("Page") => {
            if let (Some(last), (Some(w), Some(h))) = (pages.last_mut(), &size) {
                last.0.size = Some((format!("{} in", number(w)), format!("{} in", number(h))));
            }
        }
        _ => {}
    })?;
    let mut out = Vec::new();
    for (mut page, part) in pages {
        if let Some(part) = part
            && let Some(xml) = zip.read(&part)
        {
            let media = rels(&mut zip, &part);
            let mut images = |target: &str| zip_media(&mut zip, target);
            page.shapes = read_page(&xml, &masters, &media, &mut images)?;
        }
        out.push(page);
    }
    Ok(out)
}

/// `../media/image1.png` relative to `visio/pages` is `visio/media/image1.png`.
fn resolve(dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in target.split('/') {
        match part {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn zip_media(zip: &mut Zip, part: &str) -> Option<String> {
    let data = zip.read_bytes(part)?;
    let name = part.rsplit('/').next().unwrap_or(part);
    Some(format!("{name}, {} bytes, crc {:08x}", data.len(), crc32fast::hash(&data)))
}

fn read_masters(zip: &mut Zip) -> Result<HashMap<String, String>> {
    let mut masters = HashMap::new();
    if let Some(xml) = zip.read("visio/masters/masters.xml") {
        walk_xml(&xml, |ev| {
            if let Ev::Start("Master", e) = ev
                && let Some(id) = attr(e, "ID")
            {
                let name = attr(e, "Name").or_else(|| attr(e, "NameU")).unwrap_or_else(|| format!("#{id}"));
                masters.insert(id, name);
            }
        })?;
    }
    Ok(masters)
}

/// Cells shown for every shape that sets them, in this order.
const CELLS: [&str; 10] =
    ["PinX", "PinY", "Width", "Height", "Angle", "BeginX", "BeginY", "EndX", "EndY", "FillForegnd"];
const MORE_CELLS: [&str; 2] = ["LineColor", "LineWeight"];

fn read_page(
    xml: &str,
    masters: &HashMap<String, String>,
    rels: &HashMap<String, String>,
    images: &mut dyn FnMut(&str) -> Option<String>,
) -> Result<Vec<Shape>> {
    let mut top: Vec<Shape> = Vec::new();
    let mut stack: Vec<Shape> = Vec::new();
    // Sections and rows inside the current shape; data rows collect label and value.
    let mut sections: Vec<String> = Vec::new();
    let mut row: Option<(String, Option<String>, Option<String>)> = None;
    let mut in_text = false;
    let mut connects: Vec<(String, String, String)> = Vec::new();
    walk_xml(xml, |ev| match ev {
        Ev::Start("Shape", e) => {
            let id = attr(e, "ID").unwrap_or_default();
            let master = attr(e, "Master").map(|m| masters.get(&m).cloned().unwrap_or_else(|| format!("#{m}")));
            stack.push(Shape {
                id,
                kind: attr(e, "Type").unwrap_or_else(|| "Shape".into()),
                name: attr(e, "Name").or_else(|| attr(e, "NameU")).unwrap_or_default(),
                master,
                ..Default::default()
            });
        }
        Ev::End("Shape") => {
            if let Some(mut shape) = stack.pop() {
                shape.text = shape.text.trim_end().to_string();
                match stack.last_mut() {
                    Some(parent) => parent.children.push(shape),
                    None => top.push(shape),
                }
            }
        }
        Ev::Start("Section", e) => sections.push(attr(e, "N").unwrap_or_default()),
        Ev::End("Section") => {
            sections.pop();
        }
        Ev::Start("Row", e) => {
            row = Some((attr(e, "N").or_else(|| attr(e, "IX")).unwrap_or_default(), None, None));
        }
        Ev::End("Row") => {
            if let (Some((name, label, value)), Some(shape)) = (row.take(), stack.last_mut()) {
                match sections.last().map(String::as_str) {
                    Some("Property") => {
                        let label = label.filter(|l| !l.is_empty()).unwrap_or(name);
                        shape.data.push((label, value.unwrap_or_default()));
                    }
                    Some("User") => shape.data.push((format!("User.{name}"), value.unwrap_or_default())),
                    _ => {}
                }
            }
        }
        Ev::Start("Cell", e) => {
            let (Some(n), Some(shape)) = (attr(e, "N"), stack.last_mut()) else { return };
            let v = attr(e, "V").unwrap_or_default();
            if sections.is_empty() {
                if CELLS.contains(&n.as_str()) || MORE_CELLS.contains(&n.as_str()) {
                    shape.cells.insert(n, v);
                }
            } else if let Some((_, label, value)) = row.as_mut() {
                match n.as_str() {
                    "Label" => *label = Some(v),
                    "Value" => *value = Some(v),
                    _ => {}
                }
            }
        }
        Ev::Start("Text", _) if !stack.is_empty() && sections.is_empty() => in_text = true,
        Ev::End("Text") => in_text = false,
        Ev::Text(t) if in_text => {
            if let Some(shape) = stack.last_mut() {
                shape.text.push_str(t);
            }
        }
        Ev::Start("Rel", e) if !stack.is_empty() => {
            if let (Some(id), Some(shape)) = (attr(e, "id"), stack.last_mut())
                && let Some(target) = rels.get(&id)
            {
                shape.image = images(target).or_else(|| Some(target.clone()));
            }
        }
        Ev::Start("Connect", e) => {
            if let (Some(from), Some(cell), Some(to)) = (attr(e, "FromSheet"), attr(e, "FromCell"), attr(e, "ToSheet"))
            {
                connects.push((from, cell, to));
            }
        }
        _ => {}
    })?;
    for (from, cell, to) in connects {
        if let Some(shape) = find(&mut top, &from) {
            match cell.as_str() {
                "BeginX" => shape.begin = Some(to),
                "EndX" => shape.end = Some(to),
                _ => {}
            }
        }
    }
    sort(&mut top);
    Ok(top)
}

fn find<'a>(shapes: &'a mut [Shape], id: &str) -> Option<&'a mut Shape> {
    for shape in shapes {
        if shape.id == id {
            return Some(shape);
        }
        if let Some(found) = find(&mut shape.children, id) {
            return Some(found);
        }
    }
    None
}

/// By id, numerically: the order of drawing (front to back) changes with "bring to
/// front", which should not move lines.
fn sort(shapes: &mut [Shape]) {
    shapes.sort_by_key(|s| (s.id.parse::<u64>().unwrap_or(u64::MAX), s.id.clone()));
    for shape in shapes {
        sort(&mut shape.children);
    }
}

// ---- .vsd through LibreOffice (OpenDocument drawing) ----

fn read_odg(data: &[u8]) -> Result<Vec<Page>> {
    let mut zip = Zip::open(data)?;
    let xml =
        zip.read("content.xml").ok_or_else(|| Error::Unsupported("the converted drawing has no content".into()))?;
    let mut pages: Vec<Page> = Vec::new();
    let mut stack: Vec<Shape> = Vec::new();
    let mut in_paragraph = false;
    const SHAPES: [&str; 14] = [
        "custom-shape",
        "rect",
        "ellipse",
        "circle",
        "frame",
        "polygon",
        "polyline",
        "path",
        "line",
        "connector",
        "g",
        "measure",
        "caption",
        "regular-polygon",
    ];
    walk_xml(&xml, |ev| match ev {
        Ev::Start("page", e) => {
            pages.push(Page { name: attr(e, "name").unwrap_or_default(), ..Default::default() });
        }
        Ev::Start(kind, e) if SHAPES.contains(&kind) && !pages.is_empty() => {
            let mut shape = Shape {
                id: attr(e, "id").unwrap_or_default(),
                kind: odg_kind(kind).into(),
                name: attr(e, "name").unwrap_or_default(),
                begin: attr(e, "start-shape"),
                end: attr(e, "end-shape"),
                ..Default::default()
            };
            for (cell, key) in [("x", "PinX"), ("y", "PinY"), ("width", "Width"), ("height", "Height")] {
                if let Some(v) = attr(e, cell) {
                    shape.cells.insert(key.into(), v);
                }
            }
            for (cell, key) in [("x1", "BeginX"), ("y1", "BeginY"), ("x2", "EndX"), ("y2", "EndY")] {
                if let Some(v) = attr(e, cell) {
                    shape.cells.insert(key.into(), v);
                }
            }
            if let Some(t) = attr(e, "transform").filter(|t| t.contains("rotate")) {
                shape.cells.insert("Angle".into(), t);
            }
            stack.push(shape);
        }
        Ev::End(kind) if SHAPES.contains(&kind) => {
            if let Some(mut shape) = stack.pop() {
                shape.text = shape.text.trim_end().to_string();
                match (stack.last_mut(), pages.last_mut()) {
                    (Some(parent), _) => parent.children.push(shape),
                    (None, Some(page)) => page.shapes.push(shape),
                    _ => {}
                }
            }
        }
        Ev::Start("image", e) => {
            if let (Some(shape), Some(href)) = (stack.last_mut(), attr(e, "href")) {
                shape.kind = "Image".into();
                shape.image = zip_media(&mut zip, &href).or(Some(href));
            }
        }
        Ev::Start("p" | "h", _) if !stack.is_empty() => in_paragraph = true,
        Ev::End("p" | "h") if in_paragraph => {
            in_paragraph = false;
            if let Some(shape) = stack.last_mut() {
                shape.text.push('\n');
            }
        }
        Ev::Start("s", e) if in_paragraph => {
            let n = attr(e, "c").and_then(|c| c.parse().ok()).unwrap_or(1usize);
            if let Some(shape) = stack.last_mut() {
                shape.text.push_str(&" ".repeat(n));
            }
        }
        Ev::Start("tab", _) if in_paragraph => {
            if let Some(shape) = stack.last_mut() {
                shape.text.push('\t');
            }
        }
        Ev::Start("line-break", _) if in_paragraph => {
            if let Some(shape) = stack.last_mut() {
                shape.text.push('\n');
            }
        }
        Ev::Text(t) if in_paragraph => {
            if let Some(shape) = stack.last_mut() {
                shape.text.push_str(t);
            }
        }
        _ => {}
    })?;
    Ok(pages)
}

fn odg_kind(element: &str) -> &'static str {
    match element {
        "connector" => "Connector",
        "line" | "polyline" | "path" | "measure" => "Line",
        "g" => "Group",
        "frame" => "Frame",
        _ => "Shape",
    }
}

// ---- output ----

fn render_pages(pages: &[Page], details: bool) -> String {
    let mut out = String::new();
    for page in pages {
        let kind = if page.background { "Background page" } else { "Page" };
        match (&page.size, details) {
            (Some((w, h)), true) => {
                let _ = writeln!(out, "=== {kind}: {} ({w} × {h}) ===", page.name);
            }
            _ => {
                let _ = writeln!(out, "=== {kind}: {} ===", page.name);
            }
        }
        // What connectors call the shapes they join: their text, else their name.
        let mut names = HashMap::new();
        collect_names(&page.shapes, &mut names);
        for shape in &page.shapes {
            if details {
                shape_lines(shape, 0, &names, &mut out);
            } else {
                text_lines(shape, &mut out);
            }
        }
    }
    out
}

fn collect_names(shapes: &[Shape], names: &mut HashMap<String, String>) {
    for shape in shapes {
        if !shape.id.is_empty() {
            names.insert(shape.id.clone(), label(shape));
        }
        collect_names(&shape.children, names);
    }
}

/// `3 “Start”`, `3 Process`, or `3`.
fn label(shape: &Shape) -> String {
    let text: String = shape.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = shape.id.clone();
    if !text.is_empty() {
        let short: String = text.chars().take(40).collect();
        let more = if text.chars().count() > 40 { "…" } else { "" };
        let _ = write!(out, " “{short}{more}”");
    } else if !shape.name.is_empty() {
        let _ = write!(out, " {}", shape.name);
    }
    out.trim().to_string()
}

fn shape_lines(shape: &Shape, depth: usize, names: &HashMap<String, String>, out: &mut String) {
    let pad = "  ".repeat(depth);
    let connector = shape.begin.is_some() || shape.end.is_some() || shape.kind == "Connector";
    let kind = if connector {
        "Connector"
    } else if shape.image.is_some() {
        "Image"
    } else {
        kind_name(&shape.kind)
    };
    let mut head = format!("{pad}{kind}");
    if !shape.id.is_empty() {
        let _ = write!(head, " {}", shape.id);
    }
    if !shape.name.is_empty() {
        let _ = write!(head, " \"{}\"", shape.name);
    }
    if let Some(master) = &shape.master {
        let _ = write!(head, " · master {master}");
    }
    if connector {
        let end = |id: &Option<String>| match id {
            Some(id) => format!("shape {}", names.get(id).cloned().unwrap_or_else(|| id.clone())),
            None => "(not glued)".to_string(),
        };
        let _ = write!(head, ": {} → {}", end(&shape.begin), end(&shape.end));
    }
    let _ = writeln!(out, "{head}");
    let pad = "  ".repeat(depth + 1);
    for line in shape.text.lines() {
        let _ = writeln!(out, "{pad}text: {line}");
    }
    let cell = |n: &str| shape.cells.get(n).map(|v| number(v));
    if let (Some(x), Some(y)) = (cell("PinX"), cell("PinY")) {
        let mut line = format!("{pad}at {x}, {y}");
        if let (Some(w), Some(h)) = (cell("Width"), cell("Height")) {
            let _ = write!(line, " · size {w} × {h}");
        }
        let _ = writeln!(out, "{line}");
    } else if let (Some(w), Some(h)) = (cell("Width"), cell("Height")) {
        let _ = writeln!(out, "{pad}size {w} × {h}");
    }
    if let (Some(bx), Some(by), Some(ex), Some(ey)) = (cell("BeginX"), cell("BeginY"), cell("EndX"), cell("EndY")) {
        let _ = writeln!(out, "{pad}from {bx}, {by} to {ex}, {ey}");
    }
    if let Some(angle) = cell("Angle").filter(|a| a != "0") {
        let _ = writeln!(out, "{pad}angle {angle}");
    }
    let style: Vec<String> = [("FillForegnd", "fill"), ("LineColor", "line"), ("LineWeight", "line weight")]
        .iter()
        .filter_map(|(n, label)| cell(n).map(|v| format!("{label} {v}")))
        .collect();
    if !style.is_empty() {
        let _ = writeln!(out, "{pad}{}", style.join(" · "));
    }
    for (label, value) in &shape.data {
        let _ = writeln!(out, "{pad}data: {label} = {value}");
    }
    if let Some(image) = &shape.image {
        let _ = writeln!(out, "{pad}image: {image}");
    }
    for child in &shape.children {
        shape_lines(child, depth + 1, names, out);
    }
}

fn text_lines(shape: &Shape, out: &mut String) {
    for line in shape.text.lines().filter(|l| !l.trim().is_empty()) {
        let _ = writeln!(out, "{line}");
    }
    for child in &shape.children {
        text_lines(child, out);
    }
}

fn kind_name(kind: &str) -> &str {
    match kind {
        "Foreign" => "Object",
        "" => "Shape",
        other => other,
    }
}

/// Numbers to at most four decimals (`2.165354297` → `2.1654`), others as they are.
fn number(v: &str) -> String {
    match v.parse::<f64>() {
        Ok(n) if v.chars().all(|c| c.is_ascii_digit() || "-.eE+".contains(c)) => {
            let s = format!("{:.4}", n);
            let s = s.trim_end_matches('0').trim_end_matches('.');
            if s == "-0" { "0".into() } else { s.to_string() }
        }
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(number("2.165354297053872"), "2.1654");
        assert_eq!(number("0"), "0");
        assert_eq!(number("-0.00001"), "0");
        assert_eq!(number("#ff00aa"), "#ff00aa");
        assert_eq!(number("2.5cm"), "2.5cm");
        assert_eq!(resolve("visio/pages", "../media/image1.png"), "visio/media/image1.png");
    }
}
