//! Commands invoked by the web UI. Heavy work runs on blocking worker threads;
//! results are kept here and the UI fetches only what is visible.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use parallax_core::compare::{self, CompareOptions, CompareResult, Control, Excludes, Node};
use parallax_core::convert::{self, View};
use parallax_core::edit::{EditSession, Edited, Pos, Side};
use parallax_core::highlight::{self, DocHighlighter, Span, ThemeKind};
use parallax_core::ops::Removal;
use parallax_core::progress::Progress;
use parallax_core::search::{self, SearchOptions, SearchResult};
use parallax_core::source::{Source, SourceInfo, SourceOptions, SourceSpec, open_pair};
use parallax_core::text::{self, LineEnding, TextDoc};
use parallax_core::textdiff::{self, DiffOptions, DiffStats, RowKind, Section};
use parallax_core::vfs::archive::detect_format;
use parallax_core::vfs::git::{GitRepo, RefList};
use parallax_core::vfs::{Change, Data, EntryKind, split_parent};
use parallax_core::watch::{Watcher, WriteGuard};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

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
    /// The running copy or delete, for cancelling it.
    op: Mutex<Arc<Progress>>,
    /// Watches local sides for changes made outside the app.
    watcher: Mutex<Option<Watcher>>,
    /// The excludes of the last compare: changes there are not reported.
    excludes: Arc<RwLock<Excludes>>,
    exclude_patterns: Mutex<Vec<String>>,
}

impl Session {
    /// Marks `paths` as written by the app, so the watcher does not report them back.
    fn writing(&self, paths: &[String]) -> Option<WriteGuard> {
        self.watcher.lock().as_ref().map(|w| w.writing(paths))
    }
}

struct FileDiff {
    session_id: u64,
    session: Arc<Session>,
    path: String,
    edit: EditSession,
    syntax: Option<String>,
    highlighters: Option<(ThemeKind, DocHighlighter, DocHighlighter)>,
    /// Per side: the text can be edited and saved back.
    editable: [bool; 2],
    /// Per side: the file exists (a missing side is created on save).
    exists: [bool; 2],
}

impl FileDiff {
    fn source(&self, side: Side) -> &Source {
        match side {
            Side::Left => &self.session.left,
            Side::Right => &self.session.right,
        }
    }

