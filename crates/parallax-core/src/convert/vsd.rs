//! Binary Visio drawings (.vsd, .vss, .vst of Visio 2000–2010), read into the
//! same model as .vsdx, so that a drawing saved in either format reads alike in
//! the Diagram view.
//!
//! The file is an OLE compound file whose `VisioDocument` stream starts with a
//! header pointing at the trailer stream. Streams hold either pointers to further
//! streams (a tree: pages, stencils, the name table …) or a run of chunks, each a
//! record with type, id, level and data; levels nest the chunks of a page into
//! shapes and their sections. Streams may be compressed with a small LZ77 variant.
//! Values are stored in inches whatever unit the drawing shows.
//!
//! The binary format keeps the whole section of a cell a shape overrides, where
//! .vsdx keeps only the overridden cells. Cells are therefore shown where they
//! differ from what the shape inherits: from its master's shape, or else its style.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;

use super::visio::{Drawing, Page, Shape, number};
use crate::{Error, Result};

/// Streams nest a few levels deep; a deeper tree is damaged or hostile.
const MAX_DEPTH: usize = 24;
/// A stream lists at most this many pointers (real files have a few hundred).
const MAX_POINTERS: usize = 100_000;
/// Chunks nest a few levels deep (page, shape, section, row).
const MAX_LEVELS: usize = 16;

// Stream and chunk types.
const TRAILER: u32 = 0x14;
const PAGE: u32 = 0x15;
const COLOURS: u32 = 0x16;
const STYLES: u32 = 0x1a;
const STENCILS: u32 = 0x1d;
const STENCIL_PAGE: u32 = 0x1e;
const PAGES: u32 = 0x27;
const NAME_LIST: u32 = 0x2c;
const NAME: u32 = 0x2d;
const NAME_TABLE: u32 = 0x32;
const NAME_ENTRY: u32 = 0x33;
const PAGE_SHEET: u32 = 0x46;
const GROUP: u32 = 0x47;
const SHAPE: u32 = 0x48;
const STYLE_SHEET: u32 = 0x4a;
const GUIDE: u32 = 0x4d;
const FOREIGN: u32 = 0x4e;
const TEXT: u32 = 0x0e;
const FOREIGN_DATA: u32 = 0x0c;
const FIELD_LIST: u32 = 0x66;
const USER_LIST: u32 = 0x67;
const GEOMETRY_LIST: u32 = 0x6c;
const PROPERTY_LIST: u32 = 0x6d;
const LINE: u32 = 0x85;
const FILL: u32 = 0x86;
const PAGE_PROPS: u32 = 0x92;
const XFORM: u32 = 0x9b;
const XFORM_1D: u32 = 0x9d;
const FIELD: u32 = 0xa1;
const MISC: u32 = 0xa4;
const USER_CELL: u32 = 0xb4;
const PROPERTY: u32 = 0xb6;
const NAME_INDEX: u32 = 0xc9;
const NAME_INDEX_OLD: u32 = 0x34;

/// Reads a binary Visio file. Errors are `Unsupported` for versions not read here
/// (Visio 5 and older).
pub(super) fn read(data: &[u8]) -> Result<Drawing> {
    let doc = visio_stream(data)?;
    let version = *doc.get(0x1a).ok_or_else(|| bad("the header is cut off"))?;
    if !matches!(version, 6 | 11) {
        return Err(Error::Unsupported(format!("Visio files of version {version} are not read natively")));
    }
    let trailer = pointer(&doc, 0x24).ok_or_else(|| bad("the header is cut off"))?;
    let budget = doc.len().saturating_mul(32).max(1 << 20);
    let mut reader = Reader { doc: &doc, version, visited: HashSet::new(), budget };
    let root = reader.stream(&trailer, 0, 0).ok_or_else(|| bad("the trailer stream is damaged"))?;
    if root.ty != TRAILER || root.children.is_empty() {
        return Err(bad("the trailer stream is damaged"));
    }
    Ok(Interpreter::new(&root, version).drawing(&root))
}

fn bad(what: &str) -> Error {
    Error::Unsupported(format!("damaged Visio drawing: {what}"))
}

fn visio_stream(data: &[u8]) -> Result<Vec<u8>> {
    let mut file = cfb::CompoundFile::open(std::io::Cursor::new(data)).map_err(|e| bad(&e.to_string()))?;
    let mut stream = file.open_stream("/VisioDocument").map_err(|_| bad("there is no VisioDocument stream"))?;
    let mut out = Vec::new();
    // The stream cannot be longer than the file it is in.
    stream.by_ref().take(data.len() as u64).read_to_end(&mut out).map_err(|e| bad(&e.to_string()))?;
    Ok(out)
}

fn u8_at(d: &[u8], o: usize) -> Option<u8> {
    d.get(o).copied()
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?))
}

fn f64_at(d: &[u8], o: usize) -> Option<f64> {
    let v = f64::from_le_bytes(d.get(o..o.checked_add(8)?)?.try_into().ok()?);
    v.is_finite().then_some(v)
}

/// A reference to a stream: type, where it is in the document and how it is stored.
struct Pointer {
    ty: u32,
    offset: u32,
    length: u32,
    format: u16,
}

fn pointer(d: &[u8], o: usize) -> Option<Pointer> {
    Some(Pointer {
        ty: u32_at(d, o)?,
        offset: u32_at(d, o + 8)?,
        length: u32_at(d, o + 12)?,
        format: u16_at(d, o + 16)?,
    })
}

/// A stream, read: its pointers to further streams or its chunks.
struct Stream {
    ty: u32,
    /// The index of the pointer it was reached by: the id of pages, masters, shapes.
    idx: u32,
    format: u16,
    data: Vec<u8>,
    children: Vec<Stream>,
    chunks: Vec<Chunk>,
}

