//! ONNX models: the graph as text.
//!
//! An ONNX file is one protobuf `ModelProto` (see `onnx.proto`). The subset read
//! here is small (model header, graph, nodes, attributes, tensors, value types),
//! so the wire format is decoded by hand rather than with generated code. Weights
//! are not decoded: each initializer gets a checksum of its bytes, so a changed
//! weight shows as one changed line, and tiny ones (shape constants) their values.
//!
//! Nodes keep graph order (it is the topological order and meaningful);
//! initializers, value infos and metadata are sorted by name.

use std::fmt::Write as _;

use super::gguf::{count, escape, line, parameters, size};
use crate::{Error, Result};

/// Initializer bytes checksummed in all; beyond this, initializers get no checksum.
const CHECKSUM_LIMIT: u64 = 1 << 30;
/// Lines per section before the rest is summarized.
const MAX_LINES: usize = 50_000;
/// Tensors with up to this many elements show their values.
const SMALL_TENSOR: usize = 16;
/// Strings longer than this are shortened.
const LONG_STRING: usize = 200;
/// Subgraphs (If, Loop, Scan bodies) nested deeper than this are not expanded.
const MAX_DEPTH: usize = 16;

fn damaged() -> Error {
    Error::Unsupported("ONNX: the model is truncated or damaged".into())
}

// ---- Protobuf wire format ----

#[derive(Clone, Copy, Debug)]
enum Wire<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

impl<'a> Wire<'a> {
    fn int(self) -> i64 {
        match self {
            Wire::Varint(v) | Wire::Fixed64(v) => v as i64,
            Wire::Fixed32(v) => v as i64,
            Wire::Bytes(_) => 0,
        }
    }

    fn bytes(self) -> &'a [u8] {
        match self {
            Wire::Bytes(b) => b,
            _ => &[],
        }
    }

    fn string(self) -> String {
        String::from_utf8_lossy(self.bytes()).into_owned()
    }
}

fn varint(data: &[u8], pos: &mut usize) -> Result<u64> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *data.get(*pos).ok_or_else(damaged)?;
        *pos += 1;
        v |= ((b & 0x7F) as u64) << shift;
        if b < 0x80 {
            return Ok(v);
        }
    }
    Err(damaged())
}

/// The fields of a message, in order.
fn fields(data: &[u8]) -> impl Iterator<Item = Result<(u32, Wire<'_>)>> {
    let mut pos = 0;
    let mut failed = false;
    std::iter::from_fn(move || {
        if pos >= data.len() || failed {
            return None;
        }
        let item = (|| {
            let key = varint(data, &mut pos)?;
            let field = u32::try_from(key >> 3).map_err(|_| damaged())?;
            let value = match key & 7 {
                0 => Wire::Varint(varint(data, &mut pos)?),
                1 => {
                    let b = data.get(pos..pos + 8).ok_or_else(damaged)?;
                    pos += 8;
                    Wire::Fixed64(u64::from_le_bytes(b.try_into().unwrap()))
                }
                2 => {
                    let n = usize::try_from(varint(data, &mut pos)?).map_err(|_| damaged())?;
                    let b = data.get(pos..pos.checked_add(n).ok_or_else(damaged)?).ok_or_else(damaged)?;
                    pos += n;
                    Wire::Bytes(b)
                }
                5 => {
                    let b = data.get(pos..pos + 4).ok_or_else(damaged)?;
                    pos += 4;
                    Wire::Fixed32(u32::from_le_bytes(b.try_into().unwrap()))
                }
                _ => return Err(damaged()),
            };
            Ok((field, value))
        })();
        failed = item.is_err();
        Some(item)
    })
}

/// A repeated varint field, packed or not, appended to `out`.
fn push_varints(value: Wire, out: &mut Vec<i64>) -> Result<()> {
    match value {
        Wire::Bytes(b) => {
            let mut pos = 0;
            while pos < b.len() {
                out.push(varint(b, &mut pos)? as i64);
            }
        }
        v => out.push(v.int()),
    }
    Ok(())
}

