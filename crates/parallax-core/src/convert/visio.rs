//! Microsoft Visio drawings: .vsdx and its siblings (drawing, template, stencil,
//! with or without macros) are read directly, and so are the binary .vsd/.vss/.vst
//! of Visio 2000–2010 (see `vsd`). Older binary files cannot be read.
//!
//! The **Diagram** view lists every page with its shapes: name, master, text,
//! position and size, colours set on the shape, shape data, nested shapes of
//! groups, embedded images, and which shapes each connector joins. Each property
//! is a line of its own, so a diff shows what changed: also the outline of each
//! shape and, as a checksum, its other settings. The masters follow the pages,
//! and a shape without text of its own shows its master's. **Document text**
//! keeps the text alone.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use super::office::{Ev, Zip, attr, walk_xml};
use crate::{Error, Result};

/// Extensions of the XML formats (since Visio 2013).
pub const XML_EXTENSIONS: [&str; 6] = ["vsdx", "vsdm", "vstx", "vstm", "vssx", "vssm"];
/// Extensions of the binary formats (Visio 2010 and older).
pub const BINARY_EXTENSIONS: [&str; 3] = ["vsd", "vss", "vst"];

#[derive(Default)]
pub(super) struct Drawing {
    pub(super) pages: Vec<Page>,
    /// The masters (from the stencils) with their shapes, shown after the pages.
    pub(super) masters: Vec<(String, Vec<Shape>)>,
}

#[derive(Default)]
pub(super) struct Page {
    pub(super) name: String,
    pub(super) background: bool,
    /// Width and height with their unit.
    pub(super) size: Option<(String, String)>,
    pub(super) shapes: Vec<Shape>,
}

#[derive(Default)]
pub(super) struct Shape {
    /// The shape's id within its page; empty where the format has none.
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) master: Option<String>,
    pub(super) text: String,
    /// Shape cells and attributes worth showing: position, size, angle, colours.
    pub(super) cells: BTreeMap<String, String>,
    /// Shape data (label, value) and user-defined cells.
    pub(super) data: Vec<(String, String)>,
    pub(super) image: Option<String>,
    /// The outline, one entry per geometry section: `MoveTo 0, 0; LineTo 1, 0`.
    pub(super) geometry: Vec<String>,
    /// A checksum of the cells and sections not shown otherwise (text format,
    /// line ends, protection …), so that a change to them is still a difference.
    pub(super) other: Option<u32>,
    /// The master's id, and for a shape inside a group the master shape it is from.
    pub(super) master_id: Option<String>,
    pub(super) master_shape: Option<String>,
    /// The text comes from the master, the shape has none of its own.
    pub(super) inherited_text: bool,
    /// For connectors: the ids of the shapes at the begin and the end.
    pub(super) begin: Option<String>,
    pub(super) end: Option<String>,
    pub(super) children: Vec<Shape>,
}

/// The Diagram view of a .vsdx (or .vsdm, .vstx, .vssx …) file.
pub fn diagram(data: &[u8]) -> Result<String> {
    Ok(render(&read_vsdx(data)?, true))
}

/// The text of the shapes, page by page.
pub fn diagram_text(data: &[u8]) -> Result<String> {
    Ok(render(&read_vsdx(data)?, false))
}

/// The Diagram view of a binary .vsd (or .vss, .vst) file.
pub fn legacy_diagram(data: &[u8]) -> Result<String> {
    Ok(render(&read_legacy(data)?, true))
}

pub fn legacy_text(data: &[u8]) -> Result<String> {
    Ok(render(&read_legacy(data)?, false))
}

/// Read natively; versions older than Visio 2000 are not read.
fn read_legacy(data: &[u8]) -> Result<Drawing> {
    super::vsd::read(data)
}

// ---- .vsdx ----

fn read_vsdx(data: &[u8]) -> Result<Drawing> {
    let mut zip = Zip::open(data)?;
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
    let master_rels = rels(&mut zip, "visio/masters/masters.xml");
    let mut masters = read_masters(&mut zip, &master_rels)?;
    let names: HashMap<String, String> = masters.iter().map(|(id, m)| (id.clone(), m.name.clone())).collect();
    for master in masters.values_mut() {
        if let Some(part) = master.part.clone()
            && let Some(xml) = zip.read(&part)
        {
            let media = rels(&mut zip, &part);
            let mut images = |target: &str| zip_media(&mut zip, target);
            master.shapes = read_page(&xml, &names, &media, &mut images)?;
        }
    }
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
            page.shapes = read_page(&xml, &names, &media, &mut images)?;
            for shape in &mut page.shapes {
                inherit(shape, None, &masters);
            }
        }
        out.push(page);
    }
    let mut masters: Vec<Master> = masters.into_values().collect();
    masters.sort_by_key(|m| (m.id.parse::<u64>().unwrap_or(u64::MAX), m.id.clone()));
    let masters = masters.into_iter().map(|m| (m.name, m.shapes)).collect();
    Ok(Drawing { pages: out, masters })
}

