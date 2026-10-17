//! Debug symbols of ELF files from their DWARF debug info (`.debug_info`).
//!
//! The executable summary lists the symbol table: names and addresses. The DWARF
//! data says more about the same objects: the type of every global and static
//! variable (structs, arrays, pointers and typedefs resolved), statics inside
//! functions, and the address range of every function, also when the symbol table
//! is stripped. For firmware this is the list of RAM variables a calibration or
//! measurement tool works with, so it is worth comparing between builds: a changed
//! type or size shows on the variable's line, and a rebuild that moved things shows
//! as changed addresses on lines that stay aligned (sorted by name).
//!
//! Source lines are left out: they change with every edit above a function and
//! would make every line differ. Units are read in parallel; types are resolved
//! once per DIE and cached; the output is capped for very large programs.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;

use gimli::{AttributeValue, DebuggingInformationEntry, EndianSlice, Operation, RunTimeEndian, UnitOffset};
use object::{Object, ObjectSection};
use rayon::prelude::*;

use crate::{Error, Result};

/// Lines shown at most per section.
const MAX_ITEMS: usize = 200_000;
/// Types nest deeper than this only in hostile files.
const MAX_TYPE_DEPTH: usize = 16;

type R<'a> = EndianSlice<'a, RunTimeEndian>;
type Dwarf<'a> = gimli::Dwarf<R<'a>>;
type Unit<'a> = gimli::Unit<R<'a>>;
type Die<'a> = DebuggingInformationEntry<R<'a>>;

/// Whether an ELF file has DWARF debug info.
pub fn has_debug_info(data: &[u8]) -> bool {
    if !data.starts_with(b"\x7FELF") {
        return false;
    }
    object::File::parse(data).is_ok_and(|f| f.section_by_name(".debug_info").is_some_and(|s| s.size() > 0))
}

fn bad(e: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("DWARF: {e}"))
}

struct Variable {
    name: String,
    /// Compile unit, for statics.
    file: Option<String>,
    ty: String,
    size: Option<u64>,
    address: u64,
    thread_local: bool,
}

struct Function {
    name: String,
    file: Option<String>,
    low: u64,
    size: u64,
}

#[derive(Default)]
struct UnitResult {
    producer: Option<String>,
    variables: Vec<Variable>,
    functions: Vec<Function>,
}

