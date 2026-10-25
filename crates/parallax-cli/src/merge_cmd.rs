//! `parallax merge`: a 3-way merge without a window, as `git merge-file` does it.
//!
//! Exit status: 0 merged without conflicts, 1 conflicts left (written with
//! markers), 2 error.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use parallax_core::conflicts::{self, ConflictKind};
use parallax_core::merge::{self, Level, MarkerStyle, MergeOptions};
use parallax_core::source::{SourceOptions, SourceSpec};
use parallax_core::syntax_merge::{Engine, Mergiraf};
use parallax_core::text::{self, TextDoc};

#[derive(Parser)]
#[command(
    name = "parallax merge",
    bin_name = "parallax merge",
    about = "Merge three versions of a file, or the conflicted files of a repository",
    after_help = "EXAMPLES:\n  parallax merge base.c local.c remote.c -o merged.c\n  parallax merge repo@main~3:a.c repo@main:a.c repo@feature/x:a.c\n  parallax merge --repo .     # merge every conflicted file again, keep only clean results\n  parallax merge --repo . --strategy syntax   # the same, merging by syntax where lines conflict\n\nExit status: 0 merged, 1 conflicts left, 2 error."
)]
pub struct MergeCli {
    /// The common ancestor (any source: a path, repo@rev:path, an archive entry)
    #[arg(required_unless_present = "repo")]
    base: Option<String>,
    /// The local version ("ours")
    #[arg(required_unless_present = "repo")]
    local: Option<String>,
    /// The remote version ("theirs")
    #[arg(required_unless_present = "repo")]
    remote: Option<String>,

    /// Write the result here instead of to standard output
    #[arg(long, short = 'o', value_name = "FILE")]
    output: Option<PathBuf>,

    /// Conflict markers: merge (local and remote), diff3 (with the base), zdiff3
    #[arg(long, value_enum, default_value_t = Style::Merge)]
    style: Style,

    /// How hard to make conflicts small: eager, zealous (git merge), zealous-alnum (git merge-file)
    #[arg(long, value_enum, default_value_t = LevelArg::Zealous)]
    level: LevelArg,

    /// Labels after the markers, in the order local, base, remote (repeatable)
    #[arg(long, short = 'L', value_name = "LABEL")]
    label: Vec<String>,

    /// Use the histogram diff algorithm instead of Myers
    #[arg(long)]
    histogram: bool,

    /// line: merge lines as Git does. syntax: merge by the syntax of the file where
    /// lines conflict (built in). mergiraf: the same with mergiraf, installed
    /// separately. Both write the base into the markers
    #[arg(long, value_enum, default_value_t = Strategy::Line)]
    strategy: Strategy,

    /// The mergiraf program for --strategy mergiraf (default: $PARALLAX_MERGIRAF, then PATH)
    #[arg(long, value_name = "PATH")]
    mergiraf: Option<PathBuf>,

