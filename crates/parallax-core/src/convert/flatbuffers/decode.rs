//! Schema-driven decoding with bounds checks and unknown-slot rejection.

use super::schema::{Field, Schema, Table, Type, Wire, bad};
use crate::Result;
use crate::convert::score::Json;
use std::collections::{BTreeMap, BTreeSet};

struct Decoder<'a> {
    wire: Wire<'a>,
    schema: &'a Schema,
    nodes: usize,
    remaining: usize,
}

impl Decoder<'_> {
    fn count(&mut self, depth: usize) -> Result<()> {
        self.nodes += 1;
        if self.nodes > 1_000_000 || depth > 128 {
            return Err(bad("decoded data exceeds the node or nesting limit"));
        }
        self.budget(64 + depth * 2)
    }

    fn budget(&mut self, size: usize) -> Result<()> {
        self.remaining =
            self.remaining.checked_sub(size).ok_or_else(|| bad("decoded output exceeds the 64 MiB view limit"))?;
        Ok(())
    }

    fn width(&self, ty: Type) -> Result<usize> {
        Ok(match ty.base {
            1..=4 => 1,
            5 | 6 => 2,
            7 | 8 | 11 => 4,
            9 | 10 | 12 => 8,
            13 | 14 | 16 => 4,
            15 => {
                let object = self.schema.objects.get(ty.index as usize).ok_or_else(|| bad("unknown object type"))?;
                if object.structure { object.size } else { 4 }
            }
            17 => self.width(ty.element())?.checked_mul(ty.length).ok_or_else(|| bad("array size overflow"))?,
            _ => return Err(bad("unsupported field type")),
        })
    }

    fn scalar(&mut self, ty: Type, at: Option<usize>, integer: i64, real: u64) -> Result<Json> {
        let wire = self.wire;
        let bits = match at {
            None => integer as u64,
            Some(at) => match ty.base {
                1..=4 => wire.u8(at)? as u64,
                5 | 6 => wire.u16(at)? as u64,
                7 | 8 | 11 => wire.u32(at)? as u64,
                9 | 10 | 12 => wire.u64(at)?,
                _ => return Err(bad("not a scalar")),
            },
        };
        let shown = match ty.base {
            2 => {
                return match bits {
                    0 => Ok(Json::Atom("false".into())),
                    1 => Ok(Json::Atom("true".into())),
                    _ => Err(bad("invalid boolean value")),
                };
            }
            3 => (bits as i8).to_string(),
            5 => (bits as i16).to_string(),
            7 => (bits as i32).to_string(),
            9 => (bits as i64).to_string(),
            1 | 4 => (bits as u8).to_string(),
            6 => (bits as u16).to_string(),
            8 => (bits as u32).to_string(),
            10 => bits.to_string(),
            11 => {
                let n = at.map_or(f64::from_bits(real) as f32, |_| f32::from_bits(bits as u32));
                return Ok(if n.is_finite() {
                    Json::Atom(n.to_string())
                } else {
                    Json::String(format!("float(bits=0x{:08x})", n.to_bits()))
                });
            }
            12 => {
                let n = f64::from_bits(if at.is_some() { bits } else { real });
                return Ok(if n.is_finite() {
                    Json::Atom(n.to_string())
                } else {
                    Json::String(format!("double(bits=0x{:016x})", n.to_bits()))
                });
            }
            _ => return Err(bad("not a scalar")),
        };
        if ty.index >= 0 {
            let enumeration = self.schema.enums.get(ty.index as usize).ok_or_else(|| bad("unknown enum type"))?;
            let signed = match ty.base {
                3 => bits as i8 as i64,
                5 => bits as i16 as i64,
                7 => bits as i32 as i64,
                _ => bits as i64,
            };
            if let Some(value) = enumeration.values.iter().find(|v| v.value == signed) {
                self.budget((enumeration.name.len() + value.name.len()) * 6)?;
                return Ok(Json::String(format!("{}.{} ({shown})", enumeration.name, value.name)));
            }
        }
        Ok(Json::Atom(shown))
    }

    fn value(&mut self, ty: Type, at: usize, depth: usize) -> Result<Json> {
        self.count(depth)?;
        match ty.base {
            1..=12 => self.scalar(ty, Some(at), 0, 0),
            13 => {
                let target = self.wire.indirect(at)?;
                self.budget(
                    (self.wire.u32(target)? as usize).checked_mul(6).ok_or_else(|| bad("string size overflow"))?,
                )?;
                Ok(Json::String(self.wire.string(target)?))
            }
            14 => {
                if ty.element == 16 {
                    return Err(bad("union vector requires its discriminator vector"));
                }
                let element = ty.element();
                let width = self.width(element)?;
                let (start, len) = self.wire.vector(self.wire.indirect(at)?, width)?;
                let mut values = Vec::with_capacity(len);
                for i in 0..len {
                    values.push(self.value(element, start + i * width, depth + 1)?);
                }
                Ok(Json::Array(values))
            }
            15 => {
                let structure =
                    self.schema.objects.get(ty.index as usize).ok_or_else(|| bad("unknown object type"))?.structure;
                let target = if structure { at } else { self.wire.indirect(at)? };
                self.object(ty.index as usize, target, depth + 1)
            }
            17 => {
                if ty.length == 0 || ty.length > 1_000_000 {
                    return Err(bad("invalid fixed array length"));
                }
                let element = ty.element();
                let width = self.width(element)?;
                self.wire.slice(at, ty.length.checked_mul(width).ok_or_else(|| bad("array overflow"))?)?;
                let mut values = Vec::with_capacity(ty.length);
                for i in 0..ty.length {
                    values.push(self.value(element, at + i * width, depth + 1)?);
                }
                Ok(Json::Array(values))
            }
            _ => Err(bad("unsupported value type")),
        }
    }

    fn union(&mut self, ty: Type, discriminator: u8, at: Option<usize>, depth: usize) -> Result<Json> {
        if discriminator == 0 {
            if at.is_some_and(|at| self.wire.u32(at).is_ok_and(|v| v != 0)) {
                return Err(bad("NONE union has a value"));
            }
            return Ok(Json::Atom("null".into()));
        }
        let e = self.schema.enums.get(ty.index as usize).ok_or_else(|| bad("unknown union type"))?;
        if !e.union {
            return Err(bad("union references a non-union enum"));
        }
        let variant = e
            .values
            .iter()
            .find(|v| v.value == discriminator as i64)
            .ok_or_else(|| bad("unknown union discriminator"))?;
        let vty = variant.ty.ok_or_else(|| bad("union variant has no type"))?;
        let at = at.ok_or_else(|| bad("union discriminator has no value"))?;
        if vty.base == 15 && self.schema.objects.get(vty.index as usize).is_some_and(|o| o.structure) {
            return self.object(vty.index as usize, self.wire.indirect(at)?, depth + 1);
        }
        self.value(vty, at, depth + 1)
    }

    fn object(&mut self, index: usize, at: usize, depth: usize) -> Result<Json> {
        self.count(depth)?;
        let schema = self.schema;
        let object = schema.objects.get(index).ok_or_else(|| bad("unknown object type"))?;
        let table = if object.structure {
            self.wire.slice(at, object.size)?;
            None
        } else {
            Some(self.wire.table(at)?)
        };
        if let Some(table) = table {
            let ids = object.fields.iter().map(|f| f.id).collect::<BTreeSet<_>>();
            for id in 0..table.slots {
                if !ids.contains(&id) && table.field(self.wire, id)?.is_some() {
                    return Err(bad(format!(
                        "the schema does not describe field #{id} in {}. Select the matching schema",
                        object.name
                    )));
                }
            }
        }
        let mut values = BTreeMap::new();
        for field in &object.fields {
            self.budget(field.name.len() * 6 + 64 + depth * 2)?;
            let location = self.location(field, table, at, object.size)?;
            let value = if field.ty.base == 16 {
                let discriminator = object
                    .fields
                    .iter()
                    .find(|f| f.id + 1 == field.id && f.ty.base == 1 && f.name == format!("{}_type", field.name))
                    .ok_or_else(|| bad("union has no matching discriminator field"))?;
                let tag = self
                    .location(discriminator, table, at, object.size)?
                    .map(|at| self.wire.u8(at))
                    .transpose()?
                    .unwrap_or(0);
                self.union(field.ty, tag, location, depth + 1)?
            } else if field.ty.base == 14 && field.ty.element == 16 {
                let discriminator = object
                    .fields
                    .iter()
                    .find(|f| {
                        f.id + 1 == field.id
                            && f.ty.base == 14
                            && f.ty.element == 1
                            && f.name == format!("{}_type", field.name)
                    })
                    .ok_or_else(|| bad("union vector has no discriminator field"))?;
                let tags = self.location(discriminator, table, at, object.size)?;
                match (location, tags) {
                    (None, None) => Json::Atom("null".into()),
                    (Some(location), Some(tags)) => {
                        let (start, len) = self.wire.vector(self.wire.indirect(location)?, 4)?;
                        let (tag_start, tag_len) = self.wire.vector(self.wire.indirect(tags)?, 1)?;
                        if len != tag_len {
                            return Err(bad("union vector lengths differ"));
                        }
                        let mut array = Vec::with_capacity(len);
                        for i in 0..len {
                            array.push(self.union(
                                field.ty,
                                self.wire.u8(tag_start + i)?,
                                Some(start + i * 4),
                                depth + 1,
                            )?);
                        }
                        Json::Array(array)
                    }
                    _ => return Err(bad("union vector is missing values or discriminators")),
                }
            } else if let Some(location) = location {
                self.value(field.ty, location, depth + 1)?
            } else if field.required {
                return Err(bad(format!("missing required field {}.{}", object.name, field.name)));
            } else if field.optional || field.ty.base > 12 {
                Json::Atom("null".into())
            } else {
                self.scalar(field.ty, None, field.default_integer, field.default_real)?
            };
            values.insert(field.name.clone(), value);
        }
        Ok(Json::Object(values))
    }

    fn location(&self, field: &Field, table: Option<Table>, at: usize, struct_size: usize) -> Result<Option<usize>> {
        let location = match table {
            Some(table) => table.field(self.wire, field.id)?,
            None => Some(at + field.offset),
        };
        if let Some(location) = location {
            let width = self.width(field.ty)?;
            let limit = at + table.map_or(struct_size, |t| t.object_size);
            if location.checked_add(width).is_none_or(|end| end > limit) {
                return Err(bad(format!("field {} extends outside its object", field.name)));
            }
            self.wire.slice(location, width)?;
        }
        Ok(location)
    }
}

pub(super) fn decode(schema: &Schema, data: &[u8]) -> Result<Json> {
    if data.len() > 64 * 1024 * 1024 {
        return Err(bad("file exceeds the 64 MiB decoded-view limit"));
    }
    let read = |data: &[u8]| {
        let wire = Wire { bytes: data };
        if schema.identifier.as_ref().is_some_and(|id| data.get(4..8) != Some(id.as_bytes())) {
            return Err(bad("the binary file identifier does not match the selected schema"));
        }
        let root = wire.indirect(0)?;
        Decoder { wire, schema, nodes: 0, remaining: 64 * 1024 * 1024 }.object(schema.root, root, 0)
    };
    match read(data) {
        Ok(value) => Ok(value),
        Err(error) => {
            // Support size-prefixed buffers only when their length is exact.
            if data.len() >= 8
                && u32::from_le_bytes(data[..4].try_into().expect("four bytes")) as usize == data.len() - 4
            {
                read(&data[4..])
            } else {
                Err(error)
            }
        }
    }
}
