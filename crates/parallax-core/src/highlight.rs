//! Syntax highlighting for the visible part of a document.
//!
//! Uses syntect with the extended grammar set of `bat` (TypeScript, TOML,
//! Dockerfile, … on top of the Sublime defaults) and Oniguruma, the regex engine
//! the grammars are written for and about 3.5 times faster than fancy-regex.
//! Highlighted lines are kept; an edit re-parses only until the parser is back in
//! the state it had before, usually a few lines on. Parser state is check-pointed
//! every [`CHECKPOINT_EVERY`] lines, so scrolling anywhere in a large file only
//! parses from the nearest checkpoint instead of from the top. A jump far past
//! the parsed region starts a fresh parse shortly before the target instead,
//! which is exact for almost all code and keeps the first paint fast.
//!
//! Extra grammars for automotive and embedded formats (A2L, DBC, Intel HEX,
//! S-record), for FreeMarker templates, Conan files, oAW Xpand, Xtend and Check and for the
//! generated content views live in `src/syntaxes`.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Serialize;
use syntect::highlighting::{Color, HighlightIterator, HighlightState, Highlighter, Theme};
use syntect::parsing::{ParseState, ScopeStack, SyntaxDefinition, SyntaxReference, SyntaxSet};

use crate::text::{TextDoc, utf16_len};

const CHECKPOINT_EVERY: usize = 32;
/// Lines longer than this (minified code) are shown without highlighting.
const MAX_LINE_LEN: usize = 5_000;
/// Documents larger than this are shown without highlighting.
pub const MAX_DOC_BYTES: usize = 32 * 1024 * 1024;

/// Parsing more than this many lines to reach a far target is replaced by a fresh
/// parse [`RESYNC_LINES`] before it.
const FAR_JUMP: usize = 3_000;
const RESYNC_LINES: usize = 200;

const CUSTOM_SYNTAXES: &[&str] = &[
    include_str!("syntaxes/a2l.sublime-syntax"),
    include_str!("syntaxes/dbc.sublime-syntax"),
    include_str!("syntaxes/intel-hex.sublime-syntax"),
    include_str!("syntaxes/srec.sublime-syntax"),
    include_str!("syntaxes/freemarker.sublime-syntax"),
    include_str!("syntaxes/html-freemarker.sublime-syntax"),
    include_str!("syntaxes/xml-freemarker.sublime-syntax"),
    include_str!("syntaxes/conan.sublime-syntax"),
    include_str!("syntaxes/xtend-oaw.sublime-syntax"),
    include_str!("syntaxes/check-oaw.sublime-syntax"),
    include_str!("syntaxes/xpand.sublime-syntax"),
    include_str!("syntaxes/parallax-views.sublime-syntax"),
];

/// Extensions the bundled grammars do not list.
const EXTENSION_ALIASES: &[(&str, &str)] = &[("psgi", "Perl"), ("pyw", "Python")];

/// XML dialects that the bundled XML grammar does not list.
const XML_EXTENSIONS: &[&str] = &[
    "arxml", "odx", "odx-d", "odx-c", "odx-v", "odx-f", "odx-e", "odx-cs", "odx-m", "cdd", "cdfx", "fibex", "xdm",
    "epc", "ecuc", "svg", "xsd", "xsl", "xslt", "xaml", "plist", "resx", "nuspec", "props", "targets", "csproj",
    "vcxproj", "xlf", "wsdl", "kml", "gpx", "xmi", "uml", "aird", "launch", "mwe", "oaw", "xml",
];

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(|| {
    let mut builder = two_face::syntax::extra_newlines().into_builder();
    for source in CUSTOM_SYNTAXES {
        builder.add(SyntaxDefinition::load_from_str(source, true, None).expect("bundled syntax definitions are valid"));
    }
    builder.build()
});
static THEMES: LazyLock<two_face::theme::EmbeddedLazyThemeSet> = LazyLock::new(two_face::theme::extra);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ThemeKind {
    #[default]
    Dark,
    Light,
}

fn theme(kind: ThemeKind) -> &'static Theme {
    use two_face::theme::EmbeddedThemeName as N;
    THEMES.get(match kind {
        ThemeKind::Dark => N::OneHalfDark,
        ThemeKind::Light => N::OneHalfLight,
    })
}