    /// Highlighting is stale from the edited line on.
    fn edited(&mut self, edited: Edited) {
        if let Some((_, left, right)) = &mut self.highlighters {
            match edited.side {
                Side::Left => left.invalidate_from(edited.line),
                Side::Right => right.invalidate_from(edited.line),
            }
        }
    }
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

/// Reads a path typed into a path bar: `repo@rev:sub` is a Git revision.
#[tauri::command]
pub fn parse_source(text: String) -> SourceSpec {
    SourceSpec::parse(&text)
}

/// The source of the entry at `rel` on one side of a comparison, so a file opened
/// from a folder comparison can be compared on its own.
#[tauri::command]
pub async fn side_source(state: State<'_, AppState>, id: u64, left: bool, rel: String) -> CmdResult<SourceSpec> {
    let session = state.session(id)?;
    blocking(move || {
        let side = if left { &session.left } else { &session.right };
        side.spec.child(&rel).map_err(err)
    })
    .await
}

/// Completions for a path being typed in a path field.
#[tauri::command]
pub async fn complete_path(text: String) -> CmdResult<Vec<String>> {
    blocking(move || Ok(parallax_core::source::complete(&text, 40))).await
}

/// The folder containing a source, for comparing one level up.
#[tauri::command]
pub async fn source_parent(spec: SourceSpec) -> CmdResult<Option<SourceSpec>> {
    blocking(move || Ok(spec.parent())).await
}

/// Opens a source with the system's default app, or with `reveal` shows it in the
/// file manager (for a path inside an archive: the archive).
#[tauri::command]
pub async fn open_source(spec: SourceSpec, reveal: bool) -> CmdResult<()> {
    blocking(move || {
        if reveal {
            let path =
                spec.container_path().ok_or("only files and folders on disk can be shown in the file manager")?;
            tauri_plugin_opener::reveal_item_in_dir(path).map_err(err)
        } else {
            let path = spec.disk_path().ok_or("only files and folders on disk can be opened in another app")?;
            tauri_plugin_opener::open_path(path, None::<&str>).map_err(err)
        }
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistory {
    /// The file in the working tree, when the repository has one.
    worktree: Option<SourceSpec>,
    /// Each commit that changed the file, newest first, with the file as of that commit.
    commits: Vec<HistoryEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    #[serde(flatten)]
    commit: parallax_core::vfs::git::CommitInfo,
    spec: SourceSpec,
}

/// The commits that changed the file a source points to, starting from its
/// revision (or HEAD for a file in the working tree).
#[tauri::command]
pub async fn file_history(spec: SourceSpec) -> CmdResult<FileHistory> {
    blocking(move || {
        let at = spec.git_location().map_err(err)?;
        let git = GitRepo::discover(&at.repo).map_err(err)?;
        // History of the branch or tag the side is on; for a commit picked from the
        // history, or the working tree, of HEAD, so newer commits stay listed.
        let refs = git.refs(0).map_err(err)?;
        let named = |rev: &str| refs.branches.iter().chain(&refs.tags).chain(&refs.remotes).any(|r| r.name == rev);
        let start = at.rev.as_deref().filter(|r| named(r)).unwrap_or("HEAD");
        let commits = git.file_log(start, &at.path, 300, 50_000).map_err(err)?;
        let worktree = git.workdir().map(|w| SourceSpec::Local { path: w.join(&at.path).display().to_string() });
        let commits = commits
            .into_iter()
            .map(|commit| {
                let spec = SourceSpec::GitRev {
                    repo: at.repo.clone(),
                    rev: commit.short_id.clone(),
                    subpath: at.path.clone(),
                };
                HistoryEntry { commit, spec }
            })
            .collect();
        Ok(FileHistory { worktree, commits })
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
    app: AppHandle,
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
    let excludes = Excludes::new(&CompareOptions::default().excludes).map_err(err)?;
    let session = Arc::new(Session {
        left: l,
        right: r,
        control: Mutex::default(),
        op: Mutex::default(),
        watcher: Mutex::default(),
        excludes: Arc::new(RwLock::new(excludes)),
        exclude_patterns: Mutex::new(CompareOptions::default().excludes),
    });
    state.sessions.lock().insert(id, session.clone());
    start_watching(app, id, &session);
    Ok(info)
}

/// Starts watching the local sides in the background: on a large tree, setting up
/// the watches takes a moment, and the comparison need not wait for it.
fn start_watching(app: AppHandle, id: u64, session: &Arc<Session>) {
    let roots = [session.left.watch.clone(), session.right.watch.clone()];
    if roots.iter().all(Option::is_none) {
        return;
    }
    let weak = Arc::downgrade(session);
    let excludes = session.excludes.clone();
    std::thread::spawn(move || {
        let ignore = Arc::new(move |rel: &str| excludes.read().covers(rel));
        let emit = move |batch: parallax_core::watch::Batch| {
            let changed = PathsChanged { session: id, paths: batch.paths, full: batch.full, origin: None };
            let _ = app.emit("paths-changed", changed);
        };
        match Watcher::new(&roots, ignore, emit) {
            Ok(watcher) => {
                // The session may have been closed meanwhile.
                if let Some(session) = weak.upgrade() {
                    *session.watcher.lock() = Some(watcher);
                }
            }
            // Watching is a convenience: without it, F5 still compares again.
            Err(e) => eprintln!("not watching for changes: {e}"),
        }
    });
}

/// Commands without `async` run on the main thread, where any delay freezes the
/// window; closing frees a lot (watches on a big tree, the opened sources), so
/// it happens on a worker thread.
#[tauri::command]
pub async fn close_session(state: State<'_, AppState>, id: u64) -> CmdResult<()> {
    let session = state.sessions.lock().remove(&id);
    if let Some(session) = session {
        session.control.lock().cancel.store(true, Ordering::Relaxed);
        blocking(move || {
            session.watcher.lock().take();
            drop(session);
            Ok(())
        })
        .await?;
    }
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompareProgress {
    session: u64,
    files: u64,
}

#[tauri::command]
pub async fn run_compare(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    options: CompareOptions,
    invalidate: bool,
) -> CmdResult<CompareResult> {
    let session = state.session(id)?;
    if *session.exclude_patterns.lock() != options.excludes
        && let Ok(excludes) = Excludes::new(&options.excludes)
    {
        *session.excludes.write() = excludes;
        *session.exclude_patterns.lock() = options.excludes.clone();
        // On Linux, which folders are watched depends on the excludes.
        start_watching(app.clone(), id, &session);
    }
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
                let _ = app.emit("compare-progress", CompareProgress { session: id, files });
            }
        });
    }
    let result = blocking(move || {
        // A refresh: archives and folder snapshots may be out of date.
        if invalidate {
            session.left.vfs.changed(&session.left.path(""));
            session.right.vfs.changed(&session.right.path(""));
        }
        compare::compare(&session.left, &session.right, &options, &control).map_err(err)
    })
    .await;
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

/// The result of a copy or delete: the refreshed rows, also after a failure or a
/// cancel, since part of the work may be done.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpResult {
    /// Per path: the node, or `None` when it is gone from both sides.
    nodes: Vec<Option<Node>>,
    error: Option<String>,
    /// Paths that could not be moved to the trash and are still there.
    not_trashed: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpProgress {
    session: u64,
    done: u64,
    total: u64,
}

/// Runs a copy or delete with progress events and a cancel handle.
async fn run_op(
    app: AppHandle,
    session_id: u64,
    session: Arc<Session>,
    paths: Vec<String>,
    options: CompareOptions,
    op: impl FnOnce(&Session, &[String], &Progress) -> parallax_core::Result<Vec<String>> + Send + 'static,
) -> CmdResult<OpResult> {
    let progress = Arc::new(Progress::default());
    *session.op.lock() = progress.clone();
    let done = Arc::new(AtomicBool::new(false));
    {
        let (done, progress) = (done.clone(), progress.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(150));
                let (d, t) = (progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed));
                let _ = app.emit("op-progress", OpProgress { session: session_id, done: d, total: t.max(d) });
            }
        });
    }
    let result = blocking(move || {
        let guard = session.writing(&paths);
        let outcome = op(&session, &paths, &progress);
        drop(guard);
        let (error, not_trashed) = match outcome {
            Ok(not_trashed) => (None, not_trashed),
            Err(e) => (Some(err(e)), Vec::new()),
        };
        Ok(OpResult { nodes: refresh(&session, &paths, &options), error, not_trashed })
    })
    .await;
    done.store(true, Ordering::Relaxed);
    result
}

