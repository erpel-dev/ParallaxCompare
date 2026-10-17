//! Commands invoked by the web UI. Heavy work runs on blocking worker threads;
//! results are kept here and the UI fetches only what is visible.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use parallax_core::compare::{self, CompareOptions, CompareResult, Control, Node};
use parallax_core::highlight::{self, DocHighlighter, Span, ThemeKind};
use parallax_core::source::{Source, SourceInfo, SourceOptions, SourceSpec, open_pair};
use parallax_core::text::{self, LineEnding, TextDoc};
use parallax_core::textdiff::{self, DiffOptions, DiffStats, RowKind, Section, TextDiff};
use parallax_core::vfs::archive::detect_format;
use parallax_core::vfs::git::{GitRepo, RefList};
use parallax_core::vfs::split_parent;
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Two sources given on the command line, e.g. when started as `git difftool`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupRequest {
    left: SourceSpec,
    right: SourceSpec,
    merge_base: bool,
}

impl StartupRequest {
    pub fn from_args(args: impl Iterator<Item = String>) -> Option<Self> {
        let mut merge_base = false;
        let mut paths = Vec::new();
        for arg in args {
            match arg.as_str() {
                "--merge-base" => merge_base = true,
                a if a.starts_with('-') => {}
                a => paths.push(a.to_string()),
            }
        }
        match paths.as_slice() {
            [left, right] => {
                Some(StartupRequest { left: SourceSpec::parse(left), right: SourceSpec::parse(right), merge_base })
            }
            _ => None,
        }
    }
}

struct Session {
    left: Source,
    right: Source,
    control: Mutex<Arc<Control>>,
}

struct FileDiff {
    left: TextDoc,
    right: TextDoc,
    diff: TextDiff,
    syntax: Option<String>,
    highlighters: Option<(ThemeKind, DocHighlighter, DocHighlighter)>,
}

pub struct AppState {
    startup: Mutex<Option<StartupRequest>>,
    sessions: Mutex<HashMap<u64, Arc<Session>>>,
    diffs: Mutex<HashMap<u64, Arc<Mutex<FileDiff>>>>,
    next_id: AtomicU64,
}

