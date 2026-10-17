//! .NET assemblies: the API as an outline, and the IL of every method.
//!
//! A managed DLL or EXE is a PE file whose CLI header (data directory 14) points to
//! ECMA-335 metadata: a root (`BSJB`) with streams — `#~` (the tables), `#Strings`,
//! `#US` (user strings), `#GUID` and `#Blob` (signatures, constants, attribute
//! values). This module reads only the tables it needs, straight from the memory map.
//!
//! The views are written for diffing, like the Java class views: metadata tokens are
//! resolved to names (`call Shop.Item.get_Price(): decimal` instead of `0A00000C`),
//! branch targets become labels, short and long instruction forms are folded
//! (`ldarg.s 4` and `ldarg.1` are both `ldarg name`), types and members are sorted,
//! and each method in the outline has a checksum of its IL that hashes the names
//! it refers to rather than table row numbers, so adding a member does not change
//! the checksum of every other method.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use crate::{Error, Result};

/// Lines of the outline before it stops listing types.
const MAX_OUTLINE_LINES: usize = 400_000;
/// Instructions of the IL view before it stops listing methods.
const MAX_IL_INSTRUCTIONS: usize = 400_000;

fn bad(what: &str) -> Error {
    Error::Unsupported(format!(".NET assembly: {what}"))
}

/// Whether the data is a PE file with a CLI header and metadata, that is, a
/// managed (.NET) assembly or module. Native PE files have no CLI header.
pub fn is_managed(data: &[u8]) -> bool {
    Pe::parse(data).and_then(|pe| pe.metadata_root()).is_some()
}

/// Assembly identity, references, attributes, then every type with its members,
/// each method with a checksum of its IL.
pub fn outline(data: &[u8]) -> Result<String> {
    Ok(Assembly::parse(data)?.outline())
}

/// The IL of every method, tokens resolved to names.
pub fn il(data: &[u8]) -> Result<String> {
    Ok(Assembly::parse(data)?.il())
}

/// Whether two assemblies have the same metadata, IL, field data and resources,
/// and differ only in build identity: module version id (MVID), PE timestamp,
/// debug directory (PDB path and id), and where in the file method bodies sit.
/// `None` when either is not a readable assembly or the check cannot be exact.
pub fn same_code(left: &[u8], right: &[u8]) -> Option<bool> {
    Some(canonical(left)? == canonical(right)?)
}

// ---- PE ----

struct Section {
    va: u32,
    vsize: u32,
    raw_ptr: u32,
    raw_size: u32,
}

struct Pe<'a> {
    data: &'a [u8],
    machine: u16,
    characteristics: u16,
    sections: Vec<Section>,
    dirs: Vec<(u32, u32)>,
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

impl<'a> Pe<'a> {
    fn parse(data: &'a [u8]) -> Option<Self> {
        if !data.starts_with(b"MZ") {
            return None;
        }
        let pe = u32_at(data, 0x3C)? as usize;
        if data.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let coff = pe + 4;
        let machine = u16_at(data, coff)?;
        let nsections = u16_at(data, coff + 2)? as usize;
        let opt_size = u16_at(data, coff + 16)? as usize;
        let characteristics = u16_at(data, coff + 18)?;
        let opt = coff + 20;
        let (count_at, dirs_at) = match u16_at(data, opt)? {
            0x10B => (opt + 92, opt + 96),
            0x20B => (opt + 108, opt + 112),
            _ => return None,
        };
        let count = (u32_at(data, count_at)? as usize).min(16);
        let dirs = (0..count)
            .map(|i| Some((u32_at(data, dirs_at + 8 * i)?, u32_at(data, dirs_at + 8 * i + 4)?)))
            .collect::<Option<Vec<_>>>()?;
        let table = opt + opt_size;
        let sections = (0..nsections.min(96))
            .map(|i| {
                let s = table + 40 * i;
                Some(Section {
                    vsize: u32_at(data, s + 8)?,
                    va: u32_at(data, s + 12)?,
                    raw_size: u32_at(data, s + 16)?,
                    raw_ptr: u32_at(data, s + 20)?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Pe { data, machine, characteristics, sections, dirs })
    }

    /// The file bytes from an RVA to the end of its section.
    fn at(&self, rva: u32) -> Option<&'a [u8]> {
        let s = self.sections.iter().find(|s| rva >= s.va && rva - s.va < s.vsize.max(s.raw_size))?;
        let start = (s.raw_ptr as usize).checked_add((rva - s.va) as usize)?;
        let end = (s.raw_ptr as usize).checked_add(s.raw_size as usize)?.min(self.data.len());
        self.data.get(start..end)
    }

    fn slice(&self, rva: u32, size: u32) -> Option<&'a [u8]> {
        self.at(rva)?.get(..size as usize)
    }

    fn dir(&self, i: usize) -> Option<(u32, u32)> {
        self.dirs.get(i).copied().filter(|&(rva, size)| rva != 0 && size != 0)
    }

    fn cli(&self) -> Option<&'a [u8]> {
        let (rva, size) = self.dir(14)?;
        self.slice(rva, size.min(72)).filter(|c| c.len() >= 72)
    }

    fn metadata_root(&self) -> Option<&'a [u8]> {
        let cli = self.cli()?;
        let root = self.slice(u32_at(cli, 8)?, u32_at(cli, 12)?)?;
        root.starts_with(b"BSJB").then_some(root)
    }
}

// ---- Metadata tables ----

const MODULE: usize = 0x00;
const TYPE_REF: usize = 0x01;
const TYPE_DEF: usize = 0x02;
const FIELD: usize = 0x04;
const METHOD_DEF: usize = 0x06;
const PARAM: usize = 0x08;
const INTERFACE_IMPL: usize = 0x09;
const MEMBER_REF: usize = 0x0A;
const CONSTANT: usize = 0x0B;
const CUSTOM_ATTRIBUTE: usize = 0x0C;
const CLASS_LAYOUT: usize = 0x0F;
const STAND_ALONE_SIG: usize = 0x11;
const EVENT_MAP: usize = 0x12;
const EVENT: usize = 0x14;
const PROPERTY_MAP: usize = 0x15;
const PROPERTY: usize = 0x17;
const METHOD_SEMANTICS: usize = 0x18;
const MODULE_REF: usize = 0x1A;
const TYPE_SPEC: usize = 0x1B;
const FIELD_RVA: usize = 0x1D;
const ASSEMBLY: usize = 0x20;
const ASSEMBLY_REF: usize = 0x23;
const NESTED_CLASS: usize = 0x29;
const GENERIC_PARAM: usize = 0x2A;
const METHOD_SPEC: usize = 0x2B;
const GENERIC_PARAM_CONSTRAINT: usize = 0x2C;

/// A column of a metadata table.
#[derive(Clone, Copy)]
enum Col {
    U16,
    U32,
    Str,
    Guid,
    Blob,
    /// A row index into a table.
    T(usize),
    /// A coded index into one of several tables.
    K(Coded),
}

#[derive(Clone, Copy)]
enum Coded {
    TypeDefOrRef,
    HasConstant,
    HasCustomAttribute,
    HasFieldMarshal,
    HasDeclSecurity,
    MemberRefParent,
    HasSemantics,
    MethodDefOrRef,
    MemberForwarded,
    Implementation,
    CustomAttributeType,
    ResolutionScope,
    TypeOrMethodDef,
}

const NONE: usize = 0xFF;

impl Coded {
    /// The tables a coded index can point to, by tag.
    fn tables(self) -> &'static [usize] {
        match self {
            Coded::TypeDefOrRef => &[TYPE_DEF, TYPE_REF, TYPE_SPEC],
            Coded::HasConstant => &[FIELD, PARAM, PROPERTY],
            Coded::HasCustomAttribute => &[
                METHOD_DEF,
                FIELD,
                TYPE_REF,
                TYPE_DEF,
                PARAM,
                INTERFACE_IMPL,
                MEMBER_REF,
                MODULE,
                0x0E,
                PROPERTY,
                EVENT,
                STAND_ALONE_SIG,
                MODULE_REF,
                TYPE_SPEC,
                ASSEMBLY,
                ASSEMBLY_REF,
                0x26,
                0x27,
                0x28,
                GENERIC_PARAM,
                GENERIC_PARAM_CONSTRAINT,
                METHOD_SPEC,
            ],
            Coded::HasFieldMarshal => &[FIELD, PARAM],
            Coded::HasDeclSecurity => &[TYPE_DEF, METHOD_DEF, ASSEMBLY],
            Coded::MemberRefParent => &[TYPE_DEF, TYPE_REF, MODULE_REF, METHOD_DEF, TYPE_SPEC],
            Coded::HasSemantics => &[EVENT, PROPERTY],
            Coded::MethodDefOrRef => &[METHOD_DEF, MEMBER_REF],
            Coded::MemberForwarded => &[FIELD, METHOD_DEF],
            Coded::Implementation => &[0x26, ASSEMBLY_REF, 0x27],
            Coded::CustomAttributeType => &[NONE, NONE, METHOD_DEF, MEMBER_REF, NONE],
            Coded::ResolutionScope => &[MODULE, MODULE_REF, ASSEMBLY_REF, TYPE_REF],
            Coded::TypeOrMethodDef => &[TYPE_DEF, METHOD_DEF],
        }
    }

    fn bits(self) -> u32 {
        let n = self.tables().len();
        usize::BITS - (n - 1).leading_zeros()
    }

    /// (table, row) of a coded value.
    fn decode(self, value: u32) -> (usize, u32) {
        let bits = self.bits();
        let table = self.tables().get((value & ((1 << bits) - 1)) as usize).copied().unwrap_or(NONE);
        (table, value >> bits)
    }
}

/// Columns of every table an assembly may contain (ECMA-335 II.22).
fn schema(table: usize) -> Option<&'static [Col]> {
    use Coded as C;
    use Col::*;
    Some(match table {
        0x00 => &[U16, Str, Guid, Guid, Guid],
        0x01 => &[K(C::ResolutionScope), Str, Str],
        0x02 => &[U32, Str, Str, K(C::TypeDefOrRef), T(FIELD), T(METHOD_DEF)],
        0x03 => &[T(FIELD)],
        0x04 => &[U16, Str, Blob],
        0x05 => &[T(METHOD_DEF)],
        0x06 => &[U32, U16, U16, Str, Blob, T(PARAM)],
        0x07 => &[T(PARAM)],
        0x08 => &[U16, U16, Str],
        0x09 => &[T(TYPE_DEF), K(C::TypeDefOrRef)],
        0x0A => &[K(C::MemberRefParent), Str, Blob],
        0x0B => &[U16, K(C::HasConstant), Blob],
        0x0C => &[K(C::HasCustomAttribute), K(C::CustomAttributeType), Blob],
        0x0D => &[K(C::HasFieldMarshal), Blob],
        0x0E => &[U16, K(C::HasDeclSecurity), Blob],
        0x0F => &[U16, U32, T(TYPE_DEF)],
        0x10 => &[U32, T(FIELD)],
        0x11 => &[Blob],
        0x12 => &[T(TYPE_DEF), T(EVENT)],
        0x13 => &[T(EVENT)],
        0x14 => &[U16, Str, K(C::TypeDefOrRef)],
        0x15 => &[T(TYPE_DEF), T(PROPERTY)],
        0x16 => &[T(PROPERTY)],
        0x17 => &[U16, Str, Blob],
        0x18 => &[U16, T(METHOD_DEF), K(C::HasSemantics)],
        0x19 => &[T(TYPE_DEF), K(C::MethodDefOrRef), K(C::MethodDefOrRef)],
        0x1A => &[Str],
        0x1B => &[Blob],
        0x1C => &[U16, K(C::MemberForwarded), Str, T(MODULE_REF)],
        0x1D => &[U32, T(FIELD)],
        0x1E => &[U32, U32],
        0x1F => &[U32],
        0x20 => &[U32, U16, U16, U16, U16, U32, Blob, Str, Str],
        0x21 => &[U32],
        0x22 => &[U32, U32, U32],
        0x23 => &[U16, U16, U16, U16, U32, Blob, Str, Str, Blob],
        0x24 => &[U32, T(ASSEMBLY_REF)],
        0x25 => &[U32, U32, U32, T(ASSEMBLY_REF)],
        0x26 => &[U32, Str, Blob],
        0x27 => &[U32, U32, Str, Str, K(C::Implementation)],
        0x28 => &[U32, U32, Str, K(C::Implementation)],
        0x29 => &[T(TYPE_DEF), T(TYPE_DEF)],
        0x2A => &[U16, U16, K(C::TypeOrMethodDef), Str],
        0x2B => &[K(C::MethodDefOrRef), Blob],
        0x2C => &[T(GENERIC_PARAM), K(C::TypeDefOrRef)],
        _ => return None,
    })
}

