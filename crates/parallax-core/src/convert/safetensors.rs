//! safetensors model files (Hugging Face), shown from their header.
//!
//! Layout: a little-endian u64 header length, a JSON header mapping each tensor
//! name to its dtype, shape and byte range (plus an optional `__metadata__`
//! object of strings), then the raw tensor data. The views read only the header,
//! so a 10 GB file opens instantly; tensor checksums are added only when the
//! tensor data is small enough to hash quickly.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde_json::Value;

use super::gguf::{count, escape, line, natural_key, parameters, size};
use crate::{Error, Result};

/// The format caps the header at 100 MB.
const MAX_HEADER: u64 = 100_000_000;
/// Tensor data up to this size is checksummed in the tensors view.
const CHECKSUM_LIMIT: u64 = 256 << 20;
/// The similar check compares tensor bytes only up to this much tensor data.
const SIMILAR_LIMIT: u64 = 4 << 30;
/// Tensors listed one per line.
const MAX_TENSOR_LINES: usize = 50_000;

/// Whether the data starts like a safetensors file: a sane header length and a
/// JSON object whose first key is a string.
pub fn is_safetensors(data: &[u8]) -> bool {
    let Some(n) = data.get(..8).map(|b| u64::from_le_bytes(b.try_into().unwrap())) else { return false };
    if !(2..=MAX_HEADER).contains(&n) || n + 8 > data.len() as u64 {
        return false;
    }
    let header = &data[8..8 + n as usize];
    let body = header.trim_ascii();
    body.starts_with(b"{")
        && body.ends_with(b"}")
        && body[1..].trim_ascii_start().first().is_some_and(|&c| c == b'"' || c == b'}')
}

struct Tensor {
    name: String,
    dtype: String,
    shape: Vec<u64>,
    begin: u64,
    end: u64,
}

impl Tensor {
    fn elements(&self) -> u64 {
        self.shape.iter().fold(1u64, |a, &d| a.saturating_mul(d))
    }

    fn bytes(&self) -> u64 {
        self.end.saturating_sub(self.begin)
    }
}

struct SafeTensors<'a> {
    tensors: Vec<Tensor>,
    metadata: Vec<(String, String)>,
    /// The tensor data: everything after the header.
    data: &'a [u8],
    header_len: u64,
    file_size: u64,
}

fn damaged(what: &str) -> Error {
    Error::Unsupported(format!("safetensors: {what}"))
}

impl<'a> SafeTensors<'a> {
    fn parse(data: &'a [u8]) -> Result<SafeTensors<'a>> {
        if !is_safetensors(data) {
            return Err(damaged("not a safetensors file"));
        }
        let n = u64::from_le_bytes(data[..8].try_into().unwrap());
        let header: serde_json::Map<String, Value> =
            serde_json::from_slice(&data[8..8 + n as usize]).map_err(|e| damaged(&format!("bad header: {e}")))?;
        let mut tensors = Vec::with_capacity(header.len());
        let mut metadata = Vec::new();
        for (name, v) in header {
            if name == "__metadata__" {
                if let Value::Object(m) = v {
                    metadata = m
                        .into_iter()
                        .map(|(k, v)| (k, v.as_str().map_or_else(|| v.to_string(), str::to_string)))
                        .collect();
                }
                continue;
            }
            let bad = || damaged(&format!("bad entry for tensor {name}"));
            let dtype = v.get("dtype").and_then(Value::as_str).ok_or_else(bad)?.to_string();
            let shape = v
                .get("shape")
                .and_then(Value::as_array)
                .ok_or_else(bad)?
                .iter()
                .map(|d| d.as_u64().ok_or_else(bad))
                .collect::<Result<Vec<_>>>()?;
            let offsets = v.get("data_offsets").and_then(Value::as_array).ok_or_else(bad)?;
            let (Some(begin), Some(end)) =
                (offsets.first().and_then(Value::as_u64), offsets.get(1).and_then(Value::as_u64))
            else {
                return Err(bad());
            };
            if end < begin {
                return Err(bad());
            }
            tensors.push(Tensor { name, dtype, shape, begin, end });
        }
        metadata.sort();
        tensors.sort_by_cached_key(|t| natural_key(&t.name));
        Ok(SafeTensors {
            tensors,
            metadata,
            data: &data[8 + n as usize..],
            header_len: n,
            file_size: data.len() as u64,
        })
    }

    fn tensor_bytes(&self) -> u64 {
        self.tensors.iter().map(Tensor::bytes).sum()
    }

    fn slice(&self, t: &Tensor) -> Option<&'a [u8]> {
        self.data.get(usize::try_from(t.begin).ok()?..usize::try_from(t.end).ok()?)
    }