/// Compares `paths` again; `None` for a path that is gone from both sides.
fn refresh(session: &Session, paths: &[String], options: &CompareOptions) -> Vec<Option<Node>> {
    paths
        .iter()
        .map(|path| compare::compare_path(&session.left, &session.right, path, options, &Control::default()).ok())
        .map(|r| r.map(|r| r.root))
        .collect()
}

/// Copies `paths` to the other side.
#[tauri::command]
pub async fn copy_entries(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    paths: Vec<String>,
    to_right: bool,
    options: CompareOptions,
) -> CmdResult<OpResult> {
    let session = state.session(id)?;
    run_op(app, id, session, paths, options, move |s, paths, progress| {
        let (from, to) = if to_right { (&s.left, &s.right) } else { (&s.right, &s.left) };
        parallax_core::ops::copy(from, to, paths, progress).map(|_| Vec::new())
    })
    .await
}

/// Deletes `paths` on one side: into the trash, or for good with `permanent`.
#[tauri::command]
pub async fn delete_entries(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    paths: Vec<String>,
    left: bool,
    permanent: bool,
    options: CompareOptions,
) -> CmdResult<OpResult> {
    let session = state.session(id)?;
    let removal = if permanent { Removal::Permanent } else { Removal::Trash };
    run_op(app, id, session, paths, options, move |s, paths, progress| {
        parallax_core::ops::delete(if left { &s.left } else { &s.right }, paths, removal, progress)
    })
    .await
}

