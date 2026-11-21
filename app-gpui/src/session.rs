//! Opening the two sides and their files, editing, saving, searching, file
//! operations and watching, through the same core calls the Tauri app makes in
//! `app/src-tauri/src/commands.rs` (`open_session`, `open_file_diff`,
//! `file_rows`, `copy_rows`, `undo`, `save_side`, `search_diff`, `copy_entries`,
//! `delete_entries`, `refresh_paths`). The GPUI views call these directly,
//! without IPC or serialization.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::mpsc;

use parallax_core::compare::{self, CompareOptions, Control, Node, Status, Summary};
use parallax_core::convert::{self, ImageDiff, View};
use parallax_core::edit::{EditSession, Side};
use parallax_core::file_view;
use parallax_core::highlight::{self, DocHighlighter, ThemeKind};
use parallax_core::ops::{self, Removal};
use parallax_core::progress::Progress;
use parallax_core::rows::{self, RowView};
use parallax_core::search::{self, Hit, SearchOptions, SearchSide};
use parallax_core::source::{Source, SourceOptions, SourceSpec, open_pair};
use parallax_core::text::{self, LineEnding, TextDoc};
use parallax_core::textdiff::{DiffOptions, DiffStats, Section};
use parallax_core::tree;
use parallax_core::vfs::{Change, Data, EntryKind, split_parent};
use parallax_core::watch::{Batch, Watcher};
use parking_lot::Mutex;

pub const SIDES: [Side; 2] = [Side::Left, Side::Right];