    fn is_complete(&self) -> bool {
        self.tensors.iter().all(|t| t.end <= self.data.len() as u64)
    }
}

/// Per dtype: tensors, weights and bytes, largest share first.
fn dtypes(st: &SafeTensors, out: &mut String) {
    let mut by_type: BTreeMap<&str, (u64, u64, u64)> = BTreeMap::new();
    for t in &st.tensors {
        let e = by_type.entry(&t.dtype).or_default();
        e.0 += 1;
        e.1 += t.elements();
        e.2 += t.bytes();
    }
    let total = by_type.values().map(|v| v.2).sum::<u64>().max(1);
    let mut rows: Vec<_> = by_type.into_iter().collect();
    rows.sort_by_key(|(name, v)| (std::cmp::Reverse(v.2), *name));
    let _ = writeln!(out, "{:<10} {:>8} {:>14} {:>12} {:>7}", "dtype", "tensors", "parameters", "size", "share");
    for (name, (n, params, bytes)) in rows {
        let share = bytes as f64 * 100.0 / total as f64;
        let _ = writeln!(out, "{:<10} {:>8} {:>14} {:>12} {:>6.1}%", name, n, count(params), size(bytes), share);
    }
}

/// Splits `model.layers.12.self_attn.q_proj.weight` into layer 12 and role `self_attn.q_proj`.
fn layer_role(name: &str) -> Option<(u64, String)> {
    let parts: Vec<&str> = name.split('.').collect();
    let i = (1..parts.len()).find(|&i| {
        matches!(parts[i - 1], "layers" | "layer" | "h" | "blocks" | "block" | "blk") && parts[i].parse::<u64>().is_ok()
    })?;
    let rest = parts[i + 1..].join(".");
    let role = rest.strip_suffix(".weight")?;
    Some((parts[i].parse().ok()?, role.to_string()))
}

/// Overview: parameters, data types, size and the `__metadata__`.
pub fn summary(data: &[u8]) -> Result<String> {
    let st = SafeTensors::parse(data)?;
    let mut out = String::new();
    let _ = writeln!(out, "=== Model ===");
    let params: u64 = st.tensors.iter().map(Tensor::elements).sum();
    let exact = if params >= 1_000_000 { format!(" ({})", count(params)) } else { String::new() };
    line(&mut out, "Parameters", format!("{}{exact}", parameters(params)));
    line(&mut out, "Tensors", count(st.tensors.len() as u64));
    let layers: std::collections::BTreeSet<u64> =
        st.tensors.iter().filter_map(|t| layer_role(&t.name)).map(|l| l.0).collect();
    if !layers.is_empty() {
        line(&mut out, "Layers", count(layers.len() as u64));
    }
    let bytes = st.tensor_bytes();
    if params > 0 {
        line(&mut out, "Bits per weight", format!("{:.2}", bytes as f64 * 8.0 / params as f64));
    }
    line(&mut out, "Tensor data", size(bytes));
    line(&mut out, "Header", size(st.header_len));
    line(&mut out, "File size", size(st.file_size));
    if !st.is_complete() {
        let _ = writeln!(out, "! The file ends before the end of its tensor data (an incomplete download?).");
    }
    let _ = writeln!(out, "\n=== Data types ===");
    dtypes(&st, &mut out);
    let _ = writeln!(out, "\n=== Metadata ===");
    if st.metadata.is_empty() {
        let _ = writeln!(out, "(none)");
    }
    for (k, v) in &st.metadata {
        let _ = writeln!(out, "{} = {}", escape(k), escape(v));
    }
    Ok(out)
}