struct Tables<'a> {
    data: &'a [u8],
    rows: [u32; 64],
    start: [usize; 64],
    row_size: [usize; 64],
    /// Per table, (offset in the row, width) of each column.
    cols: Vec<Vec<(usize, usize)>>,
}

impl<'a> Tables<'a> {
    fn parse(stream: &'a [u8]) -> Option<Self> {
        let heap_sizes = *stream.get(6)?;
        let valid = u64::from_le_bytes(stream.get(8..16)?.try_into().ok()?);
        let mut t =
            Tables { data: stream, rows: [0; 64], start: [0; 64], row_size: [0; 64], cols: vec![Vec::new(); 64] };
        let mut pos = 24;
        for i in 0..64 {
            if valid & (1 << i) != 0 {
                t.rows[i] = u32_at(stream, pos)?;
                pos += 4;
            }
        }
        // Uncompressed (#-) streams may have 4 extra bytes after the row counts.
        if heap_sizes & 0x40 != 0 {
            pos += 4;
        }
        let wide = |bit: u8| if heap_sizes & bit != 0 { 4 } else { 2 };
        let width = |c: Col, rows: &[u32; 64]| match c {
            Col::U16 => 2,
            Col::U32 => 4,
            Col::Str => wide(1),
            Col::Guid => wide(2),
            Col::Blob => wide(4),
            Col::T(table) => {
                if rows[table] < 0x10000 {
                    2
                } else {
                    4
                }
            }
            Col::K(k) => {
                let max = k.tables().iter().filter(|&&t| t != NONE).map(|&t| rows[t]).max().unwrap_or(0);
                if max < (1 << (16 - k.bits())) { 2 } else { 4 }
            }
        };
        for i in 0..64 {
            if t.rows[i] == 0 {
                continue;
            }
            let cols = schema(i)?; // A table this reader does not know: its size is unknown too.
            let mut offset = 0;
            for &c in cols {
                let w = width(c, &t.rows);
                t.cols[i].push((offset, w));
                offset += w;
            }
            t.row_size[i] = offset;
            t.start[i] = pos;
            pos = pos.checked_add(offset.checked_mul(t.rows[i] as usize)?)?;
        }
        (pos <= stream.len()).then_some(t)
    }

    fn len(&self, table: usize) -> u32 {
        self.rows[table]
    }

    /// A cell of a 1-based row; 0 when out of range.
    fn get(&self, table: usize, row: u32, col: usize) -> u32 {
        if row == 0 || row > self.rows[table] {
            return 0;
        }
        let Some(&(offset, width)) = self.cols[table].get(col) else { return 0 };
        let at = self.start[table] + (row as usize - 1) * self.row_size[table] + offset;
        match width {
            2 => u16_at(self.data, at).map_or(0, u32::from),
            _ => u32_at(self.data, at).unwrap_or(0),
        }
    }

    /// The rows of a list column: from this row's value to the next row's (or the end).
    fn range(&self, table: usize, row: u32, col: usize, target: usize) -> std::ops::Range<u32> {
        let start = self.get(table, row, col).max(1);
        let end = if row < self.rows[table] { self.get(table, row + 1, col) } else { self.rows[target] + 1 };
        start..end.max(start).min(self.rows[target] + 1)
    }
}

// ---- Heaps and signatures ----

/// A compressed unsigned integer (ECMA-335 II.23.2) and its length.
fn compressed(b: &[u8]) -> Option<(u32, usize)> {
    let first = *b.first()?;
    if first & 0x80 == 0 {
        Some((u32::from(first), 1))
    } else if first & 0xC0 == 0x80 {
        Some(((u32::from(first & 0x3F) << 8) | u32::from(*b.get(1)?), 2))
    } else if first & 0xE0 == 0xC0 {
        let v = (u32::from(first & 0x1F) << 24)
            | (u32::from(*b.get(1)?) << 16)
            | (u32::from(*b.get(2)?) << 8)
            | u32::from(*b.get(3)?);
        Some((v, 4))
    } else {
        None
    }
}

struct Heaps<'a> {
    strings: &'a [u8],
    blob: &'a [u8],
    us: &'a [u8],
    guid: &'a [u8],
}

impl<'a> Heaps<'a> {
    fn str(&self, index: u32) -> &'a str {
        let s = self.strings.get(index as usize..).unwrap_or_default();
        let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
        std::str::from_utf8(&s[..end]).unwrap_or("?")
    }

    fn blob(&self, index: u32) -> &'a [u8] {
        let b = self.blob.get(index as usize..).unwrap_or_default();
        match compressed(b) {
            Some((len, n)) => b.get(n..n + len as usize).unwrap_or_default(),
            None => &[],
        }
    }

    fn user_string(&self, index: u32) -> String {
        let b = self.us.get(index as usize..).unwrap_or_default();
        let Some((len, n)) = compressed(b) else { return String::new() };
        let bytes = b.get(n..n + (len as usize).saturating_sub(1)).unwrap_or_default();
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    }

    fn guid(&self, index: u32) -> Option<&'a [u8]> {
        let at = (index as usize).checked_sub(1)? * 16;
        self.guid.get(at..at + 16)
    }
}

/// A reader over a signature blob.
struct Sig<'b> {
    b: &'b [u8],
    pos: usize,
}

impl<'b> Sig<'b> {
    fn new(b: &'b [u8]) -> Self {
        Sig { b, pos: 0 }
    }

    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.pos).copied()
    }

    fn uint(&mut self) -> Option<u32> {
        let (v, n) = compressed(self.b.get(self.pos..)?)?;
        self.pos += n;
        Some(v)
    }

    fn bytes(&mut self, n: usize) -> Option<&'b [u8]> {
        let s = self.b.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }
}

/// Names of the generic parameters in scope while decoding a signature.
#[derive(Default, Clone, Copy)]
struct Generics<'g> {
    of_type: &'g [String],
    of_method: &'g [String],
}

/// A decoded method signature.
struct MethodSig {
    has_this: bool,
    generic_count: u32,
    ret: String,
    params: Vec<String>,
}

/// The C# keyword for a type of `System`, if it has one.
fn keyword(full: &str) -> Option<&'static str> {
    Some(match full {
        "System.Void" => "void",
        "System.Boolean" => "bool",
        "System.Char" => "char",
        "System.SByte" => "sbyte",
        "System.Byte" => "byte",
        "System.Int16" => "short",
        "System.UInt16" => "ushort",
        "System.Int32" => "int",
        "System.UInt32" => "uint",
        "System.Int64" => "long",
        "System.UInt64" => "ulong",
        "System.Single" => "float",
        "System.Double" => "double",
        "System.Decimal" => "decimal",
        "System.String" => "string",
        "System.Object" => "object",
        "System.IntPtr" => "nint",
        "System.UIntPtr" => "nuint",
        _ => return None,
    })
}

fn primitive(element: u8) -> Option<&'static str> {
    Some(match element {
        0x01 => "void",
        0x02 => "bool",
        0x03 => "char",
        0x04 => "sbyte",
        0x05 => "byte",
        0x06 => "short",
        0x07 => "ushort",
        0x08 => "int",
        0x09 => "uint",
        0x0A => "long",
        0x0B => "ulong",
        0x0C => "float",
        0x0D => "double",
        0x0E => "string",
        0x16 => "TypedReference",
        0x18 => "nint",
        0x19 => "nuint",
        0x1C => "object",
        _ => return None,
    })
}