/// The "Debug symbols" view.
pub fn debug_symbols(data: &[u8]) -> Result<String> {
    let file = object::File::parse(data).map_err(bad)?;
    let endian = if file.is_little_endian() { RunTimeEndian::Little } else { RunTimeEndian::Big };
    // The sections, decompressed where needed, outlive the borrowed DWARF readers.
    let ids = [
        gimli::SectionId::DebugAbbrev,
        gimli::SectionId::DebugAddr,
        gimli::SectionId::DebugInfo,
        // Units read their line program header, though the view does not use lines.
        gimli::SectionId::DebugLine,
        gimli::SectionId::DebugLineStr,
        gimli::SectionId::DebugLoc,
        gimli::SectionId::DebugLocLists,
        gimli::SectionId::DebugRanges,
        gimli::SectionId::DebugRngLists,
        gimli::SectionId::DebugStr,
        gimli::SectionId::DebugStrOffsets,
    ];
    let mut sections: HashMap<gimli::SectionId, Cow<[u8]>> = HashMap::new();
    for id in ids {
        if let Some(s) = file.section_by_name(id.name()) {
            sections.insert(id, s.uncompressed_data().map_err(bad)?);
        }
    }
    if sections.get(&gimli::SectionId::DebugInfo).is_none_or(|s| s.is_empty()) {
        return Err(Error::Unsupported("no DWARF debug info".into()));
    }
    let dwarf: Dwarf = gimli::Dwarf::load(|id| -> std::result::Result<R, Error> {
        Ok(EndianSlice::new(sections.get(&id).map_or(&[][..], |c| c.as_ref()), endian))
    })?;

    let mut headers = Vec::new();
    let mut iter = dwarf.units();
    while let Ok(Some(h)) = iter.next() {
        headers.push(h);
    }
    let units: Vec<UnitResult> = headers
        .into_par_iter()
        .map(|h| match dwarf.unit(h) {
            Ok(unit) => read_unit(&dwarf, &unit),
            Err(_) => UnitResult::default(),
        })
        .collect();

    let mut producers: Vec<String> = units.iter().filter_map(|u| u.producer.clone()).collect();
    producers.sort();
    producers.dedup();
    let unit_count = units.len();
    let mut variables: Vec<Variable> = Vec::new();
    let mut functions: Vec<Function> = Vec::new();
    for u in units {
        variables.extend(u.variables);
        functions.extend(u.functions);
    }
    variables.par_sort_unstable_by(|a, b| (&a.name, &a.file, a.address).cmp(&(&b.name, &b.file, b.address)));
    variables.dedup_by(|a, b| a.name == b.name && a.file == b.file && a.address == b.address);
    functions.par_sort_unstable_by(|a, b| (&a.name, &a.file, a.low).cmp(&(&b.name, &b.file, b.low)));
    functions.dedup_by(|a, b| a.name == b.name && a.file == b.file && a.low == b.low);

    let mut out = String::with_capacity(80 * (variables.len() + functions.len()).min(2 * MAX_ITEMS) + 1024);
    line(&mut out, "Compile units", unit_count);
    for p in producers.iter().take(20) {
        line(&mut out, "Producer", p);
    }
    if producers.len() > 20 {
        line(&mut out, "Producer", format!("… {} more", producers.len() - 20));
    }
    line(&mut out, "Variables", variables.len());
    line(&mut out, "Functions", functions.len());

    let _ = writeln!(out, "\n=== Variables ({}) ===", variables.len());
    for v in variables.iter().take(MAX_ITEMS) {
        out.push_str(&v.name);
        if let Some(f) = &v.file {
            let _ = write!(out, " ({f})");
        }
        let _ = write!(out, ": {}", v.ty);
        if let Some(n) = v.size {
            let _ = write!(out, ", {n} {}", if n == 1 { "byte" } else { "bytes" });
        }
        if v.thread_local {
            let _ = writeln!(out, ", thread-local offset 0x{:x}", v.address);
        } else {
            let _ = writeln!(out, ", 0x{:08x}", v.address);
        }
    }
    if variables.len() > MAX_ITEMS {
        let _ = writeln!(out, "! {} more variables are not shown", variables.len() - MAX_ITEMS);
    }

    let _ = writeln!(out, "\n=== Functions ({}) ===", functions.len());
    for f in functions.iter().take(MAX_ITEMS) {
        out.push_str(&f.name);
        if let Some(file) = &f.file {
            let _ = write!(out, " ({file})");
        }
        let _ = writeln!(
            out,
            ": {} {}, 0x{:08x}..0x{:08x}",
            f.size,
            if f.size == 1 { "byte" } else { "bytes" },
            f.low,
            f.low + f.size
        );
    }
    if functions.len() > MAX_ITEMS {
        let _ = writeln!(out, "! {} more functions are not shown", functions.len() - MAX_ITEMS);
    }
    Ok(out)
}

