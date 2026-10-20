//! Checked readers for the standard BFBS reflection schema.
//! Layout reference: google/flatbuffers, reflection/reflection.fbs.

use crate::{Error, Result};
use serde::Serialize;
use std::collections::BTreeSet;

pub(super) fn bad(message: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("FlatBuffers: {message}"))
}

#[derive(Clone, Copy)]
pub(super) struct Wire<'a> {
    pub bytes: &'a [u8],
}

impl<'a> Wire<'a> {
    pub fn slice(&self, at: usize, size: usize) -> Result<&'a [u8]> {
        self.bytes
            .get(at..at.checked_add(size).ok_or_else(|| bad("offset overflow"))?)
            .ok_or_else(|| bad(format!("truncated or out-of-bounds data at byte {at}")))
    }
    pub fn u8(&self, at: usize) -> Result<u8> {
        Ok(self.slice(at, 1)?[0])
    }
    pub fn u16(&self, at: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(self.slice(at, 2)?.try_into().expect("two bytes")))
    }
    pub fn u32(&self, at: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.slice(at, 4)?.try_into().expect("four bytes")))
    }
    pub fn u64(&self, at: usize) -> Result<u64> {
        Ok(u64::from_le_bytes(self.slice(at, 8)?.try_into().expect("eight bytes")))
    }
    pub fn i32(&self, at: usize) -> Result<i32> {
        Ok(self.u32(at)? as i32)
    }
    pub fn indirect(&self, at: usize) -> Result<usize> {
        let offset = self.u32(at)? as usize;
        if offset < 4 {
            return Err(bad("invalid forward offset"));
        }
        let target = at.checked_add(offset).ok_or_else(|| bad("offset overflow"))?;
        self.slice(target, 1)?;
        Ok(target)
    }
    pub fn table(&self, at: usize) -> Result<Table> {
        let vtable = (at as i64)
            .checked_sub(self.i32(at)? as i64)
            .filter(|v| *v >= 0)
            .ok_or_else(|| bad("invalid vtable offset"))? as usize;
        let size = self.u16(vtable)? as usize;
        let object_size = self.u16(vtable + 2)? as usize;
        if size < 4 || !size.is_multiple_of(2) || object_size < 4 {
            return Err(bad("invalid table size"));
        }
        self.slice(vtable, size)?;
        self.slice(at, object_size)?;
        if vtable < at + object_size && at < vtable + size {
            return Err(bad("vtable overlaps its table"));
        }
        Ok(Table { at, vtable, slots: (size - 4) / 2, object_size })
    }
    pub fn string(&self, at: usize) -> Result<String> {
        let size = self.u32(at)? as usize;
        let bytes = self.slice(at + 4, size)?;
        if self.u8(at + 4 + size)? != 0 {
            return Err(bad("string has no terminator"));
        }
        std::str::from_utf8(bytes).map(str::to_string).map_err(|_| bad("string is not valid UTF-8"))
    }
    pub fn vector(&self, at: usize, width: usize) -> Result<(usize, usize)> {
        let len = self.u32(at)? as usize;
        if len > 1_000_000 {
            return Err(bad("vector exceeds the one-million-element limit"));
        }
        self.slice(at + 4, len.checked_mul(width).ok_or_else(|| bad("vector size overflow"))?)?;
        Ok((at + 4, len))
    }
}

