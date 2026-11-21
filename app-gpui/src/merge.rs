//! A three-way merge of one file, through the same core calls as the Tauri
//! app's `app/src-tauri/src/merge.rs`: the core's [`MergeSession`] merges and
//! resolves, [`parallax_core::rows`] builds the rows of both panes, and
//! [`conflicts::merge_inputs`] reads a conflicted file of a repository.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use parallax_core::conflicts::{self, ConflictKind, FileMode, Resolution};
use parallax_core::edit::Pos;
use parallax_core::highlight::{self, DocHighlighter, ThemeKind};
use parallax_core::merge::{MarkerStyle, MergeOptions};
use parallax_core::merge_session::{self, Choice, ConflictInfo, MergeSession, MergeStats, SyntaxOffer};
use parallax_core::rows::{self, MergeInputRow, MergeResultRow};
use parallax_core::syntax_merge::Engine;
use parallax_core::text::{self, TextDoc};

/// Where a merge comes from.
#[derive(Clone, Debug)]
pub enum MergeSource {
    /// Three files and the file to write, as `git mergetool` passes them. An
    /// empty base means there is none (both sides added the file).
    Files { base: String, local: String, remote: String, output: String },
    /// A conflicted file of a repository, read from the index.
    Repo { workdir: PathBuf, path: String },
}

pub struct MergeDoc {
    pub session: MergeSession,
    pub output: PathBuf,
    /// Local, base, remote.
    pub labels: [String; 3],
    pub syntax: Option<String>,
    /// Git's file in the work tree, when it differs from this merge.
    pub git_result: Option<String>,
    /// A conflicted file of a repository: the work tree and the path in it.
    pub repo: Option<(PathBuf, String)>,
    /// The syntax-aware merge's result, when it is offered only as a whole.
    pub syntax_whole: Option<String>,
    on_disk: Option<u64>,
    highlighters: [DocHighlighter; 4],
    theme: ThemeKind,
}

fn stamp(data: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

fn disk_stamp(path: &Path) -> Option<u64> {
    std::fs::read(path).ok().map(|d| stamp(&d))
}

fn read_file(path: &str) -> Result<Vec<u8>, String> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::read(path).map_err(|e| format!("{path}: {e}"))
}

fn text_or_error(name: &str, data: &[u8]) -> Result<TextDoc, String> {
    if text::is_binary(data) {
        return Err(format!("The {name} version is binary; binary files are resolved by choosing a whole version."));
    }
    Ok(text::decode(data))
}

#[derive(Debug)]
pub enum SaveError {
    /// Conflicts are left and saving with markers was not asked for.
    Conflicts,
    /// The file changed on disk since the merge opened or was last saved.
    Stale,
    Failed(String),
}

impl MergeDoc {
    pub fn open(source: &MergeSource) -> Result<MergeDoc, String> {
        let (base, local, remote, output, labels, git_result, repo, style) = match source {
            MergeSource::Files { base, local, remote, output } => {
                let output = PathBuf::from(output);
                if output.as_os_str().is_empty() || output.is_dir() {
                    return Err("Choose a file to write the result to.".into());
                }
                let (labels, repo, style) = match conflicts::conflict_of(&output) {
                    Some((state, rel)) => {
                        let style = conflicts::conflict_style(&state.workdir);
                        let labels =
                            [state.labels.local.clone(), state.labels.base.clone(), state.labels.remote.clone()];
                        (labels, Some((state.workdir, rel)), style)
                    }
                    None => (["local".to_string(), "base".to_string(), "remote".to_string()], None, MarkerStyle::Merge),
                };
                let git_result = std::fs::read(&output).ok();
                (read_file(base)?, read_file(local)?, read_file(remote)?, output, labels, git_result, repo, style)
            }
            MergeSource::Repo { workdir, path } => {
                let inputs = conflicts::merge_inputs(workdir, path).map_err(|e| e.to_string())?;
                let repo = Some((inputs.workdir.clone(), path.clone()));
                (
                    inputs.base,
                    inputs.local,
                    inputs.remote,
                    inputs.output,
                    inputs.labels,
                    inputs.git_result,
                    repo,
                    inputs.style,
                )
            }
        };
        let (b, l, r) =
            (text_or_error("base", &base)?, text_or_error("local", &local)?, text_or_error("remote", &remote)?);
        let options = MergeOptions { labels: labels.clone().map(Some), style, ..Default::default() };
        let session = MergeSession::new(b, l, r, options);
        let name = output.to_string_lossy().replace('\\', "/");
        let first_line = session.local.lines.first().map(|r| &session.local.text[r.clone()]).unwrap_or_default();
        let syntax = highlight::detect_syntax(&name, first_line).map(|s| s.name.clone());
        let git_result = git_result
            .filter(|d| !text::is_binary(d))
            .map(|d| text::decode(&d).text)
            .filter(|t| !merge_session::same_but_labels(t, session.result_text()));
        let theme = crate::style::syntax_theme();
        let grammar = syntax.as_deref().and_then(highlight::find_syntax);
        Ok(MergeDoc {
            on_disk: disk_stamp(&output),
            session,
            output,
            labels,
            syntax,
            git_result,
            repo,
            syntax_whole: None,
            highlighters: std::array::from_fn(|_| DocHighlighter::new(grammar, theme)),
            theme,
        })
    }