#[tauri::command]
pub fn cancel_op(state: State<'_, AppState>, id: u64) -> CmdResult<()> {
    state.session(id)?.op.lock().cancel.store(true, Ordering::Relaxed);
    Ok(())
}

/// Compares `paths` again after they changed outside the app (or in a file tab).
#[tauri::command]
pub async fn refresh_paths(
    state: State<'_, AppState>,
    id: u64,
    paths: Vec<String>,
    options: CompareOptions,
) -> CmdResult<Vec<Option<Node>>> {
    let session = state.session(id)?;
    blocking(move || {
        for path in &paths {
            session.left.vfs.changed(&session.left.path(path));
            session.right.vfs.changed(&session.right.path(path));
        }
        Ok(refresh(&session, &paths, &options))
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
    editable: bool,
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
        editable: false,
    }
}

/// The file's bytes as read: a memory map for large local files, so that views of
/// a header (GGUF models) touch only the start of a multi-gigabyte file.
fn load_side(source: &Source, path: &str) -> CmdResult<(SideInfo, Option<Data>)> {
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
    Ok((info, Some(data)))
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
        let lines = if minor == 1 { "line" } else { "lines" };
        reasons.push(format!("{minor} {lines} with whitespace or case differences that the current rules ignore"));
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
        let identical = left_data.is_some() && left_data.as_deref() == right_data.as_deref();
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
        let render = |data: &Option<Data>, view: View| -> Option<Result<String, String>> {
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

        let make_doc = |data: &Option<Data>,
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
        let mut left = make_doc(&left_data, left_text, &mut left_info)?;
        let mut right = make_doc(&right_data, right_text, &mut right_info)?;
        // Plain text can be edited where the source can be written and saving
        // would give back the same bytes for unedited text.
        let editable = |source: &Source, data: &Option<Data>, doc: &TextDoc, other: &Option<Data>| {
            chosen == View::Text
                && source.info.writable
                && source.vfs.writable()
                && match data {
                    Some(bytes) => !text::is_binary(bytes) && doc.encode().is_ok_and(|e| e[..] == bytes[..]),
                    None => other.as_ref().is_some_and(|o| !text::is_binary(o)),
                }
        };
        let editable = [
            editable(&session.left, &left_data, &left, &right_data),
            editable(&session.right, &right_data, &right, &left_data),
        ];
        // A missing side is created like the other file: encoding, BOM, line endings.
        if left_data.is_none() {
            left = TextDoc::empty_like(&right);
        } else if right_data.is_none() {
            right = TextDoc::empty_like(&left);
        }
        let edit = EditSession::new(left, right, options);
        let diff = edit.diff();
        let (left, right) = (edit.doc(Side::Left), edit.doc(Side::Right));

        let first_line = if left.is_empty() { right.lines.first().map(|_| right.line(0)) } else { Some(left.line(0)) };
        let syntax = match chosen {
            View::Text => syntax
                .or_else(|| highlight::detect_syntax(&name, first_line.unwrap_or_default()).map(|s| s.name.clone())),
            View::PrettyXml | View::AndroidXml => Some("XML".into()),
            View::PrettyJson => Some("JSON".into()),
            View::NormalizedYaml => Some("YAML".into()),
            View::NormalizedToml => Some("TOML".into()),
            View::ReflowedText => syntax.or_else(|| Some("reStructuredText".into())),
            View::TemplateOutline => Some("FreeMarker".into()),
            View::ChatTemplate => syntax.or_else(|| Some("Jinja2".into())),
            View::Image => None,
            _ => Some(highlight::view_syntax().name.clone()),
        };
        if note.is_none()
            && !identical
            && diff.sections.is_empty()
            && chosen == View::Text
            && let (Some(l), Some(r)) = (&left_data, &right_data)
        {
            note = Some(invisible_difference(left, right, l, r, diff.stats.minor));
        }
        left_info.editable = editable[0];
        right_info.editable = editable[1];
        let exists = [left_data.is_some(), right_data.is_some()];
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
        Ok((
            info,
            FileDiff {
                session_id: id,
                session: session.clone(),
                path,
                edit,
                syntax,
                highlighters: None,
                editable,
                exists,
            },
        ))
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
    /// The file's last line, without the terminator the other file has.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    nn: bool,
}

impl LineView {
    fn new(n: u32, line: &str, spans: Vec<Span>, marks: Vec<(u32, u32)>) -> Self {
        let total = text::utf16_len(line) as u32;
        if total <= MAX_SHOWN_LINE {
            return LineView { n, t: line.to_string(), s: spans, m: marks, x: None, nn: false };
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
            nn: false,
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
        let (doc_left, doc_right) = (fd.edit.doc(Side::Left), fd.edit.doc(Side::Right));
        let all_rows = &fd.edit.diff().rows;
        let end = (start as usize + count as usize).min(all_rows.len());
        let rows = &all_rows[(start as usize).min(end)..end];
        if fd.highlighters.as_ref().is_none_or(|(t, _, _)| *t != theme) {
            let syntax = fd.syntax.as_deref().and_then(highlight::find_syntax);
            fd.highlighters = Some((theme, DocHighlighter::new(syntax, theme), DocHighlighter::new(syntax, theme)));
        }
        let (_, hl_left, hl_right) = fd.highlighters.as_mut().expect("highlighters set above");
        let spans = |hl: &mut DocHighlighter, doc: &TextDoc, range: Option<(usize, usize)>| match range {
            Some((a, b)) => (a, hl.highlight(doc, a, b)),
            None => (0, Vec::new()),
        };
        let (left_base, left_spans) = spans(hl_left, doc_left, line_range(rows, |r| r.left));
        let (right_base, right_spans) = spans(hl_right, doc_right, line_range(rows, |r| r.right));
        // A last line whose terminator only the other side has.
        let no_newline = |doc: &TextDoc, other: &TextDoc, n: u32| {
            n as usize + 1 == doc.len() && !doc.ends_with_newline() && other.ends_with_newline()
        };
        let view = |doc: &TextDoc, n: u32, base: usize, all: &Vec<Vec<Span>>, marks: Vec<(u32, u32)>| {
            LineView::new(n, doc.line(n as usize), all.get(n as usize - base).cloned().unwrap_or_default(), marks)
        };
        Ok(rows
            .iter()
            .map(|row| {
                let (lm, rm) = match (row.kind, row.left, row.right) {
                    (RowKind::Changed, Some(l), Some(r)) => {
                        let (a, b) = textdiff::intraline(doc_left.line(l as usize), doc_right.line(r as usize));
                        (
                            a.into_iter().map(|r| (r.start, r.end)).collect(),
                            b.into_iter().map(|r| (r.start, r.end)).collect(),
                        )
                    }
                    _ => (Vec::new(), Vec::new()),
                };
                let line = |doc: &TextDoc, other: &TextDoc, n: u32, base, spans, marks| LineView {
                    nn: no_newline(doc, other, n),
                    ..view(doc, n, base, spans, marks)
                };
                RowView {
                    k: row.kind,
                    l: row.left.map(|n| line(doc_left, doc_right, n, left_base, &left_spans, lm)),
                    r: row.right.map(|n| line(doc_right, doc_left, n, right_base, &right_spans, rm)),
                }
            })
            .collect())
    })
    .await
}

/// Matches in both sides of a file comparison, as it is now (with unsaved edits).
#[tauri::command]
pub async fn search_diff(
    state: State<'_, AppState>,
    id: u64,
    query: String,
    options: SearchOptions,
) -> CmdResult<SearchResult> {
    let diff = state.diff(id)?;
    blocking(move || {
        let re = search::pattern(&query, &options)?;
        let fd = diff.lock();
        let docs = [fd.edit.doc(Side::Left), fd.edit.doc(Side::Right)];
        Ok(search::search(&fd.edit.diff().rows, docs, &re, 20_000))
    })
    .await
}

/// Frees both texts and the diff on a worker thread: for files of millions of
/// lines that takes long enough to stall the window.
#[tauri::command]
pub async fn close_file_diff(state: State<'_, AppState>, id: u64) -> CmdResult<()> {
    let diff = state.diffs.lock().remove(&id);
    blocking(move || {
        drop(diff);
        Ok(())
    })
    .await
}

/// The first call loads the syntax definitions, so it runs on a worker thread.
#[tauri::command]
pub async fn syntax_names() -> CmdResult<Vec<String>> {
    blocking(|| Ok(highlight::syntax_names())).await
}

/// The state of a file comparison after an edit, undo or save.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditInfo {
    rows: u32,
    sections: Vec<Section>,
    stats: DiffStats,
    lines: [u32; 2],
    dirty: [bool; 2],
    exists: [bool; 2],
    can_undo: bool,
    can_redo: bool,
    identical: bool,
    /// The row where the edit happened, to bring it into view.
    focus_row: Option<u32>,
    /// Where typing continues after an edit from the line editor.
    caret: Option<Caret>,
}

#[derive(Serialize)]
pub struct Caret {
    row: u32,
    line: u32,
    col: u32,
}

const SIDES: [Side; 2] = [Side::Left, Side::Right];

fn edit_info(fd: &FileDiff, focus: Option<Edited>, caret: Option<(Side, usize)>) -> EditInfo {
    let e = &fd.edit;
    let diff = e.diff();
    let caret = caret.map(|(side, at)| {
        let doc = e.doc(side);
        let line = doc.lines.partition_point(|r| r.end < at).min(doc.len().saturating_sub(1));
        let start = doc.lines.get(line).map_or(0, |r| r.start);
        let col = text::utf16_len(&doc.text[start..at.clamp(start, doc.text.len())]) as u32;
        Caret { row: e.row_of_line(side, line), line: line as u32, col }
    });
    EditInfo {
        rows: diff.rows.len() as u32,
        sections: diff.sections.clone(),
        stats: diff.stats.clone(),
        lines: SIDES.map(|s| e.doc(s).len() as u32),
        dirty: SIDES.map(|s| e.is_dirty(s)),
        exists: fd.exists,
        can_undo: e.can_undo(),
        can_redo: e.can_redo(),
        identical: e.doc(Side::Left).text == e.doc(Side::Right).text,
        focus_row: focus.map(|f| e.row_of_line(f.side, f.line)),
        caret,
    }
}

/// Runs an edit on a file comparison and reports the new state.
async fn with_diff(
    state: State<'_, AppState>,
    id: u64,
    f: impl FnOnce(&mut FileDiff) -> CmdResult<(Option<Edited>, Option<(Side, usize)>)> + Send + 'static,
) -> CmdResult<EditInfo> {
    let diff = state.diff(id)?;
    blocking(move || {
        let mut fd = diff.lock();
        let (edited, caret) = f(&mut fd)?;
        if let Some(edited) = edited {
            fd.edited(edited);
        }
        Ok(edit_info(&fd, edited, caret))
    })
    .await
}

fn check_editable(fd: &FileDiff, side: Side) -> CmdResult<()> {
    if fd.editable[side as usize] {
        Ok(())
    } else {
        Err(format!("The {} side cannot be edited here.", if side == Side::Left { "left" } else { "right" }))
    }
}

/// Replaces text between two positions; `caret` counts UTF-16 units into `text`
/// and says where typing continues.
#[tauri::command]
pub async fn edit_text(
    state: State<'_, AppState>,
    id: u64,
    side: Side,
    from: Pos,
    to: Pos,
    text: String,
    caret: Option<u32>,
) -> CmdResult<EditInfo> {
    with_diff(state, id, move |fd| {
        check_editable(fd, side)?;
        let doc = fd.edit.doc(side);
        let start = doc.offset(from.line as usize, from.col);
        // The caret's byte offset in the inserted text, whose line breaks become the file's.
        let caret = caret.map(|units| {
            let mut count = 0u32;
            let cut = text.char_indices().find(|(_, c)| {
                count += c.len_utf16() as u32;
                count > units
            });
            let prefix = &text[..cut.map_or(text.len(), |(i, _)| i)];
            let prefix = prefix.replace("\r\n", "\n").replace('\r', "\n");
            start + prefix.len() + prefix.matches('\n').count() * (doc.newline.len() - 1)
        });
        let edited = fd.edit.replace(side, from, to, &text);
        Ok((Some(edited), caret.map(|c| (side, c))))
    })
    .await
}

/// Types a new line where `side` shows a filler at `row`.
#[tauri::command]
pub async fn insert_line(
    state: State<'_, AppState>,
    id: u64,
    side: Side,
    row: u32,
    text: String,
) -> CmdResult<EditInfo> {
    with_diff(state, id, move |fd| {
        check_editable(fd, side)?;
        let line = fd.edit.line_at_row(side, row);
        let edited = fd.edit.insert_line(side, row, &text);
        // Typing continues at the end of the inserted text, which may span lines.
        let last = text.rsplit(['\n', '\r']).next().unwrap_or_default();
        let caret_line = line + text.split('\n').count() - 1;
        let doc = fd.edit.doc(side);
        let caret = doc.lines.get(caret_line).map_or(doc.text.len(), |r| r.start + last.len());
        Ok((Some(edited), Some((side, caret))))
    })
    .await
}

/// Copies the lines of `rows` (end exclusive) to the `to` side.
#[tauri::command]
pub async fn copy_rows(state: State<'_, AppState>, id: u64, start: u32, end: u32, to: Side) -> CmdResult<EditInfo> {
    with_diff(state, id, move |fd| {
        check_editable(fd, to)?;
        Ok((fd.edit.copy_rows(start..end, to), None))
    })
    .await
}

#[tauri::command]
pub async fn undo(state: State<'_, AppState>, id: u64) -> CmdResult<EditInfo> {
    with_diff(state, id, |fd| Ok((fd.edit.undo(), None))).await
}

#[tauri::command]
pub async fn redo(state: State<'_, AppState>, id: u64) -> CmdResult<EditInfo> {
    with_diff(state, id, |fd| Ok((fd.edit.redo(), None))).await
}

/// Diffs again with other rules or highlights with another grammar, keeping edits.
#[tauri::command]
pub async fn update_diff(
    state: State<'_, AppState>,
    id: u64,
    options: DiffOptions,
    syntax: Option<String>,
) -> CmdResult<EditInfo> {
    with_diff(state, id, move |fd| {
        fd.edit.set_options(options);
        if let Some(syntax) = syntax
            && fd.syntax.as_deref() != Some(&syntax)
        {
            fd.syntax = Some(syntax);
            fd.highlighters = None;
        }
        Ok((None, None))
    })
    .await
}

/// Writes one side back to its file (or into its archive).
#[tauri::command]
pub async fn save_side(app: AppHandle, state: State<'_, AppState>, id: u64, side: Side) -> CmdResult<EditInfo> {
    let info = with_diff(state, id, move |fd| {
        check_editable(fd, side)?;
        let bytes = fd.edit.doc(side).encode().map_err(|e| format!("Cannot save: {e}."))?;
        let source = fd.source(side);
        let _guard = fd.session.writing(std::slice::from_ref(&fd.path));
        let change = Change::Put {
            path: source.path(&fd.path),
            kind: EntryKind::File,
            data: Data::from_vec(bytes),
            mtime: None,
        };
        source.vfs.apply(vec![change], &Progress::default()).map_err(err)?;
        fd.edit.mark_saved(side);
        fd.exists[side as usize] = true;
        Ok((None, None))
    })
    .await?;
    if let Some(fd) = app.state::<AppState>().diffs.lock().get(&id) {
        let fd = fd.lock();
        let changed =
            PathsChanged { session: fd.session_id, paths: vec![fd.path.clone()], full: false, origin: Some(id) };
        let _ = app.emit("paths-changed", changed);
    }
    Ok(info)
}

/// Paths of a comparison that changed on disk or were saved from a file tab.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathsChanged {
    pub session: u64,
    pub paths: Vec<String>,
    /// Too much changed, or the root itself: compare everything again.
    pub full: bool,
    /// The file comparison whose save this was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<u64>,
}