impl Stream {
    fn child(&self, ty: u32) -> Option<&Stream> {
        self.children.iter().find(|c| c.ty == ty)
    }
}

/// A chunk with the chunks nested in it.
struct Chunk {
    ty: u32,
    id: u32,
    data: Vec<u8>,
    children: Vec<Chunk>,
}

impl Chunk {
    /// The chunks in this list, in the order the list gives (sub-header length,
    /// length of the order, then the item ids), the others after them.
    fn items(&self) -> Vec<&Chunk> {
        let d = &self.data;
        let mut out: Vec<&Chunk> = Vec::new();
        if let (Some(sub), Some(len)) = (u32_at(d, 0), u32_at(d, 4)) {
            let start = 8usize.saturating_add(sub as usize);
            let n = (len as usize / 4).min(d.len().saturating_sub(start) / 4);
            for i in 0..n {
                let Some(id) = u32_at(d, start + i * 4) else { break };
                if let Some(c) = self.children.iter().find(|c| c.id == id)
                    && !out.iter().any(|o| std::ptr::eq(*o, c))
                {
                    out.push(c);
                }
            }
        }
        let mut rest: Vec<&Chunk> =
            self.children.iter().filter(|c| !out.iter().any(|o| std::ptr::eq(*o, *c))).collect();
        rest.sort_by_key(|c| c.id);
        out.extend(rest);
        out
    }

    /// All chunks below this one, depth first.
    fn descendants(&self) -> Vec<&Chunk> {
        let mut out = Vec::new();
        let mut stack: Vec<&Chunk> = self.children.iter().rev().collect();
        while let Some(c) = stack.pop() {
            out.push(c);
            stack.extend(c.children.iter().rev());
        }
        out
    }
}

struct Reader<'a> {
    doc: &'a [u8],
    version: u8,
    /// Stream offsets already read: pointers may form cycles.
    visited: HashSet<u32>,
    /// Bytes left to read: many pointers to one stream must not multiply it.
    budget: usize,
}

impl Reader<'_> {
    fn stream(&mut self, ptr: &Pointer, idx: u32, depth: usize) -> Option<Stream> {
        if depth > MAX_DEPTH || ptr.ty == 0 {
            return None;
        }
        let start = ptr.offset as usize;
        let raw = self.doc.get(start..start.checked_add(ptr.length as usize)?)?;
        let compressed = ptr.format & 2 != 0;
        let data = if compressed { inflate(raw) } else { raw.to_vec() };
        self.budget = self.budget.checked_sub(data.len())?;
        let shift = if compressed { 4 } else { 0 };
        let mut stream = Stream { ty: ptr.ty, idx, format: ptr.format, data, children: Vec::new(), chunks: Vec::new() };
        match ptr.format >> 4 {
            0 | 4 | 5 => {
                stream.data = stream.data.get(shift..).unwrap_or_default().to_vec();
                if ptr.format >> 4 == 5 && ptr.ty != COLOURS && self.visited.insert(ptr.offset) {
                    let data = std::mem::take(&mut stream.data);
                    for (i, p) in pointers(&data) {
                        if let Some(child) = self.stream(&p, i, depth + 1) {
                            stream.children.push(child);
                        }
                    }
                    stream.data = data;
                }
            }
            8 | 0xc | 0xd => stream.chunks = nest(chunks(&stream.data, self.version)),
            _ => {}
        }
        Some(stream)
    }
}

/// The pointers listed in a pointer stream (data without the compression prefix),
/// with their indexes.
fn pointers(data: &[u8]) -> Vec<(u32, Pointer)> {
    let Some(base) = u32_at(data, 0).and_then(|o| (o as usize).checked_sub(4)) else { return Vec::new() };
    let count = u32_at(data, base + 4).unwrap_or(0) as usize;
    let count = count.min(MAX_POINTERS).min(data.len().saturating_sub(base) / 18);
    (0..count).filter_map(|i| Some((i as u32, pointer(data, base + 12 + i * 18)?))).filter(|(_, p)| p.ty != 0).collect()
}

/// Undoes the compression: flag bytes whose bits tell, for the next eight items,
/// a literal byte (1) or a two-byte reference into a 4 KiB window (0).
fn inflate(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() * 2);
    let mut window = [0u8; 4096];
    let mut pos = 0usize;
    let mut i = 0;
    while i < src.len() {
        let flags = src[i];
        i += 1;
        for bit in 0..8 {
            if i >= src.len() {
                break;
            }
            if flags & (1 << bit) != 0 {
                window[pos & 4095] = src[i];
                out.push(src[i]);
                i += 1;
                pos += 1;
            } else {
                let (Some(&a), Some(&b)) = (src.get(i), src.get(i + 1)) else { return out };
                i += 2;
                let length = (b as usize & 15) + 3;
                let at = ((b as usize & 0xf0) << 4) | a as usize;
                let at = if at > 4078 { at - 4078 } else { at + 18 };
                for j in 0..length {
                    let c = window[(at + j) & 4095];
                    window[(pos + j) & 4095] = c;
                    out.push(c);
                }
                pos += length;
            }
        }
    }
    out
}

/// A chunk as it is in the stream, with its level.
struct Flat {
    ty: u32,
    id: u32,
    level: u16,
    data: Vec<u8>,
}

/// The chunks of a chunk stream. The header does not say how long the trailer
/// after the data is; that depends on type, list and level, as observed in files.
fn chunks(d: &[u8], version: u8) -> Vec<Flat> {
    let mut out = Vec::new();
    let mut o = 0;
    while o < d.len() {
        while d.get(o) == Some(&0) {
            o += 1;
        }
        let (Some(ty), Some(id), Some(list), Some(len), Some(level), Some(unknown)) =
            (u32_at(d, o), u32_at(d, o + 4), u32_at(d, o + 8), u32_at(d, o + 12), u16_at(d, o + 16), u8_at(d, o + 18))
        else {
            break;
        };
        let start = o + 19;
        let Some(end) = start.checked_add(len as usize).filter(|&e| e <= d.len()) else { break };
        let trailer = if version >= 11 { trailer_11(ty, list, level, unknown) } else { trailer_6(ty, list) };
        // Lists keep the order of their items in the trailer, so it stays with the data.
        let with_trailer = end.saturating_add(trailer).min(d.len());
        out.push(Flat { ty, id, level, data: d[start..with_trailer].to_vec() });
        o = end + trailer;
    }
    out
}

