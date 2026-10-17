//! Content views: ways to turn a file into something a person can compare.
//!
//! Raw bytes are the truth, but for many formats they are a poor thing to look
//! at: a re-saved Word file differs in every byte, a re-linked ELF in its build id,
//! a minified XML file is one 5 MB line. Each [`View`] renders a file into text
//! (or, for images, pixels) that the normal diff machinery can align.

mod android;
mod a2l;
mod binary;
mod cdfx;
mod chat;
mod class;
mod dotnet;
mod dbc;
mod dwarf;
pub mod gguf;
mod image_diff;
mod linkmap;
mod mdf;
mod image_meta;
mod media;
mod office;
mod onnx;
mod parquet;
mod pretty;
mod safetensors;
mod sqlite;
mod wasm;

use serde::{Deserialize, Serialize};

pub use image_diff::{ImageDiff, ImageSide, image_diff};

use crate::text;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum View {
    /// The file as text.
    Text,
    /// Text extracted from Word, Excel, PowerPoint, OpenDocument and PDF files.
    Document,
    /// Sections, symbols and imports of ELF, PE and Mach-O files.
    Executable,
    /// Intel HEX and Motorola S-record files decoded into an address-aligned byte map.
    Firmware,
    /// XML re-indented, one element per line.
    PrettyXml,
    /// JSON re-indented.
    PrettyJson,
    /// A hex dump, 16 bytes per line.
    Hex,
    /// Raster images compared pixel by pixel, or SVG rendered.
    Image,
    /// GGUF model: architecture, size, tokenizer and quantization overview.
    Model,
    /// GGUF model: the chat templates as Jinja source.
    ChatTemplate,
    /// GGUF model: the chat template applied to sample conversations.
    ChatPreview,
    /// GGUF model: every metadata key and value.
    Metadata,
    /// GGUF model: tokenizer settings, special tokens and the vocabulary.
    Tokenizer,
    /// GGUF model: quantization recipe, layer map and tensor list.
    Tensors,
    /// Java class: declaration, fields and methods, with a checksum of each method's code.
    ClassOutline,
    /// Java class: the instructions of every method.
    Bytecode,
    /// Java class: the instructions with the source line numbers.
    BytecodeLines,
    /// SQLite database: tables, columns, indexes, views and triggers.
    DatabaseSchema,
    /// SQLite database: the rows of every table.
    DatabaseRows,
    /// Parquet or Arrow IPC file: schema, row groups or record batches, column statistics.
    DataSummary,
    /// ONNX model: opsets, inputs and outputs, initializers and the nodes of the graph.
    ModelGraph,
    /// WebAssembly module: imports, exports, data, and each function's signature and code checksum.
    WasmOutline,
    /// WebAssembly module: the instructions of every function.
    WasmText,
    /// .NET assembly: identity, references, types and members, with a checksum of each method's IL.
    AssemblyOutline,
    /// .NET assembly: the IL of every method.
    Il,
    /// Android binary XML (manifest, resources) decoded to XML text.
    AndroidXml,
    /// DBC: messages sorted by id with their signals, comments, attributes and value tables.
    CanDatabase,
    /// A2L: characteristics, measurements, conversions and the other objects, sorted by kind and name.
    A2lObjects,
    /// GNU ld / lld map: memory regions, output sections, size by object, symbols.
    MemoryMap,
    /// ELF with DWARF: global and static variables with types, functions with address ranges.
    DebugSymbols,
    /// MDF 3/4 measurement: header, channel groups and channels from the block tree.
    Measurement,
    /// CDFX calibration data: every instance's values, one per line, sorted by name.
    Calibration,
    /// Raster images: format, colour, EXIF, XMP, IPTC, ICC profile and text chunks.
    ImageMetadata,
    /// Audio and video files: container, duration, tracks, tags, chapters, attachments.
    MediaSummary,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Text => "Text",
            View::Document => "Document text",
            View::Executable => "Executable summary",
            View::Firmware => "Firmware address map",
            View::PrettyXml => "Pretty-printed XML",
            View::PrettyJson => "Pretty-printed JSON",
            View::Hex => "Hex dump",
            View::Image => "Image",
            View::Model => "Model summary",
            View::ChatTemplate => "Chat template",
            View::ChatPreview => "Chat template, rendered",
            View::Metadata => "Metadata",
            View::Tokenizer => "Tokenizer",
            View::Tensors => "Tensors and quantization",
            View::ClassOutline => "Class outline",
            View::Bytecode => "Bytecode",
            View::BytecodeLines => "Bytecode with line numbers",
            View::DatabaseSchema => "Database schema",
            View::DatabaseRows => "Database rows",
            View::DataSummary => "Data file summary",
            View::ModelGraph => "Model graph",
            View::WasmOutline => "Module outline",
            View::WasmText => "WebAssembly text",
            View::AssemblyOutline => "Assembly outline",
            View::Il => "IL",
            View::AndroidXml => "Android XML",
            View::CanDatabase => "CAN database",
            View::A2lObjects => "A2L objects",
            View::MemoryMap => "Memory map",
            View::DebugSymbols => "Debug symbols",
            View::Measurement => "Measurement summary",
            View::Calibration => "Calibration values",
            View::ImageMetadata => "Image metadata",
            View::MediaSummary => "Media summary",
        }
    }
}