/// Data types, a map of dtypes per layer, and every tensor in natural name order.
pub fn tensors(data: &[u8]) -> Result<String> {
    let st = SafeTensors::parse(data)?;
    let mut out = String::new();
    let _ = writeln!(out, "=== Data types ===");
    dtypes(&st, &mut out);

    // Layer map: one row per layer, one column per weight matrix role.
    let mut layers: BTreeMap<u64, HashMap<String, &str>> = BTreeMap::new();
    let mut roles: Vec<String> = Vec::new();
    for t in &st.tensors {
        if t.shape.iter().filter(|&&d| d > 1).count() < 2 {
            continue; // Norms and biases.
        }
        let Some((n, role)) = layer_role(&t.name) else { continue };
        if !roles.contains(&role) {
            roles.push(role.clone());
        }
        layers.entry(n).or_default().insert(role, &t.dtype);
    }
    roles.sort();
    if !layers.is_empty() {
        let width = |r: &String| r.len().max(5);
        let _ = writeln!(out, "\n=== Layer map ===");
        let mut row = format!("{:<6}", "layer");
        for r in &roles {
            let _ = write!(row, " {:<w$}", r, w = width(r));
        }
        let _ = writeln!(out, "{}", row.trim_end());
        for (n, types) in &layers {
            let mut row = format!("{n:<6}");
            for r in &roles {
                let _ = write!(row, " {:<w$}", types.get(r).copied().unwrap_or("-"), w = width(r));
            }
            let _ = writeln!(out, "{}", row.trim_end());
        }
    }

    let checksums = st.tensor_bytes() <= CHECKSUM_LIMIT && st.is_complete();
    let _ = writeln!(out, "\n=== Tensors ({}) ===", count(st.tensors.len() as u64));
    if !checksums {
        let _ = writeln!(out, "// Tensor data larger than {}: no checksums.", size(CHECKSUM_LIMIT));
    }
    for t in st.tensors.iter().take(MAX_TENSOR_LINES) {
        let shape = t.shape.iter().map(u64::to_string).collect::<Vec<_>>().join(", ");
        let _ = write!(out, "{}  [{shape}]  {}  {}", escape(&t.name), t.dtype, size(t.bytes()));
        if checksums && let Some(bytes) = st.slice(t) {
            let _ = write!(out, "  crc {:08x}", crc32fast::hash(bytes));
        }
        out.push('\n');
    }
    if st.tensors.len() > MAX_TENSOR_LINES {
        let _ = writeln!(out, "… {} more tensors not shown", count((st.tensors.len() - MAX_TENSOR_LINES) as u64));
    }
    Ok(out)
}