/// `List`1` → `List`.
fn strip_arity(name: &str) -> &str {
    match name.rfind('`') {
        Some(i) if name[i + 1..].bytes().all(|b| b.is_ascii_digit()) && i + 1 < name.len() => &name[..i],
        _ => name,
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn float(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_infinite() {
        if v > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else {
        format!("{v:?}")
    }
}

fn line(out: &mut String, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
}

/// A member declaration and its attributes.
type Member = (String, Vec<String>);

// ---- The assembly ----

struct Assembly<'a> {
    pe: Pe<'a>,
    cli: &'a [u8],
    version: String,
    t: Tables<'a>,
    h: Heaps<'a>,
    /// Per TypeDef row (index 0 = row 1), its full name with nesting (`Ns.Outer.Inner`).
    type_names: Vec<String>,
    /// Per TypeRef row, its full name.
    ref_names: Vec<String>,
    /// Generic parameter names per TypeDef row and per MethodDef row.
    type_generics: HashMap<u32, Vec<String>>,
    method_generics: HashMap<u32, Vec<String>>,
    /// Per MethodDef row, the TypeDef row that declares it.
    method_owner: Vec<u32>,
    /// Per Field row, the TypeDef row that declares it.
    field_owner: Vec<u32>,
    /// Custom attribute rows by the coded parent.
    attributes: HashMap<u32, Vec<u32>>,
    /// Constant rows by the coded parent.
    constants: HashMap<u32, u32>,
    /// InterfaceImpl rows by TypeDef row.
    interfaces: HashMap<u32, Vec<u32>>,
    /// GenericParam rows by owner (table, row).
    generic_rows: HashMap<(usize, u32), Vec<u32>>,
    /// GenericParamConstraint rows by GenericParam row.
    constraint_rows: HashMap<u32, Vec<u32>>,
    /// PropertyMap and EventMap rows by TypeDef row.
    property_map: HashMap<u32, u32>,
    event_map: HashMap<u32, u32>,
    /// (semantics, method) pairs by coded HasSemantics association.
    semantics: HashMap<u32, Vec<(u32, u32)>>,
    /// ClassLayout class size by TypeDef row.
    class_size: HashMap<u32, u32>,
    /// Memoized token texts for the IL view, and hashes of them for checksums.
    token_cache: RefCell<HashMap<u32, String>>,
    hash_cache: RefCell<HashMap<u32, u32>>,
}

impl<'a> Assembly<'a> {
    fn parse(data: &'a [u8]) -> Result<Self> {
        let pe = Pe::parse(data).ok_or_else(|| bad("not a PE file"))?;
        let cli = pe.cli().ok_or_else(|| bad("no CLI header"))?;
        let root = pe.metadata_root().ok_or_else(|| bad("no metadata"))?;
        let version_len = u32_at(root, 12).ok_or_else(|| bad("truncated metadata"))? as usize;
        let version_bytes = root.get(16..16 + version_len.min(255)).ok_or_else(|| bad("truncated metadata"))?;
        let version = String::from_utf8_lossy(version_bytes).trim_end_matches('\0').to_string();
        let mut pos = 16 + version_len;
        let nstreams = u16_at(root, pos + 2).ok_or_else(|| bad("truncated metadata"))?;
        pos += 4;
        let (mut tables, mut strings, mut blob, mut us, mut guid) = (None, &[][..], &[][..], &[][..], &[][..]);
        for _ in 0..nstreams.min(16) {
            let offset = u32_at(root, pos).ok_or_else(|| bad("truncated stream header"))? as usize;
            let size = u32_at(root, pos + 4).ok_or_else(|| bad("truncated stream header"))? as usize;
            let name_bytes = root.get(pos + 8..).ok_or_else(|| bad("truncated stream header"))?;
            let name_len = name_bytes.iter().take(32).position(|&b| b == 0).ok_or_else(|| bad("bad stream name"))?;
            let name = &name_bytes[..name_len];
            pos += 8 + (name_len + 4) / 4 * 4;
            let stream = root.get(offset..offset.saturating_add(size)).ok_or_else(|| bad("stream out of range"))?;
            match name {
                b"#~" | b"#-" => tables = Some(stream),
                b"#Strings" => strings = stream,
                b"#Blob" => blob = stream,
                b"#US" => us = stream,
                b"#GUID" => guid = stream,
                _ => {}
            }
        }
        let t = Tables::parse(tables.ok_or_else(|| bad("no tables stream"))?)
            .ok_or_else(|| bad("unreadable or unknown metadata tables"))?;
        let h = Heaps { strings, blob, us, guid };
        let mut a = Assembly {
            pe,
            cli,
            version,
            t,
            h,
            type_names: Vec::new(),
            ref_names: Vec::new(),
            type_generics: HashMap::new(),
            method_generics: HashMap::new(),
            method_owner: Vec::new(),
            field_owner: Vec::new(),
            attributes: HashMap::new(),
            constants: HashMap::new(),
            interfaces: HashMap::new(),
            generic_rows: HashMap::new(),
            constraint_rows: HashMap::new(),
            property_map: HashMap::new(),
            event_map: HashMap::new(),
            semantics: HashMap::new(),
            class_size: HashMap::new(),
            token_cache: RefCell::new(HashMap::new()),
            hash_cache: RefCell::new(HashMap::new()),
        };
        a.index();
        Ok(a)
    }

    /// Builds the lookup maps, once, so later lookups are cheap.
    fn index(&mut self) {
        let t = &self.t;
        let h = &self.h;
        // Enclosing type of nested types.
        let mut enclosing: HashMap<u32, u32> = HashMap::new();
        for r in 1..=t.len(NESTED_CLASS) {
            enclosing.insert(t.get(NESTED_CLASS, r, 0), t.get(NESTED_CLASS, r, 1));
        }
        let simple: Vec<String> = (1..=t.len(TYPE_DEF))
            .map(|r| {
                let (ns, name) = (h.str(t.get(TYPE_DEF, r, 2)), h.str(t.get(TYPE_DEF, r, 1)));
                if ns.is_empty() { name.to_string() } else { format!("{ns}.{name}") }
            })
            .collect();
        self.type_names = (1..=t.len(TYPE_DEF))
            .map(|r| {
                let mut name = simple[r as usize - 1].clone();
                let mut at = r;
                for _ in 0..16 {
                    match enclosing.get(&at) {
                        Some(&outer) if outer != at && outer >= 1 && outer as usize <= simple.len() => {
                            name = format!("{}.{name}", simple[outer as usize - 1]);
                            at = outer;
                        }
                        _ => break,
                    }
                }
                name
            })
            .collect();
        let mut ref_names: Vec<String> = Vec::with_capacity(t.len(TYPE_REF) as usize);
        for r in 1..=t.len(TYPE_REF) {
            let (ns, name) = (h.str(t.get(TYPE_REF, r, 2)), h.str(t.get(TYPE_REF, r, 1)));
            let (table, row) = Coded::ResolutionScope.decode(t.get(TYPE_REF, r, 0));
            // Nested type references point to their enclosing type reference, which comes first.
            let full = if table == TYPE_REF && row < r && row >= 1 {
                format!("{}.{name}", ref_names[row as usize - 1])
            } else if ns.is_empty() {
                name.to_string()
            } else {
                format!("{ns}.{name}")
            };
            ref_names.push(full);
        }
        self.ref_names = ref_names;
        for r in 1..=t.len(GENERIC_PARAM) {
            let (table, owner) = Coded::TypeOrMethodDef.decode(t.get(GENERIC_PARAM, r, 2));
            let number = t.get(GENERIC_PARAM, r, 0) as usize;
            let list = match table {
                TYPE_DEF => self.type_generics.entry(owner).or_default(),
                METHOD_DEF => self.method_generics.entry(owner).or_default(),
                _ => continue,
            };
            if number < 256 {
                if list.len() <= number {
                    list.resize(number + 1, String::new());
                }
                list[number] = h.str(t.get(GENERIC_PARAM, r, 3)).to_string();
            }
        }
        self.method_owner = vec![0; t.len(METHOD_DEF) as usize];
        self.field_owner = vec![0; t.len(FIELD) as usize];
        for r in 1..=t.len(TYPE_DEF) {
            for m in t.range(TYPE_DEF, r, 5, METHOD_DEF) {
                self.method_owner[m as usize - 1] = r;
            }
            for f in t.range(TYPE_DEF, r, 4, FIELD) {
                self.field_owner[f as usize - 1] = r;
            }
        }
        for r in 1..=t.len(CUSTOM_ATTRIBUTE) {
            self.attributes.entry(t.get(CUSTOM_ATTRIBUTE, r, 0)).or_default().push(r);
        }
        for r in 1..=t.len(CONSTANT) {
            self.constants.insert(t.get(CONSTANT, r, 1), r);
        }
        for r in 1..=t.len(INTERFACE_IMPL) {
            self.interfaces.entry(t.get(INTERFACE_IMPL, r, 0)).or_default().push(r);
        }
        for r in 1..=t.len(GENERIC_PARAM) {
            let owner = Coded::TypeOrMethodDef.decode(t.get(GENERIC_PARAM, r, 2));
            self.generic_rows.entry(owner).or_default().push(r);
        }
        for r in 1..=t.len(GENERIC_PARAM_CONSTRAINT) {
            self.constraint_rows.entry(t.get(GENERIC_PARAM_CONSTRAINT, r, 0)).or_default().push(r);
        }
        for r in 1..=t.len(PROPERTY_MAP) {
            self.property_map.insert(t.get(PROPERTY_MAP, r, 0), r);
        }
        for r in 1..=t.len(EVENT_MAP) {
            self.event_map.insert(t.get(EVENT_MAP, r, 0), r);
        }
        for r in 1..=t.len(METHOD_SEMANTICS) {
            let pair = (t.get(METHOD_SEMANTICS, r, 0), t.get(METHOD_SEMANTICS, r, 1));
            self.semantics.entry(t.get(METHOD_SEMANTICS, r, 2)).or_default().push(pair);
        }
        for r in 1..=t.len(CLASS_LAYOUT) {
            self.class_size.insert(t.get(CLASS_LAYOUT, r, 2), t.get(CLASS_LAYOUT, r, 1));
        }
    }

    fn generic_rows_of(&self, table: usize, owner: u32) -> &[u32] {
        self.generic_rows.get(&(table, owner)).map_or(&[], Vec::as_slice)
    }

    /// The coded HasCustomAttribute / HasConstant value of a row.
    fn coded(kind: Coded, table: usize, row: u32) -> u32 {
        let tag = kind.tables().iter().position(|&t| t == table).unwrap_or(0) as u32;
        (row << kind.bits()) | tag
    }

    // ---- Type names ----

    /// A TypeDef, TypeRef or TypeSpec as C# writes it.
    fn type_name(&self, table: usize, row: u32, g: Generics) -> String {
        let full = match table {
            TYPE_DEF => self.type_names.get(row as usize - 1).map(String::as_str),
            TYPE_REF => self.ref_names.get((row as usize).wrapping_sub(1)).map(String::as_str),
            TYPE_SPEC => {
                let mut sig = Sig::new(self.h.blob(self.t.get(TYPE_SPEC, row, 0)));
                return self.sig_type(&mut sig, g, 0).unwrap_or_else(|| "?".into());
            }
            _ => None,
        };
        match full {
            Some(full) => keyword(full).map_or_else(|| strip_arity_in_path(full), str::to_string),
            None => format!("?{table:02x}:{row}"),
        }
    }

    fn type_def_or_ref(&self, coded: u32, g: Generics) -> String {
        let (table, row) = Coded::TypeDefOrRef.decode(coded);
        self.type_name(table, row, g)
    }

    /// A type in a signature (ECMA-335 II.23.2.12).
    fn sig_type(&self, s: &mut Sig, g: Generics, depth: u32) -> Option<String> {
        if depth > 64 {
            return None;
        }
        let e = s.u8()?;
        if let Some(p) = primitive(e) {
            return Some(p.to_string());
        }
        Some(match e {
            0x0F => self.sig_type(s, g, depth + 1)? + "*",
            0x10 => format!("ref {}", self.sig_type(s, g, depth + 1)?),
            0x11 | 0x12 => self.type_def_or_ref(s.uint()?, g),
            0x13 => {
                let n = s.uint()? as usize;
                g.of_type.get(n).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| format!("!{n}"))
            }
            0x1E => {
                let n = s.uint()? as usize;
                g.of_method.get(n).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| format!("!!{n}"))
            }
            0x14 => {
                let elem = self.sig_type(s, g, depth + 1)?;
                let rank = s.uint()?;
                for _ in 0..s.uint()? {
                    s.uint()?;
                }
                for _ in 0..s.uint()? {
                    s.uint()?;
                }
                format!("{elem}[{}]", ",".repeat(rank.saturating_sub(1).min(32) as usize))
            }
            0x1D => self.sig_type(s, g, depth + 1)? + "[]",
            0x15 => {
                let base = self.sig_type(s, g, depth + 1)?;
                let n = s.uint()?;
                let args: Vec<String> =
                    (0..n.min(256)).map(|_| self.sig_type(s, g, depth + 1)).collect::<Option<_>>()?;
                if base == "System.Nullable" && args.len() == 1 {
                    format!("{}?", args[0])
                } else {
                    format!("{base}<{}>", args.join(", "))
                }
            }
            0x1B => {
                let m = self.method_sig(s, g, depth + 1)?;
                format!("delegate*<{}>", m.params.into_iter().chain([m.ret]).collect::<Vec<_>>().join(", "))
            }
            0x1F | 0x20 => {
                let coded = s.uint()?;
                let modifier = self.type_def_or_ref(coded, g);
                let inner = self.sig_type(s, g, depth + 1)?;
                // Modifiers the C# compiler uses for `in`, `readonly` and `volatile`.
                match modifier.as_str() {
                    "System.Runtime.InteropServices.InAttribute" => inner,
                    "System.Runtime.CompilerServices.IsVolatile" => format!("volatile {inner}"),
                    m => format!("{inner} modreq({m})"),
                }
            }
            0x45 => format!("pinned {}", self.sig_type(s, g, depth + 1)?),
            0x41 => "...".to_string(),
            _ => return None,
        })
    }

    fn method_sig(&self, s: &mut Sig, g: Generics, depth: u32) -> Option<MethodSig> {
        let conv = s.u8()?;
        let generic_count = if conv & 0x10 != 0 { s.uint()? } else { 0 };
        let count = s.uint()?;
        let ret = self.sig_type(s, g, depth)?;
        let mut params = Vec::new();
        for _ in 0..count.min(1024) {
            if s.peek() == Some(0x41) {
                s.u8();
                params.push("...".to_string());
            }
            params.push(self.sig_type(s, g, depth)?);
        }
        Some(MethodSig { has_this: conv & 0x20 != 0, generic_count, ret, params })
    }

    fn field_type(&self, blob: &[u8], g: Generics) -> String {
        let mut s = Sig::new(blob);
        if s.u8() != Some(0x06) {
            return "?".into();
        }
        self.sig_type(&mut s, g, 0).unwrap_or_else(|| "?".into())
    }

    fn generics_of_method(&self, method: u32) -> Generics<'_> {
        let owner = self.method_owner.get(method as usize - 1).copied().unwrap_or(0);
        Generics {
            of_type: self.type_generics.get(&owner).map_or(&[], Vec::as_slice),
            of_method: self.method_generics.get(&method).map_or(&[], Vec::as_slice),
        }
    }

    fn generics_of_type(&self, ty: u32) -> Generics<'_> {
        Generics { of_type: self.type_generics.get(&ty).map_or(&[], Vec::as_slice), of_method: &[] }
    }

    /// The name of a type without namespace, for constructors.
    fn short_type_name(&self, ty: u32) -> String {
        let name = self.h.str(self.t.get(TYPE_DEF, ty, 1));
        strip_arity(name).to_string()
    }

    // ---- Constants and attributes ----

    fn constant(&self, table: usize, row: u32) -> Option<String> {
        let r = *self.constants.get(&Self::coded(Coded::HasConstant, table, row))?;
        let ty = self.t.get(CONSTANT, r, 0) & 0xFF;
        let v = self.h.blob(self.t.get(CONSTANT, r, 2));
        let int = |n: usize| -> Option<i64> {
            let b = v.get(..n)?;
            Some(match n {
                1 => i64::from(b[0] as i8),
                2 => i64::from(i16::from_le_bytes([b[0], b[1]])),
                4 => i64::from(i32::from_le_bytes(b.try_into().ok()?)),
                _ => i64::from_le_bytes(b.try_into().ok()?),
            })
        };
        Some(match ty {
            0x02 => (v.first()? != &0).to_string(),
            0x03 => {
                let c = char::from_u32(u32::from(u16::from_le_bytes([*v.first()?, *v.get(1)?]))).unwrap_or('?');
                format!("'{}'", quote(&c.to_string()).trim_matches('"'))
            }
            0x04 => int(1)?.to_string(),
            0x05 => v.first()?.to_string(),
            0x06 => int(2)?.to_string(),
            0x07 => u16::from_le_bytes([*v.first()?, *v.get(1)?]).to_string(),
            0x08 => int(4)?.to_string(),
            0x09 => u32_at(v, 0)?.to_string(),
            0x0A => int(8)?.to_string(),
            0x0B => u64::from_le_bytes(v.get(..8)?.try_into().ok()?).to_string(),
            0x0C => float(f64::from(f32::from_le_bytes(v.get(..4)?.try_into().ok()?))) + "f",
            0x0D => float(f64::from_le_bytes(v.get(..8)?.try_into().ok()?)),
            0x0E => {
                let units: Vec<u16> = v.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                quote(&String::from_utf16_lossy(&units))
            }
            0x12 => "null".into(),
            _ => "?".into(),
        })
    }

    /// The custom attributes of a row, as `[Name(args)]`, sorted.
    fn attributes_of(&self, table: usize, row: u32) -> Vec<String> {
        let Some(rows) = self.attributes.get(&Self::coded(Coded::HasCustomAttribute, table, row)) else {
            return Vec::new();
        };
        let mut out: Vec<String> = rows.iter().map(|&r| self.attribute(r)).collect();
        out.sort();
        out
    }

    fn attribute(&self, row: u32) -> String {
        let (table, ctor) = Coded::CustomAttributeType.decode(self.t.get(CUSTOM_ATTRIBUTE, row, 1));
        let (owner, sig_blob) = match table {
            METHOD_DEF => (
                self.type_name(
                    TYPE_DEF,
                    self.method_owner.get(ctor as usize - 1).copied().unwrap_or(0),
                    Generics::default(),
                ),
                self.h.blob(self.t.get(METHOD_DEF, ctor, 4)),
            ),
            MEMBER_REF => {
                let (pt, pr) = Coded::MemberRefParent.decode(self.t.get(MEMBER_REF, ctor, 0));
                (self.type_name(pt, pr, Generics::default()), self.h.blob(self.t.get(MEMBER_REF, ctor, 2)))
            }
            _ => ("?".into(), &[][..]),
        };
        let name = owner.strip_suffix("Attribute").unwrap_or(&owner).to_string();
        let value = self.h.blob(self.t.get(CUSTOM_ATTRIBUTE, row, 2));
        match self.attribute_args(sig_blob, value) {
            Some(args) if args.is_empty() => format!("[{name}]"),
            Some(args) => format!("[{name}({})]", args.join(", ")),
            None => format!("[{name}(blob {:08x})]", crc32fast::hash(value)),
        }
    }

    /// Decodes an attribute value blob (ECMA-335 II.23.3) with the constructor's parameter types.
    fn attribute_args(&self, ctor_sig: &[u8], value: &[u8]) -> Option<Vec<String>> {
        let mut s = Sig::new(ctor_sig);
        let _conv = s.u8()?;
        let count = s.uint()?;
        let _ret = s.u8()?;
        let types: Vec<SerType> = (0..count.min(64)).map(|_| self.ser_type_of_sig(&mut s, 0)).collect::<Option<_>>()?;
        let mut v = Sig::new(value);
        if v.bytes(2)? != [1, 0] {
            return None;
        }
        let mut args: Vec<String> = types.iter().map(|t| ser_value(&mut v, t, 0)).collect::<Option<_>>()?;
        let named = u16::from_le_bytes(v.bytes(2)?.try_into().ok()?);
        for _ in 0..named.min(64) {
            let _kind = v.u8()?;
            let ty = ser_type_of_value(&mut v, 0)?;
            let name = ser_string(&mut v)?.unwrap_or_default();
            args.push(format!("{name} = {}", ser_value(&mut v, &ty, 0)?));
        }
        Some(args)
    }

    /// The serialization type of a constructor parameter.
    fn ser_type_of_sig(&self, s: &mut Sig, depth: u32) -> Option<SerType> {
        if depth > 8 {
            return None;
        }
        let e = s.u8()?;
        Some(match e {
            0x02..=0x0D => SerType::Prim(e),
            0x0E => SerType::String,
            0x1C => SerType::Object,
            0x1D => SerType::Array(Box::new(self.ser_type_of_sig(s, depth + 1)?)),
            0x11 | 0x12 => {
                let (table, row) = Coded::TypeDefOrRef.decode(s.uint()?);
                let name = self.type_name(table, row, Generics::default());
                if name == "System.Type" {
                    SerType::Type
                } else if e == 0x11 {
                    SerType::Prim(self.enum_underlying(table, row).unwrap_or(0x08))
                } else {
                    return None;
                }
            }
            _ => return None,
        })
    }

    /// The element type of an enum defined in this assembly; other enums are
    /// assumed to be `int`, as nearly all are.
    fn enum_underlying(&self, table: usize, row: u32) -> Option<u8> {
        if table != TYPE_DEF {
            return None;
        }
        let field =
            self.t.range(TYPE_DEF, row, 4, FIELD).find(|&f| self.h.str(self.t.get(FIELD, f, 1)) == "value__")?;
        let sig = self.h.blob(self.t.get(FIELD, field, 2));
        sig.get(1).copied().filter(|e| (0x02..=0x0B).contains(e))
    }

    // ---- Members ----

    fn access(flags: u32) -> &'static str {
        match flags & 7 {
            1 => "private ",
            2 => "private protected ",
            3 => "internal ",
            4 => "protected ",
            5 => "protected internal ",
            6 => "public ",
            _ => "",
        }
    }

    fn type_declaration(&self, ty: u32) -> String {
        let t = &self.t;
        let flags = t.get(TYPE_DEF, ty, 0);
        let g = self.generics_of_type(ty);
        let visibility = match flags & 7 {
            0 | 5 => "internal ",
            1 | 2 => "public ",
            3 => "private ",
            4 => "protected ",
            6 => "private protected ",
            _ => "protected internal ",
        };
        let extends = t.get(TYPE_DEF, ty, 3);
        let base = if extends == 0 { String::new() } else { self.type_def_or_ref(extends, g) };
        let interface = flags & 0x20 != 0;
        let (abstract_, sealed) = (flags & 0x80 != 0, flags & 0x100 != 0);
        let kind = if interface {
            "interface"
        } else {
            match base.as_str() {
                "System.Enum" => "enum",
                "System.ValueType" => "struct",
                "System.MulticastDelegate" => "delegate",
                _ => "class",
            }
        };
        let mut modifiers = String::new();
        if kind == "class" {
            modifiers = match (abstract_, sealed) {
                (true, true) => "static ".into(),
                (true, false) => "abstract ".into(),
                (false, true) => "sealed ".into(),
                _ => String::new(),
            };
        }
        let mut out = format!("{visibility}{modifiers}{kind} {}", self.type_names[ty as usize - 1]);
        out = strip_arity_in_path(&out);
        if let Some(params) = self.type_generics.get(&ty) {
            out.push_str(&self.generic_params(TYPE_DEF, ty, params));
        }
        let mut supers = Vec::new();
        if kind == "class" && !base.is_empty() && base != "object" {
            supers.push(base.clone());
        }
        if kind == "enum"
            && let Some(e) = self.enum_underlying(TYPE_DEF, ty).and_then(primitive).filter(|p| *p != "int")
        {
            supers.push(e.to_string());
        }
        for &r in self.interfaces.get(&ty).map_or(&[][..], Vec::as_slice) {
            supers.push(self.type_def_or_ref(t.get(INTERFACE_IMPL, r, 1), g));
        }
        if !supers.is_empty() {
            let _ = write!(out, " : {}", supers.join(", "));
        }
        out.push_str(&self.constraints(TYPE_DEF, ty, g));
        out
    }

    /// `<T, out U>`.
    fn generic_params(&self, table: usize, owner: u32, names: &[String]) -> String {
        let t = &self.t;
        let mut list = names.to_vec();
        for &r in self.generic_rows_of(table, owner) {
            {
                let n = t.get(GENERIC_PARAM, r, 0) as usize;
                let variance = match t.get(GENERIC_PARAM, r, 1) & 3 {
                    1 => "out ",
                    2 => "in ",
                    _ => "",
                };
                if let Some(slot) = list.get_mut(n) {
                    *slot = format!("{variance}{slot}");
                }
            }
        }
        format!("<{}>", list.join(", "))
    }

    /// ` where T : class, IComparable<T>, new()`.
    fn constraints(&self, table: usize, owner: u32, g: Generics) -> String {
        let t = &self.t;
        let mut out = String::new();
        for &r in self.generic_rows_of(table, owner) {
            let flags = t.get(GENERIC_PARAM, r, 1);
            let mut list = Vec::new();
            if flags & 0x04 != 0 {
                list.push("class".to_string());
            }
            if flags & 0x08 != 0 {
                list.push("struct".to_string());
            }
            for &c in self.constraint_rows.get(&r).map_or(&[][..], Vec::as_slice) {
                let ty = self.type_def_or_ref(t.get(GENERIC_PARAM_CONSTRAINT, c, 1), g);
                if !(ty == "System.ValueType" && flags & 0x08 != 0) {
                    list.push(ty);
                }
            }
            if flags & 0x10 != 0 && flags & 0x08 == 0 {
                list.push("new()".to_string());
            }
            if !list.is_empty() {
                let _ = write!(out, " where {} : {}", self.h.str(t.get(GENERIC_PARAM, r, 3)), list.join(", "));
            }
        }
        out
    }

    fn field_line(&self, f: u32, g: Generics) -> String {
        let t = &self.t;
        let flags = t.get(FIELD, f, 0);
        let ty = self.field_type(self.h.blob(t.get(FIELD, f, 2)), g);
        let mut mods = Self::access(flags).to_string();
        if flags & 0x40 != 0 {
            mods.push_str("const ");
        } else {
            if flags & 0x10 != 0 {
                mods.push_str("static ");
            }
            if flags & 0x20 != 0 {
                mods.push_str("readonly ");
            }
        }
        let mut out = format!("{mods}{ty} {}", self.h.str(t.get(FIELD, f, 1)));
        if let Some(v) = self.constant(FIELD, f) {
            let _ = write!(out, " = {v}");
        }
        out
    }

    /// Modifiers, return type, name, generic parameters and parameters of a method.
    fn method_declaration(&self, m: u32) -> String {
        let t = &self.t;
        let flags = t.get(METHOD_DEF, m, 2);
        let impl_flags = t.get(METHOD_DEF, m, 1);
        let name = self.h.str(t.get(METHOD_DEF, m, 3));
        let g = self.generics_of_method(m);
        let owner = self.method_owner.get(m as usize - 1).copied().unwrap_or(0);
        let mut s = Sig::new(self.h.blob(t.get(METHOD_DEF, m, 4)));
        let sig = self.method_sig(&mut s, g, 0);
        let mut mods = Self::access(flags).to_string();
        if flags & 0x10 != 0 {
            mods.push_str("static ");
        }
        if flags & 0x2000 != 0 {
            mods.push_str("extern ");
        }
        let is_interface = t.get(TYPE_DEF, owner, 0) & 0x20 != 0;
        if flags & 0x40 != 0 && !is_interface {
            if flags & 0x400 != 0 {
                mods.push_str("abstract ");
            } else if flags & 0x100 != 0 {
                if flags & 0x20 == 0 {
                    mods.push_str("virtual ");
                }
            } else {
                mods.push_str(if flags & 0x20 != 0 { "sealed override " } else { "override " });
            }
        }
        if impl_flags & 0x20 != 0 {
            mods.push_str("synchronized ");
        }
        if impl_flags & 0x3 == 0x3 || impl_flags & 0x1000 != 0 {
            mods.push_str("runtime ");
        }
        let Some(sig) = sig else { return format!("{mods}{name}(?)") };
        let params = self.parameters(m, &sig.params);
        let generic = match self.method_generics.get(&m) {
            Some(names) if sig.generic_count > 0 => self.generic_params(METHOD_DEF, m, names),
            _ => String::new(),
        };
        let constraints = if generic.is_empty() { String::new() } else { self.constraints(METHOD_DEF, m, g) };
        match name {
            ".ctor" => format!("{mods}{}({params})", self.short_type_name(owner)),
            ".cctor" => format!("static {}()", self.short_type_name(owner)),
            _ => format!("{mods}{} {name}{generic}({params}){constraints}", sig.ret),
        }
    }

    /// Parameters with names, `out`/`in`, and default values.
    fn parameters(&self, m: u32, types: &[String]) -> String {
        let t = &self.t;
        let mut names: Vec<Option<(u32, u32)>> = vec![None; types.len()];
        for p in t.range(METHOD_DEF, m, 5, PARAM) {
            let seq = t.get(PARAM, p, 1) as usize;
            if seq >= 1 && seq <= names.len() {
                names[seq - 1] = Some((p, t.get(PARAM, p, 0)));
            }
        }
        types
            .iter()
            .zip(names)
            .map(|(ty, p)| {
                let Some((row, flags)) = p else { return ty.clone() };
                let name = self.h.str(t.get(PARAM, row, 2));
                let mut ty = ty.clone();
                if let Some(rest) = ty.strip_prefix("ref ") {
                    // out = [Out] without [In]; in = [In] with a readonly modifier.
                    ty = if flags & 0x3 == 0x2 { format!("out {rest}") } else { ty.clone() };
                }
                let mut out = if name.is_empty() { ty } else { format!("{ty} {name}") };
                if flags & 0x1000 != 0
                    && let Some(v) = self.constant(PARAM, row)
                {
                    let _ = write!(out, " = {v}");
                }
                out
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Properties and events of a type with their accessors.
    fn properties_and_events(&self, ty: u32) -> (Vec<Member>, Vec<Member>) {
        let t = &self.t;
        let g = self.generics_of_type(ty);
        let semantics = &self.semantics;
        let mut props = Vec::new();
        let mut events = Vec::new();
        let props_range = self.property_map.get(&ty).map(|&r| t.range(PROPERTY_MAP, r, 1, PROPERTY));
        let events_range = self.event_map.get(&ty).map(|&r| t.range(EVENT_MAP, r, 1, EVENT));
        let accessor_access = |method: u32| t.get(METHOD_DEF, method, 2) & 7;
        for p in props_range.into_iter().flatten() {
            let name = self.h.str(t.get(PROPERTY, p, 1));
            let mut s = Sig::new(self.h.blob(t.get(PROPERTY, p, 2)));
            let (ty, params) = match s.u8() {
                Some(_) => {
                    let count = s.uint().unwrap_or(0);
                    let ty = self.sig_type(&mut s, g, 0).unwrap_or_else(|| "?".into());
                    let params: Vec<String> =
                        (0..count.min(64)).map(|_| self.sig_type(&mut s, g, 0).unwrap_or_else(|| "?".into())).collect();
                    (ty, params)
                }
                None => ("?".into(), Vec::new()),
            };
            let accessors = semantics.get(&Self::coded(Coded::HasSemantics, PROPERTY, p)).cloned().unwrap_or_default();
            let widest = accessors.iter().map(|&(_, m)| accessor_access(m)).max().unwrap_or(0);
            let mut parts = Vec::new();
            let mut is_static = false;
            for &(kind, m) in &accessors {
                is_static |= t.get(METHOD_DEF, m, 2) & 0x10 != 0;
                let access = if accessor_access(m) == widest { "" } else { Self::access(accessor_access(m)) };
                match kind {
                    0x2 => parts.push((0, format!("{access}get;"))),
                    0x1 => parts.push((1, format!("{access}set;"))),
                    _ => {}
                }
            }
            parts.sort();
            let indexer = if params.is_empty() { String::new() } else { format!("[{}]", params.join(", ")) };
            let statik = if is_static { "static " } else { "" };
            let text = format!(
                "{}{statik}{ty} {name}{indexer} {{ {} }}",
                Self::access(widest),
                parts.into_iter().map(|(_, p)| p).collect::<Vec<_>>().join(" ")
            );
            props.push((text, self.attributes_of(PROPERTY, p)));
        }
        for e in events_range.into_iter().flatten() {
            let name = self.h.str(t.get(EVENT, e, 1));
            let ty = self.type_def_or_ref(t.get(EVENT, e, 2), g);
            let accessors = semantics.get(&Self::coded(Coded::HasSemantics, EVENT, e)).cloned().unwrap_or_default();
            let widest = accessors.iter().map(|&(_, m)| accessor_access(m)).max().unwrap_or(0);
            events.push((format!("{}event {ty} {name}", Self::access(widest)), self.attributes_of(EVENT, e)));
        }
        (props, events)
    }

    // ---- Outline ----

    fn sorted_types(&self) -> Vec<u32> {
        let mut types: Vec<u32> = (1..=self.t.len(TYPE_DEF)).collect();
        types.sort_by(|a, b| self.type_names[*a as usize - 1].cmp(&self.type_names[*b as usize - 1]));
        types
    }

    /// Methods of a type: constructors first, then by name and signature.
    fn sorted_methods(&self, ty: u32) -> Vec<(u32, String)> {
        let mut methods: Vec<(u8, String, u32)> = self
            .t
            .range(TYPE_DEF, ty, 5, METHOD_DEF)
            .map(|m| {
                let name = self.h.str(self.t.get(METHOD_DEF, m, 3));
                let group = match name {
                    ".cctor" => 0,
                    ".ctor" => 1,
                    _ => 2,
                };
                (group, self.method_declaration(m), m)
            })
            .collect();
        methods.sort_by_cached_key(|(group, decl, _)| {
            let name = decl.split_once('(').map(|(h, _)| h.rsplit(' ').next().unwrap_or(h).to_string());
            (*group, name, decl.clone())
        });
        methods.into_iter().map(|(_, d, m)| (m, d)).collect()
    }

    fn outline(&self) -> String {
        let t = &self.t;
        let h = &self.h;
        let mut out = String::new();
        let _ = writeln!(out, "=== Assembly ===");
        if t.len(ASSEMBLY) > 0 {
            line(&mut out, "Name", h.str(t.get(ASSEMBLY, 1, 7)));
            let v: Vec<String> = (1..=4).map(|c| t.get(ASSEMBLY, 1, c).to_string()).collect();
            line(&mut out, "Version", v.join("."));
            let culture = h.str(t.get(ASSEMBLY, 1, 8));
            line(&mut out, "Culture", if culture.is_empty() { "neutral" } else { culture });
            let key = h.blob(t.get(ASSEMBLY, 1, 6));
            if !key.is_empty() {
                line(&mut out, "Public key token", hex(&public_key_token(key)));
            }
        } else {
            line(&mut out, "Module", h.str(t.get(MODULE, 1, 1)));
        }
        line(&mut out, "Runtime", &self.version);
        let cli_flags = u32_at(self.cli, 16).unwrap_or(0);
        let machine = match self.pe.machine {
            0x14C if cli_flags & 0x2 != 0 => "x86",
            0x14C if cli_flags & 0x20000 != 0 => "any CPU, 32-bit preferred",
            0x14C => "any CPU",
            0x8664 => "x64",
            0xAA64 => "ARM64",
            0x1C4 => "ARM",
            _ => "other",
        };
        line(&mut out, "Platform", machine);
        if cli_flags & 0x1 == 0 {
            line(&mut out, "Mixed mode", "contains native code");
        }
        line(&mut out, "Kind", if self.pe.characteristics & 0x2000 != 0 { "library" } else { "executable" });
        let entry = u32_at(self.cli, 20).unwrap_or(0);
        if entry != 0 && cli_flags & 0x10 == 0 {
            line(&mut out, "Entry point", self.token_text(entry));
        }

        let mut refs: Vec<String> = (1..=t.len(ASSEMBLY_REF))
            .map(|r| {
                let v: Vec<String> = (0..4).map(|c| t.get(ASSEMBLY_REF, r, c).to_string()).collect();
                let key = h.blob(t.get(ASSEMBLY_REF, r, 5));
                let token = match key.len() {
                    0 => String::new(),
                    8 => format!(", token {}", hex(key)),
                    _ => format!(", token {}", hex(&public_key_token(key))),
                };
                let culture = h.str(t.get(ASSEMBLY_REF, r, 7));
                let culture = if culture.is_empty() { String::new() } else { format!(", culture {culture}") };
                format!("{} {}{culture}{token}", h.str(t.get(ASSEMBLY_REF, r, 6)), v.join("."))
            })
            .collect();
        refs.sort();
        let _ = writeln!(out, "\n=== References ({}) ===", refs.len());
        for r in refs {
            let _ = writeln!(out, "{r}");
        }
        let mut modules: Vec<&str> = (1..=t.len(MODULE_REF)).map(|r| h.str(t.get(MODULE_REF, r, 0))).collect();
        if !modules.is_empty() {
            modules.sort();
            let _ = writeln!(out, "\n=== Native libraries ({}) ===", modules.len());
            for m in modules {
                let _ = writeln!(out, "{m}");
            }
        }
        let attrs = self.attributes_of(ASSEMBLY, 1);
        if !attrs.is_empty() {
            let _ = writeln!(out, "\n=== Assembly attributes ({}) ===", attrs.len());
            for a in attrs {
                let _ = writeln!(out, "{a}");
            }
        }

        let types = self.sorted_types();
        let _ = writeln!(out, "\n=== Types ({}) ===", types.len());
        let mut lines = out.lines().count();
        for (i, &ty) in types.iter().enumerate() {
            if lines > MAX_OUTLINE_LINES {
                let _ = writeln!(out, "… {} more types", types.len() - i);
                break;
            }
            let before = out.len();
            self.type_outline(ty, &mut out);
            lines += out[before..].bytes().filter(|&b| b == b'\n').count();
        }
        self.build_identity(&mut out);
        out
    }

    fn type_outline(&self, ty: u32, out: &mut String) {
        let t = &self.t;
        let g = self.generics_of_type(ty);
        let fields: Vec<u32> = t.range(TYPE_DEF, ty, 4, FIELD).collect();
        let methods = self.sorted_methods(ty);
        if ty == 1 && fields.is_empty() && methods.is_empty() {
            return; // <Module> without global members.
        }
        out.push('\n');
        for a in self.attributes_of(TYPE_DEF, ty) {
            let _ = writeln!(out, "{a}");
        }
        let _ = writeln!(out, "{}", self.type_declaration(ty));
        let is_enum = self.enum_underlying(TYPE_DEF, ty).is_some();
        let mut field_lines: Vec<(String, Vec<String>)> = fields
            .iter()
            .filter(|&&f| !(is_enum && self.h.str(t.get(FIELD, f, 1)) == "value__"))
            .map(|&f| (self.field_line(f, g), self.attributes_of(FIELD, f)))
            .collect();
        field_lines.sort_by(|a, b| member_name(&a.0).cmp(member_name(&b.0)));
        let (mut props, mut events) = self.properties_and_events(ty);
        props.sort_by(|a, b| member_name(&a.0).cmp(member_name(&b.0)));
        events.sort_by(|a, b| member_name(&a.0).cmp(member_name(&b.0)));
        for (text, attrs) in field_lines.into_iter().chain(props).chain(events) {
            for a in attrs {
                let _ = writeln!(out, "  {a}");
            }
            let _ = writeln!(out, "  {text}");
        }
        for (m, decl) in methods {
            for a in self.attributes_of(METHOD_DEF, m) {
                let _ = writeln!(out, "  {a}");
            }
            let _ = write!(out, "  {decl}");
            match self.body(m) {
                Some(Ok(body)) => {
                    let (crc, count) = self.checksum(m, &body);
                    let _ = writeln!(out, "  // IL {crc:08x}, {count} instructions");
                }
                Some(Err(e)) => {
                    let _ = writeln!(out, "\n  ! {e}");
                }
                None => out.push('\n'),
            }
        }
    }

    /// MVID and debug directory: what differs between builds of the same code.
    fn build_identity(&self, out: &mut String) {
        let _ = writeln!(out, "\n=== Build ===");
        if let Some(mvid) = self.h.guid(self.t.get(MODULE, 1, 2)) {
            line(out, "Module version id", guid(mvid));
        }
        let Some((rva, size)) = self.pe.dir(6) else { return };
        let Some(dir) = self.pe.slice(rva, size) else { return };
        for entry in dir.as_chunks::<28>().0.iter().take(16) {
            let kind = u32_at(entry, 12).unwrap_or(0);
            let (data_size, data_rva) = (u32_at(entry, 16).unwrap_or(0), u32_at(entry, 20).unwrap_or(0));
            match kind {
                2 => {
                    let Some(cv) = self.pe.slice(data_rva, data_size) else { continue };
                    if cv.starts_with(b"RSDS") && cv.len() > 24 {
                        let path = &cv[24..];
                        let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
                        line(out, "PDB", String::from_utf8_lossy(&path[..end]));
                    }
                }
                16 => line(out, "Deterministic", "yes"),
                17 => line(out, "Embedded PDB", format!("{data_size} bytes")),
                _ => {}
            }
        }
    }

    // ---- IL ----

    /// The method body of a MethodDef row; `None` when it has none (abstract, extern).
    fn body(&self, m: u32) -> Option<std::result::Result<Body<'a>, String>> {
        let rva = self.t.get(METHOD_DEF, m, 0);
        if rva == 0 {
            return None;
        }
        Some(Body::parse(self.pe.at(rva)).ok_or_else(|| "the IL could not be read".to_string()))
    }

    /// A checksum of a method's IL and its instruction count. Tokens are hashed as the
    /// names they stand for, so that renumbered tables do not change it.
    fn checksum(&self, m: u32, body: &Body) -> (u32, usize) {
        // Collected into one buffer and hashed once: many tiny hasher updates are slow.
        let mut buf: Vec<u8> = Vec::with_capacity(body.code.len() * 2 + 64);
        buf.extend_from_slice(&body.max_stack.to_le_bytes());
        buf.extend_from_slice(self.locals(body, self.generics_of_method(m)).as_bytes());
        let mut count = 0;
        for ins in Instructions::new(body.code) {
            count += 1;
            match ins {
                Ok(ins) => {
                    buf.extend_from_slice(ins.name.as_bytes());
                    match ins.operand {
                        Operand::None => {}
                        Operand::Token(tok) => buf.extend_from_slice(&self.token_hash(tok).to_le_bytes()),
                        Operand::Switch(targets) => {
                            for t in targets {
                                buf.extend_from_slice(&t.to_le_bytes());
                            }
                        }
                        Operand::Int(v) => buf.extend_from_slice(&v.to_le_bytes()),
                        Operand::Float(v) => buf.extend_from_slice(&v.to_le_bytes()),
                        Operand::Arg(v) | Operand::Local(v) | Operand::Branch(v) => {
                            buf.extend_from_slice(&v.to_le_bytes())
                        }
                    }
                }
                Err(at) => buf.extend_from_slice(&at.to_le_bytes()),
            }
        }
        for c in body.clauses() {
            buf.push(c.kind);
            for v in [c.try_start, c.try_end, c.handler_start, c.handler_end] {
                buf.extend_from_slice(&v.to_le_bytes());
            }
            if c.kind == 0 {
                buf.extend_from_slice(&self.token_hash(c.extra).to_le_bytes());
            }
        }
        (crc32fast::hash(&buf), count)
    }

    /// A hash of a token's text, memoized.
    fn token_hash(&self, token: u32) -> u32 {
        if let Some(&h) = self.hash_cache.borrow().get(&token) {
            return h;
        }
        let h = crc32fast::hash(self.resolve_token(token).as_bytes());
        self.hash_cache.borrow_mut().insert(token, h);
        h
    }

    fn locals(&self, body: &Body, g: Generics) -> String {
        if body.local_sig == 0 || (body.local_sig >> 24) as usize != STAND_ALONE_SIG {
            return String::new();
        }
        let mut s = Sig::new(self.h.blob(self.t.get(STAND_ALONE_SIG, body.local_sig & 0xFFFFFF, 0)));
        if s.u8() != Some(0x07) {
            return String::new();
        }
        let count = s.uint().unwrap_or(0);
        let types: Vec<String> =
            (0..count.min(4096)).map(|_| self.sig_type(&mut s, g, 0).unwrap_or_else(|| "?".into())).collect();
        types.join(", ")
    }

    /// The text of a metadata token: a member, type, string or signature, memoized.
    fn token_text(&self, token: u32) -> String {
        if let Some(text) = self.token_cache.borrow().get(&token) {
            return text.clone();
        }
        let text = self.resolve_token(token);
        self.token_cache.borrow_mut().insert(token, text.clone());
        text
    }

    fn resolve_token(&self, token: u32) -> String {
        let t = &self.t;
        let (table, row) = ((token >> 24) as usize, token & 0xFFFFFF);
        let none = Generics::default();
        match table {
            0x70 => quote(&self.h.user_string(row)),
            TYPE_DEF | TYPE_REF | TYPE_SPEC => self.type_name(table, row, none),
            FIELD => {
                let owner = self.field_owner.get(row as usize - 1).copied().unwrap_or(0);
                let g = self.generics_of_type(owner);
                format!(
                    "{}.{}: {}",
                    self.type_name(TYPE_DEF, owner, none),
                    self.h.str(t.get(FIELD, row, 1)),
                    self.field_type(self.h.blob(t.get(FIELD, row, 2)), g)
                )
            }
            METHOD_DEF => {
                let owner = self.method_owner.get(row as usize - 1).copied().unwrap_or(0);
                let mut s = Sig::new(self.h.blob(t.get(METHOD_DEF, row, 4)));
                self.member_text(
                    &self.type_name(TYPE_DEF, owner, none),
                    self.h.str(t.get(METHOD_DEF, row, 3)),
                    &mut s,
                    none,
                )
            }
            MEMBER_REF => {
                let (pt, pr) = Coded::MemberRefParent.decode(t.get(MEMBER_REF, row, 0));
                let owner = self.type_name(pt, pr, none);
                let mut s = Sig::new(self.h.blob(t.get(MEMBER_REF, row, 2)));
                self.member_text(&owner, self.h.str(t.get(MEMBER_REF, row, 1)), &mut s, none)
            }
            METHOD_SPEC => {
                let method = self.token_text({
                    let (mt, mr) = Coded::MethodDefOrRef.decode(t.get(METHOD_SPEC, row, 0));
                    ((mt as u32) << 24) | mr
                });
                let mut s = Sig::new(self.h.blob(t.get(METHOD_SPEC, row, 1)));
                let args = match (s.u8(), s.uint()) {
                    (Some(0x0A), Some(n)) => {
                        (0..n.min(64)).map(|_| self.sig_type(&mut s, none, 0).unwrap_or_else(|| "?".into())).collect()
                    }
                    _ => Vec::new(),
                };
                // Insert the type arguments after the method name.
                match method.find('(') {
                    Some(i) => format!("{}<{}>{}", &method[..i], args.join(", "), &method[i..]),
                    None => method,
                }
            }
            STAND_ALONE_SIG => {
                let mut s = Sig::new(self.h.blob(t.get(STAND_ALONE_SIG, row, 0)));
                match self.method_sig(&mut s, none, 0) {
                    Some(m) => format!("({}): {}", m.params.join(", "), m.ret),
                    None => "?".into(),
                }
            }
            _ => format!("token {token:08x}"),
        }
    }

    /// `Owner.name(params): ret` for methods, `Owner.name: type` for fields.
    fn member_text(&self, owner: &str, name: &str, s: &mut Sig, g: Generics) -> String {
        if s.peek() == Some(0x06) {
            s.u8();
            let ty = self.sig_type(s, g, 0).unwrap_or_else(|| "?".into());
            return format!("{owner}.{name}: {ty}");
        }
        match self.method_sig(s, g, 0) {
            Some(m) => {
                let statik = if m.has_this { "" } else { "static " };
                if name == ".ctor" {
                    format!("{owner}.ctor({})", m.params.join(", "))
                } else {
                    format!("{statik}{owner}.{name}({}): {}", m.params.join(", "), m.ret)
                }
            }
            None => format!("{owner}.{name}"),
        }
    }

    fn il(&self) -> String {
        let mut out = String::new();
        let mut printed = 0;
        'types: for ty in self.sorted_types() {
            let type_name = strip_arity_in_path(&self.type_names[ty as usize - 1]);
            for (m, decl) in self.sorted_methods(ty) {
                let Some(body) = self.body(m) else { continue };
                if printed > MAX_IL_INSTRUCTIONS {
                    let _ = writeln!(out, "… more methods not shown");
                    break 'types;
                }
                let _ = writeln!(out, "=== {type_name}: {decl} ===");
                match body {
                    Ok(body) => printed += self.method_il(m, &body, &mut out),
                    Err(e) => {
                        let _ = writeln!(out, "! {e}");
                    }
                }
                out.push('\n');
            }
        }
        if out.is_empty() {
            out.push_str("(no method has IL)\n");
        }
        out
    }

    fn method_il(&self, m: u32, body: &Body, out: &mut String) -> usize {
        let g = self.generics_of_method(m);
        let locals = self.locals(body, g);
        if !locals.is_empty() {
            let _ = writeln!(out, "  .locals ({locals})");
        }
        // Parameter names for ldarg and starg; argument 0 of instance methods is `this`.
        let t = &self.t;
        let is_static = t.get(METHOD_DEF, m, 2) & 0x10 != 0;
        let mut args: Vec<String> = if is_static { Vec::new() } else { vec!["this".into()] };
        let first = args.len();
        let param_rows = t.range(METHOD_DEF, m, 5, PARAM);
        let mut names: HashMap<usize, &str> = HashMap::new();
        for p in param_rows {
            names.insert(t.get(PARAM, p, 1) as usize, self.h.str(t.get(PARAM, p, 2)));
        }
        let mut s = Sig::new(self.h.blob(t.get(METHOD_DEF, m, 4)));
        let count = self.method_sig(&mut s, g, 0).map_or(0, |sig| sig.params.len());
        for i in 1..=count {
            args.push(
                names.get(&i).filter(|n| !n.is_empty()).map_or_else(|| (i + first - 1).to_string(), |n| n.to_string()),
            );
        }

        let instructions: Vec<(usize, std::result::Result<Instruction, usize>)> = {
            let mut it = Instructions::new(body.code);
            let mut list = Vec::new();
            loop {
                let at = it.pos;
                match it.next() {
                    Some(ins) => list.push((at, ins)),
                    None => break,
                }
            }
            list
        };
        let clauses = body.clauses();
        let mut targets = BTreeSet::new();
        for (_, ins) in &instructions {
            match ins {
                Ok(Instruction { operand: Operand::Branch(t), .. }) => {
                    targets.insert(*t);
                }
                Ok(Instruction { operand: Operand::Switch(list), .. }) => targets.extend(list.iter().copied()),
                _ => {}
            }
        }
        for c in &clauses {
            targets.extend([c.try_start, c.try_end, c.handler_start, c.handler_end]);
            if c.kind == 1 {
                targets.insert(c.extra);
            }
        }
        let labels: HashMap<u32, String> =
            targets.iter().enumerate().map(|(i, pc)| (*pc, format!("L{}", i + 1))).collect();
        let label = |pc: u32| labels.get(&pc).cloned().unwrap_or_else(|| format!("@{pc}"));
        for (at, ins) in &instructions {
            if let Some(l) = labels.get(&(*at as u32)) {
                let _ = writeln!(out, "  {l}:");
            }
            let ins = match ins {
                Ok(ins) => ins,
                Err(_) => {
                    let _ = writeln!(out, "  ! unknown instruction");
                    continue;
                }
            };
            let _ = write!(out, "    {}", ins.name);
            match &ins.operand {
                Operand::None => {}
                Operand::Int(v) => {
                    let _ = write!(out, " {v}");
                }
                Operand::Float(v) => {
                    let _ = write!(out, " {}", float(*v));
                }
                Operand::Arg(i) => {
                    let _ = write!(out, " {}", args.get(*i as usize).cloned().unwrap_or_else(|| i.to_string()));
                }
                Operand::Local(i) => {
                    let _ = write!(out, " {i}");
                }
                Operand::Branch(t) => {
                    let _ = write!(out, " {}", label(*t));
                }
                Operand::Switch(list) => {
                    let _ = write!(out, " {}", list.iter().map(|t| label(*t)).collect::<Vec<_>>().join(" "));
                }
                Operand::Token(tok) => {
                    let _ = write!(out, " {}", self.token_text(*tok));
                }
            }
            out.push('\n');
        }
        if let Some(l) = labels.get(&(body.code.len() as u32)) {
            let _ = writeln!(out, "  {l}:");
        }
        for c in &clauses {
            let what = match c.kind {
                0 => format!("catch {}", self.token_text(c.extra)),
                1 => format!("filter {}", label(c.extra)),
                2 => "finally".to_string(),
                _ => "fault".to_string(),
            };
            let _ = writeln!(
                out,
                "  try {}..{} {what} {}..{}",
                label(c.try_start),
                label(c.try_end),
                label(c.handler_start),
                label(c.handler_end)
            );
        }
        instructions.len()
    }
}