fn trailer_11(ty: u32, list: u32, level: u16, unknown: u8) -> usize {
    if matches!(ty, 0x1f | 0xc9 | 0x2d | 0xd1) {
        return 0;
    }
    let mut trailer = 0;
    if list != 0 || matches!(ty, 0x71 | 0x70 | 0x6b | 0x6a | 0x69 | 0x66 | 0x65 | 0x2c) {
        trailer += 8;
    }
    if list != 0
        || (level == 2 && unknown == 0x55)
        || (level == 2 && unknown == 0x54 && ty == 0xaa)
        || (level == 3 && unknown != 0x50 && unknown != 0x54)
    {
        trailer += 4;
    }
    let listed =
        matches!(ty, 0x64 | 0x65 | 0x66 | 0x69 | 0x6a | 0x6b | 0x6f | 0x71 | 0x92 | 0xa9 | 0xb4 | 0xb6 | 0xb9 | 0xc7);
    if listed && trailer != 12 && trailer != 4 {
        trailer += 4;
    }
    trailer
}

fn trailer_6(ty: u32, list: u32) -> usize {
    if matches!(ty, 0x1f | 0xc9) {
        0
    } else if list != 0 || (0x64..=0x76).contains(&ty) || matches!(ty, 0x2c | 0x0d) {
        8
    } else {
        0
    }
}

/// Nests the chunks by level: a chunk belongs to the last one before it of a
/// lower level.
fn nest(flat: Vec<Flat>) -> Vec<Chunk> {
    let mut roots: Vec<Chunk> = Vec::new();
    // Open chunks with their levels; each is moved into its parent when closed.
    let mut open: Vec<(u16, Chunk)> = Vec::new();
    let close = |open: &mut Vec<(u16, Chunk)>, roots: &mut Vec<Chunk>| {
        if let Some((_, chunk)) = open.pop() {
            match open.last_mut() {
                Some((_, parent)) => parent.children.push(chunk),
                None => roots.push(chunk),
            }
        }
    };
    for f in flat {
        while open.last().is_some_and(|(level, _)| *level >= f.level) || open.len() >= MAX_LEVELS {
            close(&mut open, &mut roots);
        }
        open.push((f.level, Chunk { ty: f.ty, id: f.id, data: f.data, children: Vec::new() }));
    }
    while !open.is_empty() {
        close(&mut open, &mut roots);
    }
    roots
}

/// A geometry row: its kind and its named cells.
type Row = (&'static str, Vec<(&'static str, String)>);

/// A shape as read, before it is compared with what it inherits.
#[derive(Default, Clone)]
struct Raw {
    id: u32,
    kind: &'static str,
    name: String,
    parent: Option<u32>,
    master: Option<u32>,
    master_shape: Option<u32>,
    line_style: Option<u32>,
    fill_style: Option<u32>,
    text: String,
    /// The cell values the shape's own sections hold.
    cells: BTreeMap<&'static str, String>,
    data: Vec<(String, String)>,
    image: Option<String>,
    /// Geometry sections, each a list of rows: kind and named cells.
    geometry: Vec<Vec<Row>>,
    other: Option<u32>,
    begin: Option<u32>,
    end: Option<u32>,
}

/// What a style sets: line weight and colour, fill colour, and the styles it
/// inherits from.
#[derive(Default)]
struct Style {
    cells: BTreeMap<&'static str, String>,
    line_parent: Option<u32>,
    fill_parent: Option<u32>,
}

struct Interpreter {
    version: u8,
    /// The name table: name id → name.
    names: HashMap<u32, String>,
    colours: Vec<[u8; 3]>,
    styles: HashMap<u32, Style>,
}

impl Interpreter {
    fn new(root: &Stream, version: u8) -> Self {
        let mut names = HashMap::new();
        if let Some(table) = root.child(NAME_TABLE) {
            for entry in table.children.iter().filter(|c| c.ty == NAME_ENTRY) {
                let d = entry.data.get(4..).unwrap_or_default();
                names.insert(entry.idx, if version >= 11 { utf16z(d) } else { latin1z(d) });
            }
        }
        let mut colours = Vec::new();
        if let Some(c) = root.child(COLOURS) {
            let n = u8_at(&c.data, 2).unwrap_or(0) as usize;
            for i in 0..n {
                if let Some(rgb) = c.data.get(4 + i * 4..7 + i * 4) {
                    colours.push([rgb[0], rgb[1], rgb[2]]);
                }
            }
        }
        let mut me = Interpreter { version, names, colours, styles: HashMap::new() };
        if let Some(styles) = root.child(STYLES) {
            for sheet in styles.chunks.iter().flat_map(|c| std::iter::once(c).chain(c.descendants())) {
                if sheet.ty == STYLE_SHEET {
                    let style_ref = |o| u32_at(&sheet.data, o).filter(|&s| s != u32::MAX && s != sheet.id);
                    let mut style =
                        Style { cells: BTreeMap::new(), line_parent: style_ref(0x22), fill_parent: style_ref(0x2a) };
                    for c in sheet.descendants() {
                        me.style_cells(c, &mut style.cells);
                    }
                    me.styles.insert(sheet.id, style);
                }
            }
        }
        me
    }

