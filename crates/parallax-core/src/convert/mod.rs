//! Content views: ways to turn a file into something a person can compare.
//!
//! Raw bytes are the truth, but for many formats they are a poor thing to look
//! at: a re-saved Word file differs in every byte, a re-linked ELF in its build id,
//! a minified XML file is one 5 MB line. Each [`View`] renders a file into text
//! (or, for images, pixels) that the normal diff machinery can align.

mod a2l;
mod android;
mod binary;
mod cdfx;
mod cert;
mod chat;
mod class;
mod data;
mod dbc;
mod dotnet;
mod dwarf;
mod email;
pub mod gguf;
mod image_diff;
mod image_meta;
mod linkmap;
mod lockfile;
mod mdf;
mod media;
mod notebook;
mod office;
mod onnx;
mod parquet;
mod pretty;
mod safetensors;
mod sqlite;
mod table;
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
    /// Lockfile: one line per pinned package and version.
    Dependencies,
    /// Certificates, requests, CRLs and keys (PEM, DER, PKCS#12), decoded.
    Certificate,
    /// E-mail (.eml, .msg): main headers, body text and attachments.
    Email,
    /// CSV/TSV: the header, then the rows sorted by the key column.
    Table,
    /// Jupyter notebook: cell sources and outputs in short form.
    Notebook,
    /// Jupyter notebook: cell sources only.
    NotebookCode,
    /// YAML with keys sorted and one layout.
    NormalizedYaml,
    /// TOML with keys sorted and one layout.
    NormalizedToml,
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
            View::Dependencies => "Dependencies",
            View::Certificate => "Certificate",
            View::Email => "E-mail",
            View::Table => "Table, sorted by key",
            View::Notebook => "Notebook",
            View::NotebookCode => "Notebook, code only",
            View::NormalizedYaml => "Normalized YAML",
            View::NormalizedToml => "Normalized TOML",
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
    Notebook,
    Lockfile,
    Yaml,
    Toml,
    Table,
    Pem,
    DerCert,
    Pkcs12,
    Eml,
    Msg,
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
    }
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
    if email::is_msg(&ext, data) {
        return Kind::Msg;
    }
    if starts(&[0xD0, 0xCF, 0x11, 0xE0]) && matches!(ext.as_str(), "doc" | "xls" | "ppt" | "dot" | "xlt" | "pot") {
        return Kind::LegacyOffice;
    }
    if starts(b"MZ") && dotnet::is_managed(data) {
        return Kind::DotNet;
    }
    if cert::is_pkcs12(&ext, data) {
        return Kind::Pkcs12;
    }
    if cert::is_der(&ext, data) {
        return Kind::DerCert;
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
    if cert::is_pem(&ext, data) {
        return Kind::Pem;
    }
    if email::is_eml(&ext) {
        return Kind::Eml;
    }
    if lockfile::format_of(name).is_some() {
        return Kind::Lockfile;
    }
    if notebook::is_notebook(&ext, data) {
        return Kind::Notebook;
    }
    if matches!(ext.as_str(), "yaml" | "yml") {
        return Kind::Yaml;
    }
    if ext == "toml" {
        return Kind::Toml;
    }
    if table::is_table(&ext) {
        return Kind::Table;
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
        Kind::Notebook => vec![View::Notebook, View::NotebookCode, View::Text],
        Kind::Lockfile => vec![View::Dependencies, View::Text],
        Kind::Yaml => vec![View::Text, View::NormalizedYaml],
        Kind::Toml => vec![View::Text, View::NormalizedToml],
        Kind::Table => vec![View::Text, View::Table],
        Kind::Pem => vec![View::Certificate, View::Text],
        Kind::DerCert | Kind::Pkcs12 => vec![View::Certificate],
        Kind::Eml => vec![View::Email, View::Text],
        Kind::Msg => vec![View::Email],
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
        View::Dependencies => lockfile::dependencies(name, data),
        View::Certificate => match kind_of(name, data) {
            Kind::Pem => cert::pem(data),
            Kind::DerCert => cert::der(data),
            Kind::Pkcs12 => cert::pkcs12(data),
            _ => Err(Error::Unsupported(format!("{name} is not a certificate or key"))),
        },
        View::Email => email::render(&extension(name), data),
        View::Table => table::sorted(&extension(name), data),
        View::Notebook => notebook::render(data, true),
        View::NotebookCode => notebook::render(data, false),
        View::NormalizedYaml => data::normalized_yaml(data),
        View::NormalizedToml => data::normalized_toml(data),
    }
}