/// Gives shapes without text of their own the text of their master shape: the
/// master's (first) shape for a shape placed from a stencil, and the master shape
/// it names for a shape inside such a group.
fn inherit(shape: &mut Shape, group_master: Option<&str>, masters: &HashMap<String, Master>) {
    let master = shape.master_id.as_deref().or(group_master);
    if shape.text.is_empty()
        && let Some(m) = master.and_then(|id| masters.get(id))
    {
        let source = match &shape.master_shape {
            Some(id) => find_ref(&m.shapes, id),
            None if shape.master_id.is_some() => m.shapes.first(),
            None => None,
        };
        if let Some(source) = source.filter(|s| !s.text.is_empty()) {
            shape.text = source.text.clone();
            shape.inherited_text = true;
        }
    }
    for child in &mut shape.children {
        inherit(child, master, masters);
    }
}

fn find_ref<'a>(shapes: &'a [Shape], id: &str) -> Option<&'a Shape> {
    shapes.iter().find_map(|s| if s.id == id { Some(s) } else { find_ref(&s.children, id) })
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

struct Master {
    id: String,
    name: String,
    /// The part with the master's shapes.
    part: Option<String>,
    shapes: Vec<Shape>,
}

fn read_masters(zip: &mut Zip, rels: &HashMap<String, String>) -> Result<HashMap<String, Master>> {
    let mut masters = HashMap::new();
    let mut current: Option<String> = None;
    if let Some(xml) = zip.read("visio/masters/masters.xml") {
        walk_xml(&xml, |ev| match ev {
            Ev::Start("Master", e) => {
                current = attr(e, "ID");
                if let Some(id) = &current {
                    let name = attr(e, "Name").or_else(|| attr(e, "NameU")).unwrap_or_else(|| format!("#{id}"));
                    masters.insert(id.clone(), Master { id: id.clone(), name, part: None, shapes: Vec::new() });
                }
            }
            Ev::Start("Rel", e) => {
                if let (Some(id), Some(rel)) = (&current, attr(e, "id"))
                    && let Some(master) = masters.get_mut(id)
                {
                    master.part = rels.get(&rel).cloned();
                }
            }
            Ev::End("Master") => current = None,
            _ => {}
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
    // Geometry rows of the current section: `LineTo 1, 0`.
    let mut path: Vec<String> = Vec::new();
    let mut point: Option<(String, Vec<(String, String)>)> = None;
    // What is not shown, per shape on the stack.
    let mut hidden: Vec<crc32fast::Hasher> = Vec::new();
    let mut in_text = false;
    let mut connects: Vec<(String, String, String)> = Vec::new();
    walk_xml(xml, |ev| match ev {
        Ev::Start("Shape", e) => {
            let id = attr(e, "ID").unwrap_or_default();
            let master_id = attr(e, "Master");
            let master = master_id.as_ref().map(|m| masters.get(m).cloned().unwrap_or_else(|| format!("#{m}")));
            stack.push(Shape {
                id,
                kind: attr(e, "Type").unwrap_or_else(|| "Shape".into()),
                name: attr(e, "Name").or_else(|| attr(e, "NameU")).unwrap_or_default(),
                master,
                master_id,
                master_shape: attr(e, "MasterShape"),
                ..Default::default()
            });
            hidden.push(crc32fast::Hasher::new());
        }
        Ev::End("Shape") => {
            if let Some(mut shape) = stack.pop() {
                shape.text = shape.text.trim_end().to_string();
                let hash = hidden.pop().map(|h| h.finalize());
                shape.other = hash.filter(|&h| h != crc32fast::Hasher::new().finalize());
                match stack.last_mut() {
                    Some(parent) => parent.children.push(shape),
                    None => top.push(shape),
                }
            }
        }
        Ev::Start("Section", e) => {
            let name = attr(e, "N").unwrap_or_default();
            if let Some(h) = hidden.last_mut().filter(|_| !matches!(name.as_str(), "Property" | "User" | "Geometry")) {
                h.update(format!("[{name}]").as_bytes());
            }
            sections.push(name);
        }
        Ev::End("Section") => {
            if sections.pop().as_deref() == Some("Geometry")
                && let Some(shape) = stack.last_mut()
                && !path.is_empty()
            {
                shape.geometry.push(std::mem::take(&mut path).join("; "));
            }
        }
        Ev::Start("Row", e) => {
            row = Some((attr(e, "N").or_else(|| attr(e, "IX")).unwrap_or_default(), None, None));
            if sections.last().map(String::as_str) == Some("Geometry") {
                point = Some((attr(e, "T").unwrap_or_else(|| "Row".into()), Vec::new()));
            }
            if let Some(h) = hidden.last_mut() {
                h.update(format!("<{}>", attr(e, "T").or_else(|| attr(e, "N")).unwrap_or_default()).as_bytes());
            }
        }
        Ev::End("Row") => {
            if let Some((kind, cells)) = point.take() {
                let values: Vec<String> = cells
                    .iter()
                    .map(|(n, v)| if n.len() == 1 { number(v) } else { format!("{n} {}", number(v)) })
                    .collect();
                path.push(if values.is_empty() { kind } else { format!("{kind} {}", values.join(", ")) });
            }
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
            let section = sections.last().map(String::as_str);
            if sections.is_empty() {
                if CELLS.contains(&n.as_str()) || MORE_CELLS.contains(&n.as_str()) {
                    shape.cells.insert(n, v);
                } else if let Some(h) = hidden.last_mut() {
                    h.update(format!("{n}={v};").as_bytes());
                }
            } else if let Some((_, cells)) = point.as_mut() {
                cells.push((n, v));
            } else if let (Some((_, label, value)), Some("Property" | "User")) = (row.as_mut(), section) {
                match n.as_str() {
                    "Label" => *label = Some(v),
                    "Value" => *value = Some(v),
                    _ => {}
                }
            } else if let Some(h) = hidden.last_mut() {
                h.update(format!("{n}={v};").as_bytes());
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

// ---- output ----

fn render(drawing: &Drawing, details: bool) -> String {
    let mut out = render_pages(&drawing.pages, details);
    // Shapes placed from a master take what they do not set themselves from it.
    if details {
        for (name, shapes) in &drawing.masters {
            let _ = writeln!(out, "=== Master: {name} ===");
            let mut names = HashMap::new();
            collect_names(shapes, &mut names);
            for shape in shapes {
                shape_lines(shape, 0, &names, &mut out);
            }
        }
    }
    out
}

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
    let text = if shape.inherited_text { "text (master)" } else { "text" };
    for line in shape.text.lines() {
        let _ = writeln!(out, "{pad}{text}: {line}");
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
    for outline in &shape.geometry {
        let _ = writeln!(out, "{pad}geometry: {outline}");
    }
    if let Some(image) = &shape.image {
        let _ = writeln!(out, "{pad}image: {image}");
    }
    if let Some(other) = shape.other {
        let _ = writeln!(out, "{pad}other settings: crc {other:08x}");
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
pub(super) fn number(v: &str) -> String {
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

    /// A drawing with one shape placed from a master, the shape with one outline.
    fn drawing(master_text: &str, line_x: &str) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        let rel =
            |target: &str| format!(r#"<Relationships><Relationship Id="rId1" Target="{target}"/></Relationships>"#);
        let parts = [
            ("visio/pages/pages.xml", r#"<Pages><Page ID="0" Name="P"><Rel r:id="rId1"/></Page></Pages>"#.to_string()),
            ("visio/pages/_rels/pages.xml.rels", rel("page1.xml")),
            (
                "visio/pages/page1.xml",
                format!(
                    r#"<PageContents><Shapes><Shape ID="1" Type="Shape" Master="0"><Cell N="PinX" V="1"/><Section N="Geometry" IX="0"><Row T="MoveTo" IX="1"><Cell N="X" V="0"/><Cell N="Y" V="0"/></Row><Row T="LineTo" IX="2"><Cell N="X" V="{line_x}"/><Cell N="Y" V="0"/></Row></Section></Shape></Shapes></PageContents>"#
                ),
            ),
            (
                "visio/masters/masters.xml",
                r#"<Masters><Master ID="0" Name="Box"><Rel r:id="rId1"/></Master></Masters>"#.to_string(),
            ),
            ("visio/masters/_rels/masters.xml.rels", rel("master1.xml")),
            (
                "visio/masters/master1.xml",
                format!(
                    r#"<MasterContents><Shapes><Shape ID="5" Type="Shape"><Text>{master_text}</Text></Shape></Shapes></MasterContents>"#
                ),
            ),
        ];
        for (name, text) in parts {
            w.start_file(name, opts).unwrap();
            w.write_all(text.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn masters_and_outlines() {
        let base = diagram(&drawing("Original master", "1")).unwrap();
        assert!(base.contains("Shape 1 · master Box\n  text (master): Original master\n"), "{base}");
        assert!(base.contains("geometry: MoveTo 0, 0; LineTo 1, 0\n"), "{base}");
        assert!(base.contains("=== Master: Box ===\nShape 5\n  text: Original master\n"), "{base}");
        assert_ne!(base, diagram(&drawing("Changed master", "1")).unwrap());
        assert_ne!(base, diagram(&drawing("Original master", "2")).unwrap());
        assert_eq!(diagram_text(&drawing("Original master", "1")).unwrap(), "=== Page: P ===\nOriginal master\n");
    }

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
