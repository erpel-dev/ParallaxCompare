//! The versions a side of a comparison can be switched to when it lies in a Git
//! repository: the working tree, the staged files, the upstream branch, other
//! branches, tags and commits. Switching keeps the path inside the repository.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::source::SourceSpec;
use crate::vfs::git::{GitRepo, RefInfo};
use crate::{Error, Result};

/// The revision text that names the staged files (the index). `~` cannot start a
/// branch or tag name, so it never names a ref.
pub const STAGED: &str = "~staged";

/// One version of a side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Version {
    /// The files on disk, with uncommitted changes.
    Worktree,
    /// What `git add` staged: what the next commit would hold.
    Staged,
    /// A branch, tag, commit or revision expression such as `HEAD~2`.
    #[serde(rename_all = "camelCase")]
    Rev { rev: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Upstream {
    /// `origin/main`.
    pub name: String,
    pub short_id: String,
    /// When the repository last fetched (seconds since the Unix epoch), if known.
    pub fetched: Option<i64>,
}

/// What a side can be switched to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Versions {
    /// The work tree (or the repository as given, for one without).
    pub repo: String,
    /// The side's path inside the repository; empty for its root.
    pub path: String,
    pub current: Version,
    /// The checked-out branch, `None` when HEAD is detached.
    pub head: Option<String>,
    pub head_short: Option<String>,
    pub upstream: Option<Upstream>,
    /// Newest commit first.
    pub branches: Vec<RefInfo>,
    pub tags: Vec<RefInfo>,
    pub remotes: Vec<RefInfo>,
    /// The repository has a work tree (and staged files).
    pub has_worktree: bool,
}

/// The versions the side `spec` can be switched to; an error when it is not in a
/// local Git repository (a plain folder, a remote repository, a path in an archive).
pub fn versions(spec: &SourceSpec) -> Result<Versions> {
    let at = spec.git_location()?;
    let git = GitRepo::discover(&at.repo)?;
    let workdir = git.workdir();
    let current = match at.rev.as_deref() {
        None => Version::Worktree,
        Some(STAGED) => Version::Staged,
        Some(rev) => Version::Rev { rev: rev.to_string() },
    };
    let refs = git.refs(0)?;
    let head_short = git.resolve("HEAD").ok().and_then(|r| r.commit).map(|c| c.short_id);
    let upstream = refs.head.as_deref().and_then(|branch| upstream_of(&git, branch, &refs.remotes));
    let newest_first = |mut list: Vec<RefInfo>| {
        list.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| a.name.cmp(&b.name)));
        list
    };
    Ok(Versions {
        repo: match (&at.rev, &workdir) {
            (None, Some(w)) => w.display().to_string(),
            _ => at.repo.clone(),
        },
        path: at.path,
        current,
        head: refs.head,
        head_short,
        upstream,
        branches: newest_first(refs.branches),
        tags: newest_first(refs.tags),
        remotes: newest_first(refs.remotes),
        has_worktree: workdir.is_some(),
    })
}

/// The remote-tracking branch `branch` pulls from (`branch.<name>.remote` and
/// `.merge`), when the repository has fetched it.
fn upstream_of(git: &GitRepo, branch: &str, remotes: &[RefInfo]) -> Option<Upstream> {
    let (remote, merge) = git
        .with(|repo| {
            let config = repo.config_snapshot();
            let remote = config.string(format!("branch.{branch}.remote").as_str()).map(|v| v.to_string());
            let merge = config.string(format!("branch.{branch}.merge").as_str()).map(|v| v.to_string());
            Ok((remote, merge))
        })
        .ok()?;
    let (remote, merge) = (remote?, merge?);
    let name = format!("{remote}/{}", merge.strip_prefix("refs/heads/").unwrap_or(&merge));
    let found = remotes.iter().find(|r| r.name == name)?;
    // The last fetch; a fresh clone has no FETCH_HEAD, but its remote branches were
    // written when it was cloned.
    let git_dir = git.git_dir();
    let fetched = [git_dir.join("FETCH_HEAD"), git_dir.join("refs/remotes").join(&name), git_dir.join("packed-refs")]
        .iter()
        .find_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    Some(Upstream { name, short_id: found.short_id.clone(), fetched })
}

/// The side `spec` as of `version`, at the same path inside its repository.
pub fn with_version(spec: &SourceSpec, version: &Version) -> Result<SourceSpec> {
    let at = spec.git_location()?;
    let git = GitRepo::discover(&at.repo)?;
    let rev = |rev: &str| SourceSpec::GitRev { repo: at.repo.clone(), rev: rev.to_string(), subpath: at.path.clone() };
    Ok(match version {
        Version::Worktree => {
            let workdir =
                git.workdir().ok_or_else(|| Error::InvalidSource(format!("{} has no working tree", at.repo)))?;
            match spec {
                // A worktree side stays one: its view of the whole repository is kept.
                SourceSpec::GitWorktree { .. } => spec.clone(),
                _ if at.path.is_empty() => SourceSpec::Local { path: workdir.display().to_string() },
                _ => SourceSpec::Local { path: join_disk(&workdir, &at.path) },
            }
        }
        Version::Staged => rev(STAGED),
        Version::Rev { rev: r } => {
            // Checked here, so a typo is reported before the comparison changes.
            git.resolve(r)?;
            rev(r)
        }
    })
}

