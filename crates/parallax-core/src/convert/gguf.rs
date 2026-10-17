//! GGUF model files (llama.cpp, Ollama, LM Studio …), shown from their header.
//!
//! The header holds everything but the weights: metadata keys (architecture,
//! tokenizer, chat template), and per tensor its name, shape and quantization
//! type. It is a few megabytes even for a 70B model, so these views read only the
//! start of a memory-mapped file.
//!
//! Layout (little-endian, version 2 and 3): `GGUF`, version u32, tensor count u64,
//! metadata count u64, the key/value pairs, the tensor infos, then the aligned data.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use crate::{Error, Result};

pub const MAGIC: &[u8] = b"GGUF";

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32),
    U64(u64),
    I64(i64),
    F32(f32),
    F64(f64),
    Bool(bool),
    String(String),
    Array(Vec<Value>),
}

impl Value {
    pub fn as_u64(&self) -> Option<u64> {
        Some(match *self {
            Value::U8(v) => v.into(),
            Value::U16(v) => v.into(),
            Value::U32(v) => v.into(),
            Value::U64(v) => v,
            Value::I8(v) => u64::try_from(v).ok()?,
            Value::I16(v) => u64::try_from(v).ok()?,
            Value::I32(v) => u64::try_from(v).ok()?,
            Value::I64(v) => u64::try_from(v).ok()?,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Value::U8(_) => "u8",
            Value::I8(_) => "i8",
            Value::U16(_) => "u16",
            Value::I16(_) => "i16",
            Value::U32(_) => "u32",
            Value::I32(_) => "i32",
            Value::U64(_) => "u64",
            Value::I64(_) => "i64",
            Value::F32(_) => "f32",
            Value::F64(_) => "f64",
            Value::Bool(_) => "bool",
            Value::String(_) => "string",
            Value::Array(_) => "array",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tensor {
    pub name: String,
    pub dims: Vec<u64>,
    pub ggml_type: u32,
    /// Offset in the data section.
    pub offset: u64,
}

impl Tensor {
    pub fn elements(&self) -> u64 {
        self.dims.iter().product()
    }

    /// Bytes of the tensor's data, when its type is known.
    pub fn bytes(&self) -> Option<u64> {
        let t = ggml_type(self.ggml_type)?;
        Some(self.elements() / t.block * t.size)
    }
}

pub struct Gguf {
    pub version: u32,
    /// Keys in file order.
    pub metadata: Vec<(String, Value)>,
    pub tensors: Vec<Tensor>,
    /// Size of the whole file, for the summary.
    pub file_size: u64,
    /// Start of the tensor data.
    pub data_offset: u64,
}

/// A GGML tensor type: name, elements per block and bytes per block.
pub struct GgmlType {
    pub name: &'static str,
    pub block: u64,
    pub size: u64,
}

impl GgmlType {
    pub fn bits_per_weight(&self) -> f64 {
        self.size as f64 * 8.0 / self.block as f64
    }
}

/// The types of `ggml.h`; removed ones (4, 5, 31–33, 36–38) are not listed.
pub fn ggml_type(id: u32) -> Option<GgmlType> {
    let (name, block, size) = match id {
        0 => ("F32", 1, 4),
        1 => ("F16", 1, 2),
        2 => ("Q4_0", 32, 18),
        3 => ("Q4_1", 32, 20),
        6 => ("Q5_0", 32, 22),
        7 => ("Q5_1", 32, 24),
        8 => ("Q8_0", 32, 34),
        9 => ("Q8_1", 32, 36),
        10 => ("Q2_K", 256, 84),
        11 => ("Q3_K", 256, 110),
        12 => ("Q4_K", 256, 144),
        13 => ("Q5_K", 256, 176),
        14 => ("Q6_K", 256, 210),
        15 => ("Q8_K", 256, 292),
        16 => ("IQ2_XXS", 256, 66),
        17 => ("IQ2_XS", 256, 74),
        18 => ("IQ3_XXS", 256, 98),
        19 => ("IQ1_S", 256, 50),
        20 => ("IQ4_NL", 32, 18),
        21 => ("IQ3_S", 256, 110),
        22 => ("IQ2_S", 256, 82),
        23 => ("IQ4_XS", 256, 136),
        24 => ("I8", 1, 1),
        25 => ("I16", 1, 2),
        26 => ("I32", 1, 4),
        27 => ("I64", 1, 8),
        28 => ("F64", 1, 8),
        29 => ("IQ1_M", 256, 56),
        30 => ("BF16", 1, 2),
        34 => ("TQ1_0", 256, 54),
        35 => ("TQ2_0", 256, 66),
        39 => ("MXFP4", 32, 17),
        _ => return None,
    };
    Some(GgmlType { name, block, size })
}

fn type_name(id: u32) -> String {
    ggml_type(id).map_or_else(|| format!("type {id}"), |t| t.name.to_string())
}

/// `general.file_type`: the quantization a file was made with (`llama_ftype`).
fn file_type_name(id: u64) -> Option<&'static str> {
    Some(match id {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        36 => "TQ1_0",
        37 => "TQ2_0",
        38 => "MXFP4_MOE",
        _ => return None,
    })
}

// ---- Parsing ----

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

fn truncated() -> Error {
    Error::Unsupported("GGUF: the header is truncated or damaged".into())
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.data.len()).ok_or_else(truncated)?;
        let out = &self.data[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("length checked"))
    }

    fn u32(&mut self) -> Result<u32> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64> {
        self.array().map(u64::from_le_bytes)
    }

    /// A count that must fit in what is left of the file, `min_size` bytes per item.
    fn count(&mut self, min_size: usize) -> Result<usize> {
        let n = usize::try_from(self.u64()?).map_err(|_| truncated())?;
        if n.saturating_mul(min_size.max(1)) > self.data.len() - self.pos {
            return Err(truncated());
        }
        Ok(n)
    }

    fn string(&mut self) -> Result<String> {
        let n = self.count(1)?;
        Ok(String::from_utf8_lossy(self.bytes(n)?).into_owned())
    }

    fn value(&mut self, kind: u32, depth: u32) -> Result<Value> {
        Ok(match kind {
            0 => Value::U8(self.array::<1>()?[0]),
            1 => Value::I8(self.array::<1>()?[0] as i8),
            2 => Value::U16(u16::from_le_bytes(self.array()?)),
            3 => Value::I16(i16::from_le_bytes(self.array()?)),
            4 => Value::U32(self.u32()?),
            5 => Value::I32(i32::from_le_bytes(self.array()?)),
            6 => Value::F32(f32::from_le_bytes(self.array()?)),
            7 => Value::Bool(self.array::<1>()?[0] != 0),
            8 => Value::String(self.string()?),
            9 if depth < 4 => {
                let item = self.u32()?;
                let n = self.count(1)?;
                let mut items = Vec::with_capacity(n.min(1 << 20));
                for _ in 0..n {
                    items.push(self.value(item, depth + 1)?);
                }
                Value::Array(items)
            }
            10 => Value::U64(self.u64()?),
            11 => Value::I64(i64::from_le_bytes(self.array()?)),
            12 => Value::F64(f64::from_le_bytes(self.array()?)),
            _ => return Err(Error::Unsupported(format!("GGUF: unknown value type {kind}"))),
        })
    }
}

impl Gguf {
    /// Reads the header of a GGUF file.
    pub fn parse(data: &[u8]) -> Result<Gguf> {
        if !data.starts_with(MAGIC) {
            return Err(Error::Unsupported("not a GGUF file".into()));
        }
        let mut r = Reader { data, pos: 4 };
        let version = r.u32()?;
        match version {
            2 | 3 => {}
            1 => return Err(Error::Unsupported("GGUF version 1 is not supported".into())),
            v if v.swap_bytes() <= 3 => return Err(Error::Unsupported("big-endian GGUF is not supported".into())),
            v => return Err(Error::Unsupported(format!("GGUF version {v} is not supported"))),
        }
        let tensor_count = r.count(24)?;
        let metadata_count = r.count(12)?;
        let mut metadata = Vec::with_capacity(metadata_count);
        for _ in 0..metadata_count {
            let key = r.string()?;
            let kind = r.u32()?;
            metadata.push((key, r.value(kind, 0)?));
        }
        let mut tensors = Vec::with_capacity(tensor_count);
        for _ in 0..tensor_count {
            let name = r.string()?;
            let n_dims = r.u32()?;
            if n_dims > 8 {
                return Err(truncated());
            }
            let dims = (0..n_dims).map(|_| r.u64()).collect::<Result<Vec<_>>>()?;
            let ggml_type = r.u32()?;
            let offset = r.u64()?;
            tensors.push(Tensor { name, dims, ggml_type, offset });
        }
        let alignment = metadata
            .iter()
            .find(|(k, _)| k == "general.alignment")
            .and_then(|(_, v)| v.as_u64())
            .filter(|a| *a > 0)
            .unwrap_or(32);
        let data_offset = (r.pos as u64).div_ceil(alignment) * alignment;
        Ok(Gguf { version, metadata, tensors, file_size: data.len() as u64, data_offset })
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.metadata.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    fn u64(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(Value::as_u64)
    }

    pub fn architecture(&self) -> &str {
        self.str("general.architecture").unwrap_or("?")
    }

    pub fn parameters(&self) -> u64 {
        self.tensors.iter().map(Tensor::elements).sum()
    }

    fn tensor_bytes(&self) -> u64 {
        self.tensors.iter().filter_map(Tensor::bytes).sum()
    }

    /// The text of token `id`.
    pub fn token(&self, id: u64) -> Option<&str> {
        match self.get("tokenizer.ggml.tokens")? {
            Value::Array(tokens) => tokens.get(usize::try_from(id).ok()?)?.as_str(),
            _ => None,
        }
    }

    /// The chat templates: the default one first (named `""`), then named ones.
    pub fn chat_templates(&self) -> Vec<(&str, &str)> {
        let mut out = Vec::new();
        if let Some(t) = self.str("tokenizer.chat_template") {
            out.push(("", t));
        }
        for (k, v) in &self.metadata {
            if let (Some(name), Some(t)) = (k.strip_prefix("tokenizer.chat_template."), v.as_str()) {
                out.push((name, t));
            }
        }
        out
    }

    /// Whether the header ends inside the file: a split part or a cut-off download
    /// does not hold all of its tensor data.
    fn is_complete(&self) -> bool {
        self.tensors.iter().all(|t| t.bytes().is_none_or(|b| self.data_offset + t.offset + b <= self.file_size))
    }
}

// ---- Formatting helpers ----

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn short_hash(data: &[u8]) -> String {
    format!("{:08x}", crc32fast::hash(data))
}

fn value_hash(value: &Value) -> String {
    let mut h = crc32fast::Hasher::new();
    hash_value(value, &mut h);
    format!("{:08x}", h.finalize())
}

fn hash_value(value: &Value, h: &mut crc32fast::Hasher) {
    match value {
        Value::String(s) => {
            h.update(&(s.len() as u64).to_le_bytes());
            h.update(s.as_bytes());
        }
        Value::Array(items) => items.iter().for_each(|v| hash_value(v, h)),
        v => h.update(scalar(v).as_bytes()),
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::U8(v) => v.to_string(),
        Value::I8(v) => v.to_string(),
        Value::U16(v) => v.to_string(),
        Value::I16(v) => v.to_string(),
        Value::U32(v) => v.to_string(),
        Value::I32(v) => v.to_string(),
        Value::U64(v) => v.to_string(),
        Value::I64(v) => v.to_string(),
        Value::F32(v) => format!("{v:?}"),
        Value::F64(v) => format!("{v:?}"),
        Value::Bool(v) => v.to_string(),
        Value::String(s) => format!("\"{}\"", escape(s)),
        Value::Array(_) => String::new(),
    }
}

/// Strings longer than this are shortened in the metadata list.
const LONG_STRING: usize = 160;
/// Arrays longer than this are summarized in the metadata list.
const LONG_ARRAY: usize = 16;

fn describe(value: &Value) -> String {
    match value {
        Value::String(s) if s.chars().count() > LONG_STRING => {
            let head: String = s.chars().take(80).collect();
            format!("\"{}…\" ({} characters, crc {})", escape(&head), s.chars().count(), short_hash(s.as_bytes()))
        }
        Value::Array(items) if items.len() > LONG_ARRAY => {
            let kind = items.first().map_or("empty", Value::type_name);
            format!("[{} × {kind}] (crc {})", items.len(), value_hash(value))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(describe).collect::<Vec<_>>().join(", ")),
        v => scalar(v),
    }
}

fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{v:.2} {}", UNITS[unit]) }
}

fn parameters(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.2} B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1} M", n as f64 / 1e6),
        n => count(n),
    }
}