/// Reads a compile unit: its variables with a fixed address and its functions.
fn read_unit(dwarf: &Dwarf, unit: &Unit) -> UnitResult {
    let mut result = UnitResult::default();
    let file = unit.name.map(|n| String::from_utf8_lossy(n.slice()).into_owned());
    let mut types = Types { dwarf, unit, cache: HashMap::new() };
    // Names of the enclosing namespaces, classes and functions, with their depth.
    let mut scopes: Vec<(isize, Option<String>, gimli::DwTag)> = Vec::new();
    let mut entries = unit.entries();
    while let Ok(Some(entry)) = entries.next_dfs() {
        let depth = entry.depth();
        while scopes.last().is_some_and(|s| s.0 >= depth) {
            scopes.pop();
        }
        let tag = entry.tag();
        match tag {
            gimli::DW_TAG_compile_unit | gimli::DW_TAG_partial_unit => {
                result.producer = attr_str(dwarf, unit, entry, gimli::DW_AT_producer);
                continue;
            }
            gimli::DW_TAG_namespace
            | gimli::DW_TAG_structure_type
            | gimli::DW_TAG_class_type
            | gimli::DW_TAG_union_type
            | gimli::DW_TAG_lexical_block => {
                let name = attr_str(dwarf, unit, entry, gimli::DW_AT_name);
                if entry.has_children() {
                    scopes.push((depth, name, tag));
                }
            }
            gimli::DW_TAG_subprogram => {
                let name = types.name_of(entry, 0);
                if let Some((low, size)) = pc_range(dwarf, unit, entry) {
                    let full = qualified(&scopes, name.as_deref().unwrap_or("<anonymous>"));
                    let external = flag(types.origin_attr(entry, gimli::DW_AT_external));
                    result.functions.push(Function {
                        name: full,
                        file: (!external).then(|| file.clone()).flatten(),
                        low,
                        size,
                    });
                }
                if entry.has_children() {
                    scopes.push((depth, name, tag));
                }
            }
            gimli::DW_TAG_variable => {
                if flag(entry.attr_value(gimli::DW_AT_declaration)) {
                    continue;
                }
                let Some((address, thread_local)) = location(dwarf, unit, entry) else { continue };
                // Variables the linker dropped keep their DWARF with address 0.
                if address == 0 && !thread_local {
                    continue;
                }
                let name = types.name_of(entry, 0).unwrap_or_else(|| "<anonymous>".into());
                let in_function = scopes.iter().any(|s| s.2 == gimli::DW_TAG_subprogram);
                let external = flag(types.origin_attr(entry, gimli::DW_AT_external));
                let type_ref = types.origin_attr(entry, gimli::DW_AT_type);
                let (ty, size) = type_ref.map_or(("?".to_string(), None), |t| types.resolve(t, 0));
                result.variables.push(Variable {
                    name: qualified(&scopes, &name),
                    file: (!external || in_function).then(|| file.clone()).flatten(),
                    ty,
                    size,
                    address,
                    thread_local,
                });
            }
            _ => {}
        }
    }
    result
}

fn qualified(scopes: &[(isize, Option<String>, gimli::DwTag)], name: &str) -> String {
    let mut s = String::new();
    for (_, n, tag) in scopes {
        if *tag == gimli::DW_TAG_lexical_block {
            continue;
        }
        s.push_str(n.as_deref().unwrap_or("<anonymous>"));
        s.push_str("::");
    }
    s.push_str(name);
    s
}

fn flag(v: Option<AttributeValue<R>>) -> bool {
    matches!(v, Some(AttributeValue::Flag(true)))
}

fn attr_str(dwarf: &Dwarf, unit: &Unit, entry: &Die, at: gimli::DwAt) -> Option<String> {
    let v = entry.attr_value(at)?;
    let s = dwarf.attr_string(unit, v).ok()?;
    Some(String::from_utf8_lossy(s.slice()).into_owned())
}

/// The fixed address of a variable: `DW_OP_addr` or `DW_OP_addrx`, optionally
/// followed by a thread-local marker.
fn location(dwarf: &Dwarf, unit: &Unit, entry: &Die) -> Option<(u64, bool)> {
    let Some(AttributeValue::Exprloc(expr)) = entry.attr_value(gimli::DW_AT_location) else { return None };
    let mut ops = expr.operations(unit.encoding());
    let address = match ops.next().ok()?? {
        Operation::Address { address } => address,
        Operation::AddressIndex { index } | Operation::ConstantIndex { index } => dwarf.address(unit, index).ok()?,
        Operation::UnsignedConstant { value } => value,
        _ => return None,
    };
    let thread_local = matches!(ops.next(), Ok(Some(Operation::TLS)));
    Some((address, thread_local))
}