/// A repeated float field, packed or not.
fn push_floats(value: Wire, out: &mut Vec<f32>) {
    match value {
        Wire::Bytes(b) => out.extend(b.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap()))),
        Wire::Fixed32(v) => out.push(f32::from_bits(v)),
        _ => {}
    }
}

/// Whether the data starts with a field of `ModelProto` in the right wire type.
pub fn is_onnx(data: &[u8]) -> bool {
    matches!(
        fields(data).next(),
        Some(Ok((1 | 5, Wire::Varint(_)) | (2..=4 | 6 | 7 | 8 | 14 | 20 | 25, Wire::Bytes(_))))
    )
}

// ---- Model structures ----

const DATA_TYPES: [&str; 27] = [
    "UNDEFINED",
    "FLOAT",
    "UINT8",
    "INT8",
    "UINT16",
    "INT16",
    "INT32",
    "INT64",
    "STRING",
    "BOOL",
    "FLOAT16",
    "DOUBLE",
    "UINT32",
    "UINT64",
    "COMPLEX64",
    "COMPLEX128",
    "BFLOAT16",
    "FLOAT8E4M3FN",
    "FLOAT8E4M3FNUZ",
    "FLOAT8E5M2",
    "FLOAT8E5M2FNUZ",
    "UINT4",
    "INT4",
    "FLOAT4E2M1",
    "FLOAT8E8M0",
    "UINT2",
    "INT2",
];

fn data_type(t: i64) -> String {
    usize::try_from(t).ok().and_then(|i| DATA_TYPES.get(i)).map_or_else(|| format!("type {t}"), |s| s.to_string())
}

/// Bits per element of a data type, where fixed.
fn bits(t: i64) -> Option<u64> {
    Some(match t {
        2 | 3 | 9 | 17..=20 | 24 => 8,
        4 | 5 | 10 | 16 => 16,
        1 | 6 | 12 => 32,
        7 | 11 | 13 | 14 => 64,
        15 => 128,
        21..=23 => 4,
        25 | 26 => 2,
        _ => return None,
    })
}

#[derive(Default)]
struct Tensor<'a> {
    name: String,
    data_type: i64,
    dims: Vec<i64>,
    /// The data fields as stored (raw_data, or the typed repeated fields).
    data: Vec<(u32, Wire<'a>)>,
    external: Vec<(String, String)>,
}

