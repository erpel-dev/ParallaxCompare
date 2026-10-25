//! Commands of the merge view and of the conflicted-files list.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use parallax_core::conflicts::{self, ConflictKind, RepoConflicts, Resolution};
use parallax_core::edit::Pos;
use parallax_core::highlight::{self, DocHighlighter, ThemeKind};
use parallax_core::merge::{BlockKind, MarkerStyle, MergeOptions};
use parallax_core::merge_session::{Choice, ConflictInfo, MergeSession, MergeStats};
use parallax_core::source::{SourceOptions, SourceSpec};
use parallax_core::text::{self, TextDoc};
use parallax_core::textdiff;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::commands::{AppState, CmdResult, LineView, blocking, err};

/// The files of a merge given on the command line, as `git mergetool` passes them.
#[derive(Debug, Clone, Default)]
pub struct MergeArgs {
    pub base: String,
    pub local: String,
    pub remote: String,
    pub output: String,
}

pub struct MergeTab {
    session: MergeSession,
    output: PathBuf,
    labels: [String; 3],
    syntax: Option<String>,
    /// Base, local, remote, result.
    highlighters: Option<(ThemeKind, [DocHighlighter; 4])>,
    /// The file as Git wrote it into the work tree, when it differs from this merge.
    git_result: Option<String>,
    /// The merge `git mergetool` started the app for: saving decides the exit status.
    startup: bool,
    /// A conflicted file of a repository: the work tree and the path in it.
    repo: Option<(PathBuf, String)>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum MergeRequest {
    /// Three sources and the file to write.
    #[serde(rename_all = "camelCase")]
    Sources { base: Option<SourceSpec>, local: SourceSpec, remote: SourceSpec, output: String },
    /// A conflicted file of a repository, read from the index.
    #[serde(rename_all = "camelCase")]
    Repo { workdir: String, path: String },
    /// The merge given on the command line.
    Startup,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeInfo {
    id: u64,
    title: String,
    output: String,
    labels: [String; 3],
    syntax: Option<String>,
    input_rows: u32,
    result_rows: u32,
    stats: MergeStats,
    conflicts: Vec<ConflictInfo>,
    /// First input row and first result row of each block.
    block_rows: Vec<(u32, u32)>,
    dirty: bool,
    clean: bool,
    can_undo: bool,
    can_redo: bool,
    /// Git's own result is in the work tree and differs from this merge.
    git_result: bool,
    startup: bool,
    repo: Option<(String, String)>,
    encoding: &'static str,
    /// The first result line an edit changed, for placing the caret.
    #[serde(skip_serializing_if = "Option::is_none")]
    changed: Option<u32>,
}

impl MergeTab {
    fn info(&self, id: u64, changed: Option<usize>) -> MergeInfo {
        let s = &self.session;
        MergeInfo {
            id,
            title: self.output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            output: self.output.display().to_string(),
            labels: self.labels.clone(),
            syntax: self.syntax.clone(),
            input_rows: s.input_row_count(),
            result_rows: s.result_row_count(),
            stats: s.stats(),
            conflicts: s.conflicts(),
            block_rows: s.block_rows(),
            dirty: s.is_dirty(),
            clean: s.is_clean(),
            can_undo: s.can_undo(),
            can_redo: s.can_redo(),
            git_result: self.git_result.is_some(),
            startup: self.startup,
            repo: self.repo.as_ref().map(|(w, p)| (w.display().to_string(), p.clone())),
            encoding: s.result().encoding,
            changed: changed.map(|l| l as u32),
        }
    }

    fn edited(&mut self, line: Option<usize>) {
        if let (Some((_, hl)), Some(line)) = (&mut self.highlighters, line) {
            hl[3].invalidate_from(line);
        }
    }
}

fn read_source(spec: &SourceSpec) -> CmdResult<(Vec<u8>, String)> {
    let source = spec.open(SourceOptions::default()).map_err(err)?;
    if !source.is_file().map_err(err)? {
        return Err(format!("{} is not a file", source.info.display));
    }
    let data = source.vfs.read(&source.root).map_err(err)?.to_vec();
    Ok((data, source.info.display.clone()))
}

fn read_file(path: &str) -> CmdResult<Vec<u8>> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

fn text_or_error(name: &str, data: &[u8]) -> CmdResult<TextDoc> {
    if text::is_binary(data) {
        return Err(format!("The {name} version is binary; binary files are resolved by choosing a whole version."));
    }
    Ok(text::decode(data))
}

/// Labels and Git's result for an output file that is a conflicted file of a repository.
fn repo_context(output: &Path) -> Option<(RepoConflicts, String)> {
    let dir = output.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let state = conflicts::repo_conflicts(dir).ok()?;
    let absolute = std::fs::canonicalize(output).ok()?;
    let workdir = std::fs::canonicalize(&state.workdir).ok()?;
    let rel = absolute.strip_prefix(&workdir).ok()?.to_string_lossy().replace('\\', "/");
    state.files.iter().any(|f| f.path == rel).then_some((state, rel))
}

/// Whether two texts are the same, but for the labels after conflict markers:
/// Git names the sides after the refs it was given, this merge after branches.
fn same_but_labels(a: &str, b: &str) -> bool {
    let marker = |l: &str| ["<<<<<<<", "|||||||", ">>>>>>>"].iter().find(|m| l.starts_with(**m)).copied();
    a.split('\n').count() == b.split('\n').count()
        && a.split('\n').zip(b.split('\n')).all(|(x, y)| match (marker(x), marker(y)) {
            (Some(m), Some(n)) => m == n,
            _ => x == y,
        })
}

#[allow(clippy::too_many_arguments)]
fn new_tab(
    base: &[u8],
    local: &[u8],
    remote: &[u8],
    output: PathBuf,
    labels: [String; 3],
    git_result: Option<Vec<u8>>,
    startup: bool,
    repo: Option<(PathBuf, String)>,
) -> CmdResult<MergeTab> {
    let (b, l, r) = (text_or_error("base", base)?, text_or_error("local", local)?, text_or_error("remote", remote)?);
    let style = repo.as_ref().map_or(MarkerStyle::Merge, |(workdir, _)| conflicts::conflict_style(workdir));
    let options = MergeOptions { labels: labels.clone().map(Some), style, ..Default::default() };
    let session = MergeSession::new(b, l, r, options);
    let name = output.to_string_lossy().replace('\\', "/");
    let first_line = session.local.lines.first().map(|r| &session.local.text[r.clone()]).unwrap_or_default();
    let syntax = highlight::detect_syntax(&name, first_line).map(|s| s.name.clone());
    // Git's file, when it is not what this merge gives (a merge driver, rerere).
    let git_result = git_result
        .filter(|d| !text::is_binary(d))
        .map(|d| text::decode(&d).text)
        .filter(|t| !same_but_labels(t, session.result_text()));
    Ok(MergeTab { session, output, labels, syntax, highlighters: None, git_result, startup, repo })
}

#[tauri::command]
pub async fn open_merge(state: State<'_, AppState>, request: MergeRequest) -> CmdResult<MergeInfo> {
    let startup = if matches!(request, MergeRequest::Startup) { state.startup_merge.lock().clone() } else { None };
    let tab = blocking(move || match request {
        MergeRequest::Sources { base, local, remote, output } => {
            let output = PathBuf::from(&output);
            if output.as_os_str().is_empty() || output.is_dir() {
                return Err("Choose a file to write the result to.".into());
            }
            let (base, base_name) = match &base {
                Some(spec) => read_source(spec)?,
                None => (Vec::new(), "base (empty)".into()),
            };
            let (local, local_name) = read_source(&local)?;
            let (remote, remote_name) = read_source(&remote)?;
            new_tab(&base, &local, &remote, output, [local_name, base_name, remote_name], None, false, None)
        }
        MergeRequest::Repo { workdir, path } => {
            let state = conflicts::repo_conflicts(Path::new(&workdir)).map_err(err)?;
            let file =
                state.files.iter().find(|f| f.path == path).ok_or_else(|| format!("{path} is not conflicted"))?;
            let read = |i: usize| match &file.stages[i] {
                Some(stage) => conflicts::stage_content(&state.workdir, stage).map_err(err),
                None => Ok(Vec::new()),
            };
            if file.stages[1].is_none() || file.stages[2].is_none() {
                return Err(format!("{path} was deleted on one side; keep or delete it as a whole file."));
            }
            let output = state.workdir.join(&path);
            let git_result = std::fs::read(&output).ok();
            let labels = [state.labels.local.clone(), state.labels.base.clone(), state.labels.remote.clone()];
            new_tab(
                &read(0)?,
                &read(1)?,
                &read(2)?,
                output,
                labels,
                git_result,
                false,
                Some((state.workdir.clone(), path)),
            )
        }
        MergeRequest::Startup => {
            let args = startup.ok_or("No merge was given on the command line.")?;
            let output = PathBuf::from(&args.output);
            let (base, local, remote) = (read_file(&args.base)?, read_file(&args.local)?, read_file(&args.remote)?);
            let git_result = std::fs::read(&output).ok();
            let (labels, repo) = match repo_context(&output) {
                Some((state, rel)) => (
                    [state.labels.local.clone(), state.labels.base.clone(), state.labels.remote.clone()],
                    Some((state.workdir, rel)),
                ),
                None => (["local".into(), "base".into(), "remote".into()], None),
            };
            new_tab(&base, &local, &remote, output, labels, git_result, true, repo)
        }
    })
    .await?;
    let id = state.id();
    let info = tab.info(id, None);
    state.merges.lock().insert(id, Arc::new(Mutex::new(tab)));
    Ok(info)
}

fn merge(state: &State<'_, AppState>, id: u64) -> CmdResult<Arc<Mutex<MergeTab>>> {
    state.merges.lock().get(&id).cloned().ok_or_else(|| "merge was closed".to_string())
}

#[tauri::command]
pub fn close_merge(state: State<'_, AppState>, id: u64) {
    state.merges.lock().remove(&id);
}

#[tauri::command]
pub async fn merge_info(state: State<'_, AppState>, id: u64) -> CmdResult<MergeInfo> {
    let tab = merge(&state, id)?;
    let info = tab.lock().info(id, None);
    Ok(info)
}

#[derive(Serialize)]
pub struct InputRowView {
    k: BlockKind,
    i: u32,
    b: Option<LineView>,
    l: Option<LineView>,
    r: Option<LineView>,
}

#[derive(Serialize)]
pub struct ResultRowView {
    k: BlockKind,
    c: Choice,
    i: u32,
    /// A line; a conflict's header row has none.
    l: Option<LineView>,
}

/// The highlighters for the theme, and the session, borrowed side by side.
fn highlighters(tab: &mut MergeTab, theme: ThemeKind) -> (&mut [DocHighlighter; 4], &MergeSession) {
    if tab.highlighters.as_ref().is_none_or(|(t, _)| *t != theme) {
        let syntax = tab.syntax.as_deref().and_then(highlight::find_syntax);
        tab.highlighters = Some((theme, std::array::from_fn(|_| DocHighlighter::new(syntax, theme))));
    }
    (&mut tab.highlighters.as_mut().expect("set above").1, &tab.session)
}

/// Syntax colours for lines `first..last` of a document, as one batch.
fn spans(
    hl: &mut DocHighlighter,
    doc: &TextDoc,
    lines: impl Iterator<Item = u32> + Clone,
) -> (usize, Vec<Vec<highlight::Span>>) {
    let (Some(first), Some(last)) = (lines.clone().min(), lines.max()) else { return (0, Vec::new()) };
    (first as usize, hl.highlight(doc, first as usize, last as usize + 1))
}

#[tauri::command]
pub async fn merge_input_rows(
    state: State<'_, AppState>,
    id: u64,
    start: u32,
    count: u32,
    theme: ThemeKind,
) -> CmdResult<Vec<InputRowView>> {
    let tab = merge(&state, id)?;
    blocking(move || {
        let mut tab = tab.lock();
        let rows = tab.session.input_rows(start, count);
        let tab = &mut *tab;
        let (hl, s) = highlighters(tab, theme);
        let (bb, bs) = spans(&mut hl[0], &s.base, rows.iter().filter_map(|r| r.base));
        let (lb, ls) = spans(&mut hl[1], &s.local, rows.iter().filter_map(|r| r.local));
        let (rb, rs) = spans(&mut hl[2], &s.remote, rows.iter().filter_map(|r| r.remote));
        let line = |doc: &TextDoc, n: u32, first: usize, all: &Vec<Vec<highlight::Span>>, marks: Vec<(u32, u32)>| {
            LineView::new(n, doc.line(n as usize), all.get(n as usize - first).cloned().unwrap_or_default(), marks)
        };
        // Changed characters of a changed line against the base line beside it.
        let marks =
            |kind: BlockKind, side: &TextDoc, n: Option<u32>, base: Option<u32>, changed: bool| -> Vec<(u32, u32)> {
                match (n, base) {
                    (Some(n), Some(b)) if changed && kind != BlockKind::Unchanged => {
                        let (_, m) = textdiff::intraline(s.base.line(b as usize), side.line(n as usize));
                        m.into_iter().map(|r| (r.start, r.end)).collect()
                    }
                    _ => Vec::new(),
                }
            };
        Ok(rows
            .iter()
            .map(|row| {
                let local_changed = matches!(row.kind, BlockKind::Local | BlockKind::Same | BlockKind::Conflict);
                let remote_changed = matches!(row.kind, BlockKind::Remote | BlockKind::Same | BlockKind::Conflict);
                InputRowView {
                    k: row.kind,
                    i: row.block,
                    b: row.base.map(|n| line(&s.base, n, bb, &bs, Vec::new())),
                    l: row.local.map(|n| {
                        line(&s.local, n, lb, &ls, marks(row.kind, &s.local, Some(n), row.base, local_changed))
                    }),
                    r: row.remote.map(|n| {
                        line(&s.remote, n, rb, &rs, marks(row.kind, &s.remote, Some(n), row.base, remote_changed))
                    }),
                }
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub async fn merge_result_rows(
    state: State<'_, AppState>,
    id: u64,
    start: u32,
    count: u32,
    theme: ThemeKind,
) -> CmdResult<Vec<ResultRowView>> {
    let tab = merge(&state, id)?;
    blocking(move || {
        let mut tab = tab.lock();
        let rows = tab.session.result_rows(start, count);
        let tab = &mut *tab;
        let (hl, session) = highlighters(tab, theme);
        let doc = session.result();
        let (first, all) = spans(&mut hl[3], doc, rows.iter().filter_map(|r| r.line));
        Ok(rows
            .iter()
            .map(|row| ResultRowView {
                k: row.kind,
                c: row.choice,
                i: row.block,
                l: row.line.map(|n| {
                    // Between conflict markers the syntax colouring goes astray.
                    let colours = if row.choice == Choice::Unresolved {
                        Vec::new()
                    } else {
                        all.get(n as usize - first).cloned().unwrap_or_default()
                    };
                    LineView::new(n, doc.line(n as usize), colours, Vec::new())
                }),
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub async fn merge_resolve(state: State<'_, AppState>, id: u64, block: u32, choice: Choice) -> CmdResult<MergeInfo> {
    let tab = merge(&state, id)?;
    blocking(move || {
        let mut tab = tab.lock();
        let line = tab.session.resolve(block as usize, choice);
        tab.edited(line);
        Ok(tab.info(id, line))
    })
    .await
}

#[tauri::command]
pub async fn merge_edit(state: State<'_, AppState>, id: u64, from: Pos, to: Pos, text: String) -> CmdResult<MergeInfo> {
    let tab = merge(&state, id)?;
    blocking(move || {
        let mut tab = tab.lock();
        let line = tab.session.replace(from, to, &text);
        tab.edited(Some(line));
        Ok(tab.info(id, Some(line)))
    })
    .await
}

/// The text of result lines, for copying.
#[tauri::command]
pub async fn merge_result_text(state: State<'_, AppState>, id: u64, start: u32, end: u32) -> CmdResult<String> {
    let tab = merge(&state, id)?;
    let tab = tab.lock();
    let doc = tab.session.result();
    let (a, b) = (doc.line_start(start as usize), doc.line_start(end as usize));
    Ok(doc.text[a..b].to_string())
}

#[tauri::command]
pub async fn merge_undo(state: State<'_, AppState>, id: u64, redo: bool) -> CmdResult<MergeInfo> {
    let tab = merge(&state, id)?;
    blocking(move || {
        let mut tab = tab.lock();
        let line = if redo { tab.session.redo() } else { tab.session.undo() };
        // Undo can change any line count: highlight from the change on.
        tab.edited(line.or(Some(0)));
        Ok(tab.info(id, line))
    })
    .await
}

/// Replaces the result with the file Git wrote into the work tree.
#[tauri::command]
pub async fn merge_use_git_result(state: State<'_, AppState>, id: u64) -> CmdResult<MergeInfo> {
    let tab = merge(&state, id)?;
    blocking(move || {
        let mut tab = tab.lock();
        let text = tab.git_result.clone().ok_or("Git's result is the same as this merge.")?;
        tab.session.replace_all(&text);
        tab.edited(Some(0));
        Ok(tab.info(id, Some(0)))
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSaved {
    info: MergeInfo,
    /// Saved without conflicts.
    clean: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSavedEvent {
    workdir: String,
    path: String,
    clean: bool,
}

/// Writes the result. With conflicts left, only when `with_markers` is set: the
/// file then keeps Git's markers and the merge is not reported as resolved.
#[tauri::command]
pub async fn merge_save(
    app: AppHandle,
    state: State<'_, AppState>,
    id: u64,
    with_markers: bool,
) -> CmdResult<MergeSaved> {
    let tab = merge(&state, id)?;
    let saved = blocking(move || {
        let mut tab = tab.lock();
        let clean = tab.session.is_clean();
        if !clean && !with_markers {
            return Err("The result still has conflicts.".into());
        }
        let data = tab.session.result().encode().map_err(|e| format!("Cannot save: {e}."))?;
        parallax_core::vfs::local::replace_file(&tab.output, &data, None, None)
            .map_err(|e| format!("{}: {e}", tab.output.display()))?;
        tab.session.mark_saved();
        Ok((tab.info(id, None), clean, tab.startup, tab.repo.clone()))
    })
    .await?;
    let (info, clean, startup, repo) = saved;
    if startup {
        state.exit_code.store(if clean { 0 } else { 1 }, Ordering::Relaxed);
    }
    if let Some((workdir, path)) = repo {
        let _ = app.emit("merge-saved", MergeSavedEvent { workdir: workdir.display().to_string(), path, clean });
    }
    Ok(MergeSaved { info, clean })
}

// ---- conflicted files of a repository ----

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictFileView {
    path: String,
    kind: ConflictKind,
    /// Both versions are text, so it can be merged line by line.
    text: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoConflictsView {
    workdir: String,
    summary: String,
    labels: conflicts::Labels,
    files: Vec<ConflictFileView>,
}

#[tauri::command]
pub async fn repo_conflicts(dir: String) -> CmdResult<RepoConflictsView> {
    blocking(move || {
        let state = conflicts::repo_conflicts(Path::new(&dir)).map_err(err)?;
        let files = state
            .files
            .iter()
            .map(|f| {
                let text = matches!(f.kind, ConflictKind::BothModified | ConflictKind::BothAdded)
                    && f.stages
                        .iter()
                        .flatten()
                        .all(|s| conflicts::stage_content(&state.workdir, s).is_ok_and(|d| !text::is_binary(&d)));
                ConflictFileView { path: f.path.clone(), kind: f.kind, text }
            })
            .collect();
        Ok(RepoConflictsView {
            workdir: state.workdir.display().to_string(),
            summary: state.summary,
            labels: state.labels,
            files,
        })
    })
    .await
}

/// Resolves a conflicted file as a whole: the local or the remote version goes
/// into the work tree, or the file is removed when that side deleted it. Returns
/// whether the file was deleted.
#[tauri::command]
pub async fn take_conflict_side(workdir: String, path: String, remote: bool) -> CmdResult<bool> {
    blocking(move || {
        let state = conflicts::repo_conflicts(Path::new(&workdir)).map_err(err)?;
        let file = state.files.iter().find(|f| f.path == path).ok_or_else(|| format!("{path} is not conflicted"))?;
        let target = state.workdir.join(&file.path);
        match &file.stages[if remote { 2 } else { 1 }] {
            Some(stage) => {
                let data = conflicts::stage_content(&state.workdir, stage).map_err(err)?;
                parallax_core::vfs::local::replace_file(&target, &data, None, None).map_err(err)?;
                Ok(false)
            }
            None => {
                if target.exists() {
                    std::fs::remove_file(&target).map_err(err)?;
                }
                Ok(true)
            }
        }
    })
    .await
}

/// Stages resolved files: `git add`, or `git rm` for those resolved by deleting.
#[tauri::command]
pub async fn stage_conflicts(workdir: String, paths: Vec<(String, bool)>) -> CmdResult<()> {
    blocking(move || {
        let paths: Vec<(String, Resolution)> = paths
            .into_iter()
            .map(|(p, delete)| (p, if delete { Resolution::Delete } else { Resolution::WorkTree }))
            .collect();
        conflicts::stage(Path::new(&workdir), &paths).map_err(err)
    })
    .await
}