    fn drawing(&self, root: &Stream) -> Drawing {
        let mut masters: Vec<(u32, String, Vec<Raw>)> = Vec::new();
        if let Some(stencils) = root.child(STENCILS) {
            let names = stencils.child(NAME_INDEX).map(|s| self.name_index(&s.data)).unwrap_or_default();
            for page in stencils.children.iter().filter(|c| c.ty == STENCIL_PAGE) {
                let name = names.get(&page.idx).cloned().unwrap_or_else(|| format!("#{}", page.idx));
                let shape_names = page
                    .child(PAGE_SHEET)
                    .map(|s| self.shape_names(s.chunks.iter().flat_map(|c| c.descendants())))
                    .unwrap_or_default();
                let mut shapes = Vec::new();
                for s in page.children.iter().filter(|c| matches!(c.ty, GROUP | SHAPE | FOREIGN | GUIDE)) {
                    if let Some(chunk) = s.chunks.iter().find(|c| c.ty == s.ty) {
                        let id = if chunk.id == u32::MAX { s.idx } else { chunk.id };
                        shapes.push(self.shape(chunk, id, &shape_names));
                    }
                }
                masters.push((page.idx, name, shapes));
            }
        }
        let mut pages = Vec::new();
        if let Some(index) = root.child(PAGES) {
            let names = index.child(NAME_INDEX).map(|s| self.name_index(&s.data)).unwrap_or_default();
            for stream in index.children.iter().filter(|c| c.ty == PAGE) {
                let name = names.get(&stream.idx).cloned().unwrap_or_else(|| format!("Page-{}", stream.idx + 1));
                let mut page = Page { name, background: stream.format & 1 == 0, ..Default::default() };
                let mut raws = Vec::new();
                for top in &stream.chunks {
                    let all: Vec<&Chunk> = std::iter::once(top).chain(top.descendants()).collect();
                    let sheet = all.iter().find(|c| c.ty == PAGE_SHEET);
                    let shape_names = sheet.map(|s| self.shape_names(s.children.iter())).unwrap_or_default();
                    if let Some(props) = sheet.and_then(|s| s.descendants().into_iter().find(|c| c.ty == PAGE_PROPS))
                        && let (Some(w), Some(h)) = (f64_at(&props.data, 1), f64_at(&props.data, 10))
                    {
                        page.size =
                            Some((format!("{} in", number(&w.to_string())), format!("{} in", number(&h.to_string()))));
                    }
                    for c in all.iter().filter(|c| matches!(c.ty, GROUP | SHAPE | FOREIGN | GUIDE)) {
                        raws.push(self.shape(c, c.id, &shape_names));
                    }
                }
                page.shapes = self.finish(raws, &masters);
                pages.push(page);
            }
        }
        let masters =
            masters.iter().map(|(_, name, shapes)| (name.clone(), self.finish(shapes.clone(), &[]))).collect();
        Drawing { pages, masters }
    }

    /// NameIDX records: name id, universal name id, element id, flags.
    fn name_index(&self, d: &[u8]) -> HashMap<u32, String> {
        let count = (u32_at(d, 0).unwrap_or(0) as usize).min(d.len() / 13);
        let mut out = HashMap::new();
        for i in 0..count {
            let o = 4 + i * 13;
            if let (Some(name), Some(element)) = (u32_at(d, o), u32_at(d, o + 8))
                && let Some(text) = self.names.get(&name)
            {
                out.insert(element, text.clone());
            }
        }
        out
    }