fn line(out: &mut String, label: &str, value: impl std::fmt::Display) {
    let _ = writeln!(out, "{:<18} {value}", format!("{label}:"));
}

/// A value for the summary: strings without quotes.
fn plain(value: &Value) -> String {
    match value {
        Value::String(s) if s.chars().count() <= LONG_STRING => escape(s),
        v => describe(v),
    }
}

// ---- Views ----

/// Architecture keys shown with a name, in this order; other `<arch>.` keys follow.
const ARCH_KEYS: &[(&str, &str)] = &[
    ("block_count", "Layers"),
    ("context_length", "Context length"),
    ("embedding_length", "Embedding size"),
    ("feed_forward_length", "Feed-forward size"),
    ("attention.head_count", "Attention heads"),
    ("attention.head_count_kv", "KV heads"),
    ("attention.key_length", "Key length"),
    ("attention.value_length", "Value length"),
    ("attention.sliding_window", "Sliding window"),
    ("attention.layer_norm_rms_epsilon", "RMS norm epsilon"),
    ("attention.layer_norm_epsilon", "Norm epsilon"),
    ("rope.dimension_count", "RoPE dimensions"),
    ("rope.freq_base", "RoPE base"),
    ("rope.scaling.type", "RoPE scaling"),
    ("rope.scaling.factor", "RoPE factor"),
    ("rope.scaling.original_context_length", "RoPE original ctx"),
    ("expert_count", "Experts"),
    ("expert_used_count", "Experts per token"),
    ("vocab_size", "Vocabulary size"),
];