/// Whether [`content_equal`] can apply to a file, judged by its name alone.
pub fn has_content_rule(name: &str) -> bool {
    lockfile::format_of(name).is_some()
        || matches!(
            extension(name).as_str(),
            "a2l"
                | "aif"
                | "aifc"
                | "aiff"
                | "bmp"
                | "cdf"
                | "cdfx"
                | "cer"
                | "cert"
                | "class"
                | "crl"
                | "crt"
                | "csr"
                | "csv"
                | "db"
                | "db3"
                | "dbc"
                | "der"
                | "dll"
                | "docm"
                | "docx"
                | "eml"
                | "exe"
                | "flac"
                | "gif"
                | "ico"
                | "ipynb"
                | "jpeg"
                | "jpg"
                | "key"
                | "mp3"
                | "msg"
                | "odp"
                | "ods"
                | "odt"
                | "pdf"
                | "pem"
                | "png"
                | "pptm"
                | "pptx"
                | "pub"
                | "safetensors"
                | "sqlite"
                | "sqlite3"
                | "tif"
                | "tiff"
                | "toml"
                | "tsv"
                | "wasm"
                | "wav"
                | "webp"
                | "winmd"
                | "xlsm"
                | "xlsx"
                | "yaml"
                | "yml"
        )
}

/// Whether two byte-different files have equal content, for formats where
/// "content" is well defined: document text, image pixels, the code of a Java
/// class. The reason to show when they do; `None` when they differ or the format
/// has no such notion.
pub fn content_equal(name: &str, left: &[u8], right: &[u8]) -> Option<&'static str> {
    let (kl, kr) = (kind_of(name, left), kind_of(name, right));
    // The same certificate as PEM on one side and DER on the other.
    if matches!(kl, Kind::Pem | Kind::DerCert) && matches!(kr, Kind::Pem | Kind::DerCert) {
        let same = cert::same_objects((left, kl == Kind::Pem), (right, kr == Kind::Pem))?;
        return same.then_some(if kl == kr {
            "same certificates and keys, different PEM text"
        } else {
            "same certificates, different encoding"
        });
    }
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
        Kind::Lockfile => {
            return lockfile::same_packages(name, left, right)?
                .then_some("same packages and versions, only order or formatting differ");
        }
        Kind::Notebook => {
            return notebook::same_cells(left, right)?.then_some("same cells, only outputs or metadata differ");
        }
        Kind::Eml | Kind::Msg => {
            return email::same_message(&extension(name), left, right)?
                .then_some("same message, only transport headers or encoding differ");
        }
        Kind::Table => return table::same_rows(&extension(name), left, right),
        Kind::Yaml => {
            return data::same_yaml(left, right)?.then_some("same data, only formatting, order or comments differ");
        }
        Kind::Toml => {
            return data::same_toml(left, right)?.then_some("same data, only formatting, order or comments differ");
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

    #[test]
    fn detects_developer_formats() {
        let fixture = |p: &str| std::fs::read(format!("{}/tests/fixtures/{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let first = |name: &str, data: &[u8]| views_for(name, data)[0];
        assert_eq!(first("a.ipynb", &fixture("notebook/analysis-v1.ipynb")), View::Notebook);
        assert_eq!(
            first("a.json", &fixture("notebook/analysis-v1.ipynb")),
            View::Text,
            "a notebook needs its extension"
        );
        assert_eq!(first("a.ipynb", b"{\"cells\": 1}"), View::Text);
        assert_eq!(first("web/package-lock.json", &fixture("lockfile/npm-v3/package-lock.json")), View::Dependencies);
        assert_eq!(first("package.json", &fixture("lockfile/npm-v3/package.json")), View::Text);
        assert_eq!(first("Cargo.lock", b"\x00\x01"), View::Hex, "binary data is never a lockfile");
        assert_eq!(views_for("x.yaml", b"a: 1\n"), vec![View::Text, View::NormalizedYaml, View::Hex]);
        assert_eq!(views_for("pnpm-lock.yaml", b"lockfileVersion: '9.0'\n")[0], View::Dependencies);
        assert_eq!(views_for("Cargo.toml", b"[a]\n"), vec![View::Text, View::NormalizedToml, View::Hex]);
        assert_eq!(views_for("t.csv", b"a,b\n1,2\n"), vec![View::Text, View::Table, View::Hex]);
        assert_eq!(first("s.pem", &fixture("cert/server.pem")), View::Certificate);
        assert_eq!(first("s.txt", &fixture("cert/server.pem")), View::Certificate);
        assert_eq!(first("s.der", &fixture("cert/server.der")), View::Certificate);
        assert_eq!(first("s.bin", &fixture("cert/server.der")), View::Hex);
        assert_eq!(first("b.p12", &fixture("cert/bundle.p12")), View::Certificate);
        assert_eq!(first("m.eml", &fixture("email/order.eml")), View::Email);
        assert_eq!(first("m.msg", &fixture("email/order.msg")), View::Email);
        assert_eq!(first("m.doc", &fixture("email/order.msg")), views_for("m.doc", b"\xD0\xCF\x11\xE0")[0]);
    }

    #[test]
    fn similar_rules_of_developer_formats() {
        let fixture = |p: &str| std::fs::read(format!("{}/tests/fixtures/{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let eq = |name: &str, a: &str, b: &str| content_equal(name, &fixture(a), &fixture(b));
        assert_eq!(
            eq("a.ipynb", "notebook/analysis-v1.ipynb", "notebook/analysis-v1-rerun.ipynb"),
            Some("same cells, only outputs or metadata differ")
        );
        assert_eq!(eq("a.ipynb", "notebook/analysis-v1.ipynb", "notebook/analysis-v2.ipynb"), None);
        assert_eq!(
            eq("package-lock.json", "lockfile/npm-v2/package-lock.json", "lockfile/npm-v3/package-lock.json"),
            Some("same packages and versions, only order or formatting differ")
        );
        assert_eq!(
            eq("c.yaml", "data/compose.yaml", "data/compose-reformatted.yaml"),
            Some("same data, only formatting, order or comments differ")
        );
        assert_eq!(
            eq("Cargo.toml", "data/Cargo.toml", "data/Cargo-reordered.toml"),
            Some("same data, only formatting, order or comments differ")
        );
        assert_eq!(
            eq("p.csv", "table/products.csv", "table/products-shuffled.csv"),
            Some("same rows, different order")
        );
        assert_eq!(eq("s.crt", "cert/server.pem", "cert/server.der"), Some("same certificates, different encoding"));
        assert_eq!(
            eq("s.pem", "cert/server.pem", "cert/server-with-text.pem"),
            Some("same certificates and keys, different PEM text")
        );
        assert_eq!(eq("s.pem", "cert/server.pem", "cert/server-renewed.pem"), None);
        assert_eq!(
            eq("m.eml", "email/order.eml", "email/order-base64.eml"),
            Some("same message, only transport headers or encoding differ")
        );
        for name in ["a.ipynb", "Cargo.lock", "go.sum", "a.yml", "a.toml", "a.tsv", "a.crt", "a.eml", "a.msg"] {
            assert!(has_content_rule(name), "{name}");
        }
        assert!(!has_content_rule("a.p12"));
    }
}
