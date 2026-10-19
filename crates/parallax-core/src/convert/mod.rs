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
mod conan;
mod data;
mod dbc;
mod dotnet;
mod dwarf;
mod ecuc;
mod email;
mod ftl;
pub mod gguf;
mod image_diff;
mod image_meta;
mod js;
mod linkmap;
mod lockfile;
mod mdf;
mod media;
mod needs;
mod notebook;
mod office;
mod onnx;
mod parquet;
mod perl;
mod ppt;
mod pretty;
mod python;
mod rst;
mod rust;
mod safetensors;
mod sqlite;
mod table;
mod visio;
mod wasm;
mod xls;
mod xpand;

use serde::{Deserialize, Serialize};

pub use image_diff::{ImageDiff, ImageSide, image_diff};

use crate::text;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum View {
    /// The file as text.
    Text,
    /// Text extracted from Word, Excel, PowerPoint, OpenDocument, Visio and PDF files.
    Document,
    /// Visio drawings: pages, shapes with their text, position, data and connections.
    Diagram,
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
    /// reStructuredText with each paragraph on one line and headings normalized.
    ReflowedText,
    /// Sphinx-Needs: every need sorted by id, with its options and text.
    Needs,
    /// FreeMarker template: directives, macro calls and interpolations, indented by nesting.
    TemplateOutline,
    /// Conan graph JSON: every package with its binary, settings, options and requirements.
    ConanGraph,
    /// conanfile.txt, profiles, conaninfo.txt: sections sorted, comments and spacing dropped.
    NormalizedConan,
    /// Python: imports, assignments, classes and functions with a checksum of each body.
    PythonOutline,
    /// Perl: per package the modules used, variables and subs with a checksum of each body.
    PerlOutline,
    /// Rust: items per module with fields, members and a checksum of each function body.
    RustOutline,
    /// JavaScript and TypeScript: imports, declarations and a checksum of each function body.
    JsOutline,
    /// Xpand template: imports and every DEFINE and AROUND with a checksum of its body.
    XpandOutline,
    /// Xtend extensions and Check constraints: imports, functions and checks with a checksum of each body.
    XtendOutline,
    /// AUTOSAR EcucValues: modules, containers and parameter values as simplified JSON, editable.
    EcucValues,
}