impl<'a> Tensor<'a> {
    fn parse(b: &'a [u8]) -> Result<Tensor<'a>> {
        let mut t = Tensor::default();
        for f in fields(b) {
            let (n, v) = f?;
            match n {
                1 => push_varints(v, &mut t.dims)?,
                2 => t.data_type = v.int(),
                8 => t.name = v.string(),
                4..=7 | 9..=11 => t.data.push((n, v)),
                13 => t.external.push(key_value(v.bytes())?),
                _ => {}
            }
        }
        Ok(t)
    }

    fn elements(&self) -> u64 {
        self.dims.iter().fold(1u64, |a, &d| a.saturating_mul(d.max(0) as u64))
    }

    /// Bytes of the data as stored in the file.
    fn stored_bytes(&self) -> u64 {
        if let Some(len) = self.external.iter().find(|(k, _)| k == "length").and_then(|(_, v)| v.parse().ok()) {
            return len;
        }
        match bits(self.data_type) {
            Some(b) => (self.elements().saturating_mul(b)).div_ceil(8),
            None => self.data.iter().map(|(_, v)| v.bytes().len() as u64).sum(),
        }
    }

    fn checksum(&self) -> u32 {
        let mut h = crc32fast::Hasher::new();
        for (n, v) in &self.data {
            h.update(&n.to_le_bytes());
            match v {
                Wire::Bytes(b) => h.update(b),
                v => h.update(&v.int().to_le_bytes()),
            }
        }
        h.finalize()
    }

    /// The values of a small numeric tensor.
    fn values(&self) -> Option<String> {
        if self.elements() as usize > SMALL_TENSOR || !self.external.is_empty() {
            return None;
        }
        let mut items: Vec<String> = Vec::new();
        for (n, v) in &self.data {
            match (n, self.data_type) {
                (9, t) => {
                    let b = v.bytes();
                    match t {
                        1 => items.extend(
                            b.chunks_exact(4).map(|c| format!("{:?}", f32::from_le_bytes(c.try_into().unwrap()))),
                        ),
                        11 => items.extend(
                            b.chunks_exact(8).map(|c| format!("{:?}", f64::from_le_bytes(c.try_into().unwrap()))),
                        ),
                        7 => items
                            .extend(b.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap()).to_string())),
                        6 => items
                            .extend(b.chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap()).to_string())),
                        12 => items
                            .extend(b.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap()).to_string())),
                        2 => items.extend(b.iter().map(u8::to_string)),
                        3 => items.extend(b.iter().map(|&x| (x as i8).to_string())),
                        9 => items.extend(b.iter().map(|&x| (x != 0).to_string())),
                        _ => return None,
                    }
                }
                (4, _) => {
                    let mut f = Vec::new();
                    push_floats(*v, &mut f);
                    items.extend(f.iter().map(|x| format!("{x:?}")));
                }
                (5 | 7 | 11, t) if t != 10 && t != 16 => {
                    let mut i = Vec::new();
                    push_varints(*v, &mut i).ok()?;
                    items.extend(i.iter().map(|x| if t == 9 { (*x != 0).to_string() } else { x.to_string() }));
                }
                (10, _) => match v {
                    Wire::Bytes(b) => items
                        .extend(b.chunks_exact(8).map(|c| format!("{:?}", f64::from_le_bytes(c.try_into().unwrap())))),
                    Wire::Fixed64(x) => items.push(format!("{:?}", f64::from_bits(*x))),
                    _ => {}
                },
                (6, _) => items.push(quote(&v.string())),
                _ => return None,
            }
        }
        Some(format!("[{}]", items.join(", ")))
    }

    /// `FLOAT [4, 8]`, followed by values, or size and checksum.
    fn describe(&self, budget: &mut u64) -> String {
        let dims = self.dims.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
        let mut s = format!("{} [{dims}]", data_type(self.data_type));
        if !self.external.is_empty() {
            let get = |k: &str| self.external.iter().find(|e| e.0 == k).map_or("", |e| e.1.as_str());
            let _ = write!(s, ", {}, external: {}", size(self.stored_bytes()), escape(get("location")));
            if !get("offset").is_empty() {
                let _ = write!(s, " at offset {}", get("offset"));
            }
            return s;
        }
        if let Some(values) = self.values() {
            let _ = write!(s, " = {values}");
            return s;
        }
        let bytes = self.stored_bytes();
        let _ = write!(s, ", {}", size(bytes));
        if bytes <= *budget {
            *budget -= bytes;
            let _ = write!(s, ", crc {:08x}", self.checksum());
        }
        s
    }
}

fn key_value(b: &[u8]) -> Result<(String, String)> {
    let (mut k, mut v) = (String::new(), String::new());
    for f in fields(b) {
        match f? {
            (1, x) => k = x.string(),
            (2, x) => v = x.string(),
            _ => {}
        }
    }
    Ok((k, v))
}

fn quote(s: &str) -> String {
    if s.chars().count() > LONG_STRING {
        let head: String = s.chars().take(LONG_STRING / 2).collect();
        format!("\"{}…\" ({} characters)", escape(&head), s.chars().count())
    } else {
        format!("\"{}\"", escape(s))
    }
}