/// Overview: what model this is, how big, how quantized.
pub fn summary(data: &[u8]) -> Result<String> {
    let g = Gguf::parse(data)?;
    let arch = g.architecture().to_string();
    let mut out = String::new();
    let _ = writeln!(out, "=== Model ===");
    line(&mut out, "Architecture", &arch);
    for (key, label) in [
        ("general.name", "Name"),
        ("general.basename", "Base name"),
        ("general.size_label", "Size label"),
        ("general.finetune", "Fine-tune"),
        ("general.organization", "Organization"),
        ("general.license", "License"),
    ] {
        if let Some(v) = g.get(key) {
            line(&mut out, label, plain(v));
        }
    }
    for i in 0..g.u64("general.base_model.count").unwrap_or(0) {
        let name = g.str(&format!("general.base_model.{i}.name")).unwrap_or("?");
        let org = g.str(&format!("general.base_model.{i}.organization")).map(|o| format!(" ({o})")).unwrap_or_default();
        line(&mut out, "Base model", format!("{name}{org}"));
    }
    let params = g.parameters();
    line(&mut out, "Parameters", format!("{} ({})", parameters(params), count(params)));
    if let Some(ft) = g.u64("general.file_type") {
        line(&mut out, "File type", format!("{} (general.file_type {ft})", file_type_name(ft).unwrap_or("?")));
    }
    let bytes = g.tensor_bytes();
    if params > 0 {
        line(&mut out, "Bits per weight", format!("{:.2}", bytes as f64 * 8.0 / params as f64));
    }
    line(&mut out, "Tensor data", size(bytes));
    line(&mut out, "File size", size(g.file_size));
    line(&mut out, "Tensors", count(g.tensors.len() as u64));
    line(&mut out, "GGUF version", g.version);
    if let (Some(no), Some(n)) = (g.u64("split.no"), g.u64("split.count")) {
        line(&mut out, "Split", format!("part {} of {n}", no + 1));
    }
    if !g.is_complete() {
        let _ = writeln!(
            out,
            "! The file ends before the end of its tensor data (a split part or an incomplete download)."
        );
    }

    let _ = writeln!(out, "\n=== Architecture ===");
    let prefix = format!("{arch}.");
    let mut shown = Vec::new();
    for (key, label) in ARCH_KEYS {
        let full = format!("{prefix}{key}");
        if let Some(v) = g.get(&full) {
            let mut text = plain(v);
            if *key == "attention.head_count_kv"
                && let (Some(kv), Some(heads)) = (v.as_u64(), g.u64(&format!("{prefix}attention.head_count")))
                && kv > 0
                && kv < heads
            {
                let _ = write!(text, " (grouped-query: {} query heads per KV head)", heads / kv);
            }
            line(&mut out, label, text);
            shown.push(full);
        }
    }
    for (k, v) in &g.metadata {
        if k.starts_with(&prefix) && !shown.contains(k) {
            line(&mut out, &k[prefix.len()..], plain(v));
        }
    }

    let _ = writeln!(out, "\n=== Tokenizer ===");
    tokenizer_summary(&g, &mut out);
    for (name, template) in g.chat_templates() {
        let label = if name.is_empty() { "Chat template".to_string() } else { format!("Template {name}") };
        let lines = template.lines().count();
        line(
            &mut out,
            &label,
            format!("{} characters, {lines} lines, crc {}", template.len(), short_hash(template.as_bytes())),
        );
    }
    if g.chat_templates().is_empty() {
        line(&mut out, "Chat template", "none");
    }

    let _ = writeln!(out, "\n=== Quantization ===");
    recipe(&g, &mut out);
    for (k, v) in &g.metadata {
        if let Some(rest) = k.strip_prefix("quantize.") {
            line(&mut out, rest, plain(v));
        }
    }
    if let Some(v) = g.get("general.quantization_version") {
        line(&mut out, "quantization_version", plain(v));
    }
    Ok(out)
}

