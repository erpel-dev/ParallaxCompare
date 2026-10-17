//! Commands invoked by the web UI. Heavy work runs on blocking worker threads;
//! results are kept here and the UI fetches only what is visible.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use parallax_core::compare::{self, CompareOptions, CompareResult, Control, Node};
use parallax_core::convert::{self, View};
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
    /// Encoding of the shown text; for converted views, the view's name.
    encoding: String,
    line_ending: LineEnding,
    bom: bool,
    lines: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewOption {
    id: View,
    label: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSideView {
    /// `data:` URL the web view can show directly.
    url: String,
    width: u32,
    height: u32,
    format: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageView {
    left: Option<ImageSideView>,
    right: Option<ImageSideView>,
    mask: Option<String>,
    different_pixels: u64,
    total_pixels: u64,
    bounds: Option<[u32; 4]>,
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
    /// The view shown and the views that can be chosen for this pair.
    view: View,
    views: Vec<ViewOption>,
    image: Option<ImageView>,
    /// Something the user should know, e.g. why a requested view is not shown.
    note: Option<String>,
}

fn missing_side() -> SideInfo {
    SideInfo {
        exists: false,
        size: 0,
        binary: false,
        encoding: String::new(),
        line_ending: LineEnding::None,
        bom: false,
        lines: 0,
    }
}

fn load_side(source: &Source, path: &str) -> CmdResult<(SideInfo, Option<Vec<u8>>)> {
    let full = source.path(path);
    let entry = match source.vfs.stat(&full) {
        Ok(entry) => entry,
        Err(parallax_core::Error::NotFound(_)) => return Ok((missing_side(), None)),
        Err(e) => return Err(err(e)),
    };
    let data = source.vfs.read(&full).map_err(err)?;
    let info = SideInfo {
        exists: true,
        size: entry.size.max(data.len() as u64),
        binary: text::is_binary(&data),
        ..missing_side()
    };
    Ok((info, Some(data.to_vec())))
}

fn empty_doc() -> TextDoc {
    text::decode(b"")
}

fn data_url(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Explains why byte-different files show no differences in the text view.
fn invisible_difference(left: &TextDoc, right: &TextDoc, l: &[u8], r: &[u8], minor: u32) -> String {
    let mut reasons = Vec::new();
    if left.encoding != right.encoding {
        reasons.push(format!("encoding ({} vs {})", left.encoding, right.encoding));
    }
    if left.bom != right.bom {
        reasons.push("byte order mark".to_string());
    }
    if left.line_ending != right.line_ending {
        reasons.push(format!("line endings ({:?} vs {:?})", left.line_ending, right.line_ending));
    }
    if l.ends_with(b"\n") != r.ends_with(b"\n") {
        reasons.push("newline at end of file".to_string());
    }
    if minor > 0 {
        reasons.push(format!("{minor} lines with whitespace or case differences that the current rules ignore"));
    }
    if reasons.is_empty() {
        "The files differ in bytes that the text view does not show.".into()
    } else {
        format!("The files differ only in: {}.", reasons.join(", "))
    }
}

#[tauri::command]
pub async fn open_file_diff(
    state: State<'_, AppState>,
    id: u64,
    path: String,
    options: DiffOptions,
    syntax: Option<String>,
    view: Option<View>,
) -> CmdResult<FileDiffInfo> {
    let session = state.session(id)?;
    let (info, file_diff) = blocking(move || {
        let (mut left_info, left_data) = load_side(&session.left, &path)?;
        let (mut right_info, right_data) = load_side(&session.right, &path)?;
        let identical = left_data.is_some() && left_data == right_data;
        // The name to pick views and grammars from: the compared path, or the root file name.
        let name = if path.is_empty() {
            let root = if session.left.root.is_empty() { &session.right.root } else { &session.left.root };
            split_parent(root).1.to_string()
        } else {
            path.clone()
        };
        let views = convert::views_for_pair(
            left_data.as_deref().map(|d| (name.as_str(), d)),
            right_data.as_deref().map(|d| (name.as_str(), d)),
        );
        let mut chosen = view.filter(|v| views.contains(v)).unwrap_or(views[0]);
        let mut note = None;
        let view_options = views.iter().map(|&v| ViewOption { id: v, label: v.label() }).collect();

        let mut image = None;
        if chosen == View::Image {
            match convert::image_diff(&name, left_data.as_deref(), right_data.as_deref()) {
                Ok(d) => {
                    let side = |s: &Option<convert::ImageSide>| {
                        s.as_ref().map(|s| ImageSideView {
                            url: data_url(&s.mime, &s.bytes),
                            width: s.width,
                            height: s.height,
                            format: s.format.clone(),
                        })
                    };
                    note = d.note.clone();
                    image = Some(ImageView {
                        left: side(&d.left),
                        right: side(&d.right),
                        mask: d.mask.as_deref().map(|m| data_url("image/png", m)),
                        different_pixels: d.different_pixels,
                        total_pixels: d.total_pixels,
                        bounds: d.bounds,
                    });
                }
                Err(e) => {
                    note = Some(format!("The image could not be decoded ({e}); showing a hex dump."));
                    chosen = View::Hex;
                }
            }
        }

        // Converted text for both sides; LibreOffice conversions take seconds, so the
        // two sides run at the same time.
        let render = |data: &Option<Vec<u8>>, view: View| -> Option<Result<String, String>> {
            data.as_ref().map(|d| convert::render(view, &name, d).map_err(err))
        };
        let (mut left_text, mut right_text) = (None, None);
        if !matches!(chosen, View::Image | View::Text) {
            (left_text, right_text) = std::thread::scope(|s| {
                let l = s.spawn(|| render(&left_data, chosen));
                let r = render(&right_data, chosen);
                (l.join().unwrap_or(None), r)
            });
            if let Some(Err(e)) = left_text.as_ref().or(right_text.as_ref()).filter(|r| r.is_err()) {
                let fallback = if left_info.binary || right_info.binary { View::Hex } else { View::Text };
                note = Some(format!("{} is not available: {e}. Showing {}.", chosen.label(), fallback.label()));
                chosen = fallback;
                (left_text, right_text) = if fallback == View::Hex {
                    (render(&left_data, fallback), render(&right_data, fallback))
                } else {
                    (None, None)
                };
            }
        }

        let make_doc = |data: &Option<Vec<u8>>,
                        converted: Option<Result<String, String>>,
                        info: &mut SideInfo|
         -> CmdResult<TextDoc> {
            let doc = match (chosen, data, converted) {
                (View::Image, _, _) | (_, None, _) => return Ok(empty_doc()),
                (View::Text, Some(bytes), _) => {
                    let doc = text::decode(bytes);
                    info.encoding = doc.encoding.to_string();
                    doc
                }
                (_, Some(_), Some(Ok(text))) => {
                    info.encoding = chosen.label().to_string();
                    text::decode(text.as_bytes())
                }
                (_, Some(_), Some(Err(e))) => return Err(e),
                (_, Some(_), None) => empty_doc(),
            };
            info.line_ending = doc.line_ending;
            info.bom = doc.bom;
            info.lines = doc.len() as u32;
            Ok(doc)
        };
        let left = make_doc(&left_data, left_text, &mut left_info)?;
        let right = make_doc(&right_data, right_text, &mut right_info)?;
        let diff = textdiff::diff_docs(&left, &right, &options);

        let first_line = if left.is_empty() { right.lines.first().map(|_| right.line(0)) } else { Some(left.line(0)) };
        let syntax = match chosen {
            View::Text => syntax
                .or_else(|| highlight::detect_syntax(&name, first_line.unwrap_or_default()).map(|s| s.name.clone())),
            View::PrettyXml => Some("XML".into()),
            View::PrettyJson => Some("JSON".into()),
            View::Image => None,
            _ => Some(highlight::view_syntax().name.clone()),
        };
        if note.is_none()
            && !identical
            && diff.sections.is_empty()
            && chosen == View::Text
            && let (Some(l), Some(r)) = (&left_data, &right_data)
        {
            note = Some(invisible_difference(&left, &right, l, r, diff.stats.minor));
        }
        let info = FileDiffInfo {
            id: 0,
            rows: diff.rows.len() as u32,
            sections: diff.sections.clone(),
            stats: diff.stats.clone(),
            left: left_info,
            right: right_info,
            syntax: syntax.clone(),
            identical,
            view: chosen,
            views: view_options,
            image,
            note,
        };
        Ok((info, FileDiff { left, right, diff, syntax, highlighters: None }))
    })
    .await?;
    let diff_id = state.id();
    state.diffs.lock().insert(diff_id, Arc::new(Mutex::new(file_diff)));
    Ok(FileDiffInfo { id: diff_id, ..info })
}

/// Longer lines are cut for display; the web view cannot lay out megabyte-long rows.
const MAX_SHOWN_LINE: u32 = 10_000;

#[derive(Serialize)]
pub struct LineView {
    /// 0-based line number.
    n: u32,
    t: String,
    /// Syntax colours.
    s: Vec<Span>,
    /// Changed character ranges within a changed line.
    m: Vec<(u32, u32)>,
    /// Number of UTF-16 units cut from the end of a very long line.
    #[serde(skip_serializing_if = "Option::is_none")]
    x: Option<u32>,
}

impl LineView {
    fn new(n: u32, line: &str, spans: Vec<Span>, marks: Vec<(u32, u32)>) -> Self {
        let total = text::utf16_len(line) as u32;
        if total <= MAX_SHOWN_LINE {
            return LineView { n, t: line.to_string(), s: spans, m: marks, x: None };
        }
        let mut units = 0u32;
        let cut = line
            .char_indices()
            .find(|(_, c)| {
                units += c.len_utf16() as u32;
                units > MAX_SHOWN_LINE
            })
            .map_or(line.len(), |(i, _)| i);
        let shown = text::utf16_len(&line[..cut]) as u32;
        let clip = |(a, b): (u32, u32)| (a < shown).then(|| (a, b.min(shown)));
        LineView {
            n,
            t: line[..cut].to_string(),
            s: spans.into_iter().filter_map(|Span(a, b, c)| clip((a, b)).map(|(a, b)| Span(a, b, c))).collect(),
            m: marks.into_iter().filter_map(clip).collect(),
            x: Some(total - shown),
        }
    }
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
        let view = |doc: &TextDoc, n: u32, base: usize, all: &Vec<Vec<Span>>, marks: Vec<(u32, u32)>| {
            LineView::new(n, doc.line(n as usize), all.get(n as usize - base).cloned().unwrap_or_default(), marks)
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