/// A `TypeProto` as text: `FLOAT [batch, 3]`, `seq(INT64 [?])`, `map(STRING, FLOAT)`.
fn type_text(b: &[u8], depth: usize) -> Result<String> {
    if depth > MAX_DEPTH {
        return Err(damaged());
    }
    let mut out = String::from("?");
    for f in fields(b) {
        let (n, v) = f?;
        out = match n {
            1 | 8 => {
                let (mut elem, mut shape) = (0, None);
                for g in fields(v.bytes()) {
                    match g? {
                        (1, x) => elem = x.int(),
                        (2, x) => shape = Some(shape_text(x.bytes())?),
                        _ => {}
                    }
                }
                let sparse = if n == 8 { "sparse " } else { "" };
                format!("{sparse}{}{}", data_type(elem), shape.map(|s| format!(" {s}")).unwrap_or_default())
            }
            4 | 9 => {
                let mut inner = String::from("?");
                for g in fields(v.bytes()) {
                    if let (1, x) = g? {
                        inner = type_text(x.bytes(), depth + 1)?;
                    }
                }
                format!("{}({inner})", if n == 4 { "seq" } else { "optional" })
            }
            5 => {
                let (mut key, mut value) = (String::from("?"), String::from("?"));
                for g in fields(v.bytes()) {
                    match g? {
                        (1, x) => key = data_type(x.int()),
                        (2, x) => value = type_text(x.bytes(), depth + 1)?,
                        _ => {}
                    }
                }
                format!("map({key}, {value})")
            }
            _ => continue,
        };
    }
    Ok(out)
}

fn shape_text(b: &[u8]) -> Result<String> {
    let mut dims = Vec::new();
    for f in fields(b) {
        if let (1, d) = f? {
            let mut dim = String::from("?");
            for g in fields(d.bytes()) {
                match g? {
                    (1, x) => dim = x.int().to_string(),
                    (2, x) => dim = x.string(),
                    _ => {}
                }
            }
            dims.push(dim);
        }
    }
    Ok(format!("[{}]", dims.join(", ")))
}

/// A `ValueInfoProto`: name and type.
fn value_info(b: &[u8]) -> Result<(String, String)> {
    let (mut name, mut ty) = (String::new(), String::from("?"));
    for f in fields(b) {
        match f? {
            (1, x) => name = x.string(),
            (2, x) => ty = type_text(x.bytes(), 0)?,
            _ => {}
        }
    }
    Ok((name, ty))
}

/// Short form of a list of items: all of them, or the first ones and a count.
fn list<T: std::fmt::Display>(items: &[T]) -> String {
    let shown: Vec<String> = items.iter().take(SMALL_TENSOR).map(T::to_string).collect();
    if items.len() > SMALL_TENSOR {
        format!("[{}, … ({} items)]", shown.join(", "), items.len())
    } else {
        format!("[{}]", shown.join(", "))
    }
}

/// Writes a graph's nodes, one per line, with subgraphs indented below their node.
struct Writer<'o> {
    out: &'o mut String,
    lines: usize,
    budget: u64,
}