fn member_name(declaration: &str) -> &str {
    let head = declaration.split(['(', '{', '=', '[']).next().unwrap_or(declaration).trim_end();
    head.rsplit(' ').next().unwrap_or(head)
}

/// `Shop.Warehouse`1` → `Shop.Warehouse` in a declaration (arity markers of nested types too).
fn strip_arity_in_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' && chars.peek().is_some_and(char::is_ascii_digit) {
            while chars.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn guid(b: &[u8]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{}-{}",
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        u16::from_le_bytes([b[4], b[5]]),
        u16::from_le_bytes([b[6], b[7]]),
        hex(&b[8..10]),
        hex(&b[10..16])
    )
}

/// The last 8 bytes of the SHA-1 of the public key, reversed.
fn public_key_token(key: &[u8]) -> Vec<u8> {
    let hash = sha1(key);
    hash[12..].iter().rev().copied().collect()
}

/// SHA-1, only for public key tokens (no security relevance).
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.as_chunks::<64>().0.iter() {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap_or_default());
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        for (x, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

// ---- Attribute values ----

/// The type of a serialized attribute argument.
enum SerType {
    Prim(u8),
    String,
    Type,
    Object,
    Array(Box<SerType>),
}

fn ser_string(v: &mut Sig) -> Option<Option<String>> {
    if v.peek()? == 0xFF {
        v.u8();
        return Some(None);
    }
    let len = v.uint()? as usize;
    Some(Some(String::from_utf8_lossy(v.bytes(len)?).into_owned()))
}

/// The type of a named argument or boxed value, written in the value blob.
fn ser_type_of_value(v: &mut Sig, depth: u32) -> Option<SerType> {
    if depth > 8 {
        return None;
    }
    let e = v.u8()?;
    Some(match e {
        0x02..=0x0D => SerType::Prim(e),
        0x0E => SerType::String,
        0x50 => SerType::Type,
        0x51 => SerType::Object,
        0x1D => SerType::Array(Box::new(ser_type_of_value(v, depth + 1)?)),
        0x55 => {
            ser_string(v)?; // The enum's type name; its underlying type is assumed to be int.
            SerType::Prim(0x08)
        }
        _ => return None,
    })
}

fn ser_value(v: &mut Sig, ty: &SerType, depth: u32) -> Option<String> {
    if depth > 8 {
        return None;
    }
    Some(match ty {
        SerType::Prim(e) => {
            let n = match e {
                0x02 | 0x04 | 0x05 => 1,
                0x03 | 0x06 | 0x07 => 2,
                0x08 | 0x09 | 0x0C => 4,
                _ => 8,
            };
            let b = v.bytes(n)?;
            match e {
                0x02 => (b[0] != 0).to_string(),
                0x03 => format!("'{}'", char::from_u32(u32::from(u16::from_le_bytes([b[0], b[1]]))).unwrap_or('?')),
                0x04 => (b[0] as i8).to_string(),
                0x05 => b[0].to_string(),
                0x06 => i16::from_le_bytes([b[0], b[1]]).to_string(),
                0x07 => u16::from_le_bytes([b[0], b[1]]).to_string(),
                0x08 => i32::from_le_bytes(b.try_into().ok()?).to_string(),
                0x09 => u32::from_le_bytes(b.try_into().ok()?).to_string(),
                0x0A => i64::from_le_bytes(b.try_into().ok()?).to_string(),
                0x0B => u64::from_le_bytes(b.try_into().ok()?).to_string(),
                0x0C => float(f64::from(f32::from_le_bytes(b.try_into().ok()?))) + "f",
                _ => float(f64::from_le_bytes(b.try_into().ok()?)),
            }
        }
        SerType::String => ser_string(v)?.map_or_else(|| "null".into(), |s| quote(&s)),
        SerType::Type => {
            ser_string(v)?.map_or_else(|| "null".into(), |s| format!("typeof({})", s.split(',').next().unwrap_or(&s)))
        }
        SerType::Object => {
            let inner = ser_type_of_value(v, depth + 1)?;
            ser_value(v, &inner, depth + 1)?
        }
        SerType::Array(elem) => {
            let n = u32::from_le_bytes(v.bytes(4)?.try_into().ok()?);
            if n == u32::MAX {
                return Some("null".into());
            }
            let items: Vec<String> = (0..n.min(4096)).map(|_| ser_value(v, elem, depth + 1)).collect::<Option<_>>()?;
            format!("[{}]", items.join(", "))
        }
    })
}

// ---- Method bodies and instructions ----

struct Body<'a> {
    max_stack: u16,
    local_sig: u32,
    code: &'a [u8],
    /// The exception handling sections after the code.
    sections: &'a [u8],
    /// Header, code and sections: the bytes the similar check compares.
    raw: &'a [u8],
}