    fn shape_names<'a>(&self, chunks: impl Iterator<Item = &'a Chunk>) -> HashMap<u32, String> {
        let mut out = HashMap::new();
        for c in chunks {
            if matches!(c.ty, NAME_INDEX | NAME_INDEX_OLD) {
                out.extend(self.name_index(&c.data));
            }
        }
        out
    }

    fn style_cells(&self, c: &Chunk, cells: &mut BTreeMap<&'static str, String>) {
        match c.ty {
            LINE => {
                if let Some(w) = f64_at(&c.data, 1) {
                    cells.insert("LineWeight", w.to_string());
                }
                if let Some(colour) = self.colour(&c.data, 9) {
                    cells.insert("LineColor", colour);
                }
            }
            FILL => {
                if let Some(colour) = self.colour(&c.data, 0) {
                    cells.insert("FillForegnd", colour);
                }
            }
            _ => {}
        }
    }

    /// A colour: palette index, then red, green, blue and alpha; the index counts
    /// where the colour itself is not given.
    fn colour(&self, d: &[u8], o: usize) -> Option<String> {
        let index = u8_at(d, o)?;
        let rgb = d.get(o + 1..o + 4)?;
        let rgb =
            if rgb == [0, 0, 0] { self.colours.get(index as usize).map(|c| c.as_slice()).unwrap_or(rgb) } else { rgb };
        Some(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]))
    }

    fn shape(&self, chunk: &Chunk, id: u32, names: &HashMap<u32, String>) -> Raw {
        let d = &chunk.data;
        let some = |o| u32_at(d, o).filter(|&v| v != u32::MAX);
        let mut raw = Raw {
            id,
            kind: match chunk.ty {
                GROUP => "Group",
                FOREIGN => "Foreign",
                GUIDE => "Guide",
                _ => "Shape",
            },
            name: names.get(&id).cloned().unwrap_or_default(),
            parent: some(10).filter(|&p| p != 0),
            master: some(18),
            master_shape: some(26),
            fill_style: some(34),
            line_style: some(42),
            ..Default::default()
        };
        let mut other = crc32fast::Hasher::new();
        let mut fields: Vec<Vec<u8>> = Vec::new();
        // Names a shape's text fields refer to.
        let mut field_names: HashMap<u32, String> = HashMap::new();
        for c in chunk.children.iter() {
            match c.ty {
                GEOMETRY_LIST => raw.geometry.push(self.geometry(c, &mut other)),
                USER_LIST | PROPERTY_LIST => self.rows(c, &mut raw.data),
                FIELD_LIST => {
                    for f in c.items().into_iter().filter(|f| f.ty == FIELD) {
                        fields.push(f.data.clone());
                    }
                }
                NAME_LIST => {
                    for n in c.children.iter().filter(|n| n.ty == NAME) {
                        field_names.insert(n.id, self.string(&n.data));
                    }
                }
                TEXT => raw.text = self.text(&c.data),
                FOREIGN_DATA => {
                    raw.image = Some(format!("{} bytes, crc {:08x}", c.data.len(), crc32fast::hash(&c.data)))
                }
                _ => {
                    for s in std::iter::once(c).chain(c.descendants()) {
                        self.cells(s, &mut raw, &mut other);
                    }
                }
            }
        }
        // Fields stand in the text as U+FFFC (Visio 2003) or U+001E (Visio 2000).
        if !fields.is_empty() {
            let mut fields = fields.iter().map(|d| field(d, &field_names));
            raw.text = raw
                .text
                .chars()
                .map(|ch| {
                    if matches!(ch, '\u{fffc}' | '\u{1e}') { fields.next().unwrap_or_default() } else { ch.to_string() }
                })
                .collect();
        }
        raw.text = raw.text.replace(['\u{fffc}', '\u{1e}'], "").trim_end().to_string();
        let hash = other.finalize();
        raw.other = (hash != crc32fast::Hasher::new().finalize()).then_some(hash);
        raw
    }

    fn cells(&self, c: &Chunk, raw: &mut Raw, other: &mut crc32fast::Hasher) {
        let d = &c.data;
        let mut set = |names: &[&'static str], at: &[usize]| {
            for (n, o) in names.iter().zip(at) {
                if let Some(v) = f64_at(d, *o) {
                    raw.cells.insert(n, v.to_string());
                }
            }
        };
        match c.ty {
            XFORM => set(&["PinX", "PinY", "Width", "Height", "Angle"], &[1, 10, 19, 28, 55]),
            XFORM_1D => set(&["BeginX", "BeginY", "EndX", "EndY"], &[1, 10, 19, 28]),
            LINE | FILL => self.style_cells(c, &mut raw.cells),
            MISC => {
                // A connector's triggers refer to itself while it is not glued.
                let (begin, end) = glue(d);
                raw.begin = raw.begin.or(begin.filter(|&b| b != raw.id));
                raw.end = raw.end.or(end.filter(|&e| e != raw.id));
            }
            0x68 | 0x65 | 0x83 | NAME_INDEX => {}
            _ => {
                other.update(&c.ty.to_le_bytes());
                other.update(d);
            }
        }
        if c.ty == MISC {
            other.update(&c.ty.to_le_bytes());
            other.update(d);
        }
    }

    fn geometry(&self, list: &Chunk, other: &mut crc32fast::Hasher) -> Vec<Row> {
        let rows = list.items();
        let mut out = Vec::new();
        for r in rows {
            let d = &r.data;
            // Cells with a unit byte before them, then plain doubles.
            let (kind, cells, plain): (&str, &[&str], &[&str]) = match r.ty {
                0x8a => ("MoveTo", &["X", "Y"], &[]),
                0x8b => ("LineTo", &["X", "Y"], &[]),
                0x8c => ("ArcTo", &["X", "Y", "A"], &[]),
                0x8d => ("InfiniteLine", &["X", "Y", "A", "B"], &[]),
                0x8f => ("Ellipse", &["X", "Y", "A", "B", "C", "D"], &[]),
                0x90 => ("EllipticalArcTo", &["X", "Y", "A", "B", "C", "D"], &[]),
                0xa5 => ("SplineStart", &["X", "Y"], &["A", "B", "C"]),
                0xa6 => ("SplineKnot", &["X", "Y"], &["A"]),
                0xc1 => ("PolylineTo", &["X", "Y"], &[]),
                0xc3 => ("NURBSTo", &["X", "Y"], &["A", "B", "C", "D"]),
                _ => {
                    other.update(&r.ty.to_le_bytes());
                    other.update(d);
                    continue;
                }
            };
            let mut values = Vec::new();
            for (i, n) in cells.iter().enumerate() {
                if let Some(v) = f64_at(d, 1 + i * 9) {
                    values.push((*n, v.to_string()));
                }
            }
            let base = cells.len() * 9;
            for (i, n) in plain.iter().enumerate() {
                if let Some(v) = f64_at(d, base + i * 8) {
                    values.push((*n, v.to_string()));
                }
            }
            out.push((kind, values));
        }
        out
    }

    /// User-defined cells and shape data: rows named through the list's NameIDX.
    fn rows(&self, list: &Chunk, data: &mut Vec<(String, String)>) {
        let names = self.shape_names(list.children.iter());
        let wide = self.version >= 11;
        for r in list.items().into_iter().filter(|r| matches!(r.ty, USER_CELL | PROPERTY)) {
            let name = names.get(&r.id).cloned().unwrap_or_else(|| format!("Row_{}", r.id));
            let value = cell_value(&r.data, wide);
            if list.ty == USER_LIST {
                data.push((format!("User.{name}"), value));
            } else {
                // Shape data shows its label (cell 2) where it has one.
                let label = literal(&r.data, 2, wide).filter(|l| !l.is_empty()).unwrap_or(name);
                data.push((label, value));
            }
        }
    }

    fn string(&self, d: &[u8]) -> String {
        if self.version >= 11 { utf16z(d) } else { latin1z(d) }
    }

    fn text(&self, d: &[u8]) -> String {
        let body = d.get(8..).unwrap_or_default();
        let text = if self.version >= 11 { utf16z(body) } else { latin1z(body) };
        text.replace("\r\n", "\n").replace('\r', "\n")
    }

    /// Turns the shapes of a page or master into the model: groups get their
    /// members, and cells are kept where they differ from what is inherited.
    fn finish(&self, raws: Vec<Raw>, masters: &[(u32, String, Vec<Raw>)]) -> Vec<Shape> {
        let ids: HashSet<u32> = raws.iter().map(|r| r.id).collect();
        let mut by_parent: BTreeMap<Option<u32>, Vec<Raw>> = BTreeMap::new();
        for r in raws {
            let parent = r.parent.filter(|p| ids.contains(p) && *p != r.id);
            by_parent.entry(parent).or_default().push(r);
        }
        let mut visited = HashSet::new();
        let mut out = self.build(None, None, &mut by_parent, masters, &mut visited, 0);
        // Members of groups that form a cycle (or nest too deep) still show.
        while let Some(parent) = by_parent.keys().next().copied() {
            out.extend(self.build(parent, None, &mut by_parent, masters, &mut visited, 0));
        }
        sort(&mut out);
        out
    }

    fn build(
        &self,
        parent: Option<u32>,
        group_master: Option<u32>,
        by_parent: &mut BTreeMap<Option<u32>, Vec<Raw>>,
        masters: &[(u32, String, Vec<Raw>)],
        visited: &mut HashSet<u32>,
        depth: usize,
    ) -> Vec<Shape> {
        let mut out = Vec::new();
        if depth > MAX_DEPTH {
            return out;
        }
        for raw in by_parent.remove(&parent).unwrap_or_default() {
            if !visited.insert(raw.id) {
                continue;
            }
            let master = masters.iter().find(|(id, _, _)| Some(*id) == raw.master.or(group_master));
            let source = master.and_then(|(_, _, shapes)| {
                let named = raw.master_shape.and_then(|s| shapes.iter().find(|m| m.id == s));
                named.or_else(|| shapes.iter().find(|m| m.parent.is_none()).filter(|_| raw.master.is_some()))
            });
            // Group members carry the master of their group; .vsdx names it there only.
            let own_master = raw.master.filter(|m| Some(*m) != group_master);
            let mut shape = Shape {
                id: raw.id.to_string(),
                kind: raw.kind.into(),
                name: raw.name.clone(),
                master: own_master.map(|m| master.map(|(_, n, _)| n.clone()).unwrap_or_else(|| format!("#{m}"))),
                master_id: own_master.map(|m| m.to_string()),
                master_shape: if own_master.is_none() { raw.master_shape.map(|s| s.to_string()) } else { None },
                text: raw.text.clone(),
                data: match source {
                    Some(s) => raw.data.iter().filter(|d| !s.data.contains(d)).cloned().collect(),
                    None => raw.data.clone(),
                },
                image: raw.image.clone(),
                other: raw.other,
                begin: raw.begin.map(|b| b.to_string()),
                end: raw.end.map(|e| e.to_string()),
                ..Default::default()
            };
            // Without text of its own a shape shows its master's, as for .vsdx.
            if let Some(s) = source.filter(|s| !s.text.is_empty() && (shape.text.is_empty() || s.text == shape.text)) {
                shape.text = s.text.clone();
                shape.inherited_text = true;
            }
            for (name, value) in &raw.cells {
                let inherited = match source {
                    Some(s) => s.cells.get(name).cloned().or_else(|| match *name {
                        "LineWeight" | "LineColor" => self.style_value(s.line_style, name),
                        "FillForegnd" => self.style_value(s.fill_style, name),
                        _ => None,
                    }),
                    None if matches!(*name, "LineWeight" | "LineColor") => self.style_value(raw.line_style, name),
                    None if *name == "FillForegnd" => self.style_value(raw.fill_style, name),
                    None => None,
                };
                if inherited.as_ref().map(|v| number(v)) != Some(number(value)) {
                    shape.cells.insert(name.to_string(), value.clone());
                }
            }
            for (i, section) in raw.geometry.iter().enumerate() {
                let master_section = source.and_then(|s| s.geometry.get(i));
                let mut rows = Vec::new();
                for (j, (kind, cells)) in section.iter().enumerate() {
                    let master_row = master_section.and_then(|m| m.get(j)).filter(|(k, _)| k == kind);
                    let cells: Vec<String> = cells
                        .iter()
                        .filter(|(n, v)| {
                            master_row.is_none_or(|(_, m)| !m.iter().any(|(mn, mv)| mn == n && number(mv) == number(v)))
                        })
                        .map(|(n, v)| if n.len() == 1 { number(v) } else { format!("{n} {}", number(v)) })
                        .collect();
                    if master_row.is_some() && cells.is_empty() {
                        continue;
                    }
                    rows.push(if cells.is_empty() { kind.to_string() } else { format!("{kind} {}", cells.join(", ")) });
                }
                if !rows.is_empty() {
                    shape.geometry.push(rows.join("; "));
                }
            }
            let members_master = raw.master.or(group_master);
            shape.children = self.build(Some(raw.id), members_master, by_parent, masters, visited, depth + 1);
            out.push(shape);
        }
        out
    }

    /// A cell's value from a style or the styles it inherits from.
    fn style_value(&self, style: Option<u32>, name: &str) -> Option<String> {
        let mut style = style;
        for _ in 0..MAX_DEPTH {
            let s = self.styles.get(&style?)?;
            if let Some(v) = s.cells.get(name) {
                return Some(v.clone());
            }
            style = if name == "FillForegnd" { s.fill_parent } else { s.line_parent };
        }
        None
    }
}