/// A highlighted range of a line: start and end in UTF-16 units, colour as `0xRRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Span(pub u32, pub u32, pub u32);

/// Picks a grammar by file name (`Makefile`), extension, then first line (`#!/bin/sh`).
pub fn detect_syntax(path: &str, first_line: &str) -> Option<&'static SyntaxReference> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ss = &*SYNTAXES;
    let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase());
    if name.eq_ignore_ascii_case("conan.lock") {
        return ss.find_syntax_by_name("JSON");
    }
    ss.find_syntax_by_extension(name)
        .or_else(|| ext.as_deref().and_then(|ext| ss.find_syntax_by_extension(ext)))
        .or_else(|| ext.as_deref().filter(|e| XML_EXTENSIONS.contains(e)).and_then(|_| ss.find_syntax_by_name("XML")))
        .or_else(|| {
            let ext = ext.as_deref()?;
            EXTENSION_ALIASES.iter().find(|(e, _)| *e == ext).and_then(|(_, s)| ss.find_syntax_by_name(s))
        })
        .or_else(|| ss.find_syntax_by_first_line(first_line))
        .filter(|s| s.name != "Plain Text")
}

/// Whether a file name denotes XML, including dialects like ARXML or ODX.
pub fn is_xml_name(path: &str) -> bool {
    detect_syntax(path, "").is_some_and(|s| s.name == "XML")
}

/// The grammar for text produced by the content views.
pub fn view_syntax() -> &'static SyntaxReference {
    SYNTAXES.find_syntax_by_name("Parallax View").expect("bundled view syntax")
}

pub fn syntax_names() -> Vec<String> {
    let mut names: Vec<String> = SYNTAXES.syntaxes().iter().filter(|s| !s.hidden).map(|s| s.name.clone()).collect();
    names.sort();
    names
}

pub fn find_syntax(name: &str) -> Option<&'static SyntaxReference> {
    SYNTAXES.find_syntax_by_name(name)
}

/// A parse state kept between calls.
#[derive(Clone)]
struct State(ParseState, HighlightState);

// SAFETY: with Oniguruma a ParseState holds `onig::Region`s, match buffers that
// are not Send only because they are raw pointers to memory the region owns
// (cloning copies it). Nothing ties them to a thread; a State is only ever used
// by whoever owns the DocHighlighter, which callers keep behind a lock.
unsafe impl Send for State {}

/// A highlighted line: a hash of its text, which says whether it still applies
/// after an edit elsewhere, and its spans.
struct Line {
    hash: u64,
    spans: Vec<Span>,
}

/// What was known past an edit, in the line numbers from before it. Parsing
/// after the edit compares its state with these; once one matches, the lines
/// beyond are taken over, shifted by the lines the edit added or removed.
struct Stale {
    /// The first line the edit touched.
    edit: usize,
    /// Lines of the document before the edit.
    before: usize,
    states: BTreeMap<usize, State>,
    /// Lines from `edit` on.
    lines: Vec<Option<Line>>,
}

/// Incremental highlighter for one document and one theme. Highlighted lines are
/// kept, so scrolling back and fetching rows again after an edit cost nothing;
/// an edit re-parses only until the parser is in the state it was in before.
pub struct DocHighlighter {
    syntax: Option<&'static SyntaxReference>,
    theme: ThemeKind,
    /// Parse states at line starts, every [`CHECKPOINT_EVERY`] lines of exactly
    /// parsed text, and where an edit's re-parse met the old states.
    states: BTreeMap<usize, State>,
    lines: Vec<Option<Line>>,
    stale: Option<Stale>,
    /// Lines of the document when it was last highlighted.
    doc_lines: usize,
    /// Lines parsed so far, for tests.
    parsed: usize,
}

fn line_hash(line: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    line.hash(&mut h);
    h.finish()
}

impl DocHighlighter {
    pub fn new(syntax: Option<&'static SyntaxReference>, theme: ThemeKind) -> Self {
        DocHighlighter {
            syntax,
            theme,
            states: BTreeMap::new(),
            lines: Vec::new(),
            stale: None,
            doc_lines: 0,
            parsed: 0,
        }
    }