    pub fn title(&self) -> String {
        self.output.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn stats(&self) -> MergeStats {
        self.session.stats()
    }

    pub fn conflicts(&self) -> Vec<ConflictInfo> {
        self.session.conflicts()
    }

    fn themed(&mut self) {
        let theme = crate::style::syntax_theme();
        if theme != self.theme {
            let grammar = self.syntax.as_deref().and_then(highlight::find_syntax);
            self.highlighters = std::array::from_fn(|_| DocHighlighter::new(grammar, theme));
            self.theme = theme;
        }
    }

    /// Input rows with colours and changed characters, as `merge_input_rows` builds them.
    pub fn input_rows(&mut self, start: u32, count: u32) -> Vec<MergeInputRow> {
        self.themed();
        let [hb, hl, hr, _] = &mut self.highlighters;
        rows::merge_input_rows(&self.session, start, count, [hb, hl, hr])
    }

    /// Result rows, as `merge_result_rows` builds them.
    pub fn result_rows(&mut self, start: u32, count: u32) -> Vec<MergeResultRow> {
        self.themed();
        rows::merge_result_rows(&self.session, start, count, &mut self.highlighters[3])
    }

    fn edited(&mut self, line: Option<usize>) {
        if let Some(line) = line {
            self.highlighters[3].invalidate_from(line);
        }
    }

    pub fn resolve(&mut self, block: u32, choice: Choice) {
        let line = self.session.resolve(block as usize, choice);
        self.edited(line);
    }

    pub fn undo(&mut self, redo: bool) -> bool {
        let line = if redo { self.session.redo() } else { self.session.undo() };
        self.edited(line.or(Some(0)));
        line.is_some()
    }

    /// The text of result line `line`.
    pub fn result_line(&self, line: u32) -> String {
        self.session.result().line(line as usize).to_string()
    }

    /// Replaces result line `line` with `text`.
    pub fn replace_line(&mut self, line: u32, text: &str) {
        let doc = self.session.result();
        let end = text::utf16_len(doc.line(line as usize)) as u32;
        let changed = self.session.replace(Pos { line, col: 0 }, Pos { line, col: end }, text);
        self.edited(Some(changed));
    }

    /// What a syntax-aware merge runs on, or `None` when it has nothing to add.
    pub fn syntax_inputs(&self) -> Option<SyntaxInputs> {
        let s = &self.session;
        if s.syntax_has_nothing_to_add() {
            return None;
        }
        Some(SyntaxInputs {
            base: s.base.text.clone(),
            local: s.local.text.clone(),
            remote: s.remote.text.clone(),
            name: self.output.to_string_lossy().into_owned(),
            labels: self.labels.clone(),
        })
    }

    /// Offers the syntax-aware result, as the Tauri app's `merge_structured` does.
    pub fn apply_syntax(&mut self, merged: Option<&str>) -> SyntaxOffer {
        let offer = self.session.offer_syntax(merged);
        self.syntax_whole = offer.whole.clone();
        offer
    }

    /// Takes the syntax-aware merge's whole result.
    pub fn use_syntax_whole(&mut self) -> bool {
        let Some(text) = self.syntax_whole.clone() else { return false };
        self.session.replace_all(&text);
        self.edited(Some(0));
        true
    }

    pub fn use_git_result(&mut self) -> bool {
        let Some(text) = self.git_result.clone() else { return false };
        self.session.replace_all(&text);
        self.edited(Some(0));
        true
    }

    /// Writes the result, as `merge_save` does. With conflicts left, only when
    /// `with_markers`; returns whether the result is free of conflicts.
    pub fn save(&mut self, with_markers: bool, overwrite: bool) -> Result<bool, SaveError> {
        let clean = self.session.is_clean();
        if !clean && !with_markers {
            return Err(SaveError::Conflicts);
        }
        if !overwrite && disk_stamp(&self.output) != self.on_disk {
            return Err(SaveError::Stale);
        }
        let data = self.session.result().encode().map_err(|e| SaveError::Failed(format!("Cannot save: {e}.")))?;
        parallax_core::vfs::local::replace_file(&self.output, &data, None, None)
            .map_err(|e| SaveError::Failed(format!("{}: {e}", self.output.display())))?;
        self.on_disk = Some(stamp(&data));
        self.session.mark_saved();
        Ok(clean)
    }
}

/// The three versions a syntax-aware merge runs on.
pub struct SyntaxInputs {
    pub base: String,
    pub local: String,
    pub remote: String,
    pub name: String,
    pub labels: [String; 3],
}

impl SyntaxInputs {
    /// Runs the built-in engine (weave), as the Tauri app does by default.
    pub fn run(&self) -> Result<Option<String>, String> {
        let labels = [self.labels[0].as_str(), self.labels[1].as_str(), self.labels[2].as_str()];
        Engine::Builtin
            .merge_text(&self.base, &self.local, &self.remote, &self.name, labels, std::time::Duration::from_secs(5))
            .map(|out| out.map(|(text, _)| text))
            .map_err(|e| e.to_string())
    }
}

/// A conflicted file of a repository, as the conflicts view lists it.
#[derive(Clone, Debug)]
pub struct ConflictEntry {
    pub path: String,
    pub kind: ConflictKind,
    /// Both versions are text, so it can be merged line by line.
    pub text: bool,
    /// A link or a submodule rather than a file, on either side.
    pub special: Option<FileMode>,
}

pub struct RepoState {
    pub workdir: PathBuf,
    pub summary: String,
    pub files: Vec<ConflictEntry>,
}

/// The conflicted files of the repository at `dir`, as `repo_conflicts` lists them.
pub fn repo_state(dir: &Path) -> Result<RepoState, String> {
    let state = conflicts::repo_conflicts(dir).map_err(|e| e.to_string())?;
    let mut stages = conflicts::StageReader::new(&state.workdir);
    let files = state
        .files
        .iter()
        .map(|f| {
            let special = f
                .stages
                .iter()
                .flatten()
                .map(|s| s.file_mode())
                .find(|m| matches!(m, FileMode::Symlink | FileMode::Gitlink));
            let text = special.is_none()
                && matches!(f.kind, ConflictKind::BothModified | ConflictKind::BothAdded)
                && f.stages.iter().flatten().all(|s| stages.read(s).is_ok_and(|d| !text::is_binary(&d)));
            ConflictEntry { path: f.path.clone(), kind: f.kind, text, special }
        })
        .collect();
    Ok(RepoState { workdir: state.workdir, summary: state.summary, files })
}

/// Resolves a conflicted file as a whole with one side, as `take_conflict_side`
/// does; returns how to stage it.
pub fn take_side(workdir: &Path, path: &str, remote: bool) -> Result<Resolution, String> {
    let state = conflicts::repo_conflicts(workdir).map_err(|e| e.to_string())?;
    let file = state.files.iter().find(|f| f.path == path).ok_or_else(|| format!("{path} is not conflicted"))?;
    let target = state.workdir.join(&file.path);
    match &file.stages[if remote { 2 } else { 1 }] {
        Some(stage) => {
            conflicts::write_stage(&state.workdir, &file.path, stage).map_err(|e| e.to_string())?;
            Ok(Resolution::WorkTree(Some(stage.file_mode())))
        }
        None => {
            if std::fs::symlink_metadata(&target).is_ok() {
                std::fs::remove_file(&target).map_err(|e| e.to_string())?;
            }
            Ok(Resolution::Delete)
        }
    }
}

pub fn kind_label(kind: ConflictKind) -> &'static str {
    match kind {
        ConflictKind::BothModified => "changed on both sides",
        ConflictKind::BothAdded => "added on both sides",
        ConflictKind::DeletedByUs => "deleted here, changed there",
        ConflictKind::DeletedByThem => "changed here, deleted there",
        ConflictKind::AddedByUs => "added here",
        ConflictKind::AddedByThem => "added there",
        ConflictKind::BothDeleted => "deleted on both sides",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn git(dir: &Path, args: &[&str]) -> bool {
        #[allow(clippy::disallowed_methods)]
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// A repository in the middle of a merge with conflicts in `a.txt` and `b.txt`.
    pub fn conflicted_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        assert!(git(d, &["init", "-q", "-b", "main"]));
        std::fs::write(d.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(d.join("b.txt"), "base\n").unwrap();
        assert!(git(d, &["add", "."]) && git(d, &["commit", "-qm", "base"]));
        assert!(git(d, &["checkout", "-qb", "feature"]));
        std::fs::write(d.join("a.txt"), "one\nTWO (feature)\nthree\n").unwrap();
        std::fs::write(d.join("b.txt"), "feature\n").unwrap();
        assert!(git(d, &["commit", "-qam", "feature"]));
        assert!(git(d, &["checkout", "-q", "main"]));
        std::fs::write(d.join("a.txt"), "one\nTWO (main)\nthree\n").unwrap();
        std::fs::write(d.join("b.txt"), "main\n").unwrap();
        assert!(git(d, &["commit", "-qam", "main"]));
        assert!(!git(d, &["merge", "-q", "feature"]), "the merge stops with conflicts");
        dir
    }

    #[test]
    fn repo_merge_resolve_save_and_stage() {
        let repo = conflicted_repo();
        let state = repo_state(repo.path()).unwrap();
        assert_eq!(state.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.txt", "b.txt"]);
        assert!(state.files.iter().all(|f| f.text));
        let mut doc = MergeDoc::open(&MergeSource::Repo { workdir: repo.path().into(), path: "a.txt".into() }).unwrap();
        assert_eq!(doc.stats().unresolved, 1);
        assert!(matches!(doc.save(false, false), Err(SaveError::Conflicts)));
        let conflict = doc.conflicts()[0];
        doc.resolve(conflict.block, Choice::Remote);
        assert_eq!(doc.stats().unresolved, 0);
        assert!(doc.undo(false));
        assert_eq!(doc.stats().unresolved, 1);
        assert!(doc.undo(true));
        assert!(doc.save(false, false).unwrap(), "saved clean");
        assert_eq!(std::fs::read_to_string(repo.path().join("a.txt")).unwrap(), "one\nTWO (feature)\nthree\n");
        // b.txt: the local version as a whole, then both are staged.
        let b = take_side(repo.path(), "b.txt", false).unwrap();
        assert_eq!(std::fs::read_to_string(repo.path().join("b.txt")).unwrap(), "main\n");
        let a = Resolution::WorkTree(None);
        conflicts::stage(repo.path(), &[("a.txt".into(), a), ("b.txt".into(), b)]).unwrap();
        assert!(repo_state(repo.path()).unwrap().files.is_empty());
    }

    #[test]
    fn syntax_aware_merge_solves_functions_added_at_the_same_place() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_string_lossy().into_owned();
        std::fs::write(p("base"), "fn a() {\n    1\n}\n").unwrap();
        std::fs::write(p("local"), "fn a() {\n    1\n}\n\nfn b() {\n    2\n}\n").unwrap();
        std::fs::write(p("remote"), "fn a() {\n    1\n}\n\nfn c() {\n    3\n}\n").unwrap();
        let source =
            MergeSource::Files { base: p("base"), local: p("local"), remote: p("remote"), output: p("out.rs") };
        let mut doc = MergeDoc::open(&source).unwrap();
        assert_eq!(doc.stats().unresolved, 1, "the line merge sees one conflict");
        let merged = doc.syntax_inputs().expect("both sides changed").run().unwrap();
        let offer = doc.apply_syntax(merged.as_deref());
        let outcome = offer.outcome.expect("weave reads Rust");
        assert_eq!((outcome.solved, outcome.conflicts), (1, 0));
        assert!(offer.whole.is_none());
        let conflict = doc.conflicts()[0];
        assert!(doc.session.syntax_suggestion(conflict.block as usize).is_some());
        doc.resolve(conflict.block, Choice::Syntax);
        assert_eq!(doc.stats().unresolved, 0);
        let text = doc.session.result_text().to_string();
        assert!(text.contains("fn b()") && text.contains("fn c()"), "{text}");
    }

    #[test]
    fn files_merge_edit_a_line_and_stale_check() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_string_lossy().into_owned();
        std::fs::write(p("base"), "a\nb\nc\n").unwrap();
        std::fs::write(p("local"), "a\nB\nc\n").unwrap();
        std::fs::write(p("remote"), "a\nb\nc\nd\n").unwrap();
        let source = MergeSource::Files { base: p("base"), local: p("local"), remote: p("remote"), output: p("out") };
        let mut doc = MergeDoc::open(&source).unwrap();
        assert_eq!((doc.stats().conflicts, doc.stats().automatic), (0, 2), "both changes merge on their own");
        assert_eq!(doc.session.result_text(), "a\nB\nc\nd\n");
        doc.replace_line(0, "A");
        assert_eq!(doc.result_line(0), "A");
        assert!(doc.save(false, false).unwrap());
        assert_eq!(std::fs::read_to_string(p("out")).unwrap(), "A\nB\nc\nd\n");
        std::fs::write(p("out"), "changed elsewhere\n").unwrap();
        doc.replace_line(3, "D");
        assert!(matches!(doc.save(false, false), Err(SaveError::Stale)));
        doc.save(false, true).unwrap();
        assert_eq!(std::fs::read_to_string(p("out")).unwrap(), "A\nB\nc\nD\n");
    }
}