/// Whether two files hold the same tensors (names, dtypes, shapes and bytes)
/// and differ only in `__metadata__` or the header layout. Reads all tensor data
/// of both files, stopping at the first difference; files with more than
/// [`SIMILAR_LIMIT`] of tensor data are not checked. `None` when either is unreadable.
pub fn same_tensors(left: &[u8], right: &[u8]) -> Option<bool> {
    let (a, b) = (SafeTensors::parse(left).ok()?, SafeTensors::parse(right).ok()?);
    if a.tensor_bytes() > SIMILAR_LIMIT {
        return None;
    }
    if a.tensors.len() != b.tensors.len() || !a.is_complete() || !b.is_complete() {
        return Some(false);
    }
    let same_entry =
        |x: &Tensor, y: &Tensor| x.name == y.name && x.dtype == y.dtype && x.shape == y.shape && x.bytes() == y.bytes();
    if !a.tensors.iter().zip(&b.tensors).all(|(x, y)| same_entry(x, y)) {
        return Some(false);
    }
    Some(a.tensors.iter().zip(&b.tensors).all(|(x, y)| a.slice(x) == b.slice(y)))
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/safetensors/", $name))
        };
    }
    const V1: &[u8] = fixture!("tiny-v1.safetensors");
    const V1_META: &[u8] = fixture!("tiny-v1-meta.safetensors");
    const V2: &[u8] = fixture!("tiny-v2.safetensors");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn summary_view() {
        let s = summary(V1).unwrap();
        for expected in [
            "Parameters:        964\n",
            "Tensors:           19\n",
            "Layers:            3\n",
            "F16              11            640     1.25 KiB   49.4%",
            "=== Metadata ===\nformat = pt\nnote = v1\n",
        ] {
            assert!(s.contains(expected), "{expected}\n---\n{s}");
        }
    }

    #[test]
    fn tensors_view() {
        let t = tensors(V1).unwrap();
        for expected in [
            "=== Layer map ===\nlayer  mlp.down_proj self_attn.k_proj self_attn.q_proj self_attn.v_proj\n0      F32           F16              F16              F16\n1      F32",
            "\n10     F32 ",
            "model.layers.0.mlp.down_proj.weight  [8, 12]  F32  384 B  crc ",
        ] {
            assert!(t.contains(expected), "{expected}\n---\n{t}");
        }
        let names: Vec<&str> = t
            .lines()
            .skip_while(|l| !l.starts_with("=== Tensors"))
            .skip(1)
            .map(|l| l.split(' ').next().unwrap())
            .collect();
        let at = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(at("model.layers.1.self_attn.v_proj.weight") < at("model.layers.10.input_layernorm.weight"));
    }

    #[test]
    fn edits_show_as_changed_lines_only() {
        let (t1, t2) = (tensors(V1).unwrap(), tensors(V2).unwrap());
        let changed: Vec<&str> = lines_only_in(&t2, &t1).into_iter().filter(|l| l.starts_with("model.")).collect();
        assert_eq!(changed.len(), 3, "{changed:?}");
        assert!(changed[0].starts_with("model.layers.0.mlp.down_proj.weight  [8, 12]  F16  192 B"), "{changed:?}");
        assert!(changed[1].starts_with("model.layers.0.mlp.up_proj.weight  [12, 8]  F16"), "{changed:?}");
        assert!(
            changed[2].starts_with("model.layers.1.self_attn.q_proj.weight  [8, 8]  F16  128 B  crc"),
            "{changed:?}"
        );
    }

    #[test]
    fn metadata_only_changes_are_similar() {
        assert_eq!(same_tensors(V1, V1_META), Some(true));
        assert_eq!(same_tensors(V1, V2), Some(false));
        assert_eq!(same_tensors(V1, b"\x02\0\0\0\0\0\0\0{}"), Some(false));
        assert_eq!(same_tensors(V1, b"garbage"), None);
        let (t1, tm) = (tensors(V1).unwrap(), tensors(V1_META).unwrap());
        assert_eq!(t1, tm);
    }

    #[test]
    fn detection_and_damage() {
        assert!(is_safetensors(V1));
        assert!(!is_safetensors(b"\x05\0\0\0\0\0\0\0hello"));
        assert!(!is_safetensors(b"\xff\xff\xff\xff\xff\xff\xff\xff{\"a\":1}"));
        assert!(!is_safetensors(b"{\"json\": true}\n"));
        for cut in 0..V1.len() {
            let _ = summary(&V1[..cut]);
            let _ = tensors(&V1[..cut]);
        }
        let mut bad = V1.to_vec();
        let header = String::from_utf8_lossy(&V1[8..200]).into_owned();
        let at = 8 + header.find("[0,32]").unwrap();
        bad[at..at + 6].copy_from_slice(b"[32,0]");
        assert!(summary(&bad).is_err());
    }
}
