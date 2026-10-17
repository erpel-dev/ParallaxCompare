//! Content views: ways to turn a file into something a person can compare.
//!
//! Raw bytes are the truth, but for many formats they are a poor thing to look
//! at: a re-saved Word file differs in every byte, a re-linked ELF in its build id,
//! a minified XML file is one 5 MB line. Each [`View`] renders a file into text
//! (or, for images, pixels) that the normal diff machinery can align.

mod android;
mod binary;
mod chat;
mod class;
mod dotnet;
pub mod gguf;
mod image_diff;
mod office;
mod pretty;
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
            View::WasmOutline => "Module outline",
            View::WasmText => "WebAssembly text",
            View::AssemblyOutline => "Assembly outline",
            View::Il => "IL",
            View::AndroidXml => "Android XML",
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
    Wasm,
    DotNet,
    AndroidXml,
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
        Kind::Executable => vec![View::Executable],
        Kind::Raster => vec![View::Image],
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
        Kind::Wasm => vec![View::WasmOutline, View::WasmText],
        Kind::DotNet => vec![View::AssemblyOutline, View::Il, View::Executable],
        Kind::AndroidXml => vec![View::AndroidXml],
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
        View::Model => gguf::summary(data),
        View::ChatTemplate => gguf::chat_template(data),
        View::ChatPreview => chat::preview(data),
        View::Metadata => gguf::metadata(data),
        View::Tokenizer => gguf::tokenizer(data),
        View::Tensors => gguf::tensors(data),
        View::ClassOutline => class::outline(data),
        View::Bytecode => class::bytecode(data, false),
        View::BytecodeLines => class::bytecode(data, true),
        View::WasmOutline => wasm::outline(data),
        View::WasmText => wasm::text(data),
        View::AssemblyOutline => dotnet::outline(data),
        View::Il => dotnet::il(data),
        View::AndroidXml => android::to_xml(data),
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
            | "wasm"
            | "dll"
            | "exe"
            | "winmd"
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
        Kind::JavaClass => {
            return class::same_code(left, right)?
                .then_some("same code, only debug info or constant pool layout differ");
        }
        Kind::Wasm => return wasm::same_code(left, right)?.then_some("same code, only custom sections differ"),
        Kind::DotNet => return dotnet::same_code(left, right)?.then_some("same code, only build identity differs"),
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
}