impl Writer<'_> {
    /// An attribute as `name=value`; subgraphs are returned to be written after the node.
    fn attribute<'a>(&mut self, b: &'a [u8], graphs: &mut Vec<(String, &'a [u8])>) -> Result<String> {
        let mut name = String::new();
        let (mut floats, mut ints, mut strings, mut tensors, mut types) = (Vec::new(), Vec::new(), Vec::new(), 0, 0);
        let mut single: Option<String> = None;
        let mut reference = None;
        for f in fields(b) {
            let (n, v) = f?;
            match n {
                1 => name = v.string(),
                2 => single = Some(format!("{:?}", f32::from_bits(v.int() as u32))),
                3 => single = Some(v.int().to_string()),
                4 => single = Some(quote(&v.string())),
                5 => single = Some(format!("tensor {}", Tensor::parse(v.bytes())?.describe(&mut self.budget))),
                6 => graphs.push((name.clone(), v.bytes())),
                7 => push_floats(v, &mut floats),
                8 => push_varints(v, &mut ints)?,
                9 => strings.push(quote(&v.string())),
                10 => tensors += 1,
                11 => graphs.push((name.clone(), v.bytes())),
                14 => single = Some(type_text(v.bytes(), 0)?),
                15 => types += 1,
                21 => reference = Some(v.string()),
                _ => {}
            }
        }
        let value = if let Some(r) = reference {
            format!("@{r}")
        } else if let Some(s) = single {
            s
        } else if !floats.is_empty() {
            list(&floats.iter().map(|f| format!("{f:?}")).collect::<Vec<_>>())
        } else if !ints.is_empty() {
            list(&ints)
        } else if !strings.is_empty() {
            list(&strings)
        } else if tensors > 0 {
            format!("<{tensors} tensors>")
        } else if types > 0 {
            format!("<{types} types>")
        } else if graphs.iter().any(|g| g.0 == name) {
            "<graph>".into()
        } else {
            "[]".into()
        };
        Ok(format!("{name}={value}"))
    }

    fn node(&mut self, b: &[u8], depth: usize) -> Result<()> {
        let (mut inputs, mut outputs, mut attrs) = (Vec::new(), Vec::new(), Vec::new());
        let (mut name, mut op, mut domain) = (String::new(), String::new(), String::new());
        let mut graphs = Vec::new();
        for f in fields(b) {
            let (n, v) = f?;
            match n {
                1 => inputs.push(v.string()),
                2 => outputs.push(v.string()),
                3 => name = v.string(),
                4 => op = v.string(),
                5 => attrs.push(self.attribute(v.bytes(), &mut graphs)?),
                7 => domain = v.string(),
                _ => {}
            }
        }
        self.lines += 1;
        if self.lines > MAX_LINES {
            return Ok(());
        }
        let indent = "    ".repeat(depth);
        let op = if domain.is_empty() || domain == "ai.onnx" { op } else { format!("{domain}::{op}") };
        let mut s =
            format!("{indent}{}({}) -> {}", escape(&op), escape(&inputs.join(", ")), escape(&outputs.join(", ")));
        if !attrs.is_empty() {
            let _ = write!(s, " {{{}}}", attrs.join(", "));
        }
        if !name.is_empty() {
            let _ = write!(s, "  // {}", escape(&name));
        }
        let _ = writeln!(self.out, "{s}");
        for (attr, g) in graphs {
            if depth + 1 > MAX_DEPTH {
                return Err(damaged());
            }
            let _ = writeln!(self.out, "{indent}  {attr}:");
            self.graph_nodes(g, depth + 1)?;
        }
        Ok(())
    }

    fn graph_nodes(&mut self, graph: &[u8], depth: usize) -> Result<()> {
        for f in fields(graph) {
            if let (1, v) = f? {
                self.node(v.bytes(), depth)?;
            }
        }
        Ok(())
    }
}