/// The address range of a function: `low_pc`/`high_pc`, or the hull of its ranges.
fn pc_range<'a>(dwarf: &Dwarf<'a>, unit: &Unit<'a>, entry: &Die<'a>) -> Option<(u64, u64)> {
    let low = match entry.attr_value(gimli::DW_AT_low_pc) {
        Some(AttributeValue::Addr(a)) => Some(a),
        Some(AttributeValue::DebugAddrIndex(i)) => dwarf.address(unit, i).ok(),
        _ => None,
    };
    if let Some(low) = low {
        let high = match entry.attr_value(gimli::DW_AT_high_pc)? {
            AttributeValue::Addr(a) => a,
            AttributeValue::DebugAddrIndex(i) => dwarf.address(unit, i).ok()?,
            v => low.checked_add(v.udata_value()?)?,
        };
        // Functions removed by the linker keep their DWARF with address 0.
        return (low != 0 && high > low).then_some((low, high - low));
    }
    let mut ranges = dwarf.die_ranges(unit, entry).ok()?;
    let (mut lo, mut size) = (u64::MAX, 0u64);
    while let Ok(Some(r)) = ranges.next() {
        if r.begin != 0 && r.end > r.begin {
            lo = lo.min(r.begin);
            size += r.end - r.begin;
        }
    }
    (lo != u64::MAX).then_some((lo, size))
}

/// Type names of one unit, cached per DIE.
struct Types<'d, 'a> {
    dwarf: &'d Dwarf<'a>,
    unit: &'d Unit<'a>,
    cache: HashMap<UnitOffset, (String, Option<u64>)>,
}