fn sort(shapes: &mut [Shape]) {
    shapes.sort_by_key(|s| (s.id.parse::<u64>().unwrap_or(u64::MAX), s.id.clone()));
    for shape in shapes {
        sort(&mut shape.children);
    }
}

/// A row's value: a type byte, then a double, or for strings the formula of cell
/// 0 when it is a string literal.
fn cell_value(d: &[u8], wide: bool) -> String {
    match u8_at(d, 0) {
        Some(0xe7 | 0xe8) => literal(d, 0, wide).unwrap_or_default(),
        Some(40) => f64_at(d, 1).map(date).unwrap_or_default(),
        Some(_) => f64_at(d, 1).map(|v| number(&v.to_string())).unwrap_or_default(),
        None => String::new(),
    }
}

/// The formula of a cell when it is a string literal: a formula block (block type
/// 2, the cell's index), token 0x60, the length in characters, the characters.
fn literal(d: &[u8], cell: u8, wide: bool) -> Option<String> {
    let o = (0..d.len().saturating_sub(3)).find(|&o| d[o] == 2 && d[o + 1] == cell && d[o + 2] == 0x60)?;
    let n = *d.get(o + 3)? as usize;
    if wide { Some(utf16z(d.get(o + 4..o + 4 + n * 2)?)) } else { Some(latin1z(d.get(o + 4..o + 4 + n)?)) }
}

