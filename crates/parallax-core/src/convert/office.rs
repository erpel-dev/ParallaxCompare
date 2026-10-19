//! Text extraction from office documents and PDF.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use quick_xml::events::Event;
use quick_xml::reader::Reader;

use crate::{Error, Result};

/// Streams the parts of an XML document that text extraction cares about.
pub(super) enum Ev<'a> {
    Start(&'a str, &'a quick_xml::events::BytesStart<'a>),
    End(&'a str),
    Text(&'a str),
}

pub(super) fn walk_xml(xml: &str, mut f: impl FnMut(Ev<'_>)) -> Result<()> {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event().map_err(|e| Error::Archive(format!("XML: {e}")))? {
            Event::Start(e) => f(Ev::Start(e.local_name().as_ref(), &e)),
            Event::Empty(e) => {
                f(Ev::Start(e.local_name().as_ref(), &e));
                f(Ev::End(e.local_name().as_ref()));
            }
            Event::End(e) => f(Ev::End(e.local_name().as_ref())),
            Event::Text(t) => f(Ev::Text(&t.xml10_content())),
            Event::CData(t) => f(Ev::Text(&t)),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => match &*r {
                        "amp" => "&".into(),
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "quot" => "\"".into(),
                        "apos" => "'".into(),
                        other => format!("&{other};"),
                    },
                };
                f(Ev::Text(&resolved));
            }
            Event::Eof => return Ok(()),
            _ => {}
        }
    }
}

pub(super) fn attr(e: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.local_name().as_ref() == name).map(|a| a.value.into_owned())
}

pub(super) struct Zip(zip::ZipArchive<Cursor<Vec<u8>>>);

impl Zip {
    pub(super) fn open(data: &[u8]) -> Result<Self> {
        zip::ZipArchive::new(Cursor::new(data.to_vec())).map(Zip).map_err(Error::archive)
    }

    pub(super) fn read(&mut self, name: &str) -> Option<String> {
        let mut file = self.0.by_name(name).ok()?;
        let mut s = String::new();
        file.read_to_string(&mut s).ok()?;
        Some(s)
    }

    pub(super) fn read_bytes(&mut self, name: &str) -> Option<Vec<u8>> {
        let mut file = self.0.by_name(name).ok()?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).ok()?;
        Some(buf)
    }

    fn names(&self) -> Vec<String> {
        self.0.file_names().filter_map(|n| n.ok()).map(|n| n.into_owned()).collect()
    }
}

/// Sorts `prefix<N>.xml` part names by their number (slide2 before slide10).
fn numbered(names: &[String], prefix: &str) -> Vec<(u32, String)> {
    let mut out: Vec<(u32, String)> = names
        .iter()
        .filter_map(|n| {
            let num = n.strip_prefix(prefix)?.strip_suffix(".xml")?.parse().ok()?;
            Some((num, n.clone()))
        })
        .collect();
    out.sort();
    out
}

pub fn ooxml_text(ext: &str, data: &[u8]) -> Result<String> {
    let mut zip = Zip::open(data)?;
    match ext.chars().next() {
        Some('d') => docx(&mut zip),
        Some('x') => xlsx(&mut zip),
        _ => pptx(&mut zip),
    }
}

fn docx(zip: &mut Zip) -> Result<String> {
    let mut out = String::new();
    let mut parts = vec![("".to_string(), "word/document.xml".to_string())];
    let names = zip.names();
    for (n, part) in numbered(&names, "word/header") {
        parts.push((format!("Header {n}"), part));
    }
    for (n, part) in numbered(&names, "word/footer") {
        parts.push((format!("Footer {n}"), part));
    }
    for extra in ["word/footnotes.xml", "word/endnotes.xml", "word/comments.xml"] {
        if names.iter().any(|n| n == extra) {
            let title = extra.trim_start_matches("word/").trim_end_matches(".xml");
            parts.push((title[..1].to_uppercase() + &title[1..], extra.to_string()));
        }
    }
    for (title, part) in parts {
        let Some(xml) = zip.read(&part) else { continue };
        if !title.is_empty() {
            out.push_str(&format!("\n=== {title} ===\n"));
        }
        let (mut in_text, mut cell_depth, mut paragraph_has_text) = (false, 0usize, false);
        walk_xml(&xml, |ev| match ev {
            Ev::Start("t", _) => in_text = true,
            Ev::End("t") => in_text = false,
            Ev::Text(t) if in_text => {
                out.push_str(t);
                paragraph_has_text = true;
            }
            Ev::Start("tab", _) => out.push('\t'),
            Ev::Start("br" | "cr", _) => out.push('\n'),
            Ev::Start("tc", _) => cell_depth += 1,
            Ev::End("tc") => {
                cell_depth = cell_depth.saturating_sub(1);
                out.push_str(" │ ");
            }
            Ev::End("tr") => out.push('\n'),
            // Paragraphs inside table cells stay on the row's line.
            Ev::End("p") if cell_depth > 0 => {
                if paragraph_has_text {
                    out.push(' ');
                }
                paragraph_has_text = false;
            }
            Ev::End("p") => {
                out.push('\n');
                paragraph_has_text = false;
            }
            _ => {}
        })?;
    }
    Ok(out)
}