struct Clause {
    kind: u8,
    try_start: u32,
    try_end: u32,
    handler_start: u32,
    handler_end: u32,
    /// The class token of a catch, or the filter offset.
    extra: u32,
}

impl<'a> Body<'a> {
    fn parse(at: Option<&'a [u8]>) -> Option<Self> {
        let b = at?;
        let first = *b.first()?;
        if first & 3 == 2 {
            let size = (first >> 2) as usize;
            let code = b.get(1..1 + size)?;
            return Some(Body { max_stack: 8, local_sig: 0, code, sections: &[], raw: &b[..1 + size] });
        }
        if first & 3 != 3 {
            return None;
        }
        let flags = u16_at(b, 0)?;
        let header = ((flags >> 12) as usize) * 4;
        let size = u32_at(b, 4)? as usize;
        let code_end = header.checked_add(size)?;
        let code = b.get(header..code_end)?;
        let mut end = code_end;
        if flags & 0x8 != 0 {
            // Sections are 4-byte aligned; each says whether another follows.
            loop {
                end = end.div_ceil(4) * 4;
                let kind = *b.get(end)?;
                let len = if kind & 0x40 != 0 {
                    u32::from_le_bytes([*b.get(end + 1)?, *b.get(end + 2)?, *b.get(end + 3)?, 0]) as usize
                } else {
                    *b.get(end + 1)? as usize
                };
                end = end.checked_add(len.max(4))?;
                if kind & 0x80 == 0 {
                    break;
                }
            }
        }
        b.get(..end)?;
        Some(Body {
            max_stack: u16_at(b, 2)?,
            local_sig: u32_at(b, 8)?,
            code,
            sections: &b[code_end..end],
            raw: &b[..end],
        })
    }