/// A text field's value: the name it shows, or its number in the unit and format
/// the field asks for.
fn field(d: &[u8], names: &HashMap<u32, String>) -> String {
    match u8_at(d, 7) {
        Some(0xe7 | 0xe8) => u32_at(d, 8).and_then(|id| names.get(&id).cloned()).unwrap_or_default(),
        Some(unit) => match f64_at(d, 8) {
            Some(v) => format_value(unit, v, field_format(d)),
            None => String::new(),
        },
        None => String::new(),
    }
}

/// The format number of a field, from its formula block for the format cell
/// (block type 2, cell 2, token 0x62 with a 16-bit number).
fn field_format(d: &[u8]) -> Option<u16> {
    (0x36..d.len().saturating_sub(4))
        .find_map(|o| (d[o] == 2 && d[o + 1] == 2 && d[o + 2] == 0x62).then(|| u16_at(d, o + 3)).flatten())
}

/// A value in its unit: lengths are stored in inches, angles in radians,
/// durations and dates in days.
fn format_value(unit: u8, v: f64, format: Option<u16>) -> String {
    let format = format.unwrap_or(0);
    if unit == 40 || (20..=36).contains(&format) || (200..=221).contains(&format) {
        return date(v);
    }
    let (factor, suffix) = match unit {
        33 => (100.0, "%"),
        42 | 44 => (1.0, " ed."),
        43 => (1.0 / 7.0, " ew."),
        45 => (24.0, " eh."),
        46 => (1440.0, " em."),
        47 => (86400.0, " es."),
        49 | 50 => (72.0, " pt"),
        51 => (6.0, " p"),
        52 | 53 => (67.7522, " d"),
        54 => (5.646, " c"),
        63..=65 | 73 => (1.0, " in"),
        66 | 67 => (1.0 / 12.0, " ft"),
        68 | 74 => (1.0 / 63360.0, " mi"),
        69 => (2.54, " cm"),
        70 => (25.4, " mm"),
        71 => (0.0254, " m"),
        72 => (0.0000254, " km"),
        75 => (1.0 / 36.0, " yd"),
        76 => (1.0 / 72913.4, " nmi"),
        80 | 81 => (180.0 / std::f64::consts::PI, " deg"),
        83 => (1.0, " rad"),
        _ => (1.0, ""),
    };
    let v = v * factor;
    // Formats 2 to 9 fix the decimal places; the even ones leave the unit out.
    let text = match format {
        2..=9 => format!("{v:.*}", ((format - 2) / 2) as usize),
        _ => number(&v.to_string()),
    };
    if (format.is_multiple_of(2) && format != 0) || suffix.is_empty() { text } else { format!("{text}{suffix}") }
}

/// Days since 30 December 1899 as month/day/year.
fn date(days: f64) -> String {
    // Days since 1 March of year 0, for the civil calendar algorithm.
    let z = (days.floor() as i64).clamp(-1_000_000, 10_000_000) - 25569 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{month:02}/{day:02}/{year}")
}

/// The shapes a connector is glued to, from the triggers in its Misc section:
/// formula blocks for BegTrigger (cell 11) and EndTrigger (cell 12) that refer to
/// another sheet's EventXFMod cell.
fn glue(d: &[u8]) -> (Option<u32>, Option<u32>) {
    let (mut begin, mut end) = (None, None);
    for o in 0..d.len().saturating_sub(15) {
        if d[o] == 2
            && matches!(d[o + 1], 0x0b | 0x0c)
            && d[o + 2] == 0x74
            && u32_at(d, o + 3) == Some(0x6000_004e)
            && d[o + 11] == 0x7a
            && u32_at(d, o + 12) == Some(0x4000_0073)
        {
            let sheet = u32_at(d, o + 7);
            if d[o + 1] == 0x0b { begin = begin.or(sheet) } else { end = end.or(sheet) }
        }
    }
    (begin, end)
}