fn tokenizer_summary(g: &Gguf, out: &mut String) {
    if let Some(m) = g.str("tokenizer.ggml.model") {
        line(out, "Model", m);
    }
    if let Some(p) = g.str("tokenizer.ggml.pre") {
        line(out, "Pre-tokenizer", p);
    }
    if let Some(all @ Value::Array(tokens)) = g.get("tokenizer.ggml.tokens") {
        line(out, "Vocabulary", format!("{} tokens (crc {})", count(tokens.len() as u64), value_hash(all)));
    }
    if let Some(Value::Array(merges)) = g.get("tokenizer.ggml.merges") {
        line(out, "Merges", count(merges.len() as u64));
    }
    for (key, label) in [
        ("bos_token_id", "BOS"),
        ("eos_token_id", "EOS"),
        ("eot_token_id", "EOT"),
        ("eom_token_id", "EOM"),
        ("padding_token_id", "Padding"),
        ("unknown_token_id", "Unknown"),
        ("seperator_token_id", "Separator"),
        ("mask_token_id", "Mask"),
    ] {
        if let Some(id) = g.u64(&format!("tokenizer.ggml.{key}")) {
            let text = g.token(id).map(|t| format!(" {}", escape(t))).unwrap_or_default();
            line(out, label, format!("{id}{text}"));
        }
    }
    for (key, label) in [("add_bos_token", "Adds BOS"), ("add_eos_token", "Adds EOS")] {
        if let Some(v) = g.get(&format!("tokenizer.ggml.{key}")) {
            line(out, label, plain(v));
        }
    }
}

