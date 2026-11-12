//! Whether edited text is still a well-formed file of its kind: XML (ARXML,
//! XSD, SVG, ODX …), JSON, YAML, TOML, and the ECUC values view. Used to warn
//! before an edit is saved into a file that other tools could no longer read.

use serde::Serialize;

use super::View;

/// What is wrong with the text, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    /// "not valid XML: …", for showing as it is.
    pub message: String,
    /// The line (0-based) the problem is on, when known.
    pub line: Option<u32>,
}

/// The formats that are checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Xml,
    Json,
    Yaml,
    Toml,
    /// The ECUC values view: lenient JSON, whose content is checked when saved.
    EcucValues,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Xml => "XML",
            Format::Json => "JSON",
            Format::Yaml => "YAML",
            Format::Toml => "TOML",
            Format::EcucValues => "ECUC values",
        }
    }
}

/// Texts longer than this are not checked as they are edited.
pub const MAX_CHECKED: usize = 16 * 1024 * 1024;

/// The format the text of `view` for the file `name` has to keep, if any.
pub fn format_of(view: View, name: &str, text: &str) -> Option<Format> {
    if view == View::EcucValues {
        return Some(Format::EcucValues);
    }
    if view != View::Text {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    let ext = base.rsplit_once('.').map_or("", |(_, e)| e);
    const XML: &[&str] = &[
        "xml",
        "arxml",
        "xsd",
        "xsl",
        "xslt",
        "svg",
        "xhtml",
        "xaml",
        "wsdl",
        "resx",
        "csproj",
        "vbproj",
        "vcxproj",
        "fsproj",
        "props",
        "targets",
        "nuspec",
        "pom",
        "kml",
        "gpx",
        "xlf",
        "xliff",
        "rss",
        "atom",
        "opf",
        "fxml",
        "cdfx",
        "fibex",
        "odx",
        "odx-d",
        "odx-c",
        "odx-cs",
        "odx-f",
        "odx-fd",
        "odx-e",
        "odx-v",
        "odx-m",
        "pdx",
        "aml",
        "dita",
        "ditamap",
        "xmi",
        "uml",
        "ecore",
        "launch",
        "storyboard",
        "xib",
        "plist",
        "mxml",
        "tmx",
        "xliff2",
    ];
    // JSON with comments (tsconfig.json, VS Code settings) would warn for every edit;
    // such files are not valid JSON to begin with, and are skipped because of that.
    const JSON: &[&str] = &["json", "geojson", "ipynb", "har", "webmanifest", "jsonld", "avsc", "gltf"];
    if XML.contains(&ext) {
        // A text plist or a .launch file must look like XML to be checked as such.
        if matches!(ext, "plist" | "launch" | "storyboard" | "xib") && !text.trim_start().starts_with('<') {
            return None;
        }
        return Some(Format::Xml);
    }
    if JSON.contains(&ext) {
        return Some(Format::Json);
    }
    match ext {
        "yaml" | "yml" => Some(Format::Yaml),
        "toml" => Some(Format::Toml),
        _ if text.trim_start_matches('\u{feff}').starts_with("<?xml") => Some(Format::Xml),
        _ => None,
    }
}

/// What makes `text` not well-formed `format`, or None when it is.
pub fn check(format: Format, text: &str) -> Option<Problem> {
    let found = match format {
        Format::Xml => xml(text),
        Format::Json => json(text),
        Format::Yaml => yaml(text),
        Format::Toml => toml_text(text),
        Format::EcucValues => super::ecuc::canonical(text).err().map(|e| {
            let line = line_in_message(&e);
            (e, line)
        }),
    };
    found.map(|(detail, line)| Problem { message: format!("not valid {}: {detail}", format.label()), line })
}

/// "line 12: …" in a message, as a 0-based line.
fn line_in_message(message: &str) -> Option<u32> {
    let at = message.find("line ")? + 5;
    let digits: String = message[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u32>().ok()?.checked_sub(1)
}

/// The 0-based line and column of byte `at`.
fn place(text: &str, at: usize) -> (u32, u32) {
    let at = at.min(text.len());
    let mut at = at;
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    let before = &text[..at];
    let line = before.matches('\n').count();
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count());
    (line as u32, column as u32)
}

fn at(text: &str, byte: usize, what: impl std::fmt::Display) -> (String, Option<u32>) {
    let (line, column) = place(text, byte);
    (format!("line {}, column {}: {what}", line + 1, column + 1), Some(line))
}

fn xml(text: &str) -> Option<(String, Option<u32>)> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::reader::Reader::from_str(text);
    reader.config_mut().check_end_names = true;
    reader.config_mut().check_comments = true;
    let mut open: Vec<(String, usize)> = Vec::new();
    let mut roots = 0;
    let mut dtd = false;
    loop {
        let before = reader.buffer_position() as usize;
        let event = match reader.read_event() {
            Ok(e) => e,
            Err(e) => {
                let pos = reader.error_position() as usize;
                return Some(at(text, pos, xml_error(&e)));
            }
        };
        match event {
            Event::Start(e) => {
                if open.is_empty() {
                    roots += 1;
                    if roots > 1 {
                        return Some(at(text, before, "a second root element; a document has one"));
                    }
                }
                if let Some(problem) = attributes(&e) {
                    return Some(at(text, before, problem));
                }
                open.push((e.name().as_ref().to_string(), before));
            }
            Event::Empty(e) => {
                if open.is_empty() {
                    roots += 1;
                    if roots > 1 {
                        return Some(at(text, before, "a second root element; a document has one"));
                    }
                }
                if let Some(problem) = attributes(&e) {
                    return Some(at(text, before, problem));
                }
            }
            Event::End(_) => {
                open.pop();
            }
            Event::Text(t) if open.is_empty() && !t.xml10_content().trim().is_empty() => {
                return Some(at(text, before, "text outside the root element"));
            }
            Event::DocType(_) => dtd = true,
            Event::GeneralRef(r) => {
                let known = matches!(&*r, "amp" | "lt" | "gt" | "quot" | "apos") || r.is_char_ref();
                if !known && !dtd {
                    return Some(at(text, before, format!("&{}; is not a defined entity", &*r)));
                }
                if r.is_char_ref() && !matches!(r.resolve_char_ref(), Ok(Some(_))) {
                    return Some(at(text, before, format!("&{}; is not a valid character", &*r)));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if let Some((name, start)) = open.last() {
        return Some(at(text, *start, format!("<{name}> is never closed")));
    }
    if roots == 0 {
        return Some(at(text, text.len(), "there is no root element"));
    }
    None
}

/// A repeated or malformed attribute.
fn attributes(e: &quick_xml::events::BytesStart<'_>) -> Option<String> {
    let mut seen = std::collections::HashSet::new();
    for a in e.attributes().with_checks(true) {
        match a {
            Ok(a) => {
                let key = a.key.as_ref().to_string();
                if !seen.insert(key.clone()) {
                    return Some(format!("the attribute {key} appears twice in <{}>", e.name().as_ref()));
                }
            }
            Err(quick_xml::events::attributes::AttrError::Duplicated(..)) => {
                return Some(format!("an attribute appears twice in <{}>", e.name().as_ref()));
            }
            Err(err) => return Some(format!("a malformed attribute in <{}>: {err}", e.name().as_ref())),
        }
    }
    None
}

fn xml_error(e: &quick_xml::Error) -> String {
    use quick_xml::errors::{Error, IllFormedError, SyntaxError};
    match e {
        Error::IllFormed(IllFormedError::MismatchedEndTag { expected, found }) => {
            format!("</{found}> closes <{expected}>")
        }
        Error::IllFormed(IllFormedError::UnmatchedEndTag(name)) => format!("</{name}> closes nothing"),
        Error::IllFormed(IllFormedError::MissingEndTag(name)) => format!("<{name}> is never closed"),
        Error::Syntax(SyntaxError::UnclosedTag) => "a tag is not closed with >".into(),
        Error::Syntax(SyntaxError::UnclosedComment) => "a comment is not closed with -->".into(),
        Error::Syntax(SyntaxError::UnclosedCData) => "a CDATA section is not closed with ]]>".into(),
        Error::Syntax(SyntaxError::UnclosedPI | SyntaxError::UnclosedXmlDecl) => {
            "a processing instruction is not closed with ?>".into()
        }
        Error::Syntax(
            SyntaxError::UnclosedSingleQuotedAttributeValue | SyntaxError::UnclosedDoubleQuotedAttributeValue,
        ) => "an attribute value is not closed with its quote".into(),
        other => other.to_string(),
    }
}

fn json(text: &str) -> Option<(String, Option<u32>)> {
    let err = serde_json::from_str::<serde::de::IgnoredAny>(text.trim_start_matches('\u{feff}')).err()?;
    let line = (err.line() as u32).checked_sub(1);
    // serde_json says "… at line 3 column 5"; the place is given once, in front.
    let message = err.to_string();
    let message = message.split(" at line ").next().unwrap_or(&message);
    Some((format!("line {}, column {}: {message}", err.line(), err.column()), line))
}

fn yaml(text: &str) -> Option<(String, Option<u32>)> {
    let mut parser = yaml_rust2::parser::Parser::new_from_str(text);
    loop {
        match parser.next_token() {
            Ok((yaml_rust2::parser::Event::StreamEnd, _)) => return None,
            Ok(_) => {}
            Err(e) => {
                let m = e.marker();
                let info = e.info();
                return Some((
                    format!("line {}, column {}: {info}", m.line(), m.col() + 1),
                    (m.line() as u32).checked_sub(1),
                ));
            }
        }
    }
}

fn toml_text(text: &str) -> Option<(String, Option<u32>)> {
    let err = text.parse::<toml::Table>().err()?;
    let message = err.message().to_string();
    match err.span() {
        Some(span) => Some(at(text, span.start, message)),
        None => Some((message, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn problem(name: &str, text: &str) -> Option<Problem> {
        check(format_of(View::Text, name, text)?, text)
    }

    #[test]
    fn xml_problems_with_their_place() {
        let ok = "<?xml version=\"1.0\"?>\n<AUTOSAR>\n  <A x=\"1\">a &amp; b</A>\n  <B/>\n</AUTOSAR>\n";
        assert_eq!(problem("System.arxml", ok), None);
        let p = problem("System.arxml", "<AUTOSAR>\n  <A>\n  </B>\n</AUTOSAR>\n").unwrap();
        assert_eq!(p.line, Some(2));
        assert!(p.message.starts_with("not valid XML: line 3, column 3: </B> closes <A>"), "{}", p.message);
        let p = problem("a.xml", "<r>\n<a>\n").unwrap();
        assert!(p.message.contains("never closed"), "{}", p.message);
        let p = problem("a.xml", "<r/>\n<s/>\n").unwrap();
        assert!(p.message.contains("second root"), "{}", p.message);
        let p = problem("a.xml", "<r a=\"1\" a=\"2\"/>").unwrap();
        assert!(p.message.contains("appears twice"), "{}", p.message);
        let p = problem("a.xml", "<r>a & b</r>").unwrap();
        assert_eq!(p.line, Some(0), "{}", p.message);
        let p = problem("a.svg", "<svg><!-- open</svg>").unwrap();
        assert!(p.message.contains("comment"), "{}", p.message);
        assert!(problem("a.xml", "<r>&nbsp;</r>").unwrap().message.contains("not a defined entity"));
        assert_eq!(problem("a.xml", "<!DOCTYPE r [<!ENTITY nbsp \"&#160;\">]><r>&nbsp;</r>"), None);
        assert!(problem("a.xml", "text").unwrap().message.contains("outside the root"));
        assert!(problem("a.xml", "<!-- only a comment -->").unwrap().message.contains("no root"));
        // Content that starts as XML is checked whatever the name.
        assert!(problem("data.config", "<?xml version=\"1.0\"?><a>").is_some());
        assert_eq!(problem("notes.txt", "<a>"), None);
    }

    #[test]
    fn json_yaml_toml_problems() {
        assert_eq!(problem("package.json", "{\"a\": [1, 2]}\n"), None);
        let p = problem("package.json", "{\n  \"a\": 1\n  \"b\": 2\n}\n").unwrap();
        assert_eq!(p.line, Some(2));
        assert!(p.message.starts_with("not valid JSON: line 3, column 3: expected `,` or `}`"), "{}", p.message);
        assert_eq!(problem("ci.yml", "a: 1\nb:\n  - x\n"), None);
        let p = problem("ci.yaml", "a: [1, 2\nb: 3\n").unwrap();
        assert!(p.message.starts_with("not valid YAML: line"), "{}", p.message);
        assert_eq!(problem("Cargo.toml", "[package]\nname = \"x\"\n"), None);
        let p = problem("Cargo.toml", "[package]\nname = \"x\n").unwrap();
        assert_eq!(p.line, Some(1), "{}", p.message);
    }

    #[test]
    fn ecuc_view_problems() {
        let ok = "{\n  \"Can\": {\n    \"@def\": \"/AUTOSAR/EcucDefs/Can\"\n  }\n}\n";
        assert_eq!(check(Format::EcucValues, ok), None);
        let p = check(Format::EcucValues, "{\n  \"Can\": {\n    \"@def\": \"/AUTOSAR/EcucDefs/Can\"\n}\n").unwrap();
        assert!(p.message.starts_with("not valid ECUC values:"), "{}", p.message);
        assert_eq!(format_of(View::EcucValues, "Can.arxml", ok), Some(Format::EcucValues));
        assert_eq!(format_of(View::PrettyXml, "Can.arxml", ok), None);
    }
}