fn column_index(cell_ref: &str) -> usize {
    cell_ref
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .fold(0usize, |acc, c| acc * 26 + (c.to_ascii_uppercase() - b'A' + 1) as usize)
        .saturating_sub(1)
}

fn xlsx(zip: &mut Zip) -> Result<String> {
    let mut shared: Vec<String> = Vec::new();
    if let Some(xml) = zip.read("xl/sharedStrings.xml") {
        let (mut in_si, mut in_t, mut current) = (false, false, String::new());
        walk_xml(&xml, |ev| match ev {
            Ev::Start("si", _) => {
                in_si = true;
                current.clear();
            }
            Ev::End("si") => {
                in_si = false;
                shared.push(std::mem::take(&mut current));
            }
            Ev::Start("t", _) if in_si => in_t = true,
            Ev::End("t") => in_t = false,
            Ev::Text(t) if in_t => current.push_str(t),
            _ => {}
        })?;
    }
    // Sheet names in workbook order, resolved to their part through the relationships.
    let mut rels: HashMap<String, String> = HashMap::new();
    if let Some(xml) = zip.read("xl/_rels/workbook.xml.rels") {
        walk_xml(&xml, |ev| {
            if let Ev::Start("Relationship", e) = ev
                && let (Some(id), Some(target)) = (attr(e, "Id"), attr(e, "Target"))
            {
                let target = target.trim_start_matches('/').trim_start_matches("xl/").to_string();
                rels.insert(id, format!("xl/{target}"));
            }
        })?;
    }
    let mut sheets: Vec<(String, String)> = Vec::new();
    if let Some(xml) = zip.read("xl/workbook.xml") {
        walk_xml(&xml, |ev| {
            if let Ev::Start("sheet", e) = ev
                && let (Some(name), Some(id)) = (attr(e, "name"), attr(e, "id"))
                && let Some(part) = rels.get(&id)
            {
                sheets.push((name, part.clone()));
            }
        })?;
    }
    let mut out = String::new();
    for (name, part) in sheets {
        out.push_str(&format!("=== Sheet: {name} ===\n"));
        let Some(xml) = zip.read(&part) else { continue };
        let mut row: Vec<String> = Vec::new();
        let (mut cell_col, mut cell_type, mut value, mut formula) =
            (0usize, String::new(), String::new(), String::new());
        let mut target: Option<&'static str> = None;
        walk_xml(&xml, |ev| match ev {
            Ev::Start("row", _) => row.clear(),
            Ev::Start("c", e) => {
                cell_col = attr(e, "r").map_or(row.len(), |r| column_index(&r));
                cell_type = attr(e, "t").unwrap_or_default();
                value.clear();
                formula.clear();
            }
            Ev::Start("v" | "t", _) => target = Some("v"),
            Ev::Start("f", _) => target = Some("f"),
            Ev::End("v" | "t" | "f") => target = None,
            Ev::Text(t) => match target {
                Some("v") => value.push_str(t),
                Some(_) => formula.push_str(t),
                None => {}
            },
            Ev::End("c") => {
                let mut shown = match cell_type.as_str() {
                    "s" => value.trim().parse::<usize>().ok().and_then(|i| shared.get(i).cloned()).unwrap_or_default(),
                    // Numbers have no type or "n"; booleans are "b".
                    "b" => (if value.trim() == "1" { "TRUE" } else { "FALSE" }).to_string(),
                    _ => value.clone(),
                };
                if !formula.is_empty() {
                    shown = format!("{shown} {{={formula}}}");
                }
                if row.len() <= cell_col {
                    row.resize(cell_col + 1, String::new());
                }
                row[cell_col] = shown;
            }
            Ev::End("row") => {
                while row.last().is_some_and(String::is_empty) {
                    row.pop();
                }
                out.push_str(&row.join("\t"));
                out.push('\n');
            }
            _ => {}
        })?;
    }
    Ok(out)
}