/// Per type: tensors, weights and bytes, largest share first.
fn recipe(g: &Gguf, out: &mut String) {
    let mut by_type: BTreeMap<u32, (u64, u64, u64)> = BTreeMap::new();
    for t in &g.tensors {
        let e = by_type.entry(t.ggml_type).or_default();
        e.0 += 1;
        e.1 += t.elements();
        e.2 += t.bytes().unwrap_or(0);
    }
    let total: u64 = by_type.values().map(|v| v.2).sum::<u64>().max(1);
    let mut rows: Vec<_> = by_type.into_iter().collect();
    rows.sort_by_key(|(_, v)| std::cmp::Reverse(v.2));
    let _ = writeln!(out, "{:<10} {:>8} {:>10} {:>12} {:>7}", "type", "tensors", "bits/wt", "size", "share");
    for (ty, (n, _, bytes)) in rows {
        let bpw = ggml_type(ty).map_or(String::from("?"), |t| format!("{:.4}", t.bits_per_weight()));
        let share = bytes as f64 * 100.0 / total as f64;
        let _ = writeln!(out, "{:<10} {:>8} {:>10} {:>12} {:>6.1}%", type_name(ty), n, bpw, size(bytes), share);
    }
}

/// Every key and value, sorted by key, one per line; long values are summarized.
pub fn metadata(data: &[u8]) -> Result<String> {
    let g = Gguf::parse(data)?;
    let mut keys: Vec<&(String, Value)> = g.metadata.iter().collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    let _ = writeln!(out, "=== {} keys ===", keys.len());
    for (k, v) in keys {
        let _ = writeln!(out, "{k} = {}", describe(v));
    }
    Ok(out)
}

