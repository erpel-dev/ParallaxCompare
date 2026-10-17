//! WebAssembly modules: the interface as an outline, and the code as text.
//!
//! Both views are written for diffing. Functions, globals and locals are shown by
//! their names (from the `name` custom section, else from exports and imports), so
//! adding a function does not renumber every call in the module; functions are
//! sorted by name; each function in the outline has a checksum of its code that
//! hashes the names it refers to rather than their indexes. Only core modules
//! (version 1) are read, not components.
//!
//! Layout: `\0asm`, version u32 = 1, then sections: id byte, LEB128 size, payload.
//! Custom sections (id 0) carry names, producers and DWARF debug info; they do
//! not change what the module does, which is what the similar check relies on.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;

use wasmparser::{
    BlockType, CompositeInnerType, DataKind, ElementItems, ElementKind, ExternalKind, FunctionBody, Imports,
    KnownCustom, Name, Operator, OperatorsReader, Parser, Payload, TableInit, TypeRef, ValType,
};

use crate::{Error, Result};

/// Instructions printed by the text view before it stops; a few hundred
/// thousand lines is as much as a diff stays usable with.
const MAX_TEXT_INSTRUCTIONS: usize = 400_000;
/// Functions listed in the outline before it stops.
const MAX_OUTLINE_FUNCTIONS: usize = 50_000;
/// Element segment items listed per segment.
const MAX_ELEMENT_ITEMS: usize = 64;

/// Whether the data is a core WebAssembly module (not a component).
pub fn is_wasm(data: &[u8]) -> bool {
    data.starts_with(b"\0asm\x01\0\0\0")
}

/// Imports, exports, memories, tables, globals, data segments, custom sections,
/// and every function's signature with a checksum of its code.
pub fn outline(data: &[u8]) -> Result<String> {
    Module::parse(data)?.outline()
}

/// The instructions of every function, with names for functions, globals and locals.
pub fn text(data: &[u8]) -> Result<String> {
    Module::parse(data)?.text()
}

/// Whether two modules have the same code and interface, custom sections aside
/// (names, producers, debug info). Compares the raw bytes of all other sections,
/// which is exact and needs no parsing. `None` when either is not a module.
pub fn same_code(left: &[u8], right: &[u8]) -> Option<bool> {
    Some(code_sections(left)? == code_sections(right)?)
}

/// The non-custom sections of a module as (id, payload) pairs.
fn code_sections(data: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    if !is_wasm(data) {
        return None;
    }
    let mut pos = 8;
    let mut sections = Vec::new();
    while pos < data.len() {
        let id = data[pos];
        let (size, len) = leb_u32(data.get(pos + 1..)?)?;
        let start = pos + 1 + len;
        let end = start.checked_add(size as usize).filter(|&e| e <= data.len())?;
        if id != 0 {
            sections.push((id, &data[start..end]));
        }
        pos = end;
    }
    Some(sections)
}