impl<'a> Types<'_, 'a> {
    fn entry(&self, offset: UnitOffset) -> Option<Die<'a>> {
        self.unit.entry(offset).ok()
    }

    /// An attribute of the entry, or of its declaration (`DW_AT_specification`) or
    /// abstract instance (`DW_AT_abstract_origin`).
    fn origin_attr(&self, entry: &Die<'a>, at: gimli::DwAt) -> Option<AttributeValue<R<'a>>> {
        if let Some(v) = entry.attr_value(at) {
            return Some(v);
        }
        let mut current = self.origin(entry)?;
        for _ in 0..4 {
            if let Some(v) = current.attr_value(at) {
                return Some(v);
            }
            current = self.origin(&current)?;
        }
        None
    }

    fn origin(&self, entry: &Die<'a>) -> Option<Die<'a>> {
        let r =
            entry.attr_value(gimli::DW_AT_specification).or_else(|| entry.attr_value(gimli::DW_AT_abstract_origin))?;
        match r {
            AttributeValue::UnitRef(o) => self.entry(o),
            _ => None,
        }
    }

    fn name_of(&self, entry: &Die<'a>, _depth: usize) -> Option<String> {
        let v = self.origin_attr(entry, gimli::DW_AT_name)?;
        let s = self.dwarf.attr_string(self.unit, v).ok()?;
        Some(String::from_utf8_lossy(s.slice()).into_owned())
    }

    /// The name and size of the type an attribute refers to.
    fn resolve(&mut self, type_ref: AttributeValue<R<'a>>, depth: usize) -> (String, Option<u64>) {
        match type_ref {
            AttributeValue::UnitRef(o) => self.type_at(o, depth),
            // A type in another unit (LTO, type units): not resolved, to keep units independent.
            AttributeValue::DebugInfoRef(_) | AttributeValue::DebugTypesRef(_) => {
                ("<type in another unit>".into(), None)
            }
            _ => ("?".into(), None),
        }
    }

    fn type_at(&mut self, offset: UnitOffset, depth: usize) -> (String, Option<u64>) {
        if let Some(t) = self.cache.get(&offset) {
            return t.clone();
        }
        if depth > MAX_TYPE_DEPTH {
            return ("…".into(), None);
        }
        let t = self.compute(offset, depth).unwrap_or_else(|| ("?".into(), None));
        self.cache.insert(offset, t.clone());
        t
    }

    fn target(&mut self, entry: &Die<'a>, depth: usize) -> (String, Option<u64>) {
        match entry.attr_value(gimli::DW_AT_type) {
            Some(t) => self.resolve(t, depth + 1),
            None => ("void".into(), None),
        }
    }

    fn compute(&mut self, offset: UnitOffset, depth: usize) -> Option<(String, Option<u64>)> {
        let entry = self.entry(offset)?;
        let name = self.name_of(&entry, depth);
        let byte_size = entry.attr_value(gimli::DW_AT_byte_size).and_then(|v| v.udata_value());
        let pointer_size = u64::from(self.unit.encoding().address_size);
        Some(match entry.tag() {
            gimli::DW_TAG_base_type | gimli::DW_TAG_unspecified_type => (name.unwrap_or_else(|| "?".into()), byte_size),
            gimli::DW_TAG_typedef => {
                let (target, size) = self.target(&entry, depth);
                let name = name.unwrap_or_else(|| "?".into());
                // `uint32_t = unsigned int`; long struct names stay behind the typedef.
                let resolved = target.rsplit(" = ").next().unwrap_or(&target);
                (if resolved == name { name } else { format!("{name} = {resolved}") }, size)
            }
            gimli::DW_TAG_structure_type => (format!("struct {}", name.as_deref().unwrap_or("<anonymous>")), byte_size),
            gimli::DW_TAG_class_type => (format!("class {}", name.as_deref().unwrap_or("<anonymous>")), byte_size),
            gimli::DW_TAG_union_type => (format!("union {}", name.as_deref().unwrap_or("<anonymous>")), byte_size),
            gimli::DW_TAG_enumeration_type => {
                let size = byte_size.or_else(|| self.target(&entry, depth).1);
                (format!("enum {}", name.as_deref().unwrap_or("<anonymous>")), size)
            }
            gimli::DW_TAG_pointer_type | gimli::DW_TAG_reference_type | gimli::DW_TAG_rvalue_reference_type => {
                let (target, _) = self.target(&entry, depth);
                let mark = match entry.tag() {
                    gimli::DW_TAG_pointer_type => "*",
                    gimli::DW_TAG_reference_type => "&",
                    _ => "&&",
                };
                let text = match target.find("(*)") {
                    // A function type: `int (int)` becomes `int (*)(int)`.
                    _ if target.ends_with(')') && !target.contains("(*") => match target.find(" (") {
                        Some(p) => format!("{} ({mark}){}", &target[..p], &target[p + 1..]),
                        None => format!("{target} {mark}"),
                    },
                    _ => format!("{} {mark}", strip_typedef(&target)),
                };
                (text, byte_size.or(Some(pointer_size)))
            }
            gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
            | gimli::DW_TAG_atomic_type => {
                let (target, size) = self.target(&entry, depth);
                let q = match entry.tag() {
                    gimli::DW_TAG_const_type => "const",
                    gimli::DW_TAG_volatile_type => "volatile",
                    gimli::DW_TAG_restrict_type => "restrict",
                    _ => "_Atomic",
                };
                (format!("{q} {target}"), size)
            }
            gimli::DW_TAG_array_type => {
                let (elem, elem_size) = self.target(&entry, depth);
                let mut dims = String::new();
                let mut count_all: Option<u64> = Some(1);
                let mut tree = self.unit.entries_tree(Some(offset)).ok()?;
                let root = tree.root().ok()?;
                let mut children = root.children();
                while let Ok(Some(child)) = children.next() {
                    let e = child.entry();
                    if e.tag() != gimli::DW_TAG_subrange_type {
                        continue;
                    }
                    let count = e.attr_value(gimli::DW_AT_count).and_then(|v| v.udata_value()).or_else(|| {
                        let upper = e.attr_value(gimli::DW_AT_upper_bound)?;
                        let upper =
                            upper.udata_value().or_else(|| upper.sdata_value().and_then(|v| u64::try_from(v).ok()))?;
                        let lower = e.attr_value(gimli::DW_AT_lower_bound).and_then(|v| v.udata_value()).unwrap_or(0);
                        (upper + 1).checked_sub(lower)
                    });
                    match count {
                        Some(n) => {
                            let _ = write!(dims, "[{n}]");
                        }
                        None => dims.push_str("[]"),
                    }
                    count_all = count_all.zip(count).and_then(|(a, b)| a.checked_mul(b));
                }
                if dims.is_empty() {
                    dims.push_str("[]");
                }
                let size = byte_size.or_else(|| elem_size.zip(count_all).and_then(|(a, b)| a.checked_mul(b)));
                (format!("{}{dims}", strip_typedef(&elem)), size)
            }
            gimli::DW_TAG_subroutine_type => {
                let (ret, _) = self.target(&entry, depth);
                let mut params = Vec::new();
                let mut tree = self.unit.entries_tree(Some(offset)).ok()?;
                let root = tree.root().ok()?;
                let mut children = root.children();
                while let Ok(Some(child)) = children.next() {
                    let e = child.entry();
                    match e.tag() {
                        gimli::DW_TAG_formal_parameter => {
                            let p = e
                                .attr_value(gimli::DW_AT_type)
                                .map_or_else(|| "?".into(), |t| self.resolve(t, depth + 1).0);
                            params.push(strip_typedef(&p).to_string());
                        }
                        gimli::DW_TAG_unspecified_parameters => params.push("...".into()),
                        _ => {}
                    }
                }
                let params = if params.is_empty() { "void".to_string() } else { params.join(", ") };
                (format!("{} ({params})", strip_typedef(&ret)), None)
            }
            gimli::DW_TAG_ptr_to_member_type => ("member pointer".into(), byte_size),
            _ => (name.unwrap_or_else(|| "?".into()), byte_size),
        })
    }
}

/// `uint32_t = unsigned int` → `uint32_t`, inside a larger type.
fn strip_typedef(t: &str) -> &str {
    match t.find(" = ") {
        Some(p) if !t[..p].contains(['(', '[']) => &t[..p],
        _ => t,
    }
}

fn line(out: &mut String, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dwarf/sensors"));
    const V2: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dwarf/sensors-v2"));

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn variables_and_functions() {
        let s = debug_symbols(V1).unwrap();
        for expected in [
            "Compile units:     2",
            "Producer:          GNU C",
            "calibration: struct calibration, 24 bytes, 0x",
            "channel_names: const char *[4], 32 bytes, 0x",
            "counter (src/sensors.c): uint32_t = unsigned int, 4 bytes, 0x",
            "filter (src/main.c): int (*)(int), 8 bytes, 0x",
            "history (src/sensors.c): int16_t[8][4], 64 bytes, 0x",
            "read_sensor::calls (src/sensors.c): int, 4 bytes, 0x",
            "mode: enum mode, 4 bytes, 0x",
            "tls_errors: int, 4 bytes, thread-local offset 0x",
            "raw (src/main.c): union raw, 4 bytes, 0x",
            "read_sensor: ",
            "smooth (src/sensors.c): ",
            "main: ",
        ] {
            assert!(s.contains(expected), "{expected}\n---\n{s}");
        }
    }

    #[test]
    fn a_changed_type_shows_on_its_line() {
        let (a, b) = (debug_symbols(V1).unwrap(), debug_symbols(V2).unwrap());
        let added = lines_only_in(&b, &a);
        assert!(
            added.iter().any(|l| l.starts_with("history (src/sensors.c): int16_t[16][4], 128 bytes, 0x")),
            "{added:?}"
        );
        assert!(added.iter().any(|l| l.starts_with("diag_level: ")), "{added:?}");
    }

    #[test]
    fn detection_and_garbage() {
        assert!(has_debug_info(V1));
        assert!(!has_debug_info(b"\x7FELF\x02\x01\x01"));
        assert!(!has_debug_info(b"MZ"));
        for end in (0..V1.len()).step_by(V1.len() / 64) {
            let _ = debug_symbols(&V1[..end]);
        }
        // Corrupt .debug_info contents.
        let mut bytes = V1.to_vec();
        let file = object::File::parse(V1).unwrap();
        let (off, size) = file.section_by_name(".debug_info").unwrap().file_range().unwrap();
        for (i, b) in bytes[off as usize..(off + size) as usize].iter_mut().enumerate() {
            if i % 7 == 3 {
                *b = b.wrapping_mul(31).wrapping_add(7);
            }
        }
        let _ = debug_symbols(&bytes);
    }
}