/// The two sides of a comparison.
pub struct Session {
    pub left: Source,
    pub right: Source,
    /// Both roots are files: show a text compare instead of a folder compare.
    pub is_file: bool,
    watcher: Mutex<Option<Watcher>>,
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
        Ok(Session { left, right, is_file: lf, watcher: Mutex::new(None) })
    }

    pub fn source(&self, side: Side) -> &Source {
        match side {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    pub fn writable(&self, side: Side) -> bool {
        let source = self.source(side);
        source.info.writable && source.vfs.writable()
    }

    /// Watches the local sides, as the Tauri app's `start_watching` does. Changes
    /// arrive on the returned channel, after the sources forgot what they cached.
    pub fn watch(self: &Arc<Self>, excludes: &[String]) -> Option<mpsc::Receiver<Batch>> {
        let roots = [self.left.watch.clone(), self.right.watch.clone()];
        if roots.iter().all(Option::is_none) {
            return None;
        }
        let excludes = compare::Excludes::new(excludes).ok()?;
        let ignore = Arc::new(move |rel: &str| excludes.covers(rel));
        let (tx, rx) = mpsc::channel();
        let weak = Arc::downgrade(self);
        let on_change = move |batch: Batch| {
            if let Some(session) = weak.upgrade() {
                for source in [&session.left, &session.right] {
                    if batch.full {
                        source.vfs.changed(&source.path(""));
                    } else {
                        for path in &batch.paths {
                            source.vfs.changed(&source.path(path));
                        }
                    }
                }
            }
            let _ = tx.send(batch);
        };
        match Watcher::new(&roots, ignore, on_change) {
            Ok(watcher) => {
                *self.watcher.lock() = Some(watcher);
                Some(rx)
            }
            // Watching is a convenience: without it, F5 still compares again.
            Err(e) => {
                eprintln!("not watching for changes: {e}");
                None
            }
        }
    }

    fn writing(&self, paths: &[String]) -> Option<parallax_core::watch::WriteGuard> {
        self.watcher.lock().as_ref().map(|w| w.writing(paths))
    }

    /// Copies `paths` to the other side, as the `copy_entries` command does.
    pub fn copy(&self, paths: &[String], to: Side, options: &CompareOptions) -> Result<(), String> {
        let filter = compare::Filter::new(options).map_err(|e| e.to_string())?;
        let (from, dest) = match to {
            Side::Right => (&self.left, &self.right),
            Side::Left => (&self.right, &self.left),
        };
        let _guard = self.writing(paths);
        ops::copy(from, dest, paths, &filter, &Progress::default()).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Deletes `paths` on one side, into the trash where possible.
    pub fn delete(&self, paths: &[String], side: Side) -> Result<Vec<String>, String> {
        let _guard = self.writing(paths);
        ops::delete(self.source(side), paths, Removal::Trash, &Progress::default()).map_err(|e| e.to_string())
    }

    /// The fresh node for each path (`None` where it is gone), as `refresh_paths` does.
    pub fn refresh(&self, paths: &[String], options: &CompareOptions) -> Vec<Option<Node>> {
        paths
            .iter()
            .map(|p| compare::refresh_path(&self.left, &self.right, p, options, &Control::default()).ok().flatten())
            .collect()
    }
}

/// Counts files by status, as the Svelte view's `countFiles` does after a
/// refresh changed part of the tree: folders are not counted themselves, while a
/// folder that could not be read and a closed archive count as one entry.
pub fn summarize(root: &Node, elapsed_ms: u64) -> Summary {
    fn walk(node: &Node, s: &mut Summary) {
        for child in node.children() {
            let closed_archive = child.unexpanded()
                && child.left().or(child.right()).is_some_and(|side| side.kind == EntryKind::Archive);
            if child.is_container() && child.status != Status::Error && !closed_archive {
                if child.unexpanded() {
                    s.identical_subtrees += 1;
                }
                walk(child, s);
                continue;
            }
            s.files += 1;
            match child.status {
                Status::Same => s.same += 1,
                Status::Similar => s.similar += 1,
                Status::Different => s.different += 1,
                Status::LeftOnly => s.left_only += 1,
                Status::RightOnly => s.right_only += 1,
                Status::Error => s.errors += 1,
            }
        }
    }
    let mut s = Summary { elapsed_ms, ..Summary::default() };
    walk(root, &mut s);
    s
}

/// Replaces the nodes at `paths` in `root` with fresh ones.
pub fn apply_refresh(root: &mut Node, paths: &[String], nodes: Vec<Option<Node>>) {
    for (path, node) in paths.iter().zip(nodes) {
        if !path.is_empty() || node.is_some() {
            tree::replace(root, path, node);
        }
    }
}

/// What the views show about one side of a file comparison.
#[derive(Clone, Debug)]
pub struct SideInfo {
    pub exists: bool,
    pub size: u64,
    pub binary: bool,
    pub encoding: String,
    pub line_ending: Option<LineEnding>,
    pub lines: usize,
}

impl SideInfo {
    fn missing() -> Self {
        SideInfo { exists: false, size: 0, binary: false, encoding: String::new(), line_ending: None, lines: 0 }
    }

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
    pub path: String,
    pub edit: EditSession,
    pub syntax: Option<String>,
    pub sides: [SideInfo; 2],
    pub note: Option<String>,
    pub view: View,
    pub views: Vec<View>,
    pub equal_view: Option<View>,
    pub identical: bool,
    pub image: Option<ImageDiff>,
    /// Per side: the text can be edited and saved back.
    pub editable: [bool; 2],
    /// Per side, for a view that saves into its file: the file's bytes.
    data: [Option<Data>; 2],
    /// Per editable side: a hash of the file as read or last saved, so a save
    /// notices another program's change in the meantime.
    on_disk: [Option<u64>; 2],
    highlighters: [DocHighlighter; 2],
    theme: ThemeKind,
}

fn stamp(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn read_side(source: &Source, path: &str) -> Result<(SideInfo, Option<Data>), String> {
    let full = source.path(path);
    source.vfs.changed(&full);
    let entry = match source.vfs.stat(&full) {
        Ok(entry) => entry,
        Err(parallax_core::Error::NotFound(_)) => return Ok((SideInfo::missing(), None)),
        Err(e) => return Err(e.to_string()),
    };
    let data = source.vfs.read(&full).map_err(|e| e.to_string())?;
    let info = SideInfo {
        exists: true,
        size: entry.size.max(data.len() as u64),
        binary: text::is_binary(&data),
        ..SideInfo::missing()
    };
    Ok((info, Some(data)))
}

/// The view's grammar, as `open_file_diff` picks it.
fn view_syntax(view: View, name: &str, first_line: &str) -> Option<String> {
    let found = |n: &str, line: &str| highlight::detect_syntax(n, line).map(|s| s.name.clone());
    match view {
        View::Text => found(name, first_line),
        View::PrettyXml | View::AndroidXml => Some("XML".into()),
        View::PrettyJson | View::ScoreConfiguration | View::FlatbufferData => Some("JSON".into()),
        View::Definitions => found(name, ""),
        View::NormalizedYaml => Some("YAML".into()),
        View::NormalizedToml => Some("TOML".into()),
        View::ReflowedText => Some("reStructuredText".into()),
        View::TemplateOutline => Some("FreeMarker".into()),
        View::NormalizedConan => Some("Conan".into()),
        View::PythonOutline => Some("Python".into()),
        View::PerlOutline => Some("Perl".into()),
        View::RustOutline => Some("Rust".into()),
        View::EcucValues | View::ArxmlStructure => Some("JSON".into()),
        View::XpandOutline => Some("Xpand".into()),
        View::XtendOutline if name.to_ascii_lowercase().ends_with(".chk") => Some("Check (oAW)".into()),
        View::XtendOutline => Some("Xtend (oAW)".into()),
        View::JsOutline => found(name, "").or_else(|| Some("JavaScript".into())),
        View::ChatTemplate => Some("Jinja2".into()),
        View::Image => None,
        _ => Some(highlight::view_syntax().name.clone()),
    }
}

/// Opens the file at `path` (relative to both roots; empty when the roots are
/// files) on both sides, in `requested` or the view the core chooses.
pub fn open_file(
    session: &Session,
    path: &str,
    requested: Option<View>,
    options: DiffOptions,
) -> Result<FileDiff, String> {
    let (mut left_info, left_data) = read_side(&session.left, path)?;
    let (mut right_info, right_data) = read_side(&session.right, path)?;
    // The name to pick views and grammars from: the compared path, or the root file name.
    let name = if path.is_empty() {
        let root = if session.left.root.is_empty() { &session.right.root } else { &session.left.root };
        split_parent(root).1.to_string()
    } else {
        path.to_string()
    };
    let chosen = file_view::choose(
        [&session.left, &session.right],
        path,
        &name,
        [left_data.as_deref(), right_data.as_deref()],
        left_info.binary || right_info.binary,
        requested,
        [None, None],
    );
    let file_view::Chosen { view, views, mut note, equal_view, identical, unread, image, texts } = chosen;
    let [left_text, right_text] = texts;
    let make_doc = |data: &Option<Data>,
                    converted: Option<Result<String, String>>,
                    info: &mut SideInfo|
     -> Result<TextDoc, String> {
        let doc = match (view, data, converted) {
            (View::Image, _, _) | (_, None, _) => return Ok(text::decode(b"")),
            (View::Text, Some(bytes), _) => {
                let doc = text::decode(bytes);
                info.encoding = doc.encoding.to_string();
                doc
            }
            (_, Some(_), Some(Ok(text))) => {
                info.encoding = view.label().to_string();
                text::decode(text.as_bytes())
            }
            (_, Some(_), Some(Err(e))) => return Err(e),
            (_, Some(_), None) => text::decode(b""),
        };
        info.line_ending = Some(doc.line_ending);
        info.lines = doc.len();
        Ok(doc)
    };
    let mut left = make_doc(&left_data, left_text, &mut left_info)?;
    let mut right = make_doc(&right_data, right_text, &mut right_info)?;
    // Plain text can be edited where the source can be written and saving would
    // give back the same bytes for unedited text, as in `open_file_diff`.
    let editable = |side: Side, data: &Option<Data>, doc: &TextDoc, other: &Option<Data>| {
        let text = view == View::Text
            && match data {
                Some(bytes) => !text::is_binary(bytes) && doc.encode().is_ok_and(|e| e[..] == bytes[..]),
                None => other.as_ref().is_some_and(|o| !text::is_binary(o)),
            };
        let converted = convert::can_write_back(view) && data.is_some();
        (text || converted) && session.writable(side)
    };
    let editable =
        [editable(Side::Left, &left_data, &left, &right_data), editable(Side::Right, &right_data, &right, &left_data)];
    // A missing side is created like the other file: encoding, BOM, line endings.
    if left_data.is_none() {
        left = TextDoc::empty_like(&right);
    } else if right_data.is_none() {
        right = TextDoc::empty_like(&left);
    }
    let options = DiffOptions { keyed: convert::keyed_view(view), ..options };
    let edit = EditSession::new(left, right, options);
    let (l, r) = (edit.doc(Side::Left), edit.doc(Side::Right));
    let first_line = if l.is_empty() { r.lines.first().map(|_| r.line(0)) } else { Some(l.line(0)) };
    let syntax = view_syntax(view, &name, first_line.unwrap_or_default());
    if note.is_none() && unread && edit.diff().sections.is_empty() && !matches!(view, View::Text | View::Hex) {
        note = Some(format!(
            "“{}” shows no difference. The files are over {} MB, so their contents were not compared byte for byte; “Hex” compares them.",
            view.label(),
            file_view::FULL_COMPARE_LIMIT >> 20
        ));
    }
    if note.is_none()
        && !identical
        && edit.diff().sections.is_empty()
        && view == View::Text
        && left_data.is_some()
        && right_data.is_some()
    {
        note =
            Some("The files differ only in bytes the text view does not show (encoding, BOM or line endings).".into());
    }
    let on_disk = [(editable[0], &left_data), (editable[1], &right_data)]
        .map(|(editable, data)| data.as_deref().filter(|_| editable).map(stamp));
    let theme = crate::style::syntax_theme();
    let grammar = syntax.as_deref().and_then(highlight::find_syntax);
    Ok(FileDiff {
        path: path.to_string(),
        edit,
        syntax,
        sides: [left_info, right_info],
        note,
        view,
        views,
        equal_view,
        identical,
        image,
        editable,
        data: if convert::can_write_back(view) { [left_data, right_data] } else { [None, None] },
        on_disk,
        highlighters: [DocHighlighter::new(grammar, theme), DocHighlighter::new(grammar, theme)],
        theme,
    })
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
    pub fn dirty(&self, side: Side) -> bool {
        self.edit.is_dirty(side)
    }

    /// Rows `start..start + count`, built by the same core function as the Tauri
    /// app's `file_rows` command.
    pub fn rows(&mut self, start: usize, count: usize) -> Vec<RowView> {
        let theme = crate::style::syntax_theme();
        if theme != self.theme {
            let grammar = self.syntax.as_deref().and_then(highlight::find_syntax);
            self.highlighters = [DocHighlighter::new(grammar, theme), DocHighlighter::new(grammar, theme)];
            self.theme = theme;
        }
        let [hl_left, hl_right] = &mut self.highlighters;
        let (left, right) = (self.edit.doc(Side::Left), self.edit.doc(Side::Right));
        rows::side_by_side(left, right, &self.edit.diff().rows, start, count, [hl_left, hl_right], None)
    }

    /// Highlighting is stale from the edited line on.
    fn edited(&mut self, edited: Option<parallax_core::edit::Edited>) -> bool {
        let Some(edited) = edited else { return false };
        self.highlighters[edited.side as usize].invalidate_from(edited.line);
        true
    }

    /// Copies the rows of a difference to one side, as `copy_rows` does.
    pub fn copy_rows(&mut self, rows: std::ops::Range<u32>, to: Side) -> bool {
        if !self.editable[to as usize] {
            return false;
        }
        let edited = self.edit.copy_rows(rows, to);
        self.edited(edited)
    }

    pub fn undo(&mut self) -> bool {
        let edited = self.edit.undo();
        self.edited(edited)
    }

    pub fn redo(&mut self) -> bool {
        let edited = self.edit.redo();
        self.edited(edited)
    }

    /// The text of `rows` on one side, one line per row with a line on that side.
    pub fn rows_text(&self, side: Side, rows: std::ops::Range<u32>) -> String {
        self.edit.rows_text(side, rows)
    }

    /// Replaces the line in `row` on one side with `text` (or adds it where the side
    /// has no line), as `replace_rows` does.
    pub fn replace_row(&mut self, side: Side, row: u32, text: &str) -> bool {
        if !self.editable[side as usize] {
            return false;
        }
        let edited = self.edit.replace_rows(side, row..row + 1, text, true);
        self.edited(edited)
    }

    /// Diffs again with other whitespace and case rules; the view keeps its own keying.
    pub fn set_options(&mut self, options: DiffOptions) {
        self.edit.set_options(DiffOptions { keyed: convert::keyed_view(self.view), ..options });
    }

    pub fn search(&self, query: &str, options: &SearchOptions) -> Result<Vec<Hit>, String> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let re = search::pattern(query, options)?;
        let docs = [self.edit.doc(Side::Left), self.edit.doc(Side::Right)];
        Ok(search::search(&self.edit.diff().rows, docs, &re, SearchSide::Both, 20_000).hits)
    }

    /// Saves one side, as `save_side` does: refuses when another program changed
    /// the file since it was read, unless `overwrite`.
    pub fn save(&mut self, session: &Session, side: Side, overwrite: bool) -> Result<(), SaveError> {
        let i = side as usize;
        if !self.editable[i] {
            return Err(SaveError::Failed("This side cannot be saved.".into()));
        }
        let source = session.source(side);
        let full = source.path(&self.path);
        source.vfs.changed(&full);
        let now = source.vfs.read(&full).ok().map(|d| stamp(&d));
        if now != self.on_disk[i] && !overwrite {
            return Err(SaveError::Stale);
        }
        let bytes = if self.view == View::Text {
            self.edit.doc(side).encode().map_err(|e| SaveError::Failed(format!("Cannot save: {e}.")))?
        } else {
            let original =
                self.data[i].as_deref().ok_or(SaveError::Failed("The file to save into is missing.".into()))?;
            let other = self.data[1 - i].as_deref();
            convert::write_back(self.view, original, &self.edit.doc(side).text, other)
                .map_err(|e| SaveError::Failed(format!("Cannot save: {e}.")))?
        };
        let kind = match source.stat(&self.path) {
            Ok(e) if e.kind == EntryKind::Symlink => EntryKind::Symlink,
            _ => EntryKind::File,
        };
        let written = Data::from_vec(bytes);
        let _guard = session.writing(std::slice::from_ref(&self.path));
        let change = Change::Put { path: full, kind, data: written.clone(), mtime: None };
        source.vfs.apply(vec![change], &Progress::default()).map_err(|e| SaveError::Failed(e.to_string()))?;
        self.on_disk[i] = Some(stamp(&written));
        self.sides[i].size = written.len() as u64;
        self.sides[i].exists = true;
        if self.data[i].is_some() {
            self.data[i] = Some(written);
        }
        self.edit.mark_saved(side);
        Ok(())
    }
}

#[derive(Debug)]
pub enum SaveError {
    /// Another program changed the file since it was read.
    Stale,
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders(files: &[(&str, &str, Option<&str>, Option<&str>)]) -> (tempfile::TempDir, Arc<Session>) {
        let dir = tempfile::tempdir().unwrap();
        for side in ["l", "r"] {
            std::fs::create_dir_all(dir.path().join(side)).unwrap();
        }
        for (dir_name, name, left, right) in files {
            for (side, content) in [("l", left), ("r", right)] {
                if let Some(content) = content {
                    let path = dir.path().join(side).join(dir_name);
                    std::fs::create_dir_all(&path).unwrap();
                    std::fs::write(path.join(name), content).unwrap();
                }
            }
        }
        let p = |s: &str| dir.path().join(s).to_string_lossy().into_owned();
        let session = Session::open(&p("l"), &p("r")).unwrap();
        (dir, Arc::new(session))
    }

    #[test]
    fn copy_save_undo_and_search_go_through_the_core() {
        let (dir, session) = folders(&[("", "a.txt", Some("one\ntwo\nthree\n"), Some("one\nTWO\nthree\nfour\n"))]);
        let mut diff = open_file(&session, "a.txt", None, DiffOptions::default()).unwrap();
        assert_eq!(diff.view, View::Text);
        assert_eq!(diff.editable, [true, true]);
        // The changed line and the added line are two differences.
        assert_eq!(diff.sections().len(), 2);
        while let Some(section) = diff.sections().first().cloned() {
            assert!(diff.copy_rows(section.start..section.end, Side::Left));
        }
        assert!(diff.dirty(Side::Left));
        assert!(diff.undo());
        assert_eq!(diff.sections().len(), 1, "undo brings back the last difference copied");
        assert!(diff.redo());
        assert!(diff.sections().is_empty());
        diff.save(&session, Side::Left, false).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join("l/a.txt")).unwrap(), "one\nTWO\nthree\nfour\n");
        assert!(!diff.dirty(Side::Left));
        let hits = diff.search("two", &SearchOptions::default()).unwrap();
        assert_eq!(hits.len(), 2, "case-insensitive by default, one per side: {hits:?}");
        // Another program changes the file: saving asks first.
        assert!(diff.replace_row(Side::Left, 0, "ONE"));
        std::fs::write(dir.path().join("l/a.txt"), "changed elsewhere\n").unwrap();
        assert!(matches!(diff.save(&session, Side::Left, false), Err(SaveError::Stale)));
        diff.save(&session, Side::Left, true).unwrap();
        assert!(std::fs::read_to_string(dir.path().join("l/a.txt")).unwrap().starts_with("ONE\n"));
    }

    #[test]
    fn copy_delete_refresh_and_counts() {
        let (dir, session) = folders(&[
            ("sub", "same.txt", Some("x"), Some("x")),
            ("sub", "changed.txt", Some("a"), Some("b")),
            ("", "left.txt", Some("only left"), None),
        ]);
        let options = CompareOptions::default();
        let mut root = compare::compare(&session.left, &session.right, &options, &Control::default()).unwrap().root;
        let s = summarize(&root, 0);
        assert_eq!((s.files, s.same, s.different, s.left_only), (3, 1, 1, 1));
        session.copy(&["left.txt".into()], Side::Right, &options).unwrap();
        assert!(dir.path().join("r/left.txt").exists());
        session.delete(&["sub/changed.txt".into()], Side::Left).unwrap();
        let paths = vec!["left.txt".to_string(), "sub/changed.txt".to_string()];
        let nodes = session.refresh(&paths, &options);
        apply_refresh(&mut root, &paths, nodes);
        let s = summarize(&root, 0);
        assert_eq!((s.files, s.same, s.different, s.left_only, s.right_only), (3, 2, 0, 0, 1));
    }

    #[test]
    fn json_opens_in_its_equal_view_and_text_on_request() {
        let (_dir, session) = folders(&[("", "x.json", Some(r#"{"a":1}"#), Some("{\n  \"a\": 1\n}\n"))]);
        let diff = open_file(&session, "x.json", None, DiffOptions::default()).unwrap();
        assert_ne!(diff.view, View::Text);
        assert!(diff.note.as_deref().is_some_and(|n| n.starts_with("≈ Similar")));
        assert!(diff.sections().is_empty());
        assert_eq!(diff.editable, [false, false], "a converted view is not edited as text");
        let text = open_file(&session, "x.json", Some(View::Text), DiffOptions::default()).unwrap();
        assert_eq!(text.view, View::Text);
        assert!(!text.sections().is_empty());
        assert_eq!(text.equal_view, Some(diff.view));
    }
}