    /// After an edit from `line` on: what is known past it becomes stale until
    /// parsing confirms it.
    pub fn invalidate_from(&mut self, line: usize) {
        // A second edit before the first was confirmed: start over from here.
        let earlier = self.stale.take().map_or(line, |s| s.edit.min(line));
        let states = self.states.split_off(&(earlier + 1));
        let lines = if earlier < self.lines.len() { self.lines.split_off(earlier) } else { Vec::new() };
        self.stale = Some(Stale { edit: earlier, before: self.doc_lines, states, lines });
    }

    pub fn syntax_name(&self) -> Option<&str> {
        self.syntax.map(|s| s.name.as_str())
    }

    /// Spans for lines `start..end`; empty vectors when highlighting is off.
    pub fn highlight(&mut self, doc: &TextDoc, start: usize, end: usize) -> Vec<Vec<Span>> {
        let end = end.min(doc.len());
        let start = start.min(end);
        let Some(syntax) = self.syntax else {
            return vec![Vec::new(); end - start];
        };
        if doc.text.len() > MAX_DOC_BYTES {
            return vec![Vec::new(); end - start];
        }
        self.doc_lines = doc.len();
        if self.lines.len() > doc.len() {
            self.lines.truncate(doc.len());
        }
        let known = |me: &Self, i: usize| me.lines.get(i).is_some_and(Option::is_some);
        let Some(first_missing) = (start..end).find(|&i| !known(self, i)) else {
            return (start..end).map(|i| self.lines[i].as_ref().map(|l| l.spans.clone()).unwrap_or_default()).collect();
        };
        let theme = theme(self.theme);
        let highlighter = Highlighter::new(theme);
        let default_fg = theme.settings.foreground.unwrap_or(Color::WHITE);
        let fresh = || (ParseState::new(syntax), HighlightState::new(&highlighter, ScopeStack::new()));
        let (mut from, (mut parse, mut hl)) = match self.states.range(..=first_missing).next_back() {
            Some((&l, s)) => (l, (s.0.clone(), s.1.clone())),
            None => (0, fresh()),
        };
        // Far beyond the exactly parsed region: resync shortly before the target and
        // keep nothing from this approximate state.
        let exact = first_missing - from <= FAR_JUMP;
        if !exact {
            (from, (parse, hl)) = (start.saturating_sub(RESYNC_LINES), fresh());
        }
        let delta = self.stale.as_ref().map_or(0, |s| doc.len() as isize - s.before as isize);
        if exact && self.lines.len() < doc.len() {
            self.lines.resize_with(doc.len(), || None);
        }
        let mut out: Vec<Vec<Span>> = Vec::with_capacity(end - start);
        // Known lines before the state parsing starts from.
        out.extend((start..from).map(|j| self.lines[j].as_ref().map(|l| l.spans.clone()).unwrap_or_default()));
        let mut line_buf = String::new();
        let mut i = from;
        while i < end {
            if exact {
                if i % CHECKPOINT_EVERY == 0 && i > 0 {
                    self.states.entry(i).or_insert_with(|| State(parse.clone(), hl.clone()));
                }
                if self.adopt_stale(doc, i, delta, &parse, &hl) && (i.max(start)..end).all(|j| known(self, j)) {
                    // The rest is known again.
                    out.extend(
                        (i.max(start)..end)
                            .map(|j| self.lines[j].as_ref().map(|l| l.spans.clone()).unwrap_or_default()),
                    );
                    return out;
                }
            }
            let line = doc.line(i);
            self.parsed += 1;
            let spans = if line.len() > MAX_LINE_LEN {
                Vec::new()
            } else {
                line_buf.clear();
                line_buf.push_str(line);
                line_buf.push('\n');
                match parse.parse_line(&line_buf, &SYNTAXES) {
                    Ok(ops) => {
                        let mut spans: Vec<Span> = Vec::new();
                        let mut pos = 0u32;
                        for (style, piece) in HighlightIterator::new(&mut hl, &ops, &line_buf, &highlighter) {
                            let piece = piece.strip_suffix('\n').unwrap_or(piece);
                            let len = utf16_len(piece) as u32;
                            let fg = style.foreground;
                            if len > 0 && fg != default_fg {
                                let rgb = (fg.r as u32) << 16 | (fg.g as u32) << 8 | fg.b as u32;
                                match spans.last_mut() {
                                    Some(last) if last.1 == pos && last.2 == rgb => last.1 += len,
                                    _ => spans.push(Span(pos, pos + len, rgb)),
                                }
                            }
                            pos += len;
                        }
                        spans
                    }
                    Err(_) => Vec::new(),
                }
            };
            if i >= start {
                out.push(spans.clone());
            }
            if exact {
                self.lines[i] = Some(Line { hash: line_hash(line), spans });
            }
            i += 1;
        }
        out
    }

