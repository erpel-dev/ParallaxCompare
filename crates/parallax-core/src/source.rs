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
use crate::vfs::{Entry, EntryKind, Vfs, join, normalize, split_parent};
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
    /// A single local file (not inside an archive).
    File,
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
    /// What to watch on disk for changes made outside the app.
    pub watch: Option<WatchRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchRoot {
    /// A folder whose contents map to compare paths relative to it; with
    /// `gitignore`, what `.gitignore` hides is not compared and need not be watched.
    Tree { dir: PathBuf, gitignore: bool },
    /// One file (an archive or a compared file): any change to it changes everything.
    File(PathBuf),
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
        let text = expand_home(text.trim());
        let text = text.as_str();
        if !text.contains('!') && Path::new(text).exists() {
            return SourceSpec::Local { path: text.to_string() };
        }
        // A remote repository: `url@rev:path`, or a link to a file or folder on GitHub & co.
        if let Some(spec) = crate::remote::parse(text) {
            return spec;
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

    /// The source for the entry at `rel` inside this one, e.g. to compare one file
    /// of a folder comparison on its own. Archives on the way are entered with `!`.
    pub fn child(&self, rel: &str) -> Result<SourceSpec> {
        let rel = normalize(rel).ok_or_else(|| Error::InvalidSource(rel.to_string()))?;
        if rel.is_empty() {
            return Ok(self.clone());
        }
        Ok(match self {
            SourceSpec::Local { path } => SourceSpec::Local { path: local_child(path, &rel) },
            SourceSpec::GitRev { repo, rev, subpath } => {
                let subpath = normalize(subpath).ok_or_else(|| Error::InvalidSource(subpath.clone()))?;
                SourceSpec::GitRev { repo: repo.clone(), rev: rev.clone(), subpath: join(&subpath, &rel) }
            }
            SourceSpec::GitWorktree { repo } => {
                let git = GitRepo::discover(repo)?;
                let workdir =
                    git.workdir().ok_or_else(|| Error::InvalidSource(format!("{repo} is a bare repository")))?;
                SourceSpec::Local { path: local_child(&workdir.display().to_string(), &rel) }
            }
        })
    }

    /// The folder containing this source, e.g. to compare one level up. `None` at
    /// the top: a file system root, or the root of a Git revision or worktree.
    pub fn parent(&self) -> Option<SourceSpec> {
        match self {
            SourceSpec::Local { path } => match path.split_once('!') {
                Some((outer, inner)) => {
                    let inner = normalize(inner)?;
                    if inner.is_empty() {
                        return local_parent(outer);
                    }
                    let (dir, _) = split_parent(&inner);
                    let path = if dir.is_empty() { outer.to_string() } else { format!("{outer}!/{dir}") };
                    Some(SourceSpec::Local { path })
                }
                None => local_parent(path),
            },
            SourceSpec::GitRev { repo, rev, subpath } => {
                let subpath = normalize(subpath)?;
                if subpath.is_empty() {
                    return None;
                }
                let (dir, _) = split_parent(&subpath);
                Some(SourceSpec::GitRev { repo: repo.clone(), rev: rev.clone(), subpath: dir.to_string() })
            }
            SourceSpec::GitWorktree { .. } => None,
        }
    }

    /// The file or folder on disk, when the source is one (not inside an archive or a Git revision).
    pub fn disk_path(&self) -> Option<PathBuf> {
        match self {
            SourceSpec::Local { path } if !path.contains('!') => Some(PathBuf::from(path)),
            SourceSpec::GitWorktree { repo } => Some(PathBuf::from(repo)),
            _ => None,
        }
    }

    /// The file on disk that holds the source: the archive for a path inside one.
    pub fn container_path(&self) -> Option<PathBuf> {
        match self {
            SourceSpec::Local { path } => Some(PathBuf::from(path.split('!').next().unwrap_or(path))),
            _ => self.disk_path(),
        }
    }

    /// Where a file is in Git: the repository, the revision (`None` for the working
    /// tree) and the path inside the repository.
    pub fn git_location(&self) -> Result<GitLocation> {
        match self {
            SourceSpec::GitRev { repo, .. } if crate::remote::is_remote(repo) => {
                Err(Error::Unsupported("the history of files in a remote repository is not available".into()))
            }
            SourceSpec::GitRev { repo, rev, subpath } => Ok(GitLocation {
                repo: repo.clone(),
                rev: Some(rev.clone()),
                path: normalize(subpath).ok_or_else(|| Error::InvalidSource(subpath.clone()))?,
            }),
            _ => {
                let path = self
                    .disk_path()
                    .ok_or_else(|| Error::InvalidSource("the file is inside an archive, not in Git".into()))?;
                let full = std::fs::canonicalize(&path).map_err(|e| Error::io(path.display().to_string(), e))?;
                let git = GitRepo::discover(full.parent().unwrap_or(&full))?;
                let workdir = git
                    .workdir()
                    .and_then(|w| std::fs::canonicalize(w).ok())
                    .ok_or_else(|| Error::InvalidSource("bare repository".into()))?;
                let rel = full.strip_prefix(&workdir).map_err(|_| Error::InvalidSource(path.display().to_string()))?;
                let rel = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                Ok(GitLocation { repo: plain_path(&workdir), rev: None, path: rel })
            }
        }
    }

    pub fn open(&self, opts: SourceOptions) -> Result<Source> {
        match self {
            SourceSpec::Local { path } => open_local(self, path, opts),
            SourceSpec::GitRev { repo, rev, subpath } => {
                // A remote repository: the commit is fetched into the cache and read from there.
                let (git, resolved, repo) = if crate::remote::is_remote(repo) {
                    let (dir, commit) = crate::remote::fetch(repo, rev)?;
                    let git = GitRepo::discover(dir.display().to_string())?;
                    let resolved = git.resolve(&commit)?;
                    (git, resolved, crate::remote::display(repo))
                } else {
                    let git = GitRepo::discover(repo)?;
                    let resolved = git.resolve(rev)?;
                    (git, resolved, repo.clone())
                };
                let repo = &repo;
                let short = resolved.commit.as_ref().map(|c| c.short_id.clone());
                let label = format!("{repo}@{rev}");
                let tree: Arc<dyn Vfs> = Arc::new(GitTreeFs::new(git, resolved.tree, label));
                let vfs = Arc::new(ArchiveMount::new(tree, opts.expand_archives));
                let root = normalize(subpath).ok_or_else(|| Error::InvalidSource(subpath.clone()))?;
                let display = if root.is_empty() { repo.clone() } else { format!("{repo} › {root}") };
                Ok(Source {
                    spec: self.clone(),
                    vfs,
                    root,
                    watch: None,
                    info: SourceInfo {
                        kind: SourceKind::GitRev,
                        display,
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
                let local: Arc<dyn Vfs> = Arc::new(LocalFs::with_gitignore(&workdir, &git.tracked_paths()?)?);
                Ok(Source {
                    spec: self.clone(),
                    vfs: Arc::new(ArchiveMount::new(local, opts.expand_archives)),
                    root: String::new(),
                    watch: Some(WatchRoot::Tree { dir: workdir.clone(), gitignore: true }),
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
    let tracked = (opts.respect_gitignore && meta.is_dir()).then(|| tracked_below(&dir)).flatten();
    let gitignore = tracked.is_some();
    let local: Arc<dyn Vfs> = match tracked {
        Some(tracked) => Arc::new(LocalFs::with_gitignore(&dir, &tracked)?),
        None => Arc::new(LocalFs::new(&dir)),
    };
    let vfs: Arc<dyn Vfs> = Arc::new(ArchiveMount::new(local, opts.expand_archives));
    let in_archive = inner.is_some() || (!meta.is_dir() && crate::vfs::archive::detect_format(&root).is_some());
    let watch = Some(if meta.is_dir() { WatchRoot::Tree { dir, gitignore } } else { WatchRoot::File(outer_path) });
    let source = Source {
        spec: spec.clone(),
        vfs,
        root,
        watch,
        info: SourceInfo {
            kind: if in_archive {
                SourceKind::Archive
            } else if meta.is_dir() {
                SourceKind::Folder
            } else {
                SourceKind::File
            },
            display: path.to_string(),
            rev: None,
            commit: None,
            commit_summary: None,
            // Archives are written by rewriting them.
            writable: true,
        },
    };
    // Fail early on a bad path inside an archive.
    if in_archive {
        source.stat("")?;
    }
    Ok(source)
}

/// The folder containing a local path, made absolute so that relative paths
/// given on the command line have a parent too.
fn local_parent(path: &str) -> Option<SourceSpec> {
    let path = std::path::absolute(path.trim_end_matches(['/', '\\'])).ok()?;
    let parent = path.parent()?;
    Some(SourceSpec::Local { path: parent.display().to_string() })
}

/// A canonical path as people write it: without the `\\?\` prefix Windows adds.
fn plain_path(path: &Path) -> String {
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_string(),
        _ => text,
    }
}

/// Where a file is in Git, see [`SourceSpec::git_location`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLocation {
    /// The repository as given, or its working directory.
    pub repo: String,
    pub rev: Option<String>,
    pub path: String,
}

/// Completions for a path being typed: entries of the folder named so far that
/// start with the last part, folders ending in `/`. After `repo@`, the
/// repository's branches and tags. At most `limit`.
pub fn complete(text: &str, limit: usize) -> Vec<String> {
    if text.contains('!') {
        return Vec::new();
    }
    if let Some(at) = text.rfind('@') {
        let (repo, rev) = (&text[..at], &text[at + 1..]);
        if !repo.is_empty() && !Path::new(text).exists() && Path::new(repo).is_dir() && !rev.contains(':') {
            let Ok(refs) = GitRepo::discover(repo).and_then(|g| g.refs(0)) else { return Vec::new() };
            return refs
                .branches
                .iter()
                .chain(&refs.tags)
                .chain(&refs.remotes)
                .filter(|r| r.name.starts_with(rev))
                .take(limit)
                .map(|r| format!("{repo}@{}", r.name))
                .collect();
        }
    }
    let cut = text.rfind(['/', '\\']).map(|i| i + 1).unwrap_or(0);
    let (dir, prefix) = text.split_at(cut);
    if dir.is_empty() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let sep = if dir.ends_with('\\') { '\\' } else { '/' };
    let lower = prefix.to_lowercase();
    let mut found: Vec<(bool, String)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let hidden = name.starts_with('.') && !prefix.starts_with('.');
            let matches = name.starts_with(prefix) || name.to_lowercase().starts_with(&lower);
            (matches && !hidden).then(|| (e.path().is_dir(), name))
        })
        .collect();
    // Folders first, then by name.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    found
        .into_iter()
        .take(limit)
        .map(|(is_dir, name)| if is_dir { format!("{dir}{name}{sep}") } else { format!("{dir}{name}") })
        .collect()
}

/// `path` (a local path, possibly `a.zip!/dir`) joined with `rel`: the first file on
/// disk along the way is an archive, and the rest of `rel` is inside it.
fn local_child(path: &str, rel: &str) -> String {
    if path.contains('!') {
        return format!("{}/{rel}", path.trim_end_matches(['/', '\\']));
    }
    let mut disk = PathBuf::from(path);
    let parts: Vec<&str> = rel.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if disk.is_file() {
            return format!("{}!/{}", disk.display(), parts[i..].join("/"));
        }
        disk.push(part);
    }
    disk.display().to_string()
}

/// For a folder inside a Git worktree: the tracked files below it, relative to it.
/// `None` when the folder is not in a worktree.
fn tracked_below(dir: &Path) -> Option<Vec<String>> {
    if !dir.ancestors().any(|d| d.join(".git").exists()) {
        return None;
    }
    let git = GitRepo::discover(dir).ok()?;
    let workdir = std::fs::canonicalize(git.workdir()?).ok()?;
    let rel = std::fs::canonicalize(dir).ok()?.strip_prefix(&workdir).ok()?.to_path_buf();
    let prefix = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
    let tracked = git.tracked_paths().ok()?;
    if prefix.is_empty() {
        return Some(tracked);
    }
    let prefix = format!("{prefix}/");
    Some(tracked.into_iter().filter_map(|p| p.strip_prefix(&prefix).map(str::to_string)).collect())
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

/// `~` and `~/…` (also `~\…`, and `~@rev` for the home folder at a Git revision)
/// in the home folder, as a shell would expand them.
fn expand_home(text: &str) -> String {
    let rest = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\', '@']) => rest,
        _ => return text.to_string(),
    };
    match std::env::home_dir() {
        Some(home) => format!("{}{rest}", home.display()),
        None => text.to_string(),
    }
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

    #[test]
    fn parse_home_folder() {
        let home = std::env::home_dir().unwrap().display().to_string();
        assert_eq!(SourceSpec::parse("~"), SourceSpec::Local { path: home.clone() });
        assert_eq!(SourceSpec::parse("~/x/y.zip"), SourceSpec::Local { path: format!("{home}/x/y.zip") });
        assert_eq!(SourceSpec::parse("~other/x"), SourceSpec::Local { path: "~other/x".into() });
        assert_eq!(
            SourceSpec::parse("~@HEAD"),
            SourceSpec::GitRev { repo: home, rev: "HEAD".into(), subpath: String::new() }
        );
    }

    #[test]
    fn child_of_folder_archive_and_git() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let zip = dir.path().join("sub").join("pack.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        w.start_file("inner/a.txt", zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut w, b"hello").unwrap();
        w.finish().unwrap();
        let root = dir.path().display().to_string();
        let local = SourceSpec::Local { path: root.clone() };

        let file = local.child("sub/pack.zip").unwrap();
        assert_eq!(file, SourceSpec::Local { path: zip.display().to_string() });

        let inner = local.child("sub/pack.zip/inner/a.txt").unwrap();
        assert_eq!(inner, SourceSpec::Local { path: format!("{}!/inner/a.txt", zip.display()) });
        let source = inner.open(SourceOptions::default()).unwrap();
        assert!(source.is_file().unwrap());

        let in_archive = SourceSpec::Local { path: format!("{}!/inner/", zip.display()) };
        assert_eq!(in_archive.child("a.txt").unwrap(), inner);
        assert_eq!(local.child("").unwrap(), local);
        assert!(local.child("../x").is_err());

        let git = SourceSpec::GitRev { repo: root.clone(), rev: "main".into(), subpath: "src".into() };
        assert_eq!(
            git.child("lib/x.rs").unwrap(),
            SourceSpec::GitRev { repo: root, rev: "main".into(), subpath: "src/lib/x.rs".into() }
        );
    }

    #[test]
    fn parent_of_sources() {
        let local = |p: &str| SourceSpec::Local { path: p.into() };
        // Local paths come back absolute, which on Windows adds the drive.
        let abs = |p: &str| local(&std::path::absolute(p).unwrap().display().to_string());
        assert_eq!(local("/data/a/b").parent(), Some(abs("/data/a")));
        assert_eq!(local("/data/a/b/").parent(), Some(abs("/data/a")));
        assert_eq!(local("/").parent(), None);
        assert_eq!(local("/x/pack.zip!/bin/lib").parent(), Some(local("/x/pack.zip!/bin")));
        assert_eq!(local("/x/pack.zip!/bin").parent(), Some(local("/x/pack.zip")));
        assert_eq!(local("/x/pack.zip!/").parent(), Some(abs("/x")));
        let git = |s: &str| SourceSpec::GitRev { repo: "r".into(), rev: "main".into(), subpath: s.into() };
        assert_eq!(git("src/lib").parent(), Some(git("src")));
        assert_eq!(git("src").parent(), Some(git("")));
        assert_eq!(git("").parent(), None);
        assert_eq!(SourceSpec::GitWorktree { repo: "r".into() }.parent(), None);
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(local("A").parent(), Some(local(&cwd.display().to_string())));
    }

    #[test]
    fn completes_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Src")).unwrap();
        std::fs::write(dir.path().join("setup.py"), "").unwrap();
        std::fs::write(dir.path().join(".secret"), "").unwrap();
        std::fs::write(dir.path().join("other"), "").unwrap();
        let root = format!("{}/", dir.path().display());
        assert_eq!(complete(&format!("{root}s"), 10), vec![format!("{root}Src/"), format!("{root}setup.py")]);
        assert_eq!(complete(&root, 10), vec![format!("{root}Src/"), format!("{root}other"), format!("{root}setup.py")]);
        assert_eq!(complete(&format!("{root}."), 10), vec![format!("{root}.secret")]);
        assert_eq!(complete(&format!("{root}s"), 1).len(), 1);
        assert!(complete("no-folder-part", 10).is_empty());
    }
}