/// The tokenizer settings, the special tokens, then every token by id.
pub fn tokenizer(data: &[u8]) -> Result<String> {
    let g = Gguf::parse(data)?;
    let mut out = String::new();
    let _ = writeln!(out, "=== Tokenizer ===");
    tokenizer_summary(&g, &mut out);
    let Some(Value::Array(tokens)) = g.get("tokenizer.ggml.tokens") else {
        let _ = writeln!(out, "\n(no vocabulary)");
        return Ok(out);
    };
    let types: Vec<i64> = match g.get("tokenizer.ggml.token_type") {
        Some(Value::Array(t)) => t.iter().map(|v| v.as_u64().map_or(0, |n| n as i64)).collect(),
        _ => Vec::new(),
    };
    let type_name = |i: usize| match types.get(i) {
        Some(2) => "unknown",
        Some(3) => "control",
        Some(4) => "user",
        Some(5) => "unused",
        Some(6) => "byte",
        _ => "normal",
    };
    let special: Vec<usize> =
        (0..tokens.len()).filter(|&i| matches!(type_name(i), "control" | "user" | "unknown")).collect();
    let _ = writeln!(out, "\n=== Special tokens ({}) ===", special.len());
    for i in special {
        let _ = writeln!(out, "{i:>7}  {:<8} {}", type_name(i), escape(tokens[i].as_str().unwrap_or("")));
    }
    let _ = writeln!(out, "\n=== Vocabulary ({} tokens) ===", tokens.len());
    for (i, t) in tokens.iter().enumerate() {
        let _ = writeln!(out, "{i:>7}  {:<8} {}", type_name(i), escape(t.as_str().unwrap_or("")));
    }
    Ok(out)
}