#[derive(Clone, Copy)]
pub(super) struct Table {
    pub at: usize,
    pub vtable: usize,
    pub slots: usize,
    pub object_size: usize,
}
impl Table {
    pub fn field(&self, wire: Wire<'_>, id: usize) -> Result<Option<usize>> {
        if id >= self.slots {
            return Ok(None);
        }
        let offset = wire.u16(self.vtable + 4 + id * 2)? as usize;
        if offset == 0 {
            Ok(None)
        } else if offset < 4 || offset >= self.object_size {
            Err(bad("field offset is outside its table"))
        } else {
            Ok(Some(self.at + offset))
        }
    }
    fn integer(&self, wire: Wire<'_>, id: usize, default: i64, width: usize) -> Result<i64> {
        self.field(wire, id)?.map_or(Ok(default), |at| {
            Ok(match width {
                1 => wire.u8(at)? as i64,
                2 => wire.u16(at)? as i64,
                4 => wire.i32(at)? as i64,
                _ => wire.u64(at)? as i64,
            })
        })
    }
    fn pointed(&self, wire: Wire<'_>, id: usize) -> Result<Option<usize>> {
        self.field(wire, id)?.map(|at| wire.indirect(at)).transpose()
    }
    fn text(&self, wire: Wire<'_>, id: usize) -> Result<Option<String>> {
        self.pointed(wire, id)?.map(|at| wire.string(at)).transpose()
    }
    fn tables(&self, wire: Wire<'_>, id: usize) -> Result<Vec<usize>> {
        let Some(at) = self.pointed(wire, id)? else {
            return Ok(Vec::new());
        };
        let (start, len) = wire.vector(at, 4)?;
        (0..len).map(|i| wire.indirect(start + i * 4)).collect()
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(super) struct Type {
    pub base: u8,
    pub element: u8,
    pub index: i32,
    pub length: usize,
}
impl Type {
    fn read(wire: Wire<'_>, at: usize) -> Result<Self> {
        let t = wire.table(at)?;
        let base = t.integer(wire, 0, 0, 1)? as u8;
        let element = t.integer(wire, 1, 0, 1)? as u8;
        if base > 17 || element > 17 {
            return Err(bad("64-bit offsets or unknown schema types are not supported"));
        }
        Ok(Self { base, element, index: t.integer(wire, 2, -1, 4)? as i32, length: t.integer(wire, 3, 0, 2)? as usize })
    }
    pub fn element(self) -> Self {
        Self { base: self.element, element: 0, index: self.index, length: 0 }
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Field {
    pub name: String,
    pub ty: Type,
    pub id: usize,
    pub offset: usize,
    pub default_integer: i64,
    pub default_real: u64,
    pub required: bool,
    pub optional: bool,
}
#[derive(Debug, Serialize)]
pub(super) struct Object {
    pub name: String,
    pub fields: Vec<Field>,
    pub structure: bool,
    pub size: usize,
}
#[derive(Debug, Serialize)]
pub(super) struct Variant {
    pub name: String,
    pub value: i64,
    pub ty: Option<Type>,
}
#[derive(Debug, Serialize)]
pub(super) struct Enum {
    pub name: String,
    pub values: Vec<Variant>,
    pub union: bool,
}
#[derive(Debug, Serialize)]
pub(super) struct Schema {
    pub objects: Vec<Object>,
    pub enums: Vec<Enum>,
    pub root: usize,
    pub identifier: Option<String>,
}

impl Schema {
    pub fn read(data: &[u8]) -> Result<Self> {
        if data.len() > 16 * 1024 * 1024 || data.get(4..8) != Some(b"BFBS") {
            return Err(bad("expected a BFBS reflection schema (maximum 16 MiB)"));
        }
        let wire = Wire { bytes: data };
        let root = wire.table(wire.indirect(0)?)?;
        let mut name_bytes = 0usize;
        let mut text = |table: Table, id| -> Result<Option<String>> {
            if let Some(at) = table.pointed(wire, id)? {
                name_bytes =
                    name_bytes.checked_add(wire.u32(at)? as usize).ok_or_else(|| bad("schema text size overflow"))?;
                if name_bytes > 16 * 1024 * 1024 {
                    return Err(bad("expanded schema names exceed the 16 MiB limit"));
                }
            }
            table.text(wire, id)
        };
        let features = root.integer(wire, 6, 0, 8)? as u64;
        if features & !7 != 0 {
            return Err(bad("schema uses unsupported advanced features (such as default strings or vectors)"));
        }
        let mut objects = Vec::new();
        let mut fields_count = 0;
        for at in root.tables(wire, 0)? {
            let obj = wire.table(at)?;
            let name = text(obj, 0)?.ok_or_else(|| bad("object has no name"))?;
            let mut fields = Vec::new();
            for at in obj.tables(wire, 1)? {
                fields_count += 1;
                if fields_count > 100_000 {
                    return Err(bad("too many schema fields"));
                }
                let f = wire.table(at)?;
                if f.integer(wire, 13, 0, 1)? != 0 {
                    return Err(bad("64-bit field offsets are not supported"));
                }
                let ty = Type::read(wire, f.pointed(wire, 1)?.ok_or_else(|| bad("field has no type"))?)?;
                fields.push(Field {
                    name: text(f, 0)?.ok_or_else(|| bad("field has no name"))?,
                    ty,
                    id: f.integer(wire, 2, 0, 2)? as usize,
                    offset: f.integer(wire, 3, 0, 2)? as usize,
                    default_integer: f.integer(wire, 4, 0, 8)?,
                    default_real: f.integer(wire, 5, 0, 8)? as u64,
                    required: f.integer(wire, 7, 0, 1)? != 0,
                    optional: f.integer(wire, 11, 0, 1)? != 0,
                });
            }
            fields.sort_by_key(|f| f.id);
            if fields.iter().map(|f| f.id).collect::<BTreeSet<_>>().len() != fields.len()
                || fields.iter().map(|f| &f.name).collect::<BTreeSet<_>>().len() != fields.len()
            {
                return Err(bad("duplicate schema field identity"));
            }
            let size = obj.integer(wire, 4, 0, 4)?;
            if size < 0 {
                return Err(bad("invalid struct size"));
            }
            objects.push(Object { name, fields, structure: obj.integer(wire, 2, 0, 1)? != 0, size: size as usize });
        }
        if objects.len() > 10_000 || objects.iter().map(|o| &o.name).collect::<BTreeSet<_>>().len() != objects.len() {
            return Err(bad("too many objects or duplicate object names"));
        }
        let mut enums = Vec::new();
        let mut enum_values = 0usize;
        for at in root.tables(wire, 1)? {
            let e = wire.table(at)?;
            let mut values = Vec::new();
            for at in e.tables(wire, 1)? {
                enum_values += 1;
                if enum_values > 100_000 {
                    return Err(bad("too many enum values"));
                }
                let v = wire.table(at)?;
                values.push(Variant {
                    name: text(v, 0)?.ok_or_else(|| bad("enum value has no name"))?,
                    value: v.integer(wire, 1, 0, 8)?,
                    ty: v.pointed(wire, 3)?.map(|at| Type::read(wire, at)).transpose()?,
                });
            }
            if values.len() > 100_000 {
                return Err(bad("too many enum values"));
            }
            enums.push(Enum {
                name: text(e, 0)?.ok_or_else(|| bad("enum has no name"))?,
                values,
                union: e.integer(wire, 2, 0, 1)? != 0,
            });
        }
        if enums.len() > 10_000 {
            return Err(bad("too many enums"));
        }
        let root_object = wire.table(root.pointed(wire, 4)?.ok_or_else(|| bad("the schema has no root_type"))?)?;
        let root_name = text(root_object, 0)?.ok_or_else(|| bad("root type has no name"))?;
        let root_index =
            objects.iter().position(|o| o.name == root_name).ok_or_else(|| bad("root type is not declared"))?;
        if objects[root_index].structure {
            return Err(bad("the root type must be a table"));
        }
        let identifier = text(root, 2)?.filter(|s| !s.is_empty());
        if identifier.as_ref().is_some_and(|s| s.len() != 4) {
            return Err(bad("invalid file identifier in schema"));
        }
        let schema = Self { objects, enums, root: root_index, identifier };
        for object in &schema.objects {
            for field in &object.fields {
                let base = if matches!(field.ty.base, 14 | 17) { field.ty.element } else { field.ty.base };
                if base == 15 && (field.ty.index < 0 || field.ty.index as usize >= schema.objects.len()) {
                    return Err(bad("invalid schema object reference"));
                }
                if field.ty.index >= 0 && base != 15 && field.ty.index as usize >= schema.enums.len() {
                    return Err(bad("invalid schema enum reference"));
                }
                if base == 0 {
                    return Err(bad("field has an unspecified type"));
                }
                if !object.structure && field.offset != 4 + field.id * 2 {
                    return Err(bad("invalid vtable field index in schema"));
                }
            }
        }
        Ok(schema)
    }
}