    /// When the state at the start of line `i` equals the state the same text had
    /// before the edit, takes over the old lines and states from there, as far as
    /// the text is unchanged. Returns whether it did.
    fn adopt_stale(&mut self, doc: &TextDoc, i: usize, delta: isize, parse: &ParseState, hl: &HighlightState) -> bool {
        let Some(st) = &self.stale else { return false };
        let old = i as isize - delta;
        if i <= st.edit || old <= st.edit as isize {
            return false;
        }
        let old = old as usize;
        match st.states.get(&old) {
            Some(State(p, h)) if p == parse && h == hl => {}
            _ => return false,
        }
        let st = self.stale.take().expect("checked above");
        let mut n = 0;
        while i + n < doc.len() {
            match st.lines.get(old + n - st.edit) {
                Some(Some(l)) if l.hash == line_hash(doc.line(i + n)) => n += 1,
                _ => break,
            }
        }
        let mut lines = st.lines;
        for k in 0..n {
            if self.lines[i + k].is_none() {
                self.lines[i + k] = lines[old + k - st.edit].take();
            }
        }
        // States at the start of the old lines old+1 ..= old+n follow from verified text.
        for (l, s) in st.states.range(old..=old + n) {
            self.states.entry(l - old + i).or_insert_with(|| s.clone());
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::decode;

    #[test]
    fn detects_common_languages() {
        let name = |p: &str, first: &str| detect_syntax(p, first).map(|s| s.name.clone());
        assert_eq!(name("src/cart.ts", "").as_deref(), Some("TypeScript"));
        assert_eq!(name("main.rs", "").as_deref(), Some("Rust"));
        assert_eq!(name("Cargo.toml", "").as_deref(), Some("TOML"));
        assert_eq!(name("script", "#!/bin/bash").as_deref(), Some("Bourne Again Shell (bash)"));
        assert_eq!(name("notes.unknownext", ""), None);
        assert_eq!(name("system.arxml", "").as_deref(), Some("XML"));
        assert_eq!(name("ecu.a2l", "").as_deref(), Some("A2L"));
        assert_eq!(name("fw.hex", "").as_deref(), Some("Intel HEX"));
        assert_eq!(name("fw.s19", "").as_deref(), Some("Motorola S-record"));
        assert_eq!(name("can.dbc", "").as_deref(), Some("DBC"));
        assert_eq!(name("mail.ftl", "").as_deref(), Some("HTML (FreeMarker)"));
        assert_eq!(name("page.ftlh", "").as_deref(), Some("HTML (FreeMarker)"));
        assert_eq!(name("feed.ftlx", "").as_deref(), Some("XML (FreeMarker)"));
        assert_eq!(name("app/conanfile.txt", "[requires]").as_deref(), Some("Conan"));
        assert_eq!(name("profiles/linux-gcc", "include(default)").as_deref(), Some("Conan"));
        assert_eq!(name("default", "[settings]").as_deref(), Some("Conan"));
        assert_eq!(name("conan.lock", "{").as_deref(), Some("JSON"));
        assert_eq!(name("conanfile.py", "").as_deref(), Some("Python"));
        assert_eq!(name("stubs/os.pyi", "").as_deref(), Some("Python"));
        assert_eq!(name("run", "#!/usr/bin/env python3").as_deref(), Some("Python"));
        for p in ["Shop/Cart.pm", "build.pl", "t/basic.t", "app.psgi"] {
            assert_eq!(name(p, "").as_deref(), Some("Perl"), "{p}");
        }
        assert_eq!(name("tool", "#!/usr/bin/perl -w").as_deref(), Some("Perl"));
        assert_eq!(name("templates/Root.xpt", "").as_deref(), Some("Xpand"));
        assert_eq!(name("Naming.ext", "").as_deref(), Some("Xtend (oAW)"));
        assert_eq!(name("Checks.chk", "").as_deref(), Some("Check (oAW)"));
        assert_eq!(name("generator.mwe", "").as_deref(), Some("XML"));
        assert_eq!(name("workflow.oaw", "").as_deref(), Some("XML"));
    }

    #[test]
    fn highlights_from_checkpoints() {
        let src: String = (0..1000).map(|i| format!("let x{i} = \"s\"; // c\n")).collect();
        let doc = decode(src.as_bytes());
        let mut h = DocHighlighter::new(detect_syntax("a.rs", ""), ThemeKind::Dark);
        let late = h.highlight(&doc, 900, 902);
        assert_eq!(late.len(), 2);
        assert!(late[0].len() >= 3, "keyword, string and comment are coloured: {:?}", late[0]);
        // Same result when reached from a checkpoint instead of from the top.
        let again = h.highlight(&doc, 900, 902);
        assert_eq!(late, again);
    }

    #[test]
    fn far_jumps_resync_without_parsing_from_the_top() {
        let src: String = (0..10_000).map(|i| format!("let x{i} = \"s\"; // c\n")).collect();
        let doc = decode(src.as_bytes());
        let mut far = DocHighlighter::new(detect_syntax("a.rs", ""), ThemeKind::Dark);
        let jumped = far.highlight(&doc, 9_000, 9_002);
        assert!(far.states.is_empty() && far.lines.iter().all(Option::is_none), "approximate states are not kept");
        let mut exact = DocHighlighter::new(detect_syntax("a.rs", ""), ThemeKind::Dark);
        assert_eq!(jumped, exact.highlight(&doc, 0, 9_002)[9_000..].to_vec());
    }

    /// The scopes FreeMarker constructs get, inside HTML markup and attributes too.
    #[test]
    fn freemarker_inside_html() {
        use syntect::parsing::ScopeStack;
        let ss = &*SYNTAXES;
        let scopes_at = |syntax: &str, line: &str, needle: &str| -> String {
            let mut state = ParseState::new(ss.find_syntax_by_name(syntax).expect("syntax"));
            let ops = state.parse_line(line, ss).expect("parses");
            let at = line.find(needle).expect("needle in line");
            let mut stack = ScopeStack::new();
            for (pos, op) in ops {
                if pos > at {
                    break;
                }
                stack.apply(&op).expect("valid ops");
            }
            format!("{stack:?}")
        };
        let html = "HTML (FreeMarker)";
        let line = r#"<a href="${user.url?html}"><#if (n > 3)>many</#if><@ui.card title="x"/><#-- note --></a>"#;
        assert!(scopes_at(html, line, "href").contains("meta.tag"), "HTML tags still");
        assert!(scopes_at(html, line, "user").contains("meta.interpolation.ftl"), "inside an attribute");
        assert!(scopes_at(html, line, "html}").contains("support.function.builtin.ftl"));
        assert!(scopes_at(html, line, "#if").contains("keyword.control.directive.ftl"));
        // `>` in parentheses is a comparison, not the end of the tag.
        assert!(scopes_at(html, line, "3)").contains("meta.tag.directive.ftl"));
        assert!(!scopes_at(html, line, "many").contains("directive.ftl"));
        assert!(scopes_at(html, line, "ui.card").contains("variable.function.macro.ftl"));
        assert!(scopes_at(html, line, "note").contains("comment.block.ftl"));
        let xml = r#"<item id="${id}">[#list xs as x]${x}[/#list]</item>"#;
        assert!(scopes_at("XML (FreeMarker)", xml, "id}").contains("meta.interpolation.ftl"));
        assert!(scopes_at("XML (FreeMarker)", xml, "#list").contains("keyword.control.directive.ftl"));
        let raw = "<#noparse>${not} <#if></#noparse>";
        assert!(!scopes_at("FreeMarker", raw, "not").contains("interpolation"));

        // Xpand statements with Xtend expressions; the output text stays plain.
        let xpt = "«REM»note«ENDREM»public «type.javaType()» get«name» { «FOREACH xs AS x SEPARATOR \", \"-»";
        assert!(scopes_at("Xpand", xpt, "note").contains("comment.block.xpand"));
        assert!(!scopes_at("Xpand", xpt, "public").contains("meta.embedded"));
        assert!(scopes_at("Xpand", xpt, "type.").contains("meta.embedded.xpand"));
        assert!(scopes_at("Xpand", xpt, "FOREACH").contains("keyword.control.xpand"));
        assert!(scopes_at("Xpand", xpt, ", ").contains("string.quoted.double.xtend-oaw"));
        let ext = "cached String javaType(Type this) : switch (name) { default : 'x' }; // c";
        assert!(scopes_at("Xtend (oAW)", ext, "cached").contains("storage.modifier.xtend-oaw"));
        assert!(scopes_at("Xtend (oAW)", ext, "javaType").contains("entity.name.function.xtend-oaw"));
        assert!(scopes_at("Xtend (oAW)", ext, "switch").contains("keyword.control.xtend-oaw"));
        assert!(scopes_at("Xtend (oAW)", ext, "// c").contains("comment.line"));
        let chk = "context Entity if abstract ERROR \"no name\" : name != null;";
        assert!(scopes_at("Check (oAW)", chk, "Entity").contains("entity.name.type.check-oaw"));
        assert!(scopes_at("Check (oAW)", chk, "ERROR").contains("keyword.other.severity.check-oaw"));
        assert!(scopes_at("Check (oAW)", chk, "no name").contains("string.quoted.double.xtend-oaw"));
    }
    /// After random edits, kept lines give exactly what a fresh highlighter gives,
    /// also for edits that open a comment or string and change everything after.
    #[test]
    fn kept_lines_match_a_fresh_parse_after_edits() {
        let py = "def f(x):\n    \"\"\"Doc.\"\"\"\n    return x  # c\n\nclass A:\n    s = 'q'\n".repeat(80);
        let c = "/* head */\nint main(void) {\n  return 0; // x\n}\nchar *s = \"str\";\n".repeat(120);
        let xml = "<a b=\"1\">\n  <!-- c -->\n  <b>text</b>\n</a>\n".repeat(150);
        let sources =
            [("a.rs", include_str!("convert/ecuc.rs")), ("a.py", &py[..]), ("a.c", &c[..]), ("a.xml", &xml[..])];
        let inserts = ["/*", "*/", "\"", "\"\"\"", "<!--", "-->", "x", "\n", "\n\n", "fn g() {\n", "}"];
        let mut rng = 0x9E3779B97F4A7C15u64;
        let mut next = move |m: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % m.max(1) as u64) as usize
        };
        for (name, src) in sources {
            let syntax = detect_syntax(name, "");
            let mut doc = decode(src.as_bytes());
            let mut kept = DocHighlighter::new(syntax, ThemeKind::Dark);
            kept.highlight(&doc, 0, doc.len());
            for round in 0..30 {
                // An edit somewhere: an insert, sometimes over a deleted range.
                let at = doc.lines[next(doc.len())].start.min(doc.text.len());
                let mut del_end = (at + if next(3) == 0 { next(200) } else { 0 }).min(doc.text.len());
                while !doc.text.is_char_boundary(del_end) {
                    del_end += 1;
                }
                let ins = inserts[next(inserts.len())];
                let line = doc.lines.partition_point(|r| r.end < at);
                doc.splice(at..del_end, ins);
                kept.invalidate_from(line);
                // Rows near the edit and further on, as the view would fetch them.
                let len = doc.len();
                let far = next(len);
                for (a, b) in [(line.saturating_sub(50), line + 150), (far, far + 200), (0, len)] {
                    let (a, b) = (a.min(len), b.min(len));
                    let mut fresh = DocHighlighter::new(syntax, ThemeKind::Dark);
                    let want = fresh.highlight(&doc, 0, b)[a..].to_vec();
                    assert_eq!(kept.highlight(&doc, a, b), want, "{name} round {round} lines {a}..{b}");
                }
            }
        }
    }

    /// An edit re-parses a few lines, not the rest of the file.
    #[test]
    fn edits_reparse_little() {
        let syntax = detect_syntax("a.rs", "");
        let mut doc = decode(include_str!("convert/ecuc.rs").as_bytes());
        let mut h = DocHighlighter::new(syntax, ThemeKind::Light);
        h.highlight(&doc, 0, doc.len());
        let at = doc.lines[1000].start;
        doc.splice(at..at, "    let inserted = 42;\n");
        h.invalidate_from(1000);
        let before = h.parsed;
        h.highlight(&doc, 900, 1300);
        assert!(h.parsed - before <= 2 * CHECKPOINT_EVERY, "{} lines parsed", h.parsed - before);
        let before = h.parsed;
        h.highlight(&doc, 0, doc.len());
        assert_eq!(h.parsed, before, "everything else is kept");
    }
}