fn utf16z(d: &[u8]) -> String {
    let units: Vec<u16> = d.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

fn latin1z(d: &[u8]) -> String {
    d.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::visio::{diagram, legacy_diagram, legacy_text};

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/visio/", $name))
        };
    }

    /// The lines naming pages, masters and shapes, and the texts.
    fn outline(view: &str) -> Vec<&str> {
        view.lines()
            .filter(|l| {
                let l = l.trim_start();
                l.starts_with("===") || l.starts_with("Shape") || l.starts_with("Group") || l.starts_with("text")
            })
            .collect()
    }

    #[test]
    fn same_drawing_as_vsdx() {
        let vsd = legacy_diagram(fixture!("dwg.vsd")).unwrap();
        let vsdx = diagram(fixture!("dwg.vsdx")).unwrap();
        assert!(read(fixture!("dwg.vsd")).is_ok());
        assert_eq!(outline(&vsd), outline(&vsdx), "{vsd}");
        assert!(vsd.starts_with("=== Page: Page-1 (11.6929 in × 8.2677 in) ===\n"), "{vsd}");
        // The colours differ: the master keeps a theme colour the binary file does
        // not store, so the shape's own colour seems to differ from it.
        assert!(vsd.contains("Shape 1 \"Square\" · master Square\n  at 2.7559, 6.0713\n"), "{vsd}");
        assert!(vsd.contains(" · line weight 0.0035\n"), "{vsd}");
        // The masters' cells and outlines read alike in both formats.
        let geometry = |v: &str| -> Vec<String> {
            v.lines().filter(|l| l.contains("geometry:") || l.contains(" · size ")).map(String::from).collect()
        };
        assert_eq!(geometry(&vsd), geometry(&vsdx));
        assert!(vsd.contains("data: User.visVersion = 15\n"), "{vsd}");
    }

    #[test]
    fn connectors_and_labels() {
        let view = legacy_diagram(fixture!("Visio11FormatLine.vsd")).unwrap();
        assert!(view.starts_with("=== Page: Page-1 (8.2677 in × 11.6929 in) ===\n"), "{view}");
        assert!(view.contains("Shape 1 \"Dynamic connector\" · master Dynamic connector\n"), "{view}");
        assert!(view.contains("\n  from 6.4961, 8.6614 to 7.7756, 8.0709\n"), "{view}");
        assert!(view.contains("Shape 7\n  text: Collosal\n  at 3.6811, 10.9646 · size 0.7874 × 0.2756\n"), "{view}");
        assert!(view.contains("=== Master: Dynamic connector ==="), "{view}");
        let drawing = read(fixture!("Visio11FormatLine.vsd")).unwrap();
        assert_eq!(drawing.pages.len(), 1);
        assert_eq!(drawing.pages[0].shapes.len(), 100);
        let text = legacy_text(fixture!("Visio11FormatLine.vsd")).unwrap();
        assert!(text.starts_with("=== Page: Page-1 ===\n"), "{text}");
        assert!(text.contains("\nCollosal\nExtra large\n"), "{text}");
    }

    #[test]
    fn glue_from_triggers() {
        // A Misc section whose BegTrigger and EndTrigger refer to sheets 4 and 9.
        let block = |cell: u8, sheet: u32| {
            let mut b = vec![0x15, 0, 0, 0, 2, cell, 0x74, 0x4e, 0, 0, 0x60];
            b.extend(sheet.to_le_bytes());
            b.extend([0x7a, 0x73, 0, 0, 0x40, 0xfe]);
            b
        };
        let mut misc = vec![0u8; 45];
        misc.extend(block(0x0c, 9));
        misc.extend(block(0x0b, 4));
        assert_eq!(glue(&misc), (Some(4), Some(9)));
        assert_eq!(glue(&misc[..50]), (None, None));
    }

    #[test]
    fn text_fields() {
        let view = legacy_diagram(fixture!("Visio11TextFieldsWithAngle.vsd")).unwrap();
        assert!(view.contains("Shape 1\n  text: TextField GeometryAngleGeneral -30 deg\n"), "{view}");
        assert!(view.contains("  angle -0.5236\n"), "{view}");
        // Visio 2000: 8-bit text and names.
        let view = legacy_diagram(fixture!("Visio6TextFieldsWithUnits.vsd")).unwrap();
        assert!(view.starts_with("=== Page: Page-1 (8.5 in × 11 in) ===\n"), "{view}");
        assert!(view.contains("Shape 1\n  text: Text with [cm] units 1 cm\n"), "{view}");
        assert_eq!(read(fixture!("Visio6TextFieldsWithUnits.vsd")).unwrap().pages[0].shapes.len(), 12);
    }

    #[test]
    fn units_and_dates() {
        assert_eq!(date(43652.139), "07/06/2019");
        assert_eq!(date(1.0), "12/31/1899");
        assert_eq!(format_value(69, 1.0 / 2.54, Some(1)), "1 cm");
        assert_eq!(format_value(69, 1.0 / 2.54, Some(4)), "1.0");
        assert_eq!(format_value(33, 0.5, None), "50%");
    }

    /// A compound file holding the given VisioDocument stream.
    fn compound(stream: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut file = cfb::CompoundFile::create(std::io::Cursor::new(Vec::new())).unwrap();
        file.create_stream("/VisioDocument").unwrap().write_all(stream).unwrap();
        file.flush().unwrap();
        file.into_inner().into_inner()
    }

    #[test]
    fn damaged_files_fail_cleanly() {
        assert!(read(b"not a compound file").is_err());
        assert!(read(&compound(b"")).is_err());
        let files =
            [&fixture!("dwg.vsd")[..], fixture!("Visio11FormatLine.vsd"), fixture!("Visio6TextFieldsWithUnits.vsd")];
        for data in files {
            assert!(read(&data[..data.len() / 2]).is_err());
            let stream = visio_stream(data).unwrap();
            // Cut off anywhere: an error or a partial drawing, never a panic.
            for n in (0..stream.len()).step_by(97) {
                let _ = read(&compound(&stream[..n]));
            }
            // Bytes changed anywhere.
            let mut seed = 7u64;
            for _ in 0..300 {
                let mut broken = stream.clone();
                for _ in 0..8 {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    let at = (seed >> 33) as usize % broken.len();
                    broken[at] = (seed >> 13) as u8;
                }
                let _ = read(&compound(&broken));
            }
        }
    }

    #[test]
    fn older_versions_are_reported() {
        let mut stream = visio_stream(fixture!("dwg.vsd")).unwrap();
        stream[0x1a] = 5;
        assert!(matches!(read(&compound(&stream)), Err(Error::Unsupported(m)) if m.contains("version 5")));
    }
}