    fn clauses(&self) -> Vec<Clause> {
        let mut out = Vec::new();
        let s = self.sections;
        let mut pos = (self.raw.len() - s.len()).div_ceil(4) * 4 - (self.raw.len() - s.len());
        while pos < s.len() {
            let kind = s[pos];
            let fat = kind & 0x40 != 0;
            let len = if fat {
                s.get(pos + 1..pos + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], 0]) as usize)
            } else {
                s.get(pos + 1).map_or(0, |&b| b as usize)
            };
            if len < 4 {
                break;
            }
            if kind & 0x3F == 1 {
                let (size, start) = if fat { (24, pos + 4) } else { (12, pos + 4) };
                let mut c = start;
                while c + size <= (pos + len).min(s.len()) && out.len() < 4096 {
                    let e = &s[c..c + size];
                    let clause = if fat {
                        Clause {
                            kind: e[0],
                            try_start: u32_at(e, 4).unwrap_or(0),
                            try_end: u32_at(e, 4).unwrap_or(0).saturating_add(u32_at(e, 8).unwrap_or(0)),
                            handler_start: u32_at(e, 12).unwrap_or(0),
                            handler_end: u32_at(e, 12).unwrap_or(0).saturating_add(u32_at(e, 16).unwrap_or(0)),
                            extra: u32_at(e, 20).unwrap_or(0),
                        }
                    } else {
                        let try_start = u32::from(u16_at(e, 2).unwrap_or(0));
                        let handler_start = u32::from(u16_at(e, 5).unwrap_or(0));
                        Clause {
                            kind: e[0],
                            try_start,
                            try_end: try_start + u32::from(e[4]),
                            handler_start,
                            handler_end: handler_start + u32::from(e[7]),
                            extra: u32_at(e, 8).unwrap_or(0),
                        }
                    };
                    out.push(clause);
                    c += size;
                }
            }
            if kind & 0x80 == 0 {
                break;
            }
            pos = (pos + len).div_ceil(4) * 4;
        }
        out
    }
}

