//! Syntax highlighting for the visible part of a document.
//!
//! Uses syntect with the extended grammar set of `bat` (TypeScript, TOML,
//! Dockerfile, … on top of the Sublime defaults). Parser state is check-pointed
//! every [`CHECKPOINT_EVERY`] lines, so scrolling anywhere in a large file only
//! parses from the nearest checkpoint instead of from the top.

use std::sync::LazyLock;

use serde::Serialize;
use syntect::highlighting::{Color, HighlightIterator, HighlightState, Highlighter, Theme};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

use crate::text::{TextDoc, utf16_len};

const CHECKPOINT_EVERY: usize = 128;
/// Lines longer than this (minified code) are shown without highlighting.
const MAX_LINE_LEN: usize = 5_000;
/// Documents larger than this are shown without highlighting.
pub const MAX_DOC_BYTES: usize = 32 * 1024 * 1024;

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);
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
    ss.find_syntax_by_extension(name)
        .or_else(|| name.rsplit_once('.').and_then(|(_, ext)| ss.find_syntax_by_extension(ext)))
        .or_else(|| ss.find_syntax_by_first_line(first_line))
        .filter(|s| s.name != "Plain Text")
}

pub fn syntax_names() -> Vec<String> {
    let mut names: Vec<String> = SYNTAXES.syntaxes().iter().filter(|s| !s.hidden).map(|s| s.name.clone()).collect();
    names.sort();
    names
}

pub fn find_syntax(name: &str) -> Option<&'static SyntaxReference> {
    SYNTAXES.find_syntax_by_name(name)
}

/// Incremental highlighter for one document and one theme.
pub struct DocHighlighter {
    syntax: Option<&'static SyntaxReference>,
    theme: ThemeKind,
    checkpoints: Vec<(ParseState, HighlightState)>,
}

impl DocHighlighter {
    pub fn new(syntax: Option<&'static SyntaxReference>, theme: ThemeKind) -> Self {
        DocHighlighter { syntax, theme, checkpoints: Vec::new() }
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
        let theme = theme(self.theme);
        let highlighter = Highlighter::new(theme);
        let default_fg = theme.settings.foreground.unwrap_or(Color::WHITE);

        let cp = (start / CHECKPOINT_EVERY).min(self.checkpoints.len());
        let (mut parse, mut hl) = match cp {
            0 => (ParseState::new(syntax), HighlightState::new(&highlighter, ScopeStack::new())),
            n => self.checkpoints[n - 1].clone(),
        };
        let mut out = Vec::with_capacity(end - start);
        let mut line_buf = String::new();
        for i in (cp * CHECKPOINT_EVERY)..end {
            if i % CHECKPOINT_EVERY == 0 && i > 0 && self.checkpoints.len() < i / CHECKPOINT_EVERY {
                self.checkpoints.push((parse.clone(), hl.clone()));
            }
            let line = doc.line(i);
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
                out.push(spans);
            }
        }
        out
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
}
