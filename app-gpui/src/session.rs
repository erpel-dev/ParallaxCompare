//! Opening the two sides and their files, through the same core calls the Tauri
//! app makes in `app/src-tauri/src/commands.rs` (`open_session`, `load_side`,
//! `open_file_diff`, `file_rows`). The GPUI views call these directly, without
//! IPC or serialization.

use parallax_core::convert::{self, View};
use parallax_core::edit::{EditSession, Side};
use parallax_core::highlight::{self, DocHighlighter, ThemeKind};
use parallax_core::rows::{self, RowView};
use parallax_core::source::{Source, SourceOptions, SourceSpec, open_pair};
use parallax_core::text::{self, LineEnding, TextDoc};
use parallax_core::textdiff::{DiffOptions, DiffStats, Section};
use parallax_core::vfs::{Data, split_parent};

/// The two sides of a comparison.
pub struct Session {
    pub left: Source,
    pub right: Source,
    /// Both roots are files: show a text compare instead of a folder compare.
    pub is_file: bool,
}

impl Session {
    /// Opens `left` and `right`, written as on the command line: a folder, a
    /// file, an archive (`a.zip!/dir`) or a Git revision (`repo@main:src`).
    pub fn open(left: &str, right: &str) -> Result<Session, String> {
        let (left, right) =
            open_pair(&SourceSpec::parse(left), &SourceSpec::parse(right), SourceOptions::default(), false)
                .map_err(|e| e.to_string())?;
        let (lf, rf) = (left.is_file().map_err(|e| e.to_string())?, right.is_file().map_err(|e| e.to_string())?);
        if lf != rf {
            return Err("A file cannot be compared with a folder.".into());
        }
        Ok(Session { left, right, is_file: lf })
    }

    pub fn source(&self, side: Side) -> &Source {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }
}

/// What the views show about one side of a file comparison.
#[derive(Clone, Debug)]
pub struct SideInfo {
    pub exists: bool,
    pub size: u64,
    pub encoding: String,
    pub line_ending: Option<LineEnding>,
    pub lines: usize,
}

impl SideInfo {
    /// As the Svelte app names line endings in its file header.
    pub fn line_ending_name(&self) -> &'static str {
        match self.line_ending {
            Some(LineEnding::Lf) => "LF",
            Some(LineEnding::CrLf) => "CRLF",
            Some(LineEnding::Cr) => "CR",
            Some(LineEnding::Mixed) => "mixed",
            Some(LineEnding::None) => "no line break",
            None => "",
        }
    }
}

/// Two files compared line by line, with highlighting state for the rows shown.
pub struct FileDiff {
    pub edit: EditSession,
    pub syntax: Option<String>,
    pub sides: [SideInfo; 2],
    /// Why the files are shown the way they are (binary files as a hex dump …).
    pub note: Option<String>,
    highlighters: [DocHighlighter; 2],
}

impl FileDiff {
    pub fn rows_total(&self) -> usize {
        self.edit.diff().rows.len()
    }
    pub fn sections(&self) -> &[Section] {
        &self.edit.diff().sections
    }
    pub fn stats(&self) -> &DiffStats {
        &self.edit.diff().stats
    }

    /// Rows `start..start + count`, built by the same core function as the Tauri
    /// app's `file_rows` command.
    pub fn rows(&mut self, start: usize, count: usize) -> Vec<RowView> {
        let [hl_left, hl_right] = &mut self.highlighters;
        let (left, right) = (self.edit.doc(Side::Left), self.edit.doc(Side::Right));
        rows::side_by_side(left, right, &self.edit.diff().rows, start, count, [hl_left, hl_right], None)
    }
}

fn read_side(source: &Source, path: &str) -> Result<Option<Data>, String> {
    let full = source.path(path);
    source.vfs.changed(&full);
    match source.vfs.stat(&full) {
        Ok(_) => source.vfs.read(&full).map(Some).map_err(|e| e.to_string()),
        Err(parallax_core::Error::NotFound(_)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Opens the file at `path` (relative to both roots; empty when the roots are
/// files) on both sides. Text is compared as text; binary files as a hex dump.
/// The Tauri app also offers the converted views (outlines, PDF text …); this
/// prototype keeps to these two.
pub fn open_file(session: &Session, path: &str) -> Result<FileDiff, String> {
    let left_data = read_side(&session.left, path)?;
    let right_data = read_side(&session.right, path)?;
    // The name to pick a grammar from: the compared path, or the root file name.
    let name = if path.is_empty() {
        let root = if session.left.root.is_empty() { &session.right.root } else { &session.left.root };
        split_parent(root).1.to_string()
    } else {
        path.to_string()
    };
    let binary = [&left_data, &right_data].into_iter().flatten().any(|d| text::is_binary(d));
    let view = if binary { View::Hex } else { View::Text };
    let decode = |data: &Option<Data>, side: Side| -> Result<(TextDoc, SideInfo), String> {
        let Some(bytes) = data else {
            return Ok((
                text::decode(b""),
                SideInfo { exists: false, size: 0, encoding: String::new(), line_ending: None, lines: 0 },
            ));
        };
        let doc = if binary {
            let hex = convert::render_source(View::Hex, session.source(side), path, &name, bytes, None)
                .map_err(|e| e.to_string())?;
            text::decode(hex.as_bytes())
        } else {
            text::decode(bytes)
        };
        let encoding = if binary { "Hex dump".to_string() } else { doc.encoding.to_string() };
        let info = SideInfo {
            exists: true,
            size: bytes.len() as u64,
            encoding,
            line_ending: Some(doc.line_ending),
            lines: doc.len(),
        };
        Ok((doc, info))
    };
    let (mut left, left_info) = decode(&left_data, Side::Left)?;
    let (mut right, right_info) = decode(&right_data, Side::Right)?;
    // A missing side looks like the other file: encoding, BOM, line endings.
    if left_data.is_none() {
        left = TextDoc::empty_like(&right);
    } else if right_data.is_none() {
        right = TextDoc::empty_like(&left);
    }
    let edit = EditSession::new(left, right, DiffOptions::default());
    let (l, r) = (edit.doc(Side::Left), edit.doc(Side::Right));
    let first_line = if l.is_empty() { r.lines.first().map(|_| r.line(0)) } else { Some(l.line(0)) };
    let syntax = match view {
        View::Text => highlight::detect_syntax(&name, first_line.unwrap_or_default()).map(|s| s.name.clone()),
        _ => Some(highlight::view_syntax().name.clone()),
    };
    let grammar = syntax.as_deref().and_then(highlight::find_syntax);
    let note = binary.then(|| "Binary files are shown as a hex dump.".to_string());
    Ok(FileDiff {
        edit,
        syntax,
        sides: [left_info, right_info],
        note,
        highlighters: [DocHighlighter::new(grammar, ThemeKind::Light), DocHighlighter::new(grammar, ThemeKind::Light)],
    })
}