#[derive(Debug)]
enum Operand {
    None,
    Int(i64),
    Float(f64),
    Arg(u32),
    Local(u32),
    /// An absolute target offset.
    Branch(u32),
    Switch(Vec<u32>),
    Token(u32),
}

struct Instruction {
    /// The mnemonic, short and long forms folded (`br.s` → `br`, `ldarg.1` → `ldarg`).
    name: &'static str,
    operand: Operand,
}

/// Operand encodings (ECMA-335 III.1.9).
#[derive(Clone, Copy)]
enum K {
    None,
    I8,
    U8Arg,
    U8Local,
    U8,
    U16Arg,
    U16Local,
    I32,
    I64,
    R4,
    R8,
    Tok,
    Br8,
    Br32,
    Switch,
    /// A fixed argument or local index, or a fixed constant: `ldarg.1`, `ldc.i4.3`.
    FixedArg(u32),
    FixedLocal(u32),
    FixedInt(i64),
}

fn opcode(b: u8) -> Option<(&'static str, K)> {
    use K::*;
    Some(match b {
        0x00 => ("nop", None),
        0x01 => ("break", None),
        0x02..=0x05 => ("ldarg", FixedArg(u32::from(b - 0x02))),
        0x06..=0x09 => ("ldloc", FixedLocal(u32::from(b - 0x06))),
        0x0A..=0x0D => ("stloc", FixedLocal(u32::from(b - 0x0A))),
        0x0E => ("ldarg", U8Arg),
        0x0F => ("ldarga", U8Arg),
        0x10 => ("starg", U8Arg),
        0x11 => ("ldloc", U8Local),
        0x12 => ("ldloca", U8Local),
        0x13 => ("stloc", U8Local),
        0x14 => ("ldnull", None),
        0x15..=0x1E => ("ldc.i4", FixedInt(i64::from(b) - 0x16)),
        0x1F => ("ldc.i4", I8),
        0x20 => ("ldc.i4", I32),
        0x21 => ("ldc.i8", I64),
        0x22 => ("ldc.r4", R4),
        0x23 => ("ldc.r8", R8),
        0x25 => ("dup", None),
        0x26 => ("pop", None),
        0x27 => ("jmp", Tok),
        0x28 => ("call", Tok),
        0x29 => ("calli", Tok),
        0x2A => ("ret", None),
        0x2B..=0x37 => (BRANCHES[(b - 0x2B) as usize], Br8),
        0x38..=0x44 => (BRANCHES[(b - 0x38) as usize], Br32),
        0x45 => ("switch", Switch),
        0x46..=0x57 => (
            [
                "ldind.i1",
                "ldind.u1",
                "ldind.i2",
                "ldind.u2",
                "ldind.i4",
                "ldind.u4",
                "ldind.i8",
                "ldind.i",
                "ldind.r4",
                "ldind.r8",
                "ldind.ref",
                "stind.ref",
                "stind.i1",
                "stind.i2",
                "stind.i4",
                "stind.i8",
                "stind.r4",
                "stind.r8",
            ][(b - 0x46) as usize],
            None,
        ),
        0x58..=0x6E => (
            [
                "add", "sub", "mul", "div", "div.un", "rem", "rem.un", "and", "or", "xor", "shl", "shr", "shr.un",
                "neg", "not", "conv.i1", "conv.i2", "conv.i4", "conv.i8", "conv.r4", "conv.r8", "conv.u4", "conv.u8",
            ][(b - 0x58) as usize],
            None,
        ),
        0x6F => ("callvirt", Tok),
        0x70 => ("cpobj", Tok),
        0x71 => ("ldobj", Tok),
        0x72 => ("ldstr", Tok),
        0x73 => ("newobj", Tok),
        0x74 => ("castclass", Tok),
        0x75 => ("isinst", Tok),
        0x76 => ("conv.r.un", None),
        0x79 => ("unbox", Tok),
        0x7A => ("throw", None),
        0x7B => ("ldfld", Tok),
        0x7C => ("ldflda", Tok),
        0x7D => ("stfld", Tok),
        0x7E => ("ldsfld", Tok),
        0x7F => ("ldsflda", Tok),
        0x80 => ("stsfld", Tok),
        0x81 => ("stobj", Tok),
        0x82..=0x8B => (
            [
                "conv.ovf.i1.un",
                "conv.ovf.i2.un",
                "conv.ovf.i4.un",
                "conv.ovf.i8.un",
                "conv.ovf.u1.un",
                "conv.ovf.u2.un",
                "conv.ovf.u4.un",
                "conv.ovf.u8.un",
                "conv.ovf.i.un",
                "conv.ovf.u.un",
            ][(b - 0x82) as usize],
            None,
        ),
        0x8C => ("box", Tok),
        0x8D => ("newarr", Tok),
        0x8E => ("ldlen", None),
        0x8F => ("ldelema", Tok),
        0x90..=0xA2 => (
            [
                "ldelem.i1",
                "ldelem.u1",
                "ldelem.i2",
                "ldelem.u2",
                "ldelem.i4",
                "ldelem.u4",
                "ldelem.i8",
                "ldelem.i",
                "ldelem.r4",
                "ldelem.r8",
                "ldelem.ref",
                "stelem.i",
                "stelem.i1",
                "stelem.i2",
                "stelem.i4",
                "stelem.i8",
                "stelem.r4",
                "stelem.r8",
                "stelem.ref",
            ][(b - 0x90) as usize],
            None,
        ),
        0xA3 => ("ldelem", Tok),
        0xA4 => ("stelem", Tok),
        0xA5 => ("unbox.any", Tok),
        0xB3..=0xBA => (
            [
                "conv.ovf.i1",
                "conv.ovf.u1",
                "conv.ovf.i2",
                "conv.ovf.u2",
                "conv.ovf.i4",
                "conv.ovf.u4",
                "conv.ovf.i8",
                "conv.ovf.u8",
            ][(b - 0xB3) as usize],
            None,
        ),
        0xC2 => ("refanyval", Tok),
        0xC3 => ("ckfinite", None),
        0xC6 => ("mkrefany", Tok),
        0xD0 => ("ldtoken", Tok),
        0xD1 => ("conv.u2", None),
        0xD2 => ("conv.u1", None),
        0xD3 => ("conv.i", None),
        0xD4 => ("conv.ovf.i", None),
        0xD5 => ("conv.ovf.u", None),
        0xD6 => ("add.ovf", None),
        0xD7 => ("add.ovf.un", None),
        0xD8 => ("mul.ovf", None),
        0xD9 => ("mul.ovf.un", None),
        0xDA => ("sub.ovf", None),
        0xDB => ("sub.ovf.un", None),
        0xDC => ("endfinally", None),
        0xDD => ("leave", Br32),
        0xDE => ("leave", Br8),
        0xDF => ("stind.i", None),
        0xE0 => ("conv.u", None),
        _ => return Option::None,
    })
}