fn pptx(zip: &mut Zip) -> Result<String> {
    let names = zip.names();
    let mut out = String::new();
    for (n, part) in numbered(&names, "ppt/slides/slide") {
        out.push_str(&format!("=== Slide {n} ===\n"));
        let Some(xml) = zip.read(&part) else { continue };
        text_runs(&xml, &mut out)?;
        if let Some(notes) = zip.read(&format!("ppt/notesSlides/notesSlide{n}.xml")) {
            let mut text = String::new();
            text_runs(&notes, &mut text)?;
            // Notes always contain the slide number placeholder; skip notes without real text.
            if text.lines().any(|l| !l.trim().is_empty() && l.trim() != n.to_string()) {
                out.push_str("--- Notes ---\n");
                out.push_str(&text);
            }
        }
    }
    Ok(out)
}

/// DrawingML text: `<a:t>` runs, a line per `<a:p>` paragraph.
fn text_runs(xml: &str, out: &mut String) -> Result<()> {
    let mut in_text = false;
    walk_xml(xml, |ev| match ev {
        Ev::Start("t", _) => in_text = true,
        Ev::End("t") => in_text = false,
        Ev::Text(t) if in_text => out.push_str(t),
        Ev::Start("br", _) => out.push('\n'),
        Ev::End("p") => out.push('\n'),
        _ => {}
    })
}

pub fn odf_text(data: &[u8]) -> Result<String> {
    let mut zip = Zip::open(data)?;
    let xml = zip.read("content.xml").ok_or_else(|| Error::Archive("content.xml missing".into()))?;
    let mut out = String::new();
    let mut depth_text = 0usize;
    walk_xml(&xml, |ev| match ev {
        Ev::Start("p" | "h", _) => depth_text += 1,
        Ev::End("p" | "h") => {
            depth_text = depth_text.saturating_sub(1);
            out.push('\n');
        }
        Ev::Text(t) if depth_text > 0 => out.push_str(t),
        Ev::Start("tab", _) => out.push('\t'),
        Ev::Start("s", e) => {
            let n = attr(e, "c").and_then(|c| c.parse().ok()).unwrap_or(1);
            out.push_str(&" ".repeat(n));
        }
        Ev::Start("line-break", _) => out.push('\n'),
        Ev::Start("table", e) => {
            if let Some(name) = attr(e, "name") {
                out.push_str(&format!("=== {name} ===\n"));
            }
        }
        Ev::End("table-cell") => out.push('\t'),
        Ev::End("table-row") => out.push('\n'),
        Ev::Start("page", e) => {
            if let Some(name) = attr(e, "name") {
                out.push_str(&format!("=== {name} ===\n"));
            }
        }
        _ => {}
    })?;
    // Cell paragraphs end with a newline before the tab; keep each table row on one line.
    Ok(out.replace("\n\t", "\t"))
}

pub fn pdf_text(data: &[u8]) -> Result<String> {
    // pdf-extract panics on some malformed files; a broken PDF must not take the app down.
    let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(data))
        .map_err(|_| Error::Unsupported("the PDF could not be parsed".into()))?
        .map_err(|e| Error::Unsupported(format!("PDF: {e}")))?;
    let mut out = String::new();
    for (i, page) in pages.iter().enumerate() {
        out.push_str(&format!("=== Page {} ===\n", i + 1));
        for line in page.lines() {
            let line = line.trim_end();
            if !line.is_empty() {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    Ok(out)
}

/// LibreOffice's `soffice`, used to convert legacy binary Office files.
pub fn soffice() -> Option<&'static PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let exe = if cfg!(windows) { "soffice.exe" } else { "soffice" };
            let in_path = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).map(|d| d.join(exe)).collect::<Vec<_>>())
                .unwrap_or_default();
            let defaults = [
                PathBuf::from(r"C:\Program Files\LibreOffice\program\soffice.exe"),
                PathBuf::from(r"C:\Program Files (x86)\LibreOffice\program\soffice.exe"),
                PathBuf::from("/usr/lib/libreoffice/program/soffice"),
                PathBuf::from("/opt/libreoffice/program/soffice"),
            ];
            in_path.into_iter().chain(defaults).find(|p| p.is_file())
        })
        .as_ref()
}

/// Reads the text of a file in one format.
type TextReader = fn(&[u8]) -> Result<String>;

/// Extracts text from .doc/.xls/.ppt: with a native reader where there is one, otherwise
/// (or when that reader fails and LibreOffice is installed) through LibreOffice.
pub fn legacy_text(ext: &str, data: &[u8]) -> Result<String> {
    match native_reader(ext).map(|read| read(data)) {
        Some(Ok(text)) => Ok(text),
        Some(Err(e)) if soffice().is_none() => Err(e),
        _ => libreoffice_text(ext, data),
    }
}

fn native_reader(ext: &str) -> Option<TextReader> {
    match ext {
        "xls" | "xlt" => Some(super::xls::text),
        "ppt" | "pot" | "pps" => Some(super::ppt::text),
        _ => None,
    }
}

