//! Turning a user-facing source description into a browsable tree.
//!
//! Text syntax (CLI, path fields in the UI):
//!
//! * `path/to/folder`, `path/to/file.txt`
//! * `release.7z`, `release.7z!/bin/` — an archive, optionally a folder inside it
//! * `~/src/repo@main`, `repo@v1.4.0:src/lib`, `repo@HEAD~3` — a Git revision, optionally a sub-folder
//!
//! A string is read as a path whenever that path exists, so folders whose names
//! contain `@` keep working.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::vfs::git::{GitRepo, GitTreeFs};
use crate::vfs::local::LocalFs;
use crate::vfs::mount::ArchiveMount;
use crate::vfs::{Entry, EntryKind, Vfs, join, normalize};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SourceSpec {
    /// A folder, a file, an archive, or a path inside an archive (`a.zip!/dir`).
    #[serde(rename_all = "camelCase")]
    Local { path: String },
    /// A Git revision (branch, tag, commit, `HEAD~2` …) of the repository at `repo`.
    #[serde(rename_all = "camelCase")]
    GitRev {
        repo: String,
        rev: String,
        #[serde(default)]
        subpath: String,
    },
    /// The working tree of a Git repository, filtered by `.gitignore`.
    #[serde(rename_all = "camelCase")]
    GitWorktree { repo: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SourceOptions {
    /// Offer archives found inside folders as expandable folders.
    pub expand_archives: bool,
    /// Hide files ignored by `.gitignore` in local folders that are inside a Git repository.
    pub respect_gitignore: bool,
}

impl Default for SourceOptions {
    fn default() -> Self {
        SourceOptions { expand_archives: true, respect_gitignore: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    Folder,
    Archive,
    GitRev,
    GitWorktree,
}

/// Display information about an opened source.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub kind: SourceKind,
    /// Main display text, e.g. the folder path or the repository path.
    pub display: String,
    /// For Git revisions: the revision as given and its abbreviated commit id.
    pub rev: Option<String>,
    pub commit: Option<String>,
    pub commit_summary: Option<String>,
    pub writable: bool,
}

pub struct Source {
    pub spec: SourceSpec,
    pub vfs: Arc<dyn Vfs>,
    /// Path of the compared root inside `vfs`.
    pub root: String,
    pub info: SourceInfo,
}

impl Source {
    pub fn path(&self, rel: &str) -> String {
        join(&self.root, rel)
    }

    pub fn stat(&self, rel: &str) -> Result<Entry> {
        self.vfs.stat(&self.path(rel))
    }

    /// Whether the root is a single file (file compare) rather than a tree.
    pub fn is_file(&self) -> Result<bool> {
        if self.root.is_empty() {
            return Ok(false);
        }
        let entry = self.stat("")?;
        let is_archive = entry.kind == EntryKind::File && crate::vfs::archive::detect_format(&entry.name).is_some();
        Ok(!entry.kind.is_container() && !is_archive)
    }
}

impl SourceSpec {
    pub fn parse(text: &str) -> SourceSpec {
        let text = text.trim();
        if !text.contains('!') && Path::new(text).exists() {
            return SourceSpec::Local { path: text.to_string() };
        }
        if let Some(at) = text.rfind('@') {
            let (repo, rest) = (&text[..at], &text[at + 1..]);
            if !repo.is_empty() && Path::new(repo).is_dir() {
                let (rev, subpath) = rest.split_once(':').unwrap_or((rest, ""));
                return SourceSpec::GitRev {
                    repo: repo.to_string(),
                    rev: if rev.is_empty() { "HEAD".to_string() } else { rev.to_string() },
                    subpath: subpath.to_string(),
                };
            }
        }
        SourceSpec::Local { path: text.to_string() }
    }

    pub fn open(&self, opts: SourceOptions) -> Result<Source> {
        match self {
            SourceSpec::Local { path } => open_local(self, path, opts),
            SourceSpec::GitRev { repo, rev, subpath } => {
                let git = GitRepo::discover(repo)?;
                let resolved = git.resolve(rev)?;
                let short = resolved.commit.as_ref().map(|c| c.short_id.clone());
                let label = format!("{repo}@{rev}");
                let tree: Arc<dyn Vfs> = Arc::new(GitTreeFs::new(git, resolved.tree, label));
                let vfs = Arc::new(ArchiveMount::new(tree, opts.expand_archives));
                let root = normalize(subpath).ok_or_else(|| Error::InvalidSource(subpath.clone()))?;
                Ok(Source {
                    spec: self.clone(),
                    vfs,
                    root,
                    info: SourceInfo {
                        kind: SourceKind::GitRev,
                        display: repo.clone(),
                        rev: Some(rev.clone()),
                        commit: short,
                        commit_summary: resolved.commit.map(|c| c.summary),
                        writable: false,
                    },
                })
            }
            SourceSpec::GitWorktree { repo } => {
                let git = GitRepo::discover(repo)?;
                let workdir =
                    git.workdir().ok_or_else(|| Error::InvalidSource(format!("{repo} is a bare repository")))?;
                let local: Arc<dyn Vfs> = Arc::new(LocalFs::with_gitignore(&workdir)?);
                Ok(Source {
                    spec: self.clone(),
                    vfs: Arc::new(ArchiveMount::new(local, opts.expand_archives)),
                    root: String::new(),
                    info: SourceInfo {
                        kind: SourceKind::GitWorktree,
                        display: workdir.display().to_string(),
                        rev: None,
                        commit: None,
                        commit_summary: None,
                        writable: true,
                    },
                })
            }
        }
    }
}

fn open_local(spec: &SourceSpec, path: &str, opts: SourceOptions) -> Result<Source> {
    let (outer, inner) = match path.split_once('!') {
        Some((outer, inner)) => (outer, Some(inner)),
        None => (path, None),
    };
    let outer_path = PathBuf::from(outer);
    let meta = std::fs::metadata(&outer_path).map_err(|e| Error::io(outer, e))?;
    let (dir, mut root) = if meta.is_dir() {
        (outer_path.clone(), String::new())
    } else {
        let name = outer_path
            .file_name()
            .ok_or_else(|| Error::InvalidSource(outer.to_string()))?
            .to_string_lossy()
            .into_owned();
        let parent = outer_path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        (parent.to_path_buf(), name)
    };
    if let Some(inner) = inner {
        let inner = normalize(inner).ok_or_else(|| Error::InvalidSource(inner.to_string()))?;
        root = join(&root, &inner);
    }
    // A gitignore snapshot only pays off for folders; for a single file it would
    // walk the whole parent folder.
    let local: Arc<dyn Vfs> = if opts.respect_gitignore && meta.is_dir() && inside_git_repo(&dir) {
        Arc::new(LocalFs::with_gitignore(&dir)?)
    } else {
        Arc::new(LocalFs::new(&dir))
    };
    let vfs: Arc<dyn Vfs> = Arc::new(ArchiveMount::new(local, opts.expand_archives));
    let in_archive = inner.is_some() || (!meta.is_dir() && crate::vfs::archive::detect_format(&root).is_some());
    let source = Source {
        spec: spec.clone(),
        vfs,
        root,
        info: SourceInfo {
            kind: if in_archive { SourceKind::Archive } else { SourceKind::Folder },
            display: path.to_string(),
            rev: None,
            commit: None,
            commit_summary: None,
            writable: !in_archive,
        },
    };
    // Fail early on a bad path inside an archive.
    if in_archive {
        source.stat("")?;
    }
    Ok(source)
}

fn inside_git_repo(dir: &Path) -> bool {
    dir.ancestors().any(|d| d.join(".git").exists())
}

/// Opens both sides of a comparison. With `merge_base`, a Git revision on the left
/// is replaced by its merge-base with the right revision, so the comparison shows
/// only what the right side changed since it branched off.
pub fn open_pair(
    left: &SourceSpec,
    right: &SourceSpec,
    opts: SourceOptions,
    merge_base: bool,
) -> Result<(Source, Source)> {
    let mut left = left.clone();
    let mut label = None;
    if merge_base
        && let (SourceSpec::GitRev { repo, rev, .. }, SourceSpec::GitRev { rev: right_rev, .. }) = (&mut left, right)
    {
        let git = GitRepo::discover(&*repo)?;
        label = Some(format!("{rev} (merge-base)"));
        *rev = git.merge_base(rev, right_rev)?;
    }
    let mut left = left.open(opts)?;
    if label.is_some() {
        left.info.rev = label;
    }
    Ok((left, right.open(opts)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_git_rev_syntax() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().display().to_string();
        assert_eq!(
            SourceSpec::parse(&format!("{repo}@feature/x:src/lib")),
            SourceSpec::GitRev { repo: repo.clone(), rev: "feature/x".into(), subpath: "src/lib".into() }
        );
        assert_eq!(SourceSpec::parse(&repo), SourceSpec::Local { path: repo.clone() });
        assert_eq!(SourceSpec::parse("missing.zip!/bin"), SourceSpec::Local { path: "missing.zip!/bin".into() });
    }
}