    /// Merge every conflicted file of the repository at this folder again: files
    /// that merge without conflicts are written to the work tree (not staged),
    /// the others are left as Git wrote them
    #[arg(long, value_name = "DIR", conflicts_with_all = ["base", "local", "remote", "output"])]
    repo: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum Style {
    Merge,
    Diff3,
    Zdiff3,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Strategy {
    Line,
    Syntax,
    Mergiraf,
}

#[derive(Clone, Copy, ValueEnum)]
enum LevelArg {
    Eager,
    Zealous,
    ZealousAlnum,
}

impl MergeCli {
    fn options(&self, labels: [Option<String>; 3]) -> MergeOptions {
        MergeOptions {
            level: match self.level {
                LevelArg::Eager => Level::Eager,
                LevelArg::Zealous => Level::Zealous,
                LevelArg::ZealousAlnum => Level::ZealousAlnum,
            },
            style: match self.style {
                Style::Merge => MarkerStyle::Merge,
                Style::Diff3 => MarkerStyle::Diff3,
                Style::Zdiff3 => MarkerStyle::Zdiff3,
            },
            histogram: self.histogram,
            labels,
        }
    }
}

fn read_source(text: &str) -> Result<Vec<u8>> {
    let source = SourceSpec::parse(text).open(SourceOptions::default()).with_context(|| format!("opening {text}"))?;
    if !source.is_file()? {
        bail!("{text} is not a file");
    }
    Ok(source.vfs.read(&source.root).with_context(|| format!("reading {text}"))?.to_vec())
}

/// The syntax-aware engine and the file name it picks the language from.
type Syntax<'a> = Option<(&'a Engine, &'a str)>;

impl MergeCli {
    fn engine(&self) -> Result<Option<Engine>> {
        match self.strategy {
            Strategy::Line => Ok(None),
            Strategy::Syntax => Ok(Some(Engine::Builtin)),
            Strategy::Mergiraf => match Mergiraf::find(self.mergiraf.as_deref()) {
                Ok(m) => Ok(Some(Engine::Mergiraf(m))),
                Err(parallax_core::Error::Unsupported(message)) => bail!(message),
                Err(e) => Err(e.into()),
            },
        }
    }
}

/// The merged text in the local file's encoding, and the number of conflicts.
fn merge_bytes(
    base: &[u8],
    local: &[u8],
    remote: &[u8],
    opts: &MergeOptions,
    syntax: Syntax,
) -> Result<(Vec<u8>, usize)> {
    for (name, data) in [("base", base), ("local", local), ("remote", remote)] {
        if text::is_binary(data) {
            bail!("the {name} version is binary; binary files cannot be merged line by line");
        }
    }
    let (b, l, r) = (text::decode(base), text::decode(local), text::decode(remote));
    let (mut merged, mut conflicts) = merge::merge_text(&b.text, &l.text, &r.text, opts);
    if let Some((engine, name)) = syntax {
        // The engine gets the decoded text, so any encoding works; the result is
        // encoded like local again below.
        let label = |i: usize, default: &'static str| opts.labels[i].as_deref().unwrap_or(default);
        let labels = [label(0, "local"), label(1, "base"), label(2, "remote")];
        let timeout = Duration::from_secs(10);
        match engine.merge_text(&b.text, &l.text, &r.text, name, labels, timeout) {
            Ok(Some(out)) => (merged, conflicts) = out,
            // The built-in engine declines files it cannot parse into units.
            Ok(None) => {}
            Err(e) => eprintln!("parallax merge: {} failed, using the line merge: {e}", engine.name()),
        }
    }
    let mut doc = TextDoc::empty_like(&l);
    doc.splice(0..0, &merged);
    Ok((doc.encode().map_err(anyhow::Error::msg)?, conflicts))
}

/// Runs `parallax merge`; `Ok(true)` when everything merged without conflicts.
pub fn run(cli: &MergeCli) -> Result<bool> {
    if let Some(dir) = &cli.repo {
        return merge_repo(cli, dir);
    }
    let (base, local, remote) = (
        cli.base.as_deref().unwrap_or_default(),
        cli.local.as_deref().unwrap_or_default(),
        cli.remote.as_deref().unwrap_or_default(),
    );
    let labels = [0, 1, 2].map(|i| cli.label.get(i).cloned().or_else(|| Some([local, base, remote][i].to_string())));
    let engine = cli.engine()?;
    let name = cli.output.as_ref().map_or(local.to_string(), |p| p.display().to_string());
    let syntax = engine.as_ref().map(|m| (m, name.as_str()));
    let (data, conflicts) =
        merge_bytes(&read_source(base)?, &read_source(local)?, &read_source(remote)?, &cli.options(labels), syntax)?;
    match &cli.output {
        Some(path) => std::fs::write(path, &data).with_context(|| format!("writing {}", path.display()))?,
        None => std::io::stdout().write_all(&data)?,
    }
    if conflicts > 0 {
        eprintln!("parallax merge: {conflicts} conflict{} left", if conflicts == 1 { "" } else { "s" });
    }
    Ok(conflicts == 0)
}

fn merge_repo(cli: &MergeCli, dir: &std::path::Path) -> Result<bool> {
    let state = conflicts::repo_conflicts(dir)?;
    if state.files.is_empty() {
        println!("No conflicted files in {}.", state.workdir.display());
        return Ok(true);
    }
    println!("{}", state.summary);
    let labels = [Some(state.labels.local.clone()), Some(state.labels.base.clone()), Some(state.labels.remote.clone())];
    let opts = cli.options(labels);
    let engine = cli.engine()?;
    let mut clean = true;
    for file in &state.files {
        let outcome = match (file.kind, &file.stages) {
            (ConflictKind::BothModified | ConflictKind::BothAdded, [base, Some(local), Some(remote)]) => {
                let read = |s: &conflicts::Stage| conflicts::stage_content(&state.workdir, s);
                let base = match base {
                    Some(b) => read(b)?,
                    None => Vec::new(),
                };
                let syntax = engine.as_ref().map(|m| (m, file.path.as_str()));
                match merge_bytes(&base, &read(local)?, &read(remote)?, &opts, syntax) {
                    Ok((data, 0)) => {
                        let path = state.workdir.join(&file.path);
                        std::fs::write(&path, data).with_context(|| format!("writing {}", path.display()))?;
                        "merged, not staged".to_string()
                    }
                    Ok((_, n)) => {
                        clean = false;
                        format!("{n} conflict{} left", if n == 1 { "" } else { "s" })
                    }
                    Err(e) => {
                        clean = false;
                        format!("{e:#}")
                    }
                }
            }
            (kind, _) => {
                clean = false;
                format!("{} (resolve as a whole file)", describe(kind))
            }
        };
        println!("  {}  {outcome}", file.path);
    }
    Ok(clean)
}

fn describe(kind: ConflictKind) -> &'static str {
    match kind {
        ConflictKind::BothModified => "both modified",
        ConflictKind::BothAdded => "both added",
        ConflictKind::DeletedByUs => "deleted by us",
        ConflictKind::DeletedByThem => "deleted by them",
        ConflictKind::AddedByUs => "added by us",
        ConflictKind::AddedByThem => "added by them",
        ConflictKind::BothDeleted => "both deleted",
    }
}