impl View {
    pub fn label(self) -> &'static str {
        match self {
            View::Text => "Text",
            View::Document => "Document text",
            View::Diagram => "Diagram",
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
            View::ReflowedText => "Text, reflowed",
            View::Needs => "Needs",
            View::TemplateOutline | View::XpandOutline => "Template outline",
            View::EcucValues => "ECUC values",
            View::ConanGraph => "Conan graph",
            View::NormalizedConan => "Normalized",
            View::PythonOutline | View::PerlOutline | View::RustOutline | View::JsOutline | View::XtendOutline => {
                "Code outline"
            }
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
    Visio,
    LegacyVisio,
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
    Rst,
    NeedsJson,
    /// FreeMarker template.
    Ftl,
    /// Conan graph JSON (`conan graph info --format=json`).
    ConanGraph,
    /// conanfile.txt, a profile, conaninfo.txt or conanbuildinfo.txt.
    ConanIni,
    ConanManifest,
    Python,
    Perl,
    Rust,
    Js(js::Dialect),
    /// Xpand template (.xpt).
    Xpand,
    /// Xtend extensions (.ext) of openArchitectureWare.
    Xtend,
    /// Check constraints (.chk).
    Check,
    /// ARXML with ECUC module configuration values.
    Ecuc,
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
        if visio::XML_EXTENSIONS.contains(&ext.as_str()) {
            return Kind::Visio;
        }
    }
    if email::is_msg(&ext, data) {
        return Kind::Msg;
    }
    if starts(&[0xD0, 0xCF, 0x11, 0xE0]) && matches!(ext.as_str(), "doc" | "xls" | "ppt" | "dot" | "xlt" | "pot") {
        return Kind::LegacyOffice;
    }
    if starts(&[0xD0, 0xCF, 0x11, 0xE0]) && visio::BINARY_EXTENSIONS.contains(&ext.as_str()) {
        return Kind::LegacyVisio;
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
    if let Some(kind) = script_kind(&ext, data) {
        return kind;
    }
    if conan::is_graph_json(name, data) {
        return Kind::ConanGraph;
    }
    if conan::is_manifest(name) {
        return Kind::ConanManifest;
    }
    if conan::is_ini(name, data) {
        return Kind::ConanIni;
    }
    if matches!(ext.as_str(), "ftl" | "ftlh" | "ftlx") && ftl::is_template(data) {
        return Kind::Ftl;
    }
    if let Some(kind) = xpand_kind(&ext, data) {
        return kind;
    }
    if matches!(ext.as_str(), "rst" | "rest") {
        return Kind::Rst;
    }
    if needs::is_needs_json(name, data) {
        return Kind::NeedsJson;
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
    if matches!(ext.as_str(), "arxml" | "xml") && ecuc::is_ecuc(data) {
        return Kind::Ecuc;
    }
    if crate::highlight::is_xml_name(name) || data.starts_with(b"<?xml") {
        return Kind::Xml;
    }
    if ext == "json" || (matches!(first_char, Some(b'{' | b'[')) && ext.is_empty()) {
        return Kind::Json;
    }
    Kind::Text
}

/// Xpand, Xtend and Check files by extension. The extensions are shared with
/// SAS transport files and CHKDSK fragments, so binary data is left alone and a
/// template must contain a statement.
fn xpand_kind(ext: &str, data: &[u8]) -> Option<Kind> {
    let kind = match ext {
        // `«` is 0xAB in ISO-8859-1, the encoding Xpand defaults to, and ends its UTF-8 form.
        "xpt" if data.contains(&0xAB) => Kind::Xpand,
        "ext" => Kind::Xtend,
        "chk" if data.windows(8).any(|w| w == b"context ") => Kind::Check,
        _ => return None,
    };
    (!data.iter().take(8192).any(|&b| b == 0)).then_some(kind)
}

/// Views that make sense for a file, best first.
pub fn views_for(name: &str, data: &[u8]) -> Vec<View> {
    let longest_line = || data.split(|&b| b == b'\n').map(<[u8]>::len).max().unwrap_or(0);
    let mut views = match kind_of(name, data) {
        Kind::Pdf | Kind::Ooxml | Kind::Odf => vec![View::Document],
        Kind::LegacyOffice if office::soffice().is_some() => vec![View::Document],
        Kind::Visio => vec![View::Diagram, View::Document],
        Kind::LegacyVisio if office::soffice().is_some() => vec![View::Diagram, View::Document],
        Kind::LegacyOffice | Kind::LegacyVisio | Kind::Binary => vec![],
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
        Kind::Rst if needs::rst_has_needs(&text::decode(data).text) => {
            vec![View::Text, View::Needs, View::ReflowedText]
        }
        Kind::Rst => vec![View::Text, View::ReflowedText],
        Kind::NeedsJson => vec![View::Needs, View::PrettyJson, View::Text],
        Kind::Ftl => vec![View::Text, View::TemplateOutline],
        Kind::ConanGraph => vec![View::ConanGraph, View::PrettyJson, View::Text],
        Kind::Python => vec![View::Text, View::PythonOutline],
        Kind::Perl => vec![View::Text, View::PerlOutline],
        Kind::Rust => vec![View::Text, View::RustOutline],
        Kind::Js(_) => vec![View::Text, View::JsOutline],
        Kind::Xpand => vec![View::Text, View::XpandOutline],
        Kind::Xtend | Kind::Check => vec![View::Text, View::XtendOutline],
        Kind::Ecuc => vec![View::EcucValues, View::Text, View::PrettyXml],
        Kind::ConanIni => vec![View::Text, View::NormalizedConan],
        Kind::ConanManifest => vec![View::Text],
    };
    views.push(View::Hex);
    views
}

/// Python and Perl by extension, `.t` tests and scripts without one by their first line.
fn script_kind(ext: &str, data: &[u8]) -> Option<Kind> {
    let first = data.split(|&b| b == b'\n').next().unwrap_or_default();
    let shebang = |lang: &[u8]| first.starts_with(b"#!") && first.windows(lang.len()).any(|w| w == lang);
    match ext {
        "py" | "pyi" | "pyw" => Some(Kind::Python),
        "rs" => Some(Kind::Rust),
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => {
            js::Dialect::of(&format!("x.{ext}")).map(Kind::Js)
        }
        "pl" | "pm" | "psgi" => Some(Kind::Perl),
        "t" if shebang(b"perl") || data.windows(4).take(4096).any(|w| w == b"use ") => Some(Kind::Perl),
        "" if shebang(b"python") => Some(Kind::Python),
        "" if shebang(b"node") => Some(Kind::Js(js::Dialect { ts: false, jsx: false })),
        "" if shebang(b"perl") => Some(Kind::Perl),
        _ => None,
    }
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
            Kind::Visio => visio::diagram_text(data),
            Kind::LegacyVisio => visio::legacy_text(&extension(name), data),
            _ => Err(Error::Unsupported(format!("{name} is not a document"))),
        },
        View::Diagram => match kind_of(name, data) {
            Kind::Visio => visio::diagram(data),
            Kind::LegacyVisio => visio::legacy_diagram(&extension(name), data),
            _ => Err(Error::Unsupported(format!("{name} is not a Visio drawing"))),
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
        View::ReflowedText => Ok(rst::reflow(&text::decode(data).text)),
        View::Needs if needs::is_needs_json(name, data) => needs::render_json(data),
        View::Needs => Ok(needs::render_rst(&text::decode(data).text)),
        View::TemplateOutline => Ok(ftl::outline(&text::decode(data).text)),
        View::ConanGraph => conan::graph(data),
        View::PythonOutline => Ok(python::outline(&text::decode(data).text)),
        View::PerlOutline => Ok(perl::outline(&text::decode(data).text)),
        View::RustOutline => Ok(rust::outline(&text::decode(data).text)),
        View::JsOutline => match kind_of(name, data) {
            Kind::Js(d) => Ok(js::outline(&text::decode(data).text, d)),
            _ => Ok(js::outline(&text::decode(data).text, js::Dialect { ts: false, jsx: true })),
        },
        View::NormalizedConan => Ok(conan::normalized(&text::decode(data).text)),
        View::XpandOutline => Ok(xpand::template_outline(&text::decode(data).text)),
        View::XtendOutline if extension(name) == "chk" => Ok(xpand::check_outline(&text::decode(data).text)),
        View::XtendOutline => Ok(xpand::extension_outline(&text::decode(data).text)),
        View::EcucValues => ecuc::render(&text::decode(data).text).map_err(Error::Unsupported),
    }
}

/// For files that differ in bytes but are ≈ similar: the first of `views` in which
/// they look the same, and why they are similar. A file opened from a folder
/// compare is shown in that view, so ≈ does not open on a page of differences.
pub fn similar_view(name: &str, left: &[u8], right: &[u8], views: &[View]) -> Option<(View, &'static str)> {
    const MAX: usize = 64 * 1024 * 1024;
    // The size first: comparing large files byte for byte is what this avoids.
    if left.len().max(right.len()) > MAX || left == right {
        return None;
    }
    let reason = content_equal(name, left, right)?;
    if views.first() == Some(&View::Image) {
        return Some((View::Image, reason));
    }
    views
        .iter()
        .filter(|v| !matches!(v, View::Text | View::Hex | View::Image))
        .find(|&&v| match (render(v, name, left), render(v, name, right)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        })
        .map(|&v| (v, reason))
}

/// Views whose edited text can be saved back into the file it was made from.
pub fn can_write_back(view: View) -> bool {
    view == View::EcucValues
}

/// The bytes of `original` with the edited text of `view` applied, in the
/// file's encoding. `other` is the file on the other side, which tells how to
/// write what `original` does not have yet.
pub fn write_back(
    view: View,
    original: &[u8],
    edited: &str,
    other: Option<&[u8]>,
) -> std::result::Result<Vec<u8>, String> {
    if view != View::EcucValues {
        return Err(format!("{} cannot be saved back", view.label()));
    }
    let mut doc = text::decode(original);
    let donor = other.map(|o| text::decode(o).text);
    let out = ecuc::write_back(&doc.text, edited, donor.as_deref())?;
    let len = doc.text.len();
    doc.splice(0..len, &out);
    doc.encode()
}

/// Source files larger than this are compared by bytes only (a minified
/// bundle of 4 MB takes about a second).
const MAX_SOURCE_COMPARE: usize = 16 * 1024 * 1024;

/// Whether [`content_equal`] can apply to a file, judged by its name alone.
pub fn has_content_rule(name: &str) -> bool {
    lockfile::format_of(name).is_some()
        || name.rsplit(['/', '\\']).next().is_some_and(|f| {
            ["needs.json", "conanfile.txt", "conaninfo.txt", "conanbuildinfo.txt", "conanmanifest.txt"]
                .iter()
                .any(|n| f.eq_ignore_ascii_case(n))
        })
        || conan::is_graph_name(name)
        || visio::XML_EXTENSIONS.contains(&extension(name).as_str())
        || visio::BINARY_EXTENSIONS.contains(&extension(name).as_str())
        || matches!(
            extension(name).as_str(),
            "a2l"
                | "rs"
                | "js"
                | "mjs"
                | "cjs"
                | "jsx"
                | "ts"
                | "mts"
                | "cts"
                | "tsx"
                | "py"
                | "pyi"
                | "pyw"
                | "pl"
                | "pm"
                | "t"
                | "psgi"
                | "arxml"
                | "xpt"
                | "ext"
                | "chk"
                | "ftl"
                | "ftlh"
                | "ftlx"
                | "rst"
                | "rest"
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
        Kind::Visio | Kind::LegacyVisio => {
            return (render(View::Diagram, name, left).ok()? == render(View::Diagram, name, right).ok()?)
                .then_some("same pages, shapes and masters, only the file layout differs");
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
        Kind::Rst => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            if rst::reflow(&l) == rst::reflow(&r) {
                return Some("same text, only line wrapping or heading underlines differ");
            }
            return needs::same_rst_needs(&l, &r)
                .then_some("same needs, only their order or the text around them differ");
        }
        Kind::Python => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return python::same_code(&l, &r).then_some("same code, only comments or formatting differ");
        }
        Kind::Rust if left.len().max(right.len()) <= MAX_SOURCE_COMPARE => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return rust::same_code(&l, &r).then_some("same code, only comments or formatting differ");
        }
        Kind::Js(d) if left.len().max(right.len()) <= MAX_SOURCE_COMPARE => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return js::same_code(&l, &r, d).then_some("same code, only comments or formatting differ");
        }
        Kind::Perl => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return perl::same_code(&l, &r).then_some("same code, only comments, POD or formatting differ");
        }
        Kind::ConanGraph => {
            return conan::same_graph(left, right)?
                .then_some("same packages and binaries, only paths, metadata, binary status or remotes differ");
        }
        Kind::ConanIni => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return (conan::normalized(&l) == conan::normalized(&r))
                .then_some("same entries, only comments, spacing or section order differ");
        }
        Kind::ConanManifest => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return conan::same_manifest(&l, &r).then_some("same files and checksums, only the time differs");
        }
        Kind::Ftl => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return (ftl::without_comments(&l) == ftl::without_comments(&r))
                .then_some("same template, only comments differ");
        }
        Kind::Xpand if left.len().max(right.len()) <= MAX_SOURCE_COMPARE => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return xpand::same_template(&l, &r)
                .then_some("same template, only comments, text outside definitions or spacing in directives differ");
        }
        Kind::Xtend | Kind::Check if left.len().max(right.len()) <= MAX_SOURCE_COMPARE => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return xpand::same_code(&l, &r).then_some("same code, only comments or formatting differ");
        }
        Kind::Ecuc => {
            let (l, r) = (text::decode(left).text, text::decode(right).text);
            return (ecuc::render(&l).ok()? == ecuc::render(&r).ok()?)
                .then_some("same configuration, only order, UUIDs, admin data, number spelling or package differ");
        }
        Kind::NeedsJson => {
            return needs::same_needs_json(left, right)?
                .then_some("same needs, only build time, line numbers or order differ");
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
    fn restructured_text_and_sphinx_needs() {
        let fixture =
            |p: &str| std::fs::read(format!("{}/tests/fixtures/needs/{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(
            views_for("doc/index.rst", &fixture("v1.rst")),
            vec![View::Text, View::Needs, View::ReflowedText, View::Hex]
        );
        assert_eq!(
            views_for("readme.rst", b"Title\n=====\n\nText.\n"),
            vec![View::Text, View::ReflowedText, View::Hex]
        );
        // A Sphinx-Needs export, whatever it is called.
        assert_eq!(views_for("_build/needs.json", &fixture("v1-needs.json"))[0], View::Needs);
        assert_eq!(views_for("export.json", &fixture("v1-needs.json"))[0], View::Needs);
        assert_eq!(views_for("other.json", b"{\"a\": 1}")[0], View::Text);

        // The same project built twice: only timestamps differ.
        let (a, b) = (fixture("v1-needs.json"), fixture("v1-rebuilt-needs.json"));
        assert_ne!(a, b);
        assert_eq!(
            content_equal("needs.json", &a, &b),
            Some("same needs, only build time, line numbers or order differ")
        );
        assert_eq!(content_equal("needs.json", &a, &fixture("v2-needs.json")), None);
        let rst = fixture("v1.rst");
        assert_eq!(
            content_equal("index.rst", &rst, &fixture("v1-rewrapped.rst")),
            Some("same text, only line wrapping or heading underlines differ")
        );
        assert_eq!(content_equal("index.rst", &rst, &fixture("v2.rst")), None);
        assert!(has_content_rule("docs/index.rst") && has_content_rule("_build/needs.json"));

        // v2 moves, re-wraps and reorders needs, and changes three things.
        let view = |name: &str, f: &str| render(View::Needs, name, &fixture(f)).unwrap();
        let (l, r) = (view("index.rst", "v1.rst"), view("index.rst", "v2.rst"));
        let changed: Vec<(&str, &str)> = l.lines().zip(r.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!(l.lines().count(), r.lines().count());
        assert_eq!(
            changed,
            [
                ("status: open", "status: done"),
                ("    At least 12 characters.", "    At least 16 characters."),
                ("links: IMPL_001", "links: IMPL_001, REQ_002"),
            ]
        );
        let json = view("needs.json", "v1-needs.json");
        assert!(json.starts_with("=== Demo · 5 needs: 1 impl, 2 req, 1 spec, 1 test ===\n"));
        assert!(!json.contains("lineno") && !json.contains("created"));
        assert!(json.contains("--- SPEC_001 [spec] Password rules ---\nlinks: REQ_001\n"));
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

    #[test]
    fn freemarker_templates() {
        let page = b"<#-- greeting -->\n<#if user??>\n  <p>Hello ${user.name}!</p>\n</#if>\n";
        assert_eq!(views_for("mail.ftlh", page), vec![View::Text, View::TemplateOutline, View::Hex]);
        assert_eq!(views_for("feed.ftlx", page), vec![View::Text, View::TemplateOutline, View::Hex]);
        assert_eq!(render(View::TemplateOutline, "mail.ftl", page).unwrap(), "<#if user??>\n  ${user.name}\n");
        // Mozilla Fluent shares the extension.
        assert_eq!(views_for("en/main.ftl", b"hello = Hello, { $name }!\n"), vec![View::Text, View::Hex]);
        assert!(has_content_rule("templates/mail.ftl"));

        let other_comment = b"<#-- says hello -->\n<#if user??>\n  <p>Hello ${user.name}!</p>\n</#if>\n";
        let no_comment = b"<#if user??>\n  <p>Hello ${user.name}!</p>\n</#if>\n";
        assert_eq!(content_equal("mail.ftl", page, other_comment), Some("same template, only comments differ"));
        assert_eq!(content_equal("mail.ftl", page, no_comment), Some("same template, only comments differ"));
        let changed = b"<#if user??>\n  <p>Hi ${user.name}!</p>\n</#if>\n";
        assert_eq!(content_equal("mail.ftl", page, changed), None);
    }

    #[test]
    fn conan_files() {
        let fixture = |f: &str| std::fs::read(format!("{}/tests/fixtures/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        // Lockfiles of Conan 2 and Conan 1.
        let lock = render(View::Dependencies, "conan.lock", &fixture("lockfile/conan2/conan.lock")).unwrap();
        assert!(lock.starts_with("Lockfile:          conan.lock v0.5 (Conan 2)\nPackages:          4\nRequirements:      build 1, host 3\n"), "{lock}");
        assert!(lock.contains("\nbuildgen 0.9#17967da9981477673f62ac43eddc1928  (build)\n"), "{lock}");
        assert!(lock.contains("\ngreet 1.0#6758f5880448684b35a3ce0af5b6b56d\n"), "{lock}");
        let old = render(View::Dependencies, "conan.lock", &fixture("lockfile/conan1/conan.lock")).unwrap();
        assert!(old.contains("conan.lock v0.4 (Conan 1)"), "{old}");
        assert!(
            old.contains(
                "\nmylib 2.0@demo/stable#d98606ba7df8f7a0bfc6c04ca48ae6bb:854a137aa0ee845d119b149356a52d39a51a7726\n"
            ),
            "{old}"
        );
        assert!(
            old.contains(
                "\ncmake 3.27.0#cb43cd731c36ea7686308c8306ee5781:4db1be536558d833e52e862fd84d64d75c2b3656  (build)\n"
            ),
            "{old}"
        );
        let (a, b) = (fixture("lockfile/conan2/conan.lock"), fixture("lockfile/conan2-updated/conan.lock"));
        assert_eq!(content_equal("conan.lock", &a, &b), None);
        let a_stamped = String::from_utf8(a.clone()).unwrap().replace("%1792328293.769473", "%1800000000.1");
        assert!(content_equal("conan.lock", &a, a_stamped.as_bytes()).is_some());

        // Graph JSON: the graph of `conan graph info` and of `conan install` agree.
        let (graph, install) = (fixture("conan/graph-v1.json"), fixture("conan/install-v1.json"));
        assert_eq!(views_for("graph.json", &graph)[0], View::ConanGraph);
        assert_eq!(views_for("whatever.json", &graph)[0], View::ConanGraph);
        let g = render(View::ConanGraph, "graph.json", &graph).unwrap();
        assert!(
            g.starts_with(
                "Packages:          4 (build 1, host 3)\nBinaries:          Cache 4\n\n=== Consumer conanfile ===\n"
            ),
            "{g}"
        );
        assert!(g.contains("\n=== buildgen/0.9#17967da9981477673f62ac43eddc1928 (build context) ===\n"), "{g}");
        assert!(g.contains("Requires:\n  greet/1.0#6758f5880448684b35a3ce0af5b6b56d\n  jsonhdr/3.1#fa511254dd41001e01b45a7b62f9a9df\nTool requires:\n  buildgen/0.9#17967da9981477673f62ac43eddc1928\n"), "{g}");
        assert!(g.contains("Options:\n  fPIC=True\n  shared=False\n"), "{g}");
        assert!(!g.contains("/home/dev"), "no paths: {g}");
        assert_eq!(
            content_equal("graph.json", &graph, &install),
            Some("same packages and binaries, only paths, metadata, binary status or remotes differ")
        );
        let downloaded =
            String::from_utf8(graph.clone()).unwrap().replace("\"binary\": \"Cache\"", "\"binary\": \"Download\"");
        assert!(content_equal("graph.json", &graph, downloaded.as_bytes()).is_some());
        assert_eq!(content_equal("graph.json", &graph, &fixture("conan/graph-v2.json")), None);
        assert!(
            has_content_rule("out/graph.json")
                && has_content_rule("conan-create.json")
                && !has_content_rule("package.json")
        );

        // conanfile.txt and profiles.
        let (c1, c2) = (fixture("conan/conanfile-v1.txt"), fixture("conan/conanfile-v2.txt"));
        assert_eq!(views_for("conanfile.txt", &c1), vec![View::Text, View::NormalizedConan, View::Hex]);
        assert_eq!(
            views_for("profiles/linux-gcc", &fixture("conan/linux-gcc-v1")),
            vec![View::Text, View::NormalizedConan, View::Hex]
        );
        let moved = "[generators]\nCMakeToolchain\nCMakeDeps\n\n# deps\n[requires]\ngreet/1.0\njsonhdr/3.1\n[layout]\ncmake_layout\n[options]\ngreet/*:shared = False\n[tool_requires]\nbuildgen/0.9\n";
        assert_eq!(
            content_equal("conanfile.txt", &c1, moved.as_bytes()),
            Some("same entries, only comments, spacing or section order differ")
        );
        assert_eq!(content_equal("conanfile.txt", &c1, &c2), None);
        assert!(has_content_rule("conanfile.txt") && has_content_rule("p/conanmanifest.txt"));
        assert_eq!(
            content_equal("conanmanifest.txt", b"1\na: 1\n", b"2\na: 1\n"),
            Some("same files and checksums, only the time differs")
        );
    }

    #[test]
    fn python_and_perl_source() {
        let fixture =
            |f: &str| std::fs::read(format!("{}/tests/fixtures/source/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        // Python: Black's reformatting is the same code, a change is not.
        let (py, black, py2) =
            (fixture("inventory-v1.py"), fixture("inventory-v1-black.py"), fixture("inventory-v2.py"));
        assert_eq!(views_for("shop/inventory.py", &py), vec![View::Text, View::PythonOutline, View::Hex]);
        assert_eq!(content_equal("inventory.py", &py, &black), Some("same code, only comments or formatting differ"));
        assert_eq!(content_equal("inventory.py", &py, &py2), None);
        let (o1, o2) = (
            render(View::PythonOutline, "inventory.py", &py).unwrap(),
            render(View::PythonOutline, "inventory.py", &black).unwrap(),
        );
        assert_eq!(o1, o2, "the outline does not change with formatting");
        assert!(o1.contains("\nclass Inventory(object):\n    \"All items, by SKU.\"  # docstring "), "{o1}");
        assert!(o1.contains("\n    def low_stock(self, counts, limit=LOW_STOCK):  # code "), "{o1}");
        let changed = render(View::PythonOutline, "inventory.py", &py2).unwrap();
        assert!(
            changed.contains("\n    def total(self, counts, currency=\"EUR\"):  # code ")
                && changed.contains("\n    def remove(self, sku):  # code "),
            "{changed}"
        );
        assert!(changed.contains("\nLOW_STOCK = 3\n"), "{changed}");

        // Perl: comments, POD and layout do not count.
        let (pm, tidy, pm2) = (fixture("Cart-v1.pm"), fixture("Cart-v1-tidy.pm"), fixture("Cart-v2.pm"));
        assert_eq!(views_for("lib/Shop/Cart.pm", &pm), vec![View::Text, View::PerlOutline, View::Hex]);
        assert_eq!(content_equal("Cart.pm", &pm, &tidy), Some("same code, only comments, POD or formatting differ"));
        assert_eq!(content_equal("Cart.pm", &pm, &pm2), None);
        let (p1, p2) =
            (render(View::PerlOutline, "Cart.pm", &pm).unwrap(), render(View::PerlOutline, "Cart.pm", &tidy).unwrap());
        assert_eq!(p1, p2);
        assert!(p1.starts_with("package Shop::Cart;\nuse Carp qw(croak);\nuse List::Util qw(sum0);\nuse strict;\nuse warnings;\nour $VERSION = '1.02';\nuse constant TAX => 0.19;\n"), "{p1}");
        assert!(p1.contains("\nsub add  # code ") && p1.contains("\n# data "), "{p1}");
        assert!(render(View::PerlOutline, "Cart.pm", &pm2).unwrap().contains("\nsub clear  # code "));

        // Scripts by their first line; `.t` tests.
        assert_eq!(views_for("bin/tool", b"#!/usr/bin/env python3\nprint(1)\n")[1], View::PythonOutline);
        assert_eq!(views_for("bin/tool", b"#!/usr/bin/perl -w\nprint 1;\n")[1], View::PerlOutline);
        assert_eq!(views_for("t/basic.t", b"use Test::More;\nok(1);\n")[1], View::PerlOutline);
        assert!(has_content_rule("x.py") && has_content_rule("Cart.pm") && has_content_rule("run.pl"));
    }

    #[test]
    fn ecuc_values_view_and_write_back() {
        let fixture =
            |p: &str| std::fs::read(format!("{}/tests/fixtures/ecuc/{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let (v1, other, v2) = (fixture("Can-v1.arxml"), fixture("Can-v1-other-tool.arxml"), fixture("Can-v2.arxml"));
        assert_eq!(
            views_for("Can_EcucValues.arxml", &v1),
            vec![View::EcucValues, View::Text, View::PrettyXml, View::Hex]
        );
        assert_eq!(views_for("System.arxml", b"<?xml version=\"1.0\"?>\n<AUTOSAR/>\n")[0], View::Text);
        // Another package name, parameter order, number spelling and no UUIDs: the same configuration.
        assert_eq!(
            content_equal("Can_EcucValues.arxml", &v1, &other),
            Some("same configuration, only order, UUIDs, admin data, number spelling or package differ")
        );
        assert_eq!(content_equal("Can_EcucValues.arxml", &v1, &v2), None);
        assert!(has_content_rule("Can_EcucValues.arxml"));

        // The view of v2 saved into v1 changes one value; everything else keeps its bytes.
        assert!(can_write_back(View::EcucValues) && !can_write_back(View::PrettyXml));
        let edited = render(View::EcucValues, "Can.arxml", &v2).unwrap();
        let saved = write_back(View::EcucValues, &v1, &edited, Some(&v2)).unwrap();
        assert_eq!(render(View::EcucValues, "Can.arxml", &saved).unwrap(), edited);
        let (a, b) = (String::from_utf8(v1.clone()).unwrap(), String::from_utf8(saved).unwrap());
        assert_eq!(a.replace(">INTERRUPT<", ">POLLING<"), b);
        let err = write_back(View::EcucValues, &v1, &edited.replace("\"POLLING\"", "POLLING"), None).unwrap_err();
        assert!(err.contains("POLLING is not a number"), "{err}");
    }

    #[test]
    fn xpand_views_and_similarity() {
        let fixture =
            |p: &str| std::fs::read(format!("{}/tests/fixtures/source/{p}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        // Templates are ISO-8859-1; the guillemets must survive decoding.
        let (t1, tidy, t2) = (fixture("Root-v1.xpt"), fixture("Root-v1-tidy.xpt"), fixture("Root-v2.xpt"));
        assert!(text::decode(&t1).text.contains("«DEFINE Root FOR Model»"));
        assert_eq!(views_for("templates/Root.xpt", &t1), vec![View::Text, View::XpandOutline, View::Hex]);
        assert_eq!(
            content_equal("Root.xpt", &t1, &tidy),
            Some("same template, only comments, text outside definitions or spacing in directives differ")
        );
        assert_eq!(content_equal("Root.xpt", &t1, &t2), None);
        let outline = render(View::XpandOutline, "Root.xpt", &t1).unwrap();
        assert_eq!(outline, render(View::XpandOutline, "Root.xpt", &tidy).unwrap());
        assert!(outline.starts_with("«EXTENSION templates::Naming»\n«IMPORT entities»\n"), "{outline}");
        assert!(
            outline.contains("«DEFINE Entity FOR Entity»") && outline.contains("«FILE javaFileName()»"),
            "{outline}"
        );
        let outline2 = render(View::XpandOutline, "Root.xpt", &t2).unwrap();
        assert!(outline2.contains("«DEFINE Setter FOR Attribute»"), "{outline2}");

        // Xtend extensions and Check constraints.
        let (e1, etidy, e2) = (fixture("Naming-v1.ext"), fixture("Naming-v1-tidy.ext"), fixture("Naming-v2.ext"));
        assert_eq!(views_for("Naming.ext", &e1), vec![View::Text, View::XtendOutline, View::Hex]);
        assert_eq!(content_equal("Naming.ext", &e1, &etidy), Some("same code, only comments or formatting differ"));
        assert_eq!(content_equal("Naming.ext", &e1, &e2), None);
        let outline = render(View::XtendOutline, "Naming.ext", &e1).unwrap();
        assert_eq!(outline, render(View::XtendOutline, "Naming.ext", &etidy).unwrap());
        assert!(outline.contains("cached String javaType(Type this)"), "{outline}");
        let (c1, c2) = (fixture("Checks-v1.chk"), fixture("Checks-v2.chk"));
        assert_eq!(views_for("Checks.chk", &c1)[1], View::XtendOutline);
        assert_eq!(content_equal("Checks.chk", &c1, &c2), None);
        // A changed severity is a changed line, not a moved one.
        let outline2 = render(View::XtendOutline, "Checks.chk", &c2).unwrap();
        let pos = |o: &str| o.lines().position(|l| l.contains("should start upper case"));
        assert_eq!(pos(&render(View::XtendOutline, "Checks.chk", &c1).unwrap()), pos(&outline2));
        let outline = render(View::XtendOutline, "Checks.chk", &c1).unwrap();
        assert!(
            outline.contains("context Attribute if type != null ERROR \"Attribute \" + name + \" has no type\""),
            "{outline}"
        );

        // The extensions also name SAS transport files and CHKDSK fragments; workflows are XML.
        assert_eq!(views_for("data.xpt", b"HEADER RECORD*******LIBRARY HEADER RECORD!!!!!!!\0\0"), vec![View::Hex]);
        assert_eq!(views_for("notes.xpt", b"plain text\n"), vec![View::Text, View::Hex]);
        assert_eq!(views_for("FILE0000.CHK", b"\0\x01\x02 binary"), vec![View::Hex]);
        assert_eq!(views_for("generator.mwe", b"<workflow>\n</workflow>\n")[1], View::PrettyXml);
        assert_eq!(views_for("generator.oaw", b"<workflow>\n</workflow>\n")[1], View::PrettyXml);
        assert!(has_content_rule("Root.xpt") && has_content_rule("a.ext") && has_content_rule("Checks.chk"));
    }

    #[test]
    fn rust_javascript_and_typescript_source() {
        let fixture =
            |f: &str| std::fs::read(format!("{}/tests/fixtures/source/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let same = Some("same code, only comments or formatting differ");
        // Rust and rustfmt.
        let (rs, fmt, rs2) = (fixture("geometry-v1.rs"), fixture("geometry-v1-fmt.rs"), fixture("geometry-v2.rs"));
        assert_eq!(views_for("src/geometry.rs", &rs), vec![View::Text, View::RustOutline, View::Hex]);
        assert_eq!(content_equal("geometry.rs", &rs, &fmt), same);
        assert_eq!(content_equal("geometry.rs", &rs, &rs2), None);
        let o = render(View::RustOutline, "geometry.rs", &rs).unwrap();
        assert_eq!(o, render(View::RustOutline, "geometry.rs", &fmt).unwrap());
        assert!(o.contains("\npub struct Point {\n    pub x: f64,\n    pub y: f64,\n}\n"), "{o}");
        assert!(o.contains("\nimpl Point {\n    pub fn distance(&self, other: &Point) -> f64  // code "), "{o}");
        let o2 = render(View::RustOutline, "geometry.rs", &rs2).unwrap();
        assert!(
            o2.contains("\n    Square { corner: Point, side: f64 },\n")
                && o2.contains("\npub fn largest(shapes: &[Shape]) -> Option<&Shape>  // code "),
            "{o2}"
        );

        // TypeScript and Prettier.
        let (ts, pretty, ts2) = (fixture("cart-v1.ts"), fixture("cart-v1-prettier.ts"), fixture("cart-v2.ts"));
        assert_eq!(views_for("src/cart.ts", &ts), vec![View::Text, View::JsOutline, View::Hex]);
        assert_eq!(content_equal("cart.ts", &ts, &pretty), same);
        assert_eq!(content_equal("cart.ts", &ts, &ts2), None);
        let o = render(View::JsOutline, "cart.ts", &ts).unwrap();
        assert_eq!(o, render(View::JsOutline, "cart.ts", &pretty).unwrap());
        assert!(o.contains("\nexport interface CartLine {\n    item: Item\n    quantity: number\n}\n"), "{o}");
        assert!(o.contains("\n    add(item: Item, quantity = 1): this  // code "), "{o}");
        assert!(render(View::JsOutline, "cart.ts", &ts2).unwrap().contains("\n    remove(id: string): this  // code "));
        // TSX: JSX whitespace and parentheses as Prettier writes them.
        let (tsx, tsx_pretty) = (fixture("Badge-v1.tsx"), fixture("Badge-v1-prettier.tsx"));
        assert_eq!(content_equal("Badge.tsx", &tsx, &tsx_pretty), same);
        assert_eq!(
            content_equal(
                "Badge.tsx",
                &tsx,
                &String::from_utf8(tsx.clone()).unwrap().replace("{' '}", "").into_bytes()
            ),
            None
        );
        assert!(has_content_rule("a.rs") && has_content_rule("b.tsx") && has_content_rule("c.mjs"));
        assert_eq!(views_for("bin/cli", b"#!/usr/bin/env node\nconsole.log(1)\n")[1], View::JsOutline);
    }

    fn vsdx(shape_a: &str, deflate: bool) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let method = if deflate { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored };
        let opts = zip::write::SimpleFileOptions::default().compression_method(method);
        let shape = |id: &str, text: &str, x: &str| {
            format!(
                r#"<Shape ID="{id}" Type="Shape"><Cell N="PinX" V="{x}"/><Cell N="PinY" V="1"/><Cell N="Width" V="1"/><Cell N="Height" V="0.5"/><Text>{text}</Text></Shape>"#
            )
        };
        let parts = [
            (
                "visio/pages/pages.xml",
                r#"<Pages><Page ID="0" NameU="Flow" Name="Flow"><PageSheet><Cell N="PageWidth" V="8.5" U="IN"/><Cell N="PageHeight" V="11" U="IN"/></PageSheet><Rel r:id="rId1"/></Page></Pages>"#.to_string(),
            ),
            (
                "visio/pages/_rels/pages.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="page1.xml"/></Relationships>"#.to_string(),
            ),
            (
                "visio/pages/page1.xml",
                format!(
                    r#"<PageContents><Shapes>{}{}<Shape ID="3" NameU="Dynamic connector" Name="Dynamic connector" Type="Shape"><Cell N="BeginX" V="1.5"/><Cell N="BeginY" V="1"/><Cell N="EndX" V="2.5"/><Cell N="EndY" V="1"/></Shape></Shapes><Connects><Connect FromSheet="3" FromCell="BeginX" ToSheet="1"/><Connect FromSheet="3" FromCell="EndX" ToSheet="2"/></Connects></PageContents>"#,
                    shape("1", shape_a, "1"),
                    shape("2", "End", "3")
                ),
            ),
        ];
        for (name, text) in parts {
            w.start_file(name, opts).unwrap();
            w.write_all(text.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn visio_diagrams() {
        let v1 = vsdx("Start", true);
        assert_eq!(views_for("flow.vsdx", &v1), vec![View::Diagram, View::Document, View::Hex]);
        let diagram = render(View::Diagram, "flow.vsdx", &v1).unwrap();
        assert!(diagram.starts_with("=== Page: Flow (8.5 in × 11 in) ===\n"), "{diagram}");
        assert!(diagram.contains("  text: Start\n  at 1, 1 · size 1 × 0.5\n"), "{diagram}");
        assert!(diagram.contains("shape 1 “Start” → shape 2 “End”"), "{diagram}");
        assert_eq!(render(View::Document, "flow.vsdx", &v1).unwrap(), "=== Page: Flow ===\nStart\nEnd\n");

        // Saved again with other compression: same drawing.
        let stored = vsdx("Start", false);
        assert_ne!(v1, stored);
        assert_eq!(
            content_equal("flow.vsdx", &v1, &stored),
            Some("same pages, shapes and masters, only the file layout differs")
        );
        assert_eq!(content_equal("flow.vsdx", &v1, &vsdx("Begin", true)), None);
        assert!(has_content_rule("flow.vsdx") && has_content_rule("old.VSD"));
    }
}