fn join_disk(dir: &Path, rel: &str) -> String {
    let mut path = dir.to_path_buf();
    path.extend(rel.split('/'));
    path.display().to_string()
}

/// How many tracked files of the working tree (below `path`, when given) differ from
/// HEAD or the index, as `git status` counts them; `None` when Git takes too long
/// or is not available.
pub fn changed_files(repo: &str, path: Option<&str>) -> Option<u32> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo).args([
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=no",
        "--ignore-submodules=dirty",
    ]);
    if let Some(path) = path.filter(|p| !p.is_empty()) {
        cmd.arg("--").arg(format!(":(literal){path}"));
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0").env("GIT_OPTIONAL_LOCKS", "0");
    let out = crate::process::run_bytes(&mut cmd, "git", "git status", Duration::from_secs(20)).ok()?;
    // Renames take two NUL-terminated paths; their status starts with R or C.
    let mut count = 0;
    let mut records = out.split(|&b| b == 0).filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        count += 1;
        if matches!(record.first(), Some(b'R' | b'C')) {
            records.next();
        }
    }
    Some(count)
}

/// Fetches the remotes of `repo` (`git fetch`), so the upstream branch is current.
/// Only on request: it needs the network and may ask for credentials, which fail
/// here instead of waiting for input.
pub fn fetch(repo: &str) -> Result<()> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(repo).args(["fetch", "--quiet"]);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    crate::process::run_bytes(&mut cmd, "git", "git fetch", Duration::from_secs(300)).map(|_| ())
}

/// The commit both `a` and `b` (revisions of `repo`) descend from.
pub fn merge_base(repo: &str, a: &str, b: &str) -> Result<String> {
    let git = GitRepo::discover(repo)?;
    let base = git.merge_base(a, b)?;
    Ok(git.resolve(&base)?.commit.map_or(base, |c| c.short_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceOptions;
    use std::fs;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn switching_keeps_the_path_inside_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let (origin, repo) = (dir.path().join("origin"), dir.path().join("work"));
        fs::create_dir_all(origin.join("src")).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        fs::write(origin.join("src/a.txt"), "v1\n").unwrap();
        git(&origin, &["add", "-A"]);
        git(&origin, &["commit", "-q", "-m", "one"]);
        git(dir.path(), &["clone", "-q", origin.to_str().unwrap(), "work"]);
        git(&repo, &["tag", "v1"]);
        fs::write(repo.join("src/a.txt"), "v2\n").unwrap();
        git(&repo, &["commit", "-q", "-am", "two"]);
        fs::write(repo.join("src/a.txt"), "staged\n").unwrap();
        git(&repo, &["add", "-A"]);
        fs::write(repo.join("src/a.txt"), "on disk\n").unwrap();

        let file = SourceSpec::Local { path: repo.join("src/a.txt").display().to_string() };
        let v = versions(&file).unwrap();
        assert_eq!(v.current, Version::Worktree);
        assert_eq!(v.path, "src/a.txt");
        assert_eq!(v.head.as_deref(), Some("main"));
        let upstream = v.upstream.unwrap();
        assert_eq!(upstream.name, "origin/main");
        assert!(upstream.fetched.is_some(), "a clone has fetched");
        assert!(v.tags.iter().any(|t| t.name == "v1"));
        assert_eq!(changed_files(&v.repo, None), Some(1));
        assert_eq!(changed_files(&v.repo, Some("src/a.txt")), Some(1));
        assert_eq!(changed_files(&v.repo, Some("other.txt")), Some(0));

        let read = |spec: &SourceSpec| {
            let source = spec.open(SourceOptions::default()).unwrap();
            String::from_utf8(source.vfs.read(&source.root).unwrap().to_vec()).unwrap()
        };
        let at = |version: Version| with_version(&file, &version).unwrap();
        assert_eq!(read(&at(Version::Staged)), "staged\n");
        assert_eq!(read(&at(Version::Rev { rev: "HEAD".into() })), "v2\n");
        assert_eq!(read(&at(Version::Rev { rev: "origin/main".into() })), "v1\n");
        assert!(with_version(&file, &Version::Rev { rev: "no-such-branch".into() }).is_err());
        // Back from a revision to the file on disk.
        let staged = at(Version::Staged);
        assert_eq!(versions(&staged).unwrap().current, Version::Staged);
        let back = with_version(&staged, &Version::Worktree).unwrap();
        assert_eq!(read(&back), "on disk\n");

        // A folder side switches as a whole, and the staged files list as a tree.
        let folder = SourceSpec::Local { path: repo.join("src").display().to_string() };
        let staged = with_version(&folder, &Version::Staged).unwrap();
        let source = staged.open(SourceOptions::default()).unwrap();
        let names: Vec<String> = source.vfs.list(&source.root).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["a.txt"]);
        assert_eq!(merge_base(&v.repo, "HEAD", "origin/main").unwrap(), upstream.short_id);
    }

    #[test]
    fn folders_outside_repositories_have_no_versions() {
        let dir = tempfile::tempdir().unwrap();
        assert!(versions(&SourceSpec::Local { path: dir.path().display().to_string() }).is_err());
    }
}