/// Whether .`ext` files are read without LibreOffice.
pub fn has_native_reader(ext: &str) -> bool {
    native_reader(ext).is_some()
}

/// Converts .doc/.xls/.ppt to their OOXML counterpart with LibreOffice, then extracts text.
fn libreoffice_text(ext: &str, data: &[u8]) -> Result<String> {
    let target = match ext.chars().next() {
        Some('d') => "docx",
        Some('x') => "xlsx",
        _ => "pptx",
    };
    ooxml_text(target, &convert(ext, data, target)?)
}

/// Converts `data`, a file of type `ext`, to `target` (an extension) with LibreOffice.
pub(super) fn convert(ext: &str, data: &[u8], target: &str) -> Result<Vec<u8>> {
    let exe = soffice().ok_or_else(|| Error::Unsupported(format!(".{ext} files need LibreOffice to be installed")))?;
    let dir = std::env::temp_dir().join(format!("parallax-convert-{}-{}", std::process::id(), unique()));
    std::fs::create_dir_all(&dir).map_err(|e| Error::io(dir.display().to_string(), e))?;
    let cleanup = Cleanup(dir.clone());
    let input = dir.join(format!("input.{ext}"));
    std::fs::write(&input, data).map_err(|e| Error::io(input.display().to_string(), e))?;
    // A private profile lets several conversions run at once.
    let profile = format!(
        "-env:UserInstallation=file:///{}",
        dir.join("profile").display().to_string().replace('\\', "/").trim_start_matches('/')
    );
    let mut child = Command::new(exe)
        .args([profile.as_str(), "--headless", "--norestore", "--convert-to", target, "--outdir"])
        .arg(&dir)
        .arg(&input)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::io(exe.display().to_string(), e))?;
    let started = Instant::now();
    loop {
        if child.try_wait().map_err(|e| Error::io("soffice", e))?.is_some() {
            break;
        }
        if started.elapsed() > Duration::from_secs(90) {
            let _ = child.kill();
            return Err(Error::Unsupported("LibreOffice did not finish converting within 90 s".into()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let output = dir.join(format!("input.{target}"));
    let converted =
        std::fs::read(&output).map_err(|_| Error::Unsupported("LibreOffice could not convert the file".into()))?;
    drop(cleanup);
    Ok(converted)
}

fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workbook(cells: &str) -> Vec<u8> {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        let parts = [
            (
                "xl/workbook.xml",
                r#"<workbook><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_string(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#
                    .to_string(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                format!("<worksheet><sheetData><row r=\"1\">{cells}</row></sheetData></worksheet>"),
            ),
        ];
        for (name, text) in parts {
            w.start_file(name, opts).unwrap();
            w.write_all(text.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn cell_types() {
        let text = |cells: &str| ooxml_text("xlsx", &workbook(cells)).unwrap();
        // Numbers may leave out the type.
        assert_eq!(text(r#"<c r="A1"><v>2</v></c>"#), "=== Sheet: S ===\n2\n");
        assert_ne!(text(r#"<c r="A1"><v>2</v></c>"#), text(r#"<c r="A1"><v>3</v></c>"#));
        assert_eq!(
            text(
                r#"<c r="A1" t="n"><v>-1.5</v></c><c r="B1" t="b"><v>1</v></c><c r="C1" t="b"><v>0</v></c><c r="D1"/>"#
            ),
            "=== Sheet: S ===\n-1.5\tTRUE\tFALSE\n"
        );
        assert_eq!(text(r#"<c r="A1"><f>B1+1</f><v>4</v></c>"#), "=== Sheet: S ===\n4 {=B1+1}\n");
    }

    #[test]
    fn legacy_without_libreoffice() {
        let fixture = |f: &str| {
            std::fs::read(format!("{}/tests/fixtures/legacy-office/{f}", env!("CARGO_MANIFEST_DIR"))).unwrap()
        };
        let ppt = legacy_text("pps", &fixture("ppt-deck.ppt")).unwrap();
        assert!(ppt.starts_with("=== Slide 1 ===\nQuarterly Review\n"), "{ppt}");
        let xls = legacy_text("xls", &fixture("xls-workbook.xls")).unwrap();
        assert!(xls.starts_with("=== Sheet: Parts ===\n"), "{xls}");
        if soffice().is_none() {
            assert!(legacy_text("xls", b"\xD0\xCF\x11\xE0 damaged").is_err());
        }
    }

    #[test]
    fn columns() {
        assert_eq!(column_index("A1"), 0);
        assert_eq!(column_index("Z9"), 25);
        assert_eq!(column_index("AA10"), 26);
    }
}