/// The model header, inputs and outputs, initializers, then every node in graph order.
pub fn graph(data: &[u8]) -> Result<String> {
    let mut out = String::new();
    let (mut ir, mut producer, mut producer_version, mut domain, mut model_version, mut doc) =
        (None, String::new(), String::new(), String::new(), None, String::new());
    let (mut opsets, mut props, mut functions) = (Vec::new(), Vec::new(), Vec::new());
    let mut graph: &[u8] = &[];
    for f in fields(data) {
        let (n, v) = f?;
        match n {
            1 => ir = Some(v.int()),
            2 => producer = v.string(),
            3 => producer_version = v.string(),
            4 => domain = v.string(),
            5 => model_version = Some(v.int()),
            6 => doc = v.string(),
            7 => graph = v.bytes(),
            8 => {
                let (mut d, mut version) = (String::new(), 0);
                for g in fields(v.bytes()) {
                    match g? {
                        (1, x) => d = x.string(),
                        (2, x) => version = x.int(),
                        _ => {}
                    }
                }
                opsets.push((if d.is_empty() { "ai.onnx".to_string() } else { d }, version));
            }
            14 => props.push(key_value(v.bytes())?),
            25 => functions.push(v.bytes()),
            _ => {}
        }
    }
    let Some(ir) = ir else { return Err(Error::Unsupported("ONNX: no IR version; not an ONNX model".into())) };

    // The graph's parts.
    let (mut graph_name, mut graph_doc) = (String::new(), String::new());
    let (mut inputs, mut outputs, mut infos, mut initializers) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut nodes, mut sparse) = (0usize, 0usize);
    for f in fields(graph) {
        let (n, v) = f?;
        match n {
            1 => nodes += 1,
            2 => graph_name = v.string(),
            5 => initializers.push(Tensor::parse(v.bytes())?),
            10 => graph_doc = v.string(),
            11 => inputs.push(value_info(v.bytes())?),
            12 => outputs.push(value_info(v.bytes())?),
            13 => infos.push(value_info(v.bytes())?),
            15 => sparse += 1,
            _ => {}
        }
    }

    let _ = writeln!(out, "=== Model ===");
    line(&mut out, "IR version", ir);
    if !producer.is_empty() {
        line(&mut out, "Producer", escape(format!("{producer} {producer_version}").trim()));
    }
    if !domain.is_empty() {
        line(&mut out, "Domain", escape(&domain));
    }
    if let Some(v) = model_version {
        line(&mut out, "Model version", v);
    }
    opsets.sort();
    for (d, v) in &opsets {
        line(&mut out, "Opset", format!("{} {v}", escape(d)));
    }
    if !graph_name.is_empty() {
        line(&mut out, "Graph", escape(&graph_name));
    }
    for d in [&doc, &graph_doc] {
        if !d.is_empty() {
            line(&mut out, "Description", quote(d));
        }
    }
    line(&mut out, "Nodes", count(nodes as u64));
    let params: u64 = initializers.iter().map(Tensor::elements).sum();
    let bytes: u64 = initializers.iter().map(Tensor::stored_bytes).sum();
    line(
        &mut out,
        "Initializers",
        format!("{} ({} parameters, {})", count(initializers.len() as u64), parameters(params), size(bytes)),
    );
    if sparse > 0 {
        line(&mut out, "Sparse initializers", count(sparse as u64));
    }
    if !functions.is_empty() {
        line(&mut out, "Functions", count(functions.len() as u64));
    }

    if !props.is_empty() {
        props.sort();
        let _ = writeln!(out, "\n=== Metadata ===");
        for (k, v) in &props {
            let _ = writeln!(out, "{} = {}", escape(k), quote(v));
        }
    }
    for (title, list) in [("Inputs", &inputs), ("Outputs", &outputs)] {
        let _ = writeln!(out, "\n=== {title} ===");
        for (name, ty) in list {
            let _ = writeln!(out, "{}: {ty}", escape(name));
        }
    }

    let mut w = Writer { out: &mut out, lines: 0, budget: CHECKSUM_LIMIT };
    if !initializers.is_empty() {
        initializers.sort_by(|a, b| a.name.cmp(&b.name));
        let _ = writeln!(w.out, "\n=== Initializers ({}) ===", count(initializers.len() as u64));
        for t in initializers.iter().take(MAX_LINES) {
            let text = t.describe(&mut w.budget);
            let _ = writeln!(w.out, "{}: {text}", escape(&t.name));
        }
        if initializers.len() > MAX_LINES {
            let _ = writeln!(w.out, "… {} more initializers not shown", count((initializers.len() - MAX_LINES) as u64));
        }
    }

    let _ = writeln!(w.out, "\n=== Nodes ({}) ===", count(nodes as u64));
    w.graph_nodes(graph, 0)?;
    if w.lines > MAX_LINES {
        let _ = writeln!(w.out, "… {} more nodes not shown", count((w.lines - MAX_LINES) as u64));
    }

    for f in functions {
        let (mut name, mut fdomain, mut ins, mut outs) = (String::new(), String::new(), Vec::new(), Vec::new());
        for g in fields(f) {
            match g? {
                (1, x) => name = x.string(),
                (4, x) => ins.push(x.string()),
                (5, x) => outs.push(x.string()),
                (10, x) => fdomain = x.string(),
                _ => {}
            }
        }
        let _ = writeln!(
            w.out,
            "\n=== Function {}::{}({}) -> {} ===",
            escape(&fdomain),
            escape(&name),
            ins.join(", "),
            outs.join(", ")
        );
        for g in fields(f) {
            if let (7, x) = g? {
                w.node(x.bytes(), 0)?;
            }
        }
    }

    if !infos.is_empty() {
        infos.sort();
        let _ = writeln!(out, "\n=== Value info ===");
        for (name, ty) in infos.iter().take(MAX_LINES) {
            let _ = writeln!(out, "{}: {ty}", escape(name));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/onnx/", $name))
        };
    }
    const V1: &[u8] = fixture!("mlp-v1.onnx");
    const V2: &[u8] = fixture!("mlp-v2.onnx");

    fn lines_only_in<'t>(a: &'t str, b: &str) -> Vec<&'t str> {
        a.lines().filter(|l| !b.lines().any(|m| m == *l)).collect()
    }

    #[test]
    fn graph_view() {
        let g = graph(V1).unwrap();
        for expected in [
            "IR version:        8\nProducer:          regenerate.py 1.0\nOpset:             ai.onnx 17\nOpset:             ai.onnx.ml 3\nGraph:             mlp\nDescription:       \"A tiny MLP\"\nNodes:             5\nInitializers:      5 (69 parameters, 284 B)\n",
            "=== Metadata ===\nauthor = \"tests\"\nversion = \"1\"\n",
            "=== Inputs ===\nx: FLOAT [batch, 4]\n\n=== Outputs ===\nprobs: FLOAT [batch, 3]\n",
            "fc1.bias: FLOAT [8] = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]\nfc1.weight: FLOAT [4, 8], 128 B, crc ",
            "shape: INT64 [2] = [-1, 3]\n",
            "=== Nodes (5) ===\nGemm(x, fc1.weight, fc1.bias) -> h1 {alpha=1.0, beta=1.0, transB=0}  // fc1\nRelu(h1) -> a1  // relu\n",
            "Softmax(flat) -> probs {axis=-1}  // softmax\n",
        ] {
            assert!(g.contains(expected), "{expected}\n---\n{g}");
        }
    }

    #[test]
    fn changes_show_as_changed_lines_only() {
        let (g1, g2) = (graph(V1).unwrap(), graph(V2).unwrap());
        let added = lines_only_in(&g2, &g1);
        assert_eq!(added.len(), 6, "{added:?}");
        assert_eq!(added[..2], ["Nodes:             6", "version = \"2\""]);
        assert!(added[2].starts_with("fc2.weight: FLOAT [8, 3], 96 B, crc "), "{added:?}");
        assert_eq!(
            &added[3..],
            ["=== Nodes (6) ===", "Dropout(a1) -> d1  // dropout", "Gemm(d1, fc2.weight, fc2.bias) -> logits  // fc2"]
        );
    }

    #[test]
    fn detection_and_damage() {
        assert!(is_onnx(V1));
        assert!(!is_onnx(b"hello world"));
        assert!(!is_onnx(b""));
        for cut in 0..V1.len() {
            let _ = graph(&V1[..cut]);
            let mut bad = V1.to_vec();
            bad[cut] ^= 0xA5;
            let _ = graph(&bad);
        }
    }
}