/// Hex dumps of larger files would be millions of lines; a byte-level compare is the better tool there.
pub const MAX_HEX_BYTES: usize = 16 * 1024 * 1024;
/// Minified XML and JSON is pretty-printed by default when a line is longer than this.
const LONG_LINE: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Pdf,
    Ooxml,
    Odf,
    LegacyOffice,
    Executable,
    Raster,
    Svg,
    IntelHex,
    Srec,
    Xml,
    Json,
    Text,
    Binary,
    Gguf,
    JavaClass,
    Sqlite,
    DataFile,
    SafeTensors,
    Onnx,
    Wasm,
    DotNet,
    AndroidXml,
    Dbc,
    A2l,
    LinkMap,
    Mdf,
    Cdfx,
    /// HEIF and AVIF images: metadata only, the pixels are not decoded.
    Heif,
    Media,
}

fn extension(name: &str) -> String {
    name.rsplit('/').next().unwrap_or(name).rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn kind_of(name: &str, data: &[u8]) -> Kind {
    let ext = extension(name);
    let starts = |magic: &[u8]| data.starts_with(magic);
    let first_char = data.iter().find(|b| !b.is_ascii_whitespace()).copied();
    if starts(b"%PDF-") {
        return Kind::Pdf;
    }
    if starts(gguf::MAGIC) {
        return Kind::Gguf;
    }
    if ext == "onnx" && onnx::is_onnx(data) {
        return Kind::Onnx;
    }
    if safetensors::is_safetensors(data) {
        return Kind::SafeTensors;
    }
    if sqlite::is_sqlite(data) {
        return Kind::Sqlite;
    }
    if parquet::is_parquet(data) || parquet::is_arrow(data) {
        return Kind::DataFile;
    if mdf::is_mdf(data) {
        return Kind::Mdf;
    }
    if class::is_class(data) {
        return Kind::JavaClass;
    }
    if wasm::is_wasm(data) {
        return Kind::Wasm;
    }
    if ext == "xml" && android::is_binary_xml(data) {
        return Kind::AndroidXml;
    }
    if starts(b"PK\x03\x04") {
        if matches!(ext.as_str(), "docx" | "docm" | "dotx" | "xlsx" | "xlsm" | "xltx" | "pptx" | "pptm" | "potx") {
            return Kind::Ooxml;
        }
        if matches!(ext.as_str(), "odt" | "ods" | "odp" | "ott" | "ots" | "otp") {
            return Kind::Odf;
        }
    }
    if starts(&[0xD0, 0xCF, 0x11, 0xE0]) && matches!(ext.as_str(), "doc" | "xls" | "ppt" | "dot" | "xlt" | "pot") {
        return Kind::LegacyOffice;
    }
    if starts(b"MZ") && dotnet::is_managed(data) {
        return Kind::DotNet;
    }
    if starts(b"\x7FELF")
        || starts(b"MZ")
        || starts(&[0xCF, 0xFA, 0xED, 0xFE])
        || starts(&[0xCE, 0xFA, 0xED, 0xFE])
        || starts(&[0xCA, 0xFE, 0xBA, 0xBE])
    {
        return Kind::Executable;
    }
    if media::is_heif(data) {
        return Kind::Heif;
    }
    if media::detect(&ext, data).is_some() {
        return Kind::Media;
    }
    if image::guess_format(data).is_ok() {
        return Kind::Raster;
    }
    if text::is_binary(data) {
        return Kind::Binary;
    }
    if ext == "svg" {
        return Kind::Svg;
    }
    if matches!(ext.as_str(), "hex" | "ihex" | "ihx" | "h86" | "mcs" | "a43") && first_char == Some(b':') {
        return Kind::IntelHex;
    }
    if matches!(ext.as_str(), "s19" | "s28" | "s37" | "srec" | "mot" | "sx" | "s") && first_char == Some(b'S') {
        return Kind::Srec;
    }
    if ext == "dbc" {
        return Kind::Dbc;
    }
    if ext == "a2l" {
        return Kind::A2l;
    }
    if ext == "map" && linkmap::detect(data).is_some() {
        return Kind::LinkMap;
    }
    if matches!(ext.as_str(), "cdfx" | "cdf") && cdfx::is_cdfx(data) {
        return Kind::Cdfx;
    }
    if crate::highlight::is_xml_name(name) || data.starts_with(b"<?xml") {
        return Kind::Xml;
    }
    if ext == "json" || (matches!(first_char, Some(b'{' | b'[')) && ext.is_empty()) {
        return Kind::Json;
    }
    Kind::Text
}

/// Views that make sense for a file, best first.
pub fn views_for(name: &str, data: &[u8]) -> Vec<View> {
    let longest_line = || data.split(|&b| b == b'\n').map(<[u8]>::len).max().unwrap_or(0);
    let mut views = match kind_of(name, data) {
        Kind::Pdf | Kind::Ooxml | Kind::Odf => vec![View::Document],
        Kind::LegacyOffice if office::soffice().is_some() => vec![View::Document],
        Kind::LegacyOffice | Kind::Binary => vec![],
        Kind::Executable if dwarf::has_debug_info(data) => vec![View::Executable, View::DebugSymbols],
        Kind::Executable => vec![View::Executable],
        Kind::Raster => vec![View::Image, View::ImageMetadata],
        Kind::Heif => vec![View::ImageMetadata],
        Kind::Media => vec![View::MediaSummary],
        Kind::Svg => vec![View::Text, View::PrettyXml, View::Image],
        Kind::IntelHex | Kind::Srec => vec![View::Firmware, View::Text],
        Kind::Xml if longest_line() > LONG_LINE => vec![View::PrettyXml, View::Text],
        Kind::Xml => vec![View::Text, View::PrettyXml],
        Kind::Json if longest_line() > LONG_LINE => vec![View::PrettyJson, View::Text],
        Kind::Json => vec![View::Text, View::PrettyJson],
        Kind::Text => vec![View::Text],
        Kind::Gguf => {
            vec![View::Model, View::ChatTemplate, View::ChatPreview, View::Metadata, View::Tokenizer, View::Tensors]
        }
        Kind::JavaClass => vec![View::ClassOutline, View::Bytecode, View::BytecodeLines],
        Kind::Sqlite => vec![View::DatabaseSchema, View::DatabaseRows],
        Kind::DataFile => vec![View::DataSummary],
        Kind::SafeTensors => vec![View::Model, View::Tensors],
        Kind::Onnx => vec![View::ModelGraph],
        Kind::Wasm => vec![View::WasmOutline, View::WasmText],
        Kind::DotNet => vec![View::AssemblyOutline, View::Il, View::Executable],
        Kind::AndroidXml => vec![View::AndroidXml],
        Kind::Dbc => vec![View::CanDatabase, View::Text],
        Kind::A2l => vec![View::A2lObjects, View::Text],
        Kind::LinkMap => vec![View::MemoryMap, View::Text],
        Kind::Mdf => vec![View::Measurement],
        Kind::Cdfx => vec![View::Calibration, View::Text, View::PrettyXml],
    };
    views.push(View::Hex);
    views
}

/// Views for a pair of files (either side may be missing): those both support, in
/// the order of the left side.
pub fn views_for_pair(left: Option<(&str, &[u8])>, right: Option<(&str, &[u8])>) -> Vec<View> {
    match (left.map(|(n, d)| views_for(n, d)), right.map(|(n, d)| views_for(n, d))) {
        (Some(l), Some(r)) => l.into_iter().filter(|v| r.contains(v)).collect(),
        (Some(v), None) | (None, Some(v)) => v,
        (None, None) => vec![View::Text],
    }
}

/// Renders a file as text for a text-based view.
pub fn render(view: View, name: &str, data: &[u8]) -> Result<String> {
    match view {
        View::Text => Ok(text::decode(data).text),
        View::Document => match kind_of(name, data) {
            Kind::Pdf => office::pdf_text(data),
            Kind::Ooxml => office::ooxml_text(&extension(name), data),
            Kind::Odf => office::odf_text(data),
            Kind::LegacyOffice => office::legacy_text(&extension(name), data),
            _ => Err(Error::Unsupported(format!("{name} is not a document"))),
        },
        View::Executable => binary::executable_summary(data),
        View::Firmware => binary::firmware_map(data),
        View::PrettyXml => pretty::xml(&text::decode(data).text),
        View::PrettyJson => pretty::json(data),
        View::Hex => binary::hex_dump(data),
        View::Image => Err(Error::Unsupported("images are not rendered as text".into())),
        View::Model if safetensors::is_safetensors(data) => safetensors::summary(data),
        View::Model => gguf::summary(data),
        View::ChatTemplate => gguf::chat_template(data),
        View::ChatPreview => chat::preview(data),
        View::Metadata => gguf::metadata(data),
        View::Tokenizer => gguf::tokenizer(data),
        View::Tensors if safetensors::is_safetensors(data) => safetensors::tensors(data),
        View::Tensors => gguf::tensors(data),
        View::ClassOutline => class::outline(data),
        View::Bytecode => class::bytecode(data, false),
        View::BytecodeLines => class::bytecode(data, true),
        View::DatabaseSchema => sqlite::schema(data),
        View::DatabaseRows => sqlite::rows(data),
        View::DataSummary => parquet::summary(data),
        View::ModelGraph => onnx::graph(data),
        View::WasmOutline => wasm::outline(data),
        View::WasmText => wasm::text(data),
        View::AssemblyOutline => dotnet::outline(data),
        View::Il => dotnet::il(data),
        View::AndroidXml => android::to_xml(data),
        View::CanDatabase => dbc::summary(&text::decode(data).text),
        View::A2lObjects => a2l::objects(&text::decode(data).text),
        View::MemoryMap => linkmap::memory_map(&text::decode(data).text),
        View::DebugSymbols => dwarf::debug_symbols(data),
        View::Measurement => mdf::summary(data),
        View::Calibration => cdfx::values(&text::decode(data).text),
        View::ImageMetadata => image_meta::metadata(data),
        View::MediaSummary => media::summary(&extension(name), data),
    }
}

/// Whether [`content_equal`] can apply to a file, judged by its name alone.
pub fn has_content_rule(name: &str) -> bool {
    matches!(
        extension(name).as_str(),
        "pdf"
            | "docx"
            | "docm"
            | "xlsx"
            | "xlsm"
            | "pptx"
            | "pptm"
            | "odt"
            | "ods"
            | "odp"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "webp"
            | "tif"
            | "tiff"
            | "ico"
            | "class"
            | "db"
            | "sqlite"
            | "sqlite3"
            | "db3"
            | "safetensors"
            | "wasm"
            | "dll"
            | "exe"
            | "winmd"
            | "dbc"
            | "a2l"
            | "cdfx"
            | "cdf"
            | "mp3"
            | "flac"
            | "wav"
            | "aif"
            | "aiff"
            | "aifc"
    )
}

/// Whether two byte-different files have equal content, for formats where
/// "content" is well defined: document text, image pixels, the code of a Java
/// class. The reason to show when they do; `None` when they differ or the format
/// has no such notion.
pub fn content_equal(name: &str, left: &[u8], right: &[u8]) -> Option<&'static str> {
    let (kl, kr) = (kind_of(name, left), kind_of(name, right));
    if kl != kr {
        return None;
    }
    let equal = match kl {
        Kind::Pdf | Kind::Ooxml | Kind::Odf => {
            render(View::Document, name, left).ok()? == render(View::Document, name, right).ok()?
        }
        Kind::Raster => image_diff::pixels_equal(left, right)?,
        Kind::Media => return media::same_media(&extension(name), left, right),
        Kind::JavaClass => {
            return class::same_code(left, right)?
                .then_some("same code, only debug info or constant pool layout differ");
        }
        Kind::Sqlite => {
            return sqlite::same_content(left, right)?.then_some("same schema and rows, only page layout differs");
        }
        Kind::SafeTensors => {
            return safetensors::same_tensors(left, right)?.then_some("same tensors, only metadata differs");
        }
        Kind::Wasm => return wasm::same_code(left, right)?.then_some("same code, only custom sections differ"),
        Kind::DotNet => return dotnet::same_code(left, right)?.then_some("same code, only build identity differs"),
        Kind::Dbc => {
            return dbc::same_database(&text::decode(left).text, &text::decode(right).text)
                .then_some("same messages and signals, only order or formatting differ");
        }
        Kind::A2l => {
            return a2l::same_objects(&text::decode(left).text, &text::decode(right).text)?
                .then_some("same objects, only order or formatting differ");
        }
        Kind::Cdfx => {
            return cdfx::same_values(&text::decode(left).text, &text::decode(right).text)?
                .then_some("same values, only order or formatting differ");
        }
        _ => false,
    };
    equal.then_some("same content, the file bytes differ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_views() {
        assert_eq!(views_for("a.pdf", b"%PDF-1.4\n"), vec![View::Document, View::Hex]);
        assert_eq!(views_for("fw.hex", b":10000000\n"), vec![View::Firmware, View::Text, View::Hex]);
        assert_eq!(views_for("fw.s19", b"S00F0000\n"), vec![View::Firmware, View::Text, View::Hex]);
        assert_eq!(
            views_for("x.arxml", b"<?xml version=\"1.0\"?>\n<A/>\n"),
            vec![View::Text, View::PrettyXml, View::Hex]
        );
        let minified = format!("<a>{}</a>", "<b>x</b>".repeat(500));
        assert_eq!(views_for("x.xml", minified.as_bytes())[0], View::PrettyXml);
        assert_eq!(views_for("a.bin", b"\x00\x01"), vec![View::Hex]);
        let class = b"\xCA\xFE\xBA\xBE\x00\x00\x00\x41";
        assert_eq!(
            views_for("A.class", class),
            vec![View::ClassOutline, View::Bytecode, View::BytecodeLines, View::Hex]
        );
        let universal_binary = b"\xCA\xFE\xBA\xBE\x00\x00\x00\x02\x01\x00\x00\x07";
        assert_eq!(views_for("tool", universal_binary)[0], View::Executable);
        assert_eq!(views_for("notes.txt", b"hello"), vec![View::Text, View::Hex]);
        let pair = views_for_pair(Some(("a.xml", b"<a/>")), Some(("a.xml", b"\x00")));
        assert_eq!(pair, vec![View::Hex]);
    }

    macro_rules! fixture {
        ($path:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/", $path))
        };
    }

    #[test]
    fn detects_data_and_model_files() {
        let sqlite = fixture!("sqlite/shop-v1.sqlite");
        for name in ["shop.db", "shop.sqlite", "shop.sqlite3", "shop.db3", "shop", "Cookies"] {
            assert_eq!(views_for(name, sqlite), vec![View::DatabaseSchema, View::DatabaseRows, View::Hex], "{name}");
        }
        let data_files: [&[u8]; 3] = [
            fixture!("parquet/sales-v1.parquet"),
            fixture!("parquet/sales-v1.feather"),
            fixture!("parquet/sales-v2.arrow"),
        ];
        for data in data_files {
            assert_eq!(views_for("data", data), vec![View::DataSummary, View::Hex]);
        }
        let st = fixture!("safetensors/tiny-v1.safetensors");
        assert_eq!(views_for("model.safetensors", st), vec![View::Model, View::Tensors, View::Hex]);
        assert!(render(View::Model, "model.safetensors", st).unwrap().contains("Tensors:           19"));
        let onnx = fixture!("onnx/mlp-v1.onnx");
        assert_eq!(views_for("mlp.onnx", onnx), vec![View::ModelGraph, View::Hex]);
        assert_ne!(views_for("mlp.bin", onnx)[0], View::ModelGraph, "ONNX is recognised by its extension");
        // Look-alikes: the magic alone is not enough.
        assert_eq!(views_for("x.db", b"SQLite format 3\0"), vec![View::Hex]);
        assert_eq!(views_for("x.parquet", b"PAR1 but no footer"), vec![View::Text, View::Hex]);
        assert_eq!(views_for("notes.txt", b"\x10\0\0\0\0\0\0\0not json at all!"), vec![View::Hex]);
    }

    #[test]
    fn similar_data_and_model_files() {
        let v1 = fixture!("sqlite/shop-v1.sqlite");
        assert!(has_content_rule("shop.sqlite") && has_content_rule("a.db") && has_content_rule("m.safetensors"));
        assert_eq!(
            content_equal("shop.sqlite", v1, fixture!("sqlite/shop-v1-vacuum.db")),
            Some("same schema and rows, only page layout differs")
        );
        assert_eq!(content_equal("shop.sqlite", v1, fixture!("sqlite/shop-v2.sqlite")), None);
        let st = fixture!("safetensors/tiny-v1.safetensors");
        assert_eq!(
            content_equal("m.safetensors", st, fixture!("safetensors/tiny-v1-meta.safetensors")),
            Some("same tensors, only metadata differs")
        );
        assert_eq!(content_equal("m.safetensors", st, fixture!("safetensors/tiny-v2.safetensors")), None);
    }
}