impl AppState {
    pub fn new(startup: Option<StartupRequest>) -> Self {
        AppState {
            startup: Mutex::new(startup),
            sessions: Mutex::new(HashMap::new()),
            diffs: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn session(&self, id: u64) -> CmdResult<Arc<Session>> {
        self.sessions.lock().get(&id).cloned().ok_or_else(|| "comparison was closed".to_string())
    }

    fn diff(&self, id: u64) -> CmdResult<Arc<Mutex<FileDiff>>> {
        self.diffs.lock().get(&id).cloned().ok_or_else(|| "file comparison was closed".to_string())
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> CmdResult<T> + Send + 'static) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(err)?
}

#[tauri::command]
pub fn startup_request(state: State<'_, AppState>) -> Option<StartupRequest> {
    state.startup.lock().take()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathInfo {
    exists: bool,
    is_dir: bool,
    is_archive: bool,
    /// Working directory of the Git repository containing the path, if any.
    git_root: Option<String>,
}

#[tauri::command]
pub async fn inspect_path(path: String) -> CmdResult<PathInfo> {
    blocking(move || {
        let p = std::path::Path::new(&path);
        let meta = std::fs::metadata(p).ok();
        let git_root = if meta.is_some() {
            GitRepo::discover(p).ok().and_then(|r| r.workdir()).map(|w| w.display().to_string())
        } else {
            None
        };
        Ok(PathInfo {
            exists: meta.is_some(),
            is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
            is_archive: detect_format(&path).is_some(),
            git_root,
        })
    })
    .await
}

#[tauri::command]
pub async fn git_refs(repo: String) -> CmdResult<RefList> {
    blocking(move || GitRepo::discover(&repo).and_then(|r| r.refs(40)).map_err(err)).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    id: u64,
    left: SourceInfo,
    right: SourceInfo,
    /// Both roots are files: show a text compare instead of a folder compare.
    is_file: bool,
}

#[tauri::command]
pub async fn open_session(
    state: State<'_, AppState>,
    left: SourceSpec,
    right: SourceSpec,
    options: SourceOptions,
    merge_base: bool,
) -> CmdResult<SessionInfo> {
    let (l, r) = blocking(move || open_pair(&left, &right, options, merge_base).map_err(err)).await?;
    let (lf, rf) = (l.is_file().map_err(err)?, r.is_file().map_err(err)?);
    if lf != rf {
        return Err("cannot compare a file with a folder".into());
    }
    let id = state.id();
    let info = SessionInfo { id, left: l.info.clone(), right: r.info.clone(), is_file: lf };
    state.sessions.lock().insert(id, Arc::new(Session { left: l, right: r, control: Mutex::default() }));
    Ok(info)
}

#[tauri::command]
pub fn close_session(state: State<'_, AppState>, id: u64) {
    if let Some(session) = state.sessions.lock().remove(&id) {
        session.control.lock().cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    session: u64,
    files: u64,
}

#[tauri::command]
pub async fn run_compare(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    options: CompareOptions,
) -> CmdResult<CompareResult> {
    let session = state.session(id)?;
    let control = Arc::new(Control::default());
    {
        let mut slot = session.control.lock();
        slot.cancel.store(true, Ordering::Relaxed);
        *slot = control.clone();
    }
    let done = Arc::new(AtomicBool::new(false));
    {
        let (done, control) = (done.clone(), control.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(120));
                let files = control.files_done.load(Ordering::Relaxed);
                let _ = app.emit("compare-progress", Progress { session: id, files });
            }
        });
    }
    let result =
        blocking(move || compare::compare(&session.left, &session.right, &options, &control).map_err(err)).await;
    done.store(true, Ordering::Relaxed);
    result
}

#[tauri::command]
pub fn cancel_compare(state: State<'_, AppState>, id: u64) -> CmdResult<()> {
    state.session(id)?.control.lock().cancel.store(true, Ordering::Relaxed);
    Ok(())
}

/// Compares one subtree again: expands an identical Git folder or refreshes after a change.
#[tauri::command]
pub async fn expand(state: State<'_, AppState>, id: u64, path: String, options: CompareOptions) -> CmdResult<Node> {
    let session = state.session(id)?;
    blocking(move || {
        compare::compare_path(&session.left, &session.right, &path, &options, &Control::default())
            .map(|r| r.root)
            .map_err(err)
    })
    .await
}

/// Copies `path` to the other side and returns the refreshed node.
#[tauri::command]
pub async fn copy_entry(
    state: State<'_, AppState>,
    id: u64,
    path: String,
    to_right: bool,
    options: CompareOptions,
) -> CmdResult<Node> {
    let session = state.session(id)?;
    blocking(move || {
        let (from, to) = if to_right { (&session.left, &session.right) } else { (&session.right, &session.left) };
        parallax_core::ops::copy(from, to, &path).map_err(err)?;
        compare::compare_path(&session.left, &session.right, &path, &options, &Control::default())
            .map(|r| r.root)
            .map_err(err)
    })
    .await
}

/// Deletes `path` on one side; returns the refreshed node, or `None` when it is gone on both sides.
#[tauri::command]
pub async fn delete_entry(
    state: State<'_, AppState>,
    id: u64,
    path: String,
    left: bool,
    options: CompareOptions,
) -> CmdResult<Option<Node>> {
    let session = state.session(id)?;
    blocking(move || {
        let source = if left { &session.left } else { &session.right };
        parallax_core::ops::delete(source, &path).map_err(err)?;
        match compare::compare_path(&session.left, &session.right, &path, &options, &Control::default()) {
            Ok(r) => Ok(Some(r.root)),
            Err(parallax_core::Error::NotFound(_)) => Ok(None),
            Err(e) => Err(err(e)),
        }
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SideInfo {
    exists: bool,
    size: u64,
    binary: bool,
    encoding: String,
    line_ending: LineEnding,
    bom: bool,
    lines: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiffInfo {
    id: u64,
    rows: u32,
    sections: Vec<Section>,
    stats: DiffStats,
    left: SideInfo,
    right: SideInfo,
    syntax: Option<String>,
    /// Byte-identical files.
    identical: bool,
}

fn load_side(source: &Source, path: &str) -> CmdResult<(SideInfo, Option<Vec<u8>>)> {
    let full = source.path(path);
    let entry = match source.vfs.stat(&full) {
        Ok(entry) => entry,
        Err(parallax_core::Error::NotFound(_)) => {
            let info = SideInfo {
                exists: false,
                size: 0,
                binary: false,
                encoding: String::new(),
                line_ending: LineEnding::None,
                bom: false,
                lines: 0,
            };
            return Ok((info, None));
        }
        Err(e) => return Err(err(e)),
    };
    let data = source.vfs.read(&full).map_err(err)?;
    let info = SideInfo {
        exists: true,
        size: entry.size.max(data.len() as u64),
        binary: text::is_binary(&data),
        encoding: String::new(),
        line_ending: LineEnding::None,
        bom: false,
        lines: 0,
    };
    Ok((info, Some(data.to_vec())))
}

fn empty_doc() -> TextDoc {
    text::decode(b"")
}

#[tauri::command]
pub async fn open_file_diff(
    state: State<'_, AppState>,
    id: u64,
    path: String,
    options: DiffOptions,
    syntax: Option<String>,
) -> CmdResult<FileDiffInfo> {
    let session = state.session(id)?;
    let (info, file_diff) = blocking(move || {
        let (mut left_info, left_data) = load_side(&session.left, &path)?;
        let (mut right_info, right_data) = load_side(&session.right, &path)?;
        let identical = left_data.is_some() && left_data == right_data;
        let binary = left_info.binary || right_info.binary;
        let decode = |data: &Option<Vec<u8>>, info: &mut SideInfo| -> TextDoc {
            match data {
                Some(bytes) if !binary => {
                    let doc = text::decode(bytes);
                    info.encoding = doc.encoding.to_string();
                    info.line_ending = doc.line_ending;
                    info.bom = doc.bom;
                    info.lines = doc.len() as u32;
                    doc
                }
                _ => empty_doc(),
            }
        };
        let left = decode(&left_data, &mut left_info);
        let right = decode(&right_data, &mut right_info);
        let diff = textdiff::diff_docs(&left, &right, &options);
        // The name to pick a grammar from: the compared path, or the root file name.
        let syntax_path = if path.is_empty() {
            let root = if session.left.root.is_empty() { &session.right.root } else { &session.left.root };
            split_parent(root).1.to_string()
        } else {
            path.clone()
        };
        let first_line = if left.is_empty() { right.lines.first().map(|_| right.line(0)) } else { Some(left.line(0)) };
        let syntax = syntax
            .or_else(|| highlight::detect_syntax(&syntax_path, first_line.unwrap_or_default()).map(|s| s.name.clone()));
        let info = FileDiffInfo {
            id: 0,
            rows: diff.rows.len() as u32,
            sections: diff.sections.clone(),
            stats: diff.stats.clone(),
            left: left_info,
            right: right_info,
            syntax: syntax.clone(),
            identical,
        };
        Ok((info, FileDiff { left, right, diff, syntax, highlighters: None }))
    })
    .await?;
    let diff_id = state.id();
    state.diffs.lock().insert(diff_id, Arc::new(Mutex::new(file_diff)));
    Ok(FileDiffInfo { id: diff_id, ..info })
}

#[derive(Serialize)]
pub struct LineView {
    /// 0-based line number.
    n: u32,
    t: String,
    /// Syntax colours.
    s: Vec<Span>,
    /// Changed character ranges within a changed line.
    m: Vec<(u32, u32)>,
}

#[derive(Serialize)]
pub struct RowView {
    k: RowKind,
    l: Option<LineView>,
    r: Option<LineView>,
}

fn line_range(rows: &[textdiff::Row], pick: impl Fn(&textdiff::Row) -> Option<u32>) -> Option<(usize, usize)> {
    let first = rows.iter().find_map(&pick)? as usize;
    let last = rows.iter().rev().find_map(&pick)? as usize;
    Some((first, last + 1))
}

#[tauri::command]
pub async fn file_rows(
    state: State<'_, AppState>,
    id: u64,
    start: u32,
    count: u32,
    theme: ThemeKind,
) -> CmdResult<Vec<RowView>> {
    let diff = state.diff(id)?;
    blocking(move || {
        let mut fd = diff.lock();
        let fd = &mut *fd;
        let end = (start as usize + count as usize).min(fd.diff.rows.len());
        let rows = &fd.diff.rows[(start as usize).min(end)..end];
        if fd.highlighters.as_ref().is_none_or(|(t, _, _)| *t != theme) {
            let syntax = fd.syntax.as_deref().and_then(highlight::find_syntax);
            fd.highlighters = Some((theme, DocHighlighter::new(syntax, theme), DocHighlighter::new(syntax, theme)));
        }
        let (_, hl_left, hl_right) = fd.highlighters.as_mut().expect("highlighters set above");
        let spans = |hl: &mut DocHighlighter, doc: &TextDoc, range: Option<(usize, usize)>| match range {
            Some((a, b)) => (a, hl.highlight(doc, a, b)),
            None => (0, Vec::new()),
        };
        let (left_base, left_spans) = spans(hl_left, &fd.left, line_range(rows, |r| r.left));
        let (right_base, right_spans) = spans(hl_right, &fd.right, line_range(rows, |r| r.right));
        let view = |doc: &TextDoc, n: u32, base: usize, all: &Vec<Vec<Span>>, marks: Vec<(u32, u32)>| LineView {
            n,
            t: doc.line(n as usize).to_string(),
            s: all.get(n as usize - base).cloned().unwrap_or_default(),
            m: marks,
        };
        Ok(rows
            .iter()
            .map(|row| {
                let (lm, rm) = match (row.kind, row.left, row.right) {
                    (RowKind::Changed, Some(l), Some(r)) => {
                        let (a, b) = textdiff::intraline(fd.left.line(l as usize), fd.right.line(r as usize));
                        (
                            a.into_iter().map(|r| (r.start, r.end)).collect(),
                            b.into_iter().map(|r| (r.start, r.end)).collect(),
                        )
                    }
                    _ => (Vec::new(), Vec::new()),
                };
                RowView {
                    k: row.kind,
                    l: row.left.map(|n| view(&fd.left, n, left_base, &left_spans, lm)),
                    r: row.right.map(|n| view(&fd.right, n, right_base, &right_spans, rm)),
                }
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub fn close_file_diff(state: State<'_, AppState>, id: u64) {
    state.diffs.lock().remove(&id);
}

#[tauri::command]
pub fn syntax_names() -> Vec<String> {
    highlight::syntax_names()
}