fn leb_u32(bytes: &[u8]) -> Option<(u32, usize)> {
    let mut value: u32 = 0;
    for (i, &b) in bytes.iter().take(5).enumerate() {
        value |= u32::from(b & 0x7F) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

fn err(e: wasmparser::BinaryReaderError) -> Error {
    Error::Unsupported(format!("WebAssembly: {e}"))
}

#[derive(Default)]
struct Function<'a> {
    type_index: u32,
    import: Option<(&'a str, &'a str)>,
    body: Option<FunctionBody<'a>>,
}

#[derive(Default)]
struct Module<'a> {
    data: &'a [u8],
    /// Per type index, a function signature as WAT writes it, or another type.
    types: Vec<String>,
    /// Per type index, the number of parameters of a function type.
    param_counts: Vec<u32>,
    functions: Vec<Function<'a>>,
    /// Imported tables, memories, globals and tags, as outline lines.
    imports: Vec<String>,
    exports: Vec<(&'a str, ExternalKind, u32)>,
    memories: Vec<String>,
    tables: Vec<String>,
    /// Global types; the init expression for those defined here.
    globals: Vec<(String, Option<wasmparser::ConstExpr<'a>>)>,
    data_segments: Vec<wasmparser::Data<'a>>,
    elements: Vec<wasmparser::Element<'a>>,
    custom: Vec<(&'a str, usize)>,
    producers: Vec<String>,
    start: Option<u32>,
    function_names: HashMap<u32, &'a str>,
    /// Per function index, the name shown (see [`Self::function_name`]).
    display_names: Vec<String>,
    /// Per function index, whether it has a real name (not `funcN`).
    named: Vec<bool>,
    global_names: HashMap<u32, &'a str>,
    data_names: HashMap<u32, &'a str>,
    local_names: HashMap<u32, HashMap<u32, &'a str>>,
    /// Problems met while reading, shown as warnings.
    notes: Vec<String>,
}

impl<'a> Module<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        if !is_wasm(data) {
            return Err(Error::Unsupported("not a WebAssembly module".into()));
        }
        let mut m = Module { data, ..Default::default() };
        let mut defined = Vec::new();
        let mut bodies = Vec::new();
        for payload in Parser::new(0).parse_all(data) {
            match payload.map_err(err)? {
                Payload::TypeSection(r) => {
                    for group in r {
                        for sub in group.map_err(err)?.into_types() {
                            let (text, params) = match &sub.composite_type.inner {
                                CompositeInnerType::Func(f) => (signature(f.params(), f.results()), f.params().len()),
                                other => (other.to_string(), 0),
                            };
                            m.types.push(text);
                            m.param_counts.push(params as u32);
                        }
                    }
                }
                Payload::ImportSection(r) => {
                    for imports in r {
                        let list: Vec<(&str, &str, TypeRef)> = match imports.map_err(err)? {
                            Imports::Single(_, i) => vec![(i.module, i.name, i.ty)],
                            Imports::Compact1 { module, items } => items
                                .into_iter()
                                .map(|i| i.map(|i| (module, i.name, i.ty)))
                                .collect::<std::result::Result<_, _>>()
                                .map_err(err)?,
                            Imports::Compact2 { module, ty, names } => names
                                .into_iter()
                                .map(|n| n.map(|n| (module, n, ty)))
                                .collect::<std::result::Result<_, _>>()
                                .map_err(err)?,
                        };
                        for (module, name, ty) in list {
                            m.import(module, name, ty);
                        }
                    }
                }
                Payload::FunctionSection(r) => {
                    for t in r {
                        defined.push(t.map_err(err)?);
                    }
                }
                Payload::TableSection(r) => {
                    for t in r {
                        let t = t.map_err(err)?;
                        let init = match t.init {
                            TableInit::RefNull => String::new(),
                            TableInit::Expr(e) => format!(" = {}", m.const_expr(&e)),
                        };
                        m.tables.push(format!("{}{init}", table_type(&t.ty)));
                    }
                }
                Payload::MemorySection(r) => {
                    for t in r {
                        m.memories.push(memory_type(&t.map_err(err)?));
                    }
                }
                Payload::GlobalSection(r) => {
                    for g in r {
                        let g = g.map_err(err)?;
                        m.globals.push((global_type(&g.ty), Some(g.init_expr)));
                    }
                }
                Payload::ExportSection(r) => {
                    for e in r {
                        let e = e.map_err(err)?;
                        m.exports.push((e.name, e.kind, e.index));
                    }
                }
                Payload::StartSection { func, .. } => m.start = Some(func),
                Payload::ElementSection(r) => {
                    for e in r {
                        m.elements.push(e.map_err(err)?);
                    }
                }
                Payload::DataSection(r) => {
                    for d in r {
                        m.data_segments.push(d.map_err(err)?);
                    }
                }
                Payload::CodeSectionEntry(body) => bodies.push(body),
                Payload::CustomSection(c) => {
                    m.custom.push((c.name(), c.data().len()));
                    match c.as_known() {
                        KnownCustom::Name(names) => m.read_names(names),
                        KnownCustom::Producers(fields) => {
                            for field in fields.into_iter().flatten() {
                                for v in field.values.into_iter().flatten() {
                                    let version = if v.version.is_empty() { "" } else { " " };
                                    m.producers.push(format!("{} {}{version}{}", field.name, v.name, v.version));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        if bodies.len() != defined.len() {
            m.notes.push(format!("{} functions declared but {} bodies", defined.len(), bodies.len()));
        }
        let mut bodies = bodies.into_iter();
        for type_index in defined {
            m.functions.push(Function { type_index, import: None, body: bodies.next() });
        }
        m.name_functions();
        Ok(m)
    }

    fn import(&mut self, module: &'a str, name: &'a str, ty: TypeRef) {
        let line = match ty {
            TypeRef::Func(t) | TypeRef::FuncExact(t) => {
                self.functions.push(Function { type_index: t, import: Some((module, name)), body: None });
                return;
            }
            TypeRef::Table(t) => {
                self.tables.push(format!("{} (imported)", table_type(&t)));
                format!("table {}", table_type(&t))
            }
            TypeRef::Memory(t) => {
                self.memories.push(format!("{} (imported)", memory_type(&t)));
                format!("memory {}", memory_type(&t))
            }
            TypeRef::Global(t) => {
                self.globals.push((global_type(&t), None));
                format!("global {}", global_type(&t))
            }
            TypeRef::Tag(t) => format!("tag {}", self.type_text(t.func_type_idx)),
        };
        self.imports.push(format!("{module}.{name}: {line}"));
    }

    fn read_names(&mut self, names: wasmparser::NameSectionReader<'a>) {
        let map = |m: wasmparser::NameMap<'a>| -> HashMap<u32, &'a str> {
            m.into_iter().flatten().map(|n| (n.index, n.name)).collect()
        };
        for name in names.into_iter().flatten() {
            match name {
                Name::Function(m) => self.function_names = map(m),
                Name::Global(m) => self.global_names = map(m),
                Name::Data(m) => self.data_names = map(m),
                Name::Local(m) => {
                    for f in m.into_iter().flatten() {
                        self.local_names.insert(f.index, map(f.names));
                    }
                }
                _ => {}
            }
        }
    }

    fn type_text(&self, index: u32) -> String {
        self.types.get(index as usize).cloned().unwrap_or_else(|| format!("(type {index})"))
    }

    /// A function's name: from the name section, else an export or import name.
    fn function_name(&self, index: u32) -> Cow<'_, str> {
        match self.display_names.get(index as usize) {
            Some(name) => Cow::Borrowed(name),
            None => Cow::Owned(format!("func{index}")),
        }
    }

    /// Computes [`Self::display_names`], once, so lookups are cheap.
    fn name_functions(&mut self) {
        let mut exported: HashMap<u32, &str> = HashMap::new();
        for (name, kind, index) in &self.exports {
            if matches!(kind, ExternalKind::Func | ExternalKind::FuncExact) {
                exported.entry(*index).or_insert(name);
            }
        }
        self.display_names = (0..self.functions.len() as u32)
            .map(|i| {
                if let Some(name) = self.function_names.get(&i) {
                    return demangle(name);
                }
                if let Some(name) = exported.get(&i) {
                    return (*name).to_string();
                }
                match self.functions[i as usize].import {
                    Some((module, name)) => format!("{module}.{name}"),
                    None => format!("func{i}"),
                }
            })
            .collect();
        self.named = (0..self.functions.len() as u32)
            .map(|i| self.function_names.contains_key(&i) || exported.contains_key(&i))
            .collect();
    }

    fn global_name(&self, index: u32) -> String {
        self.global_names.get(&index).map_or_else(|| format!("global{index}"), |n| (*n).to_string())
    }

    fn const_expr(&self, e: &wasmparser::ConstExpr) -> String {
        let mut parts = Vec::new();
        let mut reader = e.get_operators_reader();
        let names = Names::new(self, None);
        while let Ok(op) = reader.read() {
            if matches!(op, Operator::End) {
                break;
            }
            let mut s = String::new();
            names.instruction(&op, &mut s);
            parts.push(s);
        }
        parts.join(" ")
    }

    // ---- Outline ----

    fn outline(&self) -> Result<String> {
        let mut out = String::new();
        let imported = self.functions.iter().filter(|f| f.import.is_some()).count();
        let _ = writeln!(out, "=== Module ===");
        line(&mut out, "Functions", format!("{} defined, {imported} imported", self.functions.len() - imported));
        let code_size: usize = self
            .functions
            .iter()
            .filter_map(|f| f.body.as_ref())
            .map(|b| (b.range().end - b.range().start) as usize)
            .sum();
        line(&mut out, "Code size", format!("{code_size} bytes"));
        if let Some(start) = self.start {
            line(&mut out, "Start function", self.function_name(start));
        }
        let mut producers = self.producers.clone();
        producers.sort();
        for p in producers {
            line(&mut out, "Producer", p);
        }
        for n in &self.notes {
            let _ = writeln!(out, "! {n}");
        }

        let mut imports: Vec<String> = self
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let (module, name) = f.import?;
                let named = self.function_name(i as u32);
                let alias = if named == format!("{module}.{name}") { String::new() } else { format!(" as {named}") };
                Some(format!("{module}.{name}: func{} {}", alias, self.type_text(f.type_index)))
            })
            .chain(self.imports.iter().cloned())
            .collect();
        imports.sort();
        let _ = writeln!(out, "\n=== Imports ({}) ===", imports.len());
        for i in imports {
            let _ = writeln!(out, "{i}");
        }

        let mut exports: Vec<String> = self
            .exports
            .iter()
            .map(|&(name, kind, index)| {
                let target = match kind {
                    ExternalKind::Func | ExternalKind::FuncExact => {
                        let f = self.function_name(index);
                        let ty = self.functions.get(index as usize).map(|f| self.type_text(f.type_index));
                        let alias = if f == name { String::new() } else { format!(" = {f}") };
                        format!("func{alias} {}", ty.unwrap_or_default())
                    }
                    ExternalKind::Global => format!("global {}", self.global_name(index)),
                    ExternalKind::Memory => format!("memory {index}"),
                    ExternalKind::Table => format!("table {index}"),
                    ExternalKind::Tag => format!("tag {index}"),
                };
                format!("{name}: {}", target.trim_end())
            })
            .collect();
        exports.sort();
        let _ = writeln!(out, "\n=== Exports ({}) ===", exports.len());
        for e in exports {
            let _ = writeln!(out, "{e}");
        }

        if !self.memories.is_empty() || !self.tables.is_empty() {
            let _ = writeln!(out, "\n=== Memories and tables ===");
            for (i, mem) in self.memories.iter().enumerate() {
                let _ = writeln!(out, "memory {i}: {mem}");
            }
            for (i, t) in self.tables.iter().enumerate() {
                let _ = writeln!(out, "table {i}: {t}");
            }
        }

        if !self.globals.is_empty() {
            let _ = writeln!(out, "\n=== Globals ({}) ===", self.globals.len());
            for (i, (ty, init)) in self.globals.iter().enumerate() {
                let init =
                    init.as_ref().map_or_else(|| " (imported)".to_string(), |e| format!(" = {}", self.const_expr(e)));
                let _ = writeln!(out, "global {}: {ty}{init}", self.global_name(i as u32));
            }
        }

        if !self.data_segments.is_empty() {
            let total: usize = self.data_segments.iter().map(|d| d.data.len()).sum();
            let _ = writeln!(out, "\n=== Data segments ({}, {total} bytes) ===", self.data_segments.len());
            for (i, d) in self.data_segments.iter().enumerate() {
                let name =
                    self.data_names.get(&(i as u32)).map_or_else(|| format!("data {i}"), |n| format!("data {n}"));
                let place = match &d.kind {
                    DataKind::Passive => "passive".to_string(),
                    DataKind::Active { memory_index, offset_expr } => {
                        let mem = if *memory_index == 0 { String::new() } else { format!("memory {memory_index} ") };
                        format!("{mem}at {}", self.const_expr(offset_expr))
                    }
                };
                let _ = writeln!(out, "{name}: {place}, {} bytes, crc32 {:08x}", d.data.len(), crc32fast::hash(d.data));
            }
        }

        if !self.elements.is_empty() {
            let _ = writeln!(out, "\n=== Element segments ({}) ===", self.elements.len());
            for (i, e) in self.elements.iter().enumerate() {
                let _ = writeln!(out, "elem {i}: {}", self.element(e));
            }
        }

        if !self.custom.is_empty() {
            let _ = writeln!(out, "\n=== Custom sections ({}) ===", self.custom.len());
            for (name, size) in &self.custom {
                let _ = writeln!(out, "{name}: {size} bytes");
            }
        }

        let order = self.sorted_functions();
        let _ = writeln!(out, "\n=== Functions ({}) ===", order.len());
        for &i in order.iter().take(MAX_OUTLINE_FUNCTIONS) {
            let f = &self.functions[i as usize];
            let Some(body) = &f.body else { continue };
            let header = self.header(i, f);
            match self.checksum(body) {
                Some((crc, count)) => {
                    let _ = writeln!(out, "{header}  // code {crc:08x}, {count} instructions");
                }
                None => {
                    let _ = writeln!(out, "{header}\n! the code could not be read");
                }
            }
        }
        if order.len() > MAX_OUTLINE_FUNCTIONS {
            let _ = writeln!(out, "… {} more functions", order.len() - MAX_OUTLINE_FUNCTIONS);
        }
        Ok(out)
    }

    fn element(&self, e: &wasmparser::Element) -> String {
        let place = match &e.kind {
            ElementKind::Passive => "passive".to_string(),
            ElementKind::Declared => "declared".to_string(),
            ElementKind::Active { table_index, offset_expr } => {
                let table = table_index.filter(|&t| t != 0).map(|t| format!("table {t} ")).unwrap_or_default();
                format!("{table}at {}", self.const_expr(offset_expr))
            }
        };
        let (count, items): (u32, Vec<String>) = match &e.items {
            ElementItems::Functions(r) => (
                r.count(),
                r.clone()
                    .into_iter()
                    .take(MAX_ELEMENT_ITEMS)
                    .flatten()
                    .map(|f| self.function_name(f).into_owned())
                    .collect(),
            ),
            ElementItems::Expressions(_, r) => (
                r.count(),
                r.clone().into_iter().take(MAX_ELEMENT_ITEMS).flatten().map(|e| self.const_expr(&e)).collect(),
            ),
        };
        let more = if count as usize > items.len() {
            format!(" … {} more", count as usize - items.len())
        } else {
            String::new()
        };
        format!("{place}, {count} items: {}{more}", items.join(" "))
    }

    /// Defined functions, named ones by name, then unnamed ones by index.
    fn sorted_functions(&self) -> Vec<u32> {
        let mut order: Vec<(bool, String, u32)> = self
            .functions
            .iter()
            .enumerate()
            .filter(|(_, f)| f.body.is_some())
            .map(|(i, _)| {
                let named = self.named[i];
                let i = i as u32;
                (!named, if named { self.function_name(i).into_owned() } else { String::new() }, i)
            })
            .collect();
        order.sort();
        order.into_iter().map(|(_, _, i)| i).collect()
    }

    /// `func name (param i32 i32) (result i32)`.
    fn header(&self, index: u32, f: &Function) -> String {
        format!("func {} {}", self.function_name(index), self.type_text(f.type_index)).trim_end().to_string()
    }

    /// A checksum of a function's code and its instruction count. Hashes the raw
    /// bytes of each instruction, except that references to functions, globals and
    /// types are hashed by name or signature, so renumbering does not change it.
    fn checksum(&self, body: &FunctionBody) -> Option<(u32, usize)> {
        let mut hasher = crc32fast::Hasher::new();
        let mut reader = body.get_operators_reader().ok()?;
        let start = body.range().start as usize;
        hasher.update(self.data.get(start..reader.original_position() as usize)?);
        let mut count = 0;
        while !reader.eof() {
            let at = reader.original_position() as usize;
            let op = reader.read().ok()?;
            count += 1;
            match op {
                Operator::Call { function_index }
                | Operator::ReturnCall { function_index }
                | Operator::RefFunc { function_index } => {
                    hasher.update(&self.data[at..at + 1]);
                    hasher.update(self.function_name(function_index).as_bytes());
                }
                Operator::GlobalGet { global_index } | Operator::GlobalSet { global_index } => {
                    hasher.update(&self.data[at..at + 1]);
                    hasher.update(self.global_name(global_index).as_bytes());
                }
                Operator::CallIndirect { type_index, table_index }
                | Operator::ReturnCallIndirect { type_index, table_index } => {
                    hasher.update(&self.data[at..at + 1]);
                    hasher.update(self.type_text(type_index).as_bytes());
                    hasher.update(&table_index.to_le_bytes());
                }
                Operator::Block { blockty: BlockType::FuncType(t) }
                | Operator::Loop { blockty: BlockType::FuncType(t) }
                | Operator::If { blockty: BlockType::FuncType(t) } => {
                    hasher.update(&self.data[at..at + 1]);
                    hasher.update(self.type_text(t).as_bytes());
                }
                _ => hasher.update(self.data.get(at..reader.original_position() as usize)?),
            }
        }
        Some((hasher.finalize(), count))
    }

    // ---- Text ----

    fn text(&self) -> Result<String> {
        let mut out = String::new();
        let order = self.sorted_functions();
        let mut printed = 0;
        for (n, &i) in order.iter().enumerate() {
            if printed >= MAX_TEXT_INSTRUCTIONS {
                let _ = writeln!(out, "… {} more functions not shown", order.len() - n);
                break;
            }
            let f = &self.functions[i as usize];
            let Some(body) = &f.body else { continue };
            let _ = writeln!(out, "=== {} ===", self.header(i, f));
            let names = Names::new(self, self.local_names.get(&i));
            match self.body_text(f.type_index, body, &names, &mut out) {
                Ok(count) => printed += count,
                Err(e) => {
                    let _ = writeln!(out, "! the code could not be read: {e}");
                }
            }
            out.push('\n');
        }
        if order.is_empty() {
            out.push_str("(no function has code)\n");
        }
        Ok(out)
    }

    fn body_text(
        &self,
        type_index: u32,
        body: &FunctionBody,
        names: &Names,
        out: &mut String,
    ) -> wasmparser::Result<usize> {
        let mut locals = Vec::new();
        // Locals are numbered after the parameters.
        let mut index = self.param_counts.get(type_index as usize).copied().unwrap_or(0);
        for local in body.get_locals_reader()? {
            let (count, ty) = local?;
            for _ in 0..count.min(1000) {
                locals.push(match names.local(index) {
                    Some(name) => format!("${name} {ty}"),
                    None => ty.to_string(),
                });
                index += 1;
            }
            if count > 1000 {
                locals.push(format!("… {} more {ty}", count - 1000));
                index = index.saturating_add(count - 1000);
            }
        }
        if !locals.is_empty() {
            let _ = writeln!(out, "  (local {})", locals.join(" "));
        }
        let mut reader: OperatorsReader = body.get_operators_reader()?;
        let mut depth = 1usize;
        let mut count = 0;
        while !reader.eof() {
            let op = reader.read()?;
            count += 1;
            match op {
                Operator::End if reader.eof() => break,
                Operator::End
                | Operator::Else
                | Operator::Catch { .. }
                | Operator::CatchAll
                | Operator::Delegate { .. } => depth = depth.saturating_sub(1).max(1),
                _ => {}
            }
            let _ = write!(out, "{:width$}", "", width = 2 * depth.min(40));
            names.instruction(&op, out);
            out.push('\n');
            if matches!(
                op,
                Operator::Block { .. }
                    | Operator::Loop { .. }
                    | Operator::If { .. }
                    | Operator::Else
                    | Operator::Try { .. }
                    | Operator::TryTable { .. }
                    | Operator::Catch { .. }
                    | Operator::CatchAll
            ) {
                depth += 1;
            }
        }
        Ok(count)
    }
}

/// Resolves indexes to names while printing instructions.
struct Names<'m, 'a> {
    module: &'m Module<'a>,
    locals: Option<&'m HashMap<u32, &'a str>>,
}

impl<'m, 'a> Names<'m, 'a> {
    fn new(module: &'m Module<'a>, locals: Option<&'m HashMap<u32, &'a str>>) -> Self {
        Names { module, locals }
    }

    fn local(&self, index: u32) -> Option<&str> {
        self.locals.and_then(|l| l.get(&index)).copied()
    }

    fn block_type(&self, ty: BlockType) -> String {
        match ty {
            BlockType::Empty => String::new(),
            BlockType::Type(t) => format!(" (result {t})"),
            BlockType::FuncType(t) => format!(" {}", self.module.type_text(t)),
        }
    }

    fn instruction(&self, op: &Operator, out: &mut String) {
        let m = self.module;
        let _ = match op {
            Operator::Block { blockty } => write!(out, "block{}", self.block_type(*blockty)),
            Operator::Loop { blockty } => write!(out, "loop{}", self.block_type(*blockty)),
            Operator::If { blockty } => write!(out, "if{}", self.block_type(*blockty)),
            Operator::Br { relative_depth } => write!(out, "br {relative_depth}"),
            Operator::BrIf { relative_depth } => write!(out, "br_if {relative_depth}"),
            Operator::BrTable { targets } => {
                let list: Vec<String> = targets.targets().flatten().map(|t| t.to_string()).collect();
                write!(out, "br_table {} default {}", list.join(" "), targets.default())
            }
            Operator::Call { function_index } => write!(out, "call {}", m.function_name(*function_index)),
            Operator::ReturnCall { function_index } => write!(out, "return_call {}", m.function_name(*function_index)),
            Operator::RefFunc { function_index } => write!(out, "ref.func {}", m.function_name(*function_index)),
            Operator::CallIndirect { type_index, table_index } => {
                let table = if *table_index == 0 { String::new() } else { format!(" table {table_index}") };
                write!(out, "call_indirect{table} {}", m.type_text(*type_index))
            }
            Operator::ReturnCallIndirect { type_index, table_index } => {
                let table = if *table_index == 0 { String::new() } else { format!(" table {table_index}") };
                write!(out, "return_call_indirect{table} {}", m.type_text(*type_index))
            }
            Operator::LocalGet { local_index } => write!(out, "local.get {}", self.local_text(*local_index)),
            Operator::LocalSet { local_index } => write!(out, "local.set {}", self.local_text(*local_index)),
            Operator::LocalTee { local_index } => write!(out, "local.tee {}", self.local_text(*local_index)),
            Operator::GlobalGet { global_index } => write!(out, "global.get {}", m.global_name(*global_index)),
            Operator::GlobalSet { global_index } => write!(out, "global.set {}", m.global_name(*global_index)),
            Operator::I32Const { value } => write!(out, "i32.const {value}"),
            Operator::I64Const { value } => write!(out, "i64.const {value}"),
            Operator::F32Const { value } => write!(out, "f32.const {:?}", f32::from_bits(value.bits())),
            Operator::F64Const { value } => write!(out, "f64.const {:?}", f64::from_bits(value.bits())),
            Operator::MemorySize { mem: 0 } => write!(out, "memory.size"),
            Operator::MemoryGrow { mem: 0 } => write!(out, "memory.grow"),
            Operator::MemoryFill { mem: 0 } => write!(out, "memory.fill"),
            Operator::MemoryCopy { dst_mem: 0, src_mem: 0 } => write!(out, "memory.copy"),
            Operator::TypedSelect { ty } => write!(out, "select (result {ty})"),
            op => {
                generic(op, out);
                Ok(())
            }
        };
    }

    fn local_text(&self, index: u32) -> String {
        self.local(index).map_or_else(|| index.to_string(), |n| format!("${n}"))
    }
}

/// Prints an instruction from its debug form: the variant name as a WAT mnemonic
/// (`I32Load8U` → `i32.load8_u`), then its operands, memory arguments as WAT writes
/// them (`offset=8`, `align=1` only when not the natural alignment).
fn generic(op: &Operator, out: &mut String) {
    let debug = format!("{op:?}");
    let (variant, fields) = match debug.split_once(" { ") {
        Some((v, f)) => (v, f.trim_end_matches(" }")),
        None => (debug.as_str(), ""),
    };
    out.push_str(&mnemonic(variant));
    if let Some(memarg) = fields.split_once("memarg: MemArg { ").map(|(_, m)| m) {
        let field = |name: &str| {
            memarg
                .split(", ")
                .find_map(|kv| kv.trim_end_matches(" }").strip_prefix(name)?.strip_prefix(": "))
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        let (align, natural, offset, memory) = (field("align"), field("max_align"), field("offset"), field("memory"));
        if memory != 0 {
            let _ = write!(out, " {memory}");
        }
        if offset != 0 {
            let _ = write!(out, " offset={offset}");
        }
        if align != natural {
            let _ = write!(out, " align={}", 1u64 << align.min(63));
        }
        if let Some(lane) = fields.split(", ").find_map(|kv| kv.strip_prefix("lane: ")) {
            let _ = write!(out, " {lane}");
        }
        return;
    }
    if !fields.is_empty() {
        for kv in fields.split(", ") {
            let value = kv.split_once(": ").map_or(kv, |(_, v)| v);
            let _ = write!(out, " {value}");
        }
    }
}

/// `I32TruncSatF32S` → `i32.trunc_sat_f32_s`, `LocalGet` → `local.get`, `BrTable` → `br_table`.
fn mnemonic(variant: &str) -> String {
    const PREFIXES: &[&str] = &[
        "I32", "I64", "F32", "F64", "V128", "I8x16", "I16x8", "I32x4", "I64x2", "F32x4", "F64x2", "Local", "Global",
        "Memory", "Table", "Ref", "Data", "Elem", "Atomic", "Struct", "Array", "Any", "Extern", "I31",
    ];
    let mut words: Vec<String> = Vec::new();
    for c in variant.chars() {
        if c.is_ascii_uppercase() || words.is_empty() {
            words.push(String::new());
        }
        if let Some(w) = words.last_mut() {
            w.push(c);
        }
    }
    match words.split_first() {
        Some((first, rest)) if PREFIXES.contains(&first.as_str()) && !rest.is_empty() => {
            format!("{}.{}", first.to_lowercase(), rest.join("_").to_lowercase())
        }
        _ => words.join("_").to_lowercase(),
    }
}

/// Rust symbol names (legacy and v0 mangling) as Rust writes them, without the hash.
fn demangle(name: &str) -> String {
    match rustc_demangle::try_demangle(name) {
        Ok(d) => format!("{d:#}"),
        Err(_) => name.to_string(),
    }
}

fn signature(params: &[ValType], results: &[ValType]) -> String {
    let list = |l: &[ValType]| l.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ");
    let mut out = Vec::new();
    if !params.is_empty() {
        out.push(format!("(param {})", list(params)));
    }
    if !results.is_empty() {
        out.push(format!("(result {})", list(results)));
    }
    out.join(" ")
}

fn limits(initial: u64, maximum: Option<u64>) -> String {
    match maximum {
        Some(max) => format!("{initial}..{max}"),
        None => format!("{initial}.."),
    }
}

fn memory_type(m: &wasmparser::MemoryType) -> String {
    let mut out = format!("{} pages", limits(m.initial, m.maximum));
    if m.memory64 {
        out.push_str(", 64-bit");
    }
    if m.shared {
        out.push_str(", shared");
    }
    out
}

fn table_type(t: &wasmparser::TableType) -> String {
    format!("{} {}", t.element_type, limits(t.initial, t.maximum))
}

fn global_type(g: &wasmparser::GlobalType) -> String {
    if g.mutable { format!("mut {}", g.content_type) } else { g.content_type.to_string() }
}

fn line(out: &mut String, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/wasm/", $name))
        };
    }
    const V1: &[u8] = fixture!("counter-v1.wasm");
    const V1_STRIPPED: &[u8] = fixture!("counter-v1-stripped.wasm");
    const V1_DEBUG: &[u8] = fixture!("counter-v1-debug.wasm");
    const V2: &[u8] = fixture!("counter-v2.wasm");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn outline_of_a_module() {
        let o = outline(V1).unwrap();
        for expected in [
            "Functions:         7 defined, 1 imported",
            "env.host_log: func as counter::host_log (param i32)",
            "increment: func (param i32) (result i32)",
            "memory: memory 0",
            "global __stack_pointer: mut i32 = i32.const 1048576",
            "data .rodata: at i32.const 1048576, 29 bytes, crc32 20b49bcf",
            "elem 0: at i32.const 1, 2 items: counter::square counter::double",
            "name: ",
            "func sum_to (param i32) (result i64)  // code ",
        ] {
            assert!(o.contains(expected), "{expected}\n---\n{o}");
        }
    }

    #[test]
    fn text_uses_names() {
        let t = text(V1).unwrap();
        for expected in [
            "=== func increment (param i32) (result i32) ===\n  i32.const 0\n  i32.const 0\n  i32.load offset=",
            "  call counter::host_log\n",
            "  global.get __stack_pointer\n",
            "  call_indirect (param i32) (result i32)\n",
            "  block\n    local.get 0\n",
            "  i64.extend_i32_u\n",
        ] {
            assert!(t.contains(expected), "{expected}\n---\n{t}");
        }
        // Without the name section, exported functions keep their export names.
        let stripped = text(V1_STRIPPED).unwrap();
        assert!(stripped.contains("=== func increment (param i32) (result i32) ==="), "{stripped}");
        assert!(stripped.contains("  call env.host_log\n"), "{stripped}");
    }

    #[test]
    fn a_diff_shows_only_what_changed() {
        let (o1, o2) = (outline(V1).unwrap(), outline(V2).unwrap());
        let strip = |l: &str| l.split("  // code").next().unwrap().to_string();
        let added: Vec<String> = lines_only_in(&o2, &o1).into_iter().map(strip).collect();
        assert!(added.contains(&"reset: func".to_string()), "{added:?}");
        assert!(added.contains(&"func sum_to (param i32) (result i64)".to_string()), "{added:?}");
        // Functions whose code is unchanged keep their line, checksum included.
        for unchanged in ["func apply ", "func counter::double ", "func counter::square "] {
            assert!(!added.iter().any(|l| l.starts_with(unchanged)), "{unchanged}: {added:?}");
        }
    }

    #[test]
    fn same_code_ignores_custom_sections() {
        assert_eq!(same_code(V1, V1_STRIPPED), Some(true));
        assert_eq!(same_code(V1, V1_DEBUG), Some(true));
        assert_eq!(same_code(V1, V2), Some(false));
        assert_eq!(same_code(V1, b"\0asm\x01\0\0\0\x01\x7f"), None, "a truncated section");
        assert_eq!(same_code(V1, b"\0asm\x0d\0\x01\0"), None, "a component");
    }

    #[test]
    fn detects_modules_only() {
        assert!(is_wasm(V1));
        assert!(!is_wasm(b"\0asm\x0d\0\x01\0"));
        assert!(!is_wasm(b"\0asm"));
    }

    #[test]
    fn truncated_or_garbage_input_never_panics() {
        for data in [V1, V1_DEBUG] {
            for len in 0..data.len() {
                let _ = outline(&data[..len]);
                let _ = text(&data[..len]);
                let _ = same_code(&data[..len], data);
            }
        }
        let mut corrupt = V1.to_vec();
        for i in (8..corrupt.len()).step_by(7) {
            corrupt[i] ^= 0xFF;
            let _ = outline(&corrupt);
            let _ = text(&corrupt);
        }
    }
}