const BRANCHES: [&str; 13] =
    ["br", "brfalse", "brtrue", "beq", "bge", "bgt", "ble", "blt", "bne.un", "bge.un", "bgt.un", "ble.un", "blt.un"];

fn opcode_fe(b: u8) -> Option<(&'static str, K)> {
    use K::*;
    Some(match b {
        0x00 => ("arglist", None),
        0x01 => ("ceq", None),
        0x02 => ("cgt", None),
        0x03 => ("cgt.un", None),
        0x04 => ("clt", None),
        0x05 => ("clt.un", None),
        0x06 => ("ldftn", Tok),
        0x07 => ("ldvirtftn", Tok),
        0x09 => ("ldarg", U16Arg),
        0x0A => ("ldarga", U16Arg),
        0x0B => ("starg", U16Arg),
        0x0C => ("ldloc", U16Local),
        0x0D => ("ldloca", U16Local),
        0x0E => ("stloc", U16Local),
        0x0F => ("localloc", None),
        0x11 => ("endfilter", None),
        0x12 => ("unaligned.", U8),
        0x13 => ("volatile.", None),
        0x14 => ("tail.", None),
        0x15 => ("initobj", Tok),
        0x16 => ("constrained.", Tok),
        0x17 => ("cpblk", None),
        0x18 => ("initblk", None),
        0x19 => ("no.", U8),
        0x1A => ("rethrow", None),
        0x1C => ("sizeof", Tok),
        0x1D => ("refanytype", None),
        0x1E => ("readonly.", None),
        _ => return Option::None,
    })
}

/// Decodes IL; yields `Err(offset)` for an unknown opcode and stops there.
struct Instructions<'c> {
    code: &'c [u8],
    pos: usize,
    failed: bool,
}

impl<'c> Instructions<'c> {
    fn new(code: &'c [u8]) -> Self {
        Instructions { code, pos: 0, failed: false }
    }

    fn take(&mut self, n: usize) -> Option<&'c [u8]> {
        let s = self.code.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }

    fn decode(&mut self) -> Option<Instruction> {
        let b = *self.take(1)?.first()?;
        let (name, kind) = if b == 0xFE { opcode_fe(*self.take(1)?.first()?)? } else { opcode(b)? };
        let i32_ = |s: &[u8]| i32::from_le_bytes([s[0], s[1], s[2], s[3]]);
        let operand = match kind {
            K::None => Operand::None,
            K::FixedArg(i) => Operand::Arg(i),
            K::FixedLocal(i) => Operand::Local(i),
            K::FixedInt(v) => Operand::Int(v),
            K::I8 => Operand::Int(i64::from(self.take(1)?[0] as i8)),
            K::U8 => Operand::Int(i64::from(self.take(1)?[0])),
            K::U8Arg => Operand::Arg(u32::from(self.take(1)?[0])),
            K::U8Local => Operand::Local(u32::from(self.take(1)?[0])),
            K::U16Arg => Operand::Arg(u32::from(u16_at(self.take(2)?, 0)?)),
            K::U16Local => Operand::Local(u32::from(u16_at(self.take(2)?, 0)?)),
            K::I32 => Operand::Int(i64::from(i32_(self.take(4)?))),
            K::I64 => Operand::Int(i64::from_le_bytes(self.take(8)?.try_into().ok()?)),
            K::R4 => Operand::Float(f64::from(f32::from_le_bytes(self.take(4)?.try_into().ok()?))),
            K::R8 => Operand::Float(f64::from_le_bytes(self.take(8)?.try_into().ok()?)),
            K::Tok => Operand::Token(u32_at(self.take(4)?, 0)?),
            K::Br8 => {
                let d = i64::from(self.take(1)?[0] as i8);
                Operand::Branch((self.pos as i64 + d) as u32)
            }
            K::Br32 => {
                let d = i64::from(i32_(self.take(4)?));
                Operand::Branch((self.pos as i64 + d) as u32)
            }
            K::Switch => {
                let n = u32_at(self.take(4)?, 0)? as usize;
                let table = self.take(n.checked_mul(4)?)?;
                let base = self.pos as i64;
                Operand::Switch(table.as_chunks::<4>().0.iter().map(|c| (base + i64::from(i32_(c))) as u32).collect())
            }
        };
        Some(Instruction { name, operand })
    }
}

impl Iterator for Instructions<'_> {
    type Item = std::result::Result<Instruction, usize>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.pos >= self.code.len() {
            return None;
        }
        let at = self.pos;
        match self.decode() {
            Some(i) => Some(Ok(i)),
            None => {
                self.failed = true;
                Some(Err(at))
            }
        }
    }
}

// ---- The similar check ----

/// Everything of an assembly that is code or API, with build identity removed:
/// the tables with method body RVAs and field data RVAs zeroed, the heaps with the
/// MVID zeroed, the method bodies, field initial data and managed resources.
#[derive(PartialEq)]
struct Canonical<'a> {
    cli_flags: u32,
    entry_point: u32,
    tables: Vec<u8>,
    heaps: [&'a [u8]; 3],
    guids: Vec<u8>,
    bodies: Vec<&'a [u8]>,
    field_data: Vec<&'a [u8]>,
    resources: &'a [u8],
}

fn canonical(data: &[u8]) -> Option<Canonical<'_>> {
    let a = Assembly::parse(data).ok()?;
    let t = &a.t;
    let mut tables = t.data.to_vec();
    let mut zero = |table: usize, col: usize| {
        let (offset, width) = t.cols[table].get(col).copied()?;
        for r in 0..t.rows[table] as usize {
            let at = t.start[table] + r * t.row_size[table] + offset;
            tables.get_mut(at..at + width)?.fill(0);
        }
        Some(())
    };
    zero(METHOD_DEF, 0);
    zero(FIELD_RVA, 0);
    let mut guids = a.h.guid.to_vec();
    let mvid = t.get(MODULE, 1, 2) as usize;
    if mvid >= 1 {
        guids.get_mut((mvid - 1) * 16..mvid * 16)?.fill(0);
    }
    let mut bodies = Vec::new();
    for m in 1..=t.len(METHOD_DEF) {
        match a.body(m) {
            Some(Ok(body)) => bodies.push(body.raw),
            Some(Err(_)) => return None,
            None => {}
        }
    }
    let mut field_data = Vec::new();
    for r in 1..=t.len(FIELD_RVA) {
        let field = t.get(FIELD_RVA, r, 1);
        let size = field_data_size(&a, field)?;
        field_data.push(a.pe.slice(t.get(FIELD_RVA, r, 0), size)?);
    }
    let resources = match u32_at(a.cli, 24).zip(u32_at(a.cli, 28)) {
        Some((rva, size)) if rva != 0 && size != 0 => a.pe.slice(rva, size)?,
        _ => &[],
    };
    Some(Canonical {
        cli_flags: u32_at(a.cli, 16)?,
        entry_point: u32_at(a.cli, 20)?,
        tables,
        heaps: [a.h.strings, a.h.us, a.h.blob],
        guids,
        bodies,
        field_data,
        resources,
    })
}

/// The size of a field's initial data: from its value type's class layout, or
/// the size of a primitive. `None` when it cannot be known.
fn field_data_size(a: &Assembly, field: u32) -> Option<u32> {
    let sig = a.h.blob(a.t.get(FIELD, field, 2));
    let mut s = Sig::new(sig);
    if s.u8()? != 0x06 {
        return None;
    }
    match s.u8()? {
        0x02 | 0x04 | 0x05 => Some(1),
        0x03 | 0x06 | 0x07 => Some(2),
        0x08 | 0x09 | 0x0C => Some(4),
        0x0A | 0x0B | 0x0D => Some(8),
        0x11 => {
            let (table, row) = Coded::TypeDefOrRef.decode(s.uint()?);
            if table != TYPE_DEF {
                return None;
            }
            a.class_size.get(&row).copied()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dotnet/", $name))
        };
    }
    const V1: &[u8] = fixture!("Inventory-v1.dll");
    const V1_REBUILT: &[u8] = fixture!("Inventory-v1-rebuilt.dll");
    const V1_RELEASE: &[u8] = fixture!("Inventory-v1-release.dll");
    const V2: &[u8] = fixture!("Inventory-v2.dll");

    const NATIVE: &[u8] = fixture!("native.dll");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn outline_of_an_assembly() {
        let o = outline(V1).unwrap();
        for expected in [
            "Name:              Inventory\nVersion:           1.2.0.0\nCulture:           neutral\n",
            "System.Runtime 8.0.0.0, token b03f5f7f11d50a3a",
            "[System.Reflection.AssemblyTitle(\"Inventory\")]",
            "[System.Runtime.CompilerServices.RuntimeCompatibility(WrapNonExceptionThrows = true)]",
            "public sealed class Shop.Stock.Item : Shop.Stock.IPriced, System.IComparable<Shop.Stock.Item>\n",
            "  public const int MaxNameLength = 40\n",
            "  public decimal Price { get; private set; }\n",
            "  public Item(string name, decimal price)  // IL ",
            "public enum Shop.Stock.Unit : byte\n",
            "  public const Shop.Stock.Unit Litre = 10\n",
            "public class Shop.Stock.Warehouse<T> where T : class, Shop.Stock.IPriced\n",
            "  public event System.EventHandler Changed\n",
            "  protected internal virtual int Count(System.Func<T, bool> filter)  // IL ",
            "  internal static string Describe(object o, ref int counter, out bool ok)  // IL ",
            "PDB:               /build/a/Inventory.pdb",
        ] {
            assert!(o.contains(expected), "{expected}\n---\n{o}");
        }
    }

    #[test]
    fn il_is_symbolic() {
        let text = il(V1).unwrap();
        for expected in [
            "=== Shop.Stock.Item: public void Discount(int percent) ===\n    nop\n    ldarg this\n    ldarg this\n    call Shop.Stock.Item.get_Price(): decimal\n    ldc.i4 100\n    ldarg percent\n",
            "    ldstr \"name too long\"\n",
            "    newobj System.ArgumentException.ctor(string, string)\n",
            "    call static System.Threading.Interlocked.CompareExchange<System.EventHandler>(ref !!0, !!0, !!0): !!0\n",
            "  try L1..L4 finally L4..L5\n",
        ] {
            assert!(text.contains(expected), "{expected}\n---\n{text}");
        }
        assert!(!text.contains("token "), "every token is resolved:\n{text}");
    }

    #[test]
    fn a_diff_shows_only_what_changed() {
        let (o1, o2) = (outline(V1).unwrap(), outline(V2).unwrap());
        let strip = |l: &str| l.split("  // IL").next().unwrap().to_string();
        let removed: Vec<String> = lines_only_in(&o1, &o2).into_iter().map(strip).collect();
        let mvid = o1.lines().find(|l| l.starts_with("Module version id:")).unwrap();
        assert_eq!(removed, ["Version:           1.2.0.0", "  public void Discount(int percent)", mvid]);
        let added: Vec<String> = lines_only_in(&o2, &o1).into_iter().map(strip).collect();
        assert!(added.contains(&"  public bool IsExpensive(decimal limit)".to_string()), "{added:?}");
        assert_eq!(added.len(), 4, "{added:?}");
    }

    #[test]
    fn same_code_ignores_build_identity_only() {
        assert_eq!(same_code(V1, V1_REBUILT), Some(true));
        assert_ne!(outline(V1).unwrap(), outline(V1_REBUILT).unwrap(), "the outline shows the MVID and PDB");
        assert_eq!(same_code(V1, V1_RELEASE), Some(false), "optimized IL differs");
        assert_eq!(same_code(V1, V2), Some(false));
        assert_eq!(same_code(V1, NATIVE), None);
    }

    #[test]
    fn detects_managed_assemblies_only() {
        assert!(is_managed(V1));
        assert!(!is_managed(NATIVE));
        assert!(!is_managed(b"MZ\x90\0"));
        assert!(outline(NATIVE).is_err());
    }

    #[test]
    fn truncated_or_garbage_input_never_panics() {
        for len in (0..V1.len()).step_by(3) {
            let _ = outline(&V1[..len]);
            let _ = il(&V1[..len]);
            let _ = same_code(&V1[..len], V1);
        }
        let mut corrupt = V1.to_vec();
        for i in (0x80..corrupt.len()).step_by(5) {
            corrupt[i] = corrupt[i].wrapping_add(0x5B);
            let _ = outline(&corrupt);
            let _ = il(&corrupt);
            let _ = same_code(&corrupt, V1);
        }
    }
}