/// The chat templates as Jinja source. Templates stored as one long line are
/// broken before each `{%` tag, so that a change shows as one changed line.
pub fn chat_template(data: &[u8]) -> Result<String> {
    let g = Gguf::parse(data)?;
    let templates = g.chat_templates();
    if templates.is_empty() {
        return Ok("{# This file has no chat template. #}\n".into());
    }
    let mut out = String::new();
    for (name, template) in templates {
        if !name.is_empty() {
            let _ = writeln!(out, "\n{{# === tokenizer.chat_template.{name} === #}}");
        }
        if template.lines().any(|l| l.chars().count() > 300) {
            let _ = writeln!(out, "{{# Stored as long lines; line breaks were added before {{% tags for display. #}}");
            out.push_str(&break_at_tags(template));
        } else {
            out.push_str(template);
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(out)
}

fn break_at_tags(template: &str) -> String {
    let mut out = String::with_capacity(template.len() + template.len() / 20);
    let mut rest = template;
    while let Some(i) = rest.find("{%") {
        out.push_str(&rest[..i]);
        if !out.is_empty() && !out.trim_end_matches([' ', '\t']).ends_with('\n') {
            out.push('\n');
        }
        out.push_str("{%");
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// Sorts tensor names so that `blk.10` follows `blk.9`.
fn natural_key(name: &str) -> Vec<(String, u64)> {
    name.split('.').map(|part| part.parse::<u64>().map_or((part.to_string(), 0), |n| (String::new(), n))).collect()
}

/// Roles shown first in the layer map, in this order.
const ROLES: &[&str] = &[
    "attn_q",
    "attn_k",
    "attn_v",
    "attn_qkv",
    "attn_output",
    "ffn_gate",
    "ffn_up",
    "ffn_down",
    "ffn_gate_inp",
    "ffn_gate_exps",
    "ffn_up_exps",
    "ffn_down_exps",
];

/// The quantization recipe, a map of types per layer, and every tensor.
pub fn tensors(data: &[u8]) -> Result<String> {
    let g = Gguf::parse(data)?;
    let mut out = String::new();
    let _ = writeln!(out, "=== Quantization ===");
    recipe(&g, &mut out);

    // Layer map: one row per layer, one column per weight matrix role.
    let mut layers: BTreeMap<u64, HashMap<String, u32>> = BTreeMap::new();
    let mut roles: Vec<String> = Vec::new();
    for t in &g.tensors {
        let mut parts = t.name.splitn(3, '.');
        let (Some("blk"), Some(n), Some(rest)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let (Ok(n), Some(role)) = (n.parse::<u64>(), rest.strip_suffix(".weight")) else { continue };
        if t.dims.iter().filter(|&&d| d > 1).count() < 2 {
            continue; // Norms and other vectors are kept in F32 anyway.
        }
        if !roles.iter().any(|r| r == role) {
            roles.push(role.to_string());
        }
        layers.entry(n).or_default().insert(role.to_string(), t.ggml_type);
    }
    roles.sort_by_key(|r| (ROLES.iter().position(|k| k == r).unwrap_or(ROLES.len()), r.clone()));
    if !layers.is_empty() {
        let width = |r: &String| r.len().max(7);
        let _ = writeln!(out, "\n=== Layer map ===");
        let _ = write!(out, "{:<6}", "layer");
        for r in &roles {
            let _ = write!(out, " {:<w$}", r, w = width(r));
        }
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
        out.push('\n');
        for (n, types) in &layers {
            let _ = write!(out, "{n:<6}");
            for r in &roles {
                let ty = types.get(r).map_or_else(|| "-".to_string(), |&t| type_name(t));
                let _ = write!(out, " {:<w$}", ty, w = width(r));
            }
            let trimmed = out.trim_end().len();
            out.truncate(trimmed);
            out.push('\n');
        }
    }

    let _ = writeln!(out, "\n=== Tensors ({}) ===", g.tensors.len());
    let mut sorted: Vec<&Tensor> = g.tensors.iter().collect();
    sorted.sort_by_cached_key(|t| natural_key(&t.name));
    let name_width = sorted.iter().map(|t| t.name.len()).max().unwrap_or(0);
    for t in sorted {
        let shape = format!("[{}]", t.dims.iter().map(u64::to_string).collect::<Vec<_>>().join(", "));
        let bytes = t.bytes().map_or(String::from("?"), size);
        let _ = writeln!(out, "{:<name_width$}  {:<20} {:<8} {:>11}", t.name, shape, type_name(t.ggml_type), bytes);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a GGUF header with these keys and tensors (data omitted).
    pub(crate) fn build(metadata: &[(&str, Value)], tensors: &[(&str, &[u64], u32)]) -> Vec<u8> {
        fn string(out: &mut Vec<u8>, s: &str) {
            out.extend((s.len() as u64).to_le_bytes());
            out.extend(s.as_bytes());
        }
        fn value(out: &mut Vec<u8>, v: &Value) {
            match v {
                Value::U32(n) => out.extend(n.to_le_bytes()),
                Value::U64(n) => out.extend(n.to_le_bytes()),
                Value::F32(n) => out.extend(n.to_le_bytes()),
                Value::Bool(b) => out.push(*b as u8),
                Value::String(s) => string(out, s),
                Value::I32(n) => out.extend(n.to_le_bytes()),
                Value::Array(items) => {
                    out.extend(kind(&items[0]).to_le_bytes());
                    out.extend((items.len() as u64).to_le_bytes());
                    items.iter().for_each(|i| value(out, i));
                }
                _ => unimplemented!(),
            }
        }
        fn kind(v: &Value) -> u32 {
            match v {
                Value::U32(_) => 4,
                Value::I32(_) => 5,
                Value::F32(_) => 6,
                Value::Bool(_) => 7,
                Value::String(_) => 8,
                Value::Array(_) => 9,
                Value::U64(_) => 10,
                _ => unimplemented!(),
            }
        }
        let mut out = b"GGUF".to_vec();
        out.extend(3u32.to_le_bytes());
        out.extend((tensors.len() as u64).to_le_bytes());
        out.extend((metadata.len() as u64).to_le_bytes());
        for (k, v) in metadata {
            string(&mut out, k);
            out.extend(kind(v).to_le_bytes());
            value(&mut out, v);
        }
        let mut offset = 0u64;
        for (name, dims, ty) in tensors {
            string(&mut out, name);
            out.extend((dims.len() as u32).to_le_bytes());
            dims.iter().for_each(|d| out.extend(d.to_le_bytes()));
            out.extend(ty.to_le_bytes());
            out.extend(offset.to_le_bytes());
            let t = ggml_type(*ty).unwrap();
            offset += (dims.iter().product::<u64>() / t.block * t.size).div_ceil(32) * 32;
        }
        out
    }

    pub(crate) fn sample(template: &str, attn_v: u32) -> Vec<u8> {
        let tokens = ["<unk>", "<s>", "</s>", "<|im_start|>", "<|im_end|>", "hello", "Ġworld"];
        let s = |v: &str| Value::String(v.into());
        build(
            &[
                ("general.architecture", s("llama")),
                ("general.name", s("Test Model")),
                ("general.file_type", Value::U32(15)),
                ("llama.block_count", Value::U32(2)),
                ("llama.context_length", Value::U32(4096)),
                ("llama.attention.head_count", Value::U32(8)),
                ("llama.attention.head_count_kv", Value::U32(2)),
                ("llama.rope.freq_base", Value::F32(10000.0)),
                ("tokenizer.ggml.model", s("gpt2")),
                ("tokenizer.ggml.tokens", Value::Array(tokens.iter().map(|t| s(t)).collect())),
                (
                    "tokenizer.ggml.token_type",
                    Value::Array([2, 3, 3, 3, 3, 1, 1].iter().map(|&t| Value::I32(t)).collect()),
                ),
                ("tokenizer.ggml.bos_token_id", Value::U32(1)),
                ("tokenizer.ggml.eos_token_id", Value::U32(4)),
                ("tokenizer.chat_template", s(template)),
            ],
            &[
                ("token_embd.weight", &[256, 7], 14),
                ("blk.0.attn_q.weight", &[256, 256], 12),
                ("blk.0.attn_v.weight", &[256, 64], attn_v),
                ("blk.0.attn_norm.weight", &[256], 0),
                ("blk.1.attn_q.weight", &[256, 256], 12),
                ("blk.1.attn_v.weight", &[256, 64], 12),
                ("blk.10.attn_q.weight", &[256, 256], 12),
                ("output.weight", &[256, 7], 14),
            ],
        )
    }

    #[test]
    fn parses_and_summarizes() {
        let data = sample("{{ bos_token }}", 14);
        let g = Gguf::parse(&data).unwrap();
        assert_eq!(g.architecture(), "llama");
        assert_eq!(g.tensors.len(), 8);
        assert_eq!(g.token(4), Some("<|im_end|>"));
        let s = summary(&data).unwrap();
        assert!(s.contains("File type:         Q4_K_M (general.file_type 15)"), "{s}");
        assert!(s.contains("KV heads:          2 (grouped-query: 4 query heads per KV head)"), "{s}");
        assert!(s.contains("EOS:               4 <|im_end|>"), "{s}");
        // Only the header was written: the tensor data is missing.
        assert!(s.contains("! The file ends before the end of its tensor data"), "{s}");
    }

    #[test]
    fn layer_map_and_tensor_order() {
        let t = tensors(&sample("", 14)).unwrap();
        assert!(t.contains("layer  attn_q  attn_v\n0      Q4_K    Q6_K\n1      Q4_K    Q4_K\n10     Q4_K"), "{t}");
        let order: Vec<&str> = t
            .lines()
            .skip_while(|l| !l.starts_with("=== Tensors"))
            .skip(1)
            .map(|l| l.split(' ').next().unwrap())
            .collect();
        assert_eq!(
            &order[..4],
            ["blk.0.attn_norm.weight", "blk.0.attn_q.weight", "blk.0.attn_v.weight", "blk.1.attn_q.weight"]
        );
        assert_eq!(order[5], "blk.10.attn_q.weight");
    }

    #[test]
    fn metadata_tokenizer_and_template() {
        let data = sample("{%- for m in messages %}{{ m.content }}{% endfor %}", 14);
        let m = metadata(&data).unwrap();
        assert!(m.contains("llama.rope.freq_base = 10000.0\n"), "{m}");
        assert!(m.contains("tokenizer.ggml.bos_token_id = 1\n"), "{m}");
        let t = tokenizer(&data).unwrap();
        assert!(t.contains("      3  control  <|im_start|>\n"), "{t}");
        assert!(t.contains("      6  normal   Ġworld\n"), "{t}");
        let long = format!("{{%- for m in messages %}}{}{{% endfor %}}", "x".repeat(400));
        let c = chat_template(&sample(&long, 14)).unwrap();
        assert!(c.contains("\n{%- for m in messages %}"), "{c}");
        assert!(c.contains(&format!("x{}\n{{% endfor %}}", "x".repeat(399))), "{c}");
    }

    #[test]
    fn rejects_damaged_headers() {
        let data = sample("", 14);
        for cut in [3, 10, 30, 200, data.len() - 5] {
            assert!(Gguf::parse(&data[..cut]).is_err(), "cut at {cut}");
        }
        let mut huge = data.clone();
        huge[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(Gguf::parse(&huge).is_err());
    }
}
