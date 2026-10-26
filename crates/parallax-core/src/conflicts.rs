//! The conflicted files of a Git repository in the middle of a merge, rebase,
//! cherry-pick or revert, read from the index (stages 1, 2 and 3), and staging
//! their resolution.
//!
//! Git's own command line does the work: it knows every index format and
//! extension, and staging must go through it anyway so hooks and attributes apply.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use crate::{Error, Result};

/// What Git is in the middle of; it decides what "ours" and "theirs" mean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Operation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
    /// Unmerged paths without one of the above, e.g. after `git stash pop`.
    Other,
}

/// How a path conflicts, by which index stages it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictKind {
    BothModified,
    BothAdded,
    DeletedByUs,
    DeletedByThem,
    AddedByUs,
    AddedByThem,
    BothDeleted,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stage {
    pub mode: String,
    pub oid: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictFile {
    /// Path relative to the work tree, with `/`.
    pub path: String,
    pub kind: ConflictKind,
    /// Stages 1 (base), 2 (ours, local) and 3 (theirs, remote).
    pub stages: [Option<Stage>; 3],
}

/// Names for the three versions, as the user knows them.
#[derive(Debug, Clone, Serialize)]
pub struct Labels {
    pub local: String,
    pub base: String,
    pub remote: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoConflicts {
    pub workdir: PathBuf,
    pub operation: Operation,
    /// One line on what is going on: "Merging feature/x into main".
    pub summary: String,
    pub labels: Labels,
    pub files: Vec<ConflictFile>,
}

fn git(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0");
    let command = args.first().copied().unwrap_or_default();
    crate::process::run_bytes(&mut cmd, "git", &format!("git {command}"), Duration::from_secs(120))
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String> {
    git(dir, args).map(|out| String::from_utf8_lossy(&out).trim().to_string())
}

/// The conflict style the repository is configured for (`merge.conflictStyle`),
/// so a merge writes the markers Git would.
pub fn conflict_style(dir: &Path) -> crate::merge::MarkerStyle {
    use crate::merge::MarkerStyle;
    match git_text(dir, &["config", "--get", "merge.conflictStyle"]).as_deref() {
        Ok("diff3") => MarkerStyle::Diff3,
        Ok("zdiff3") => MarkerStyle::Zdiff3,
        _ => MarkerStyle::Merge,
    }
}

/// The work tree containing `dir`, or `None` when it is not in a Git work tree.
pub fn workdir(dir: &Path) -> Option<PathBuf> {
    git_text(dir, &["rev-parse", "--show-toplevel"]).ok().filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// Parses `git ls-files -u -z`: `mode oid stage\tpath` records.
fn parse_unmerged(out: &[u8]) -> Vec<ConflictFile> {
    let mut files: Vec<ConflictFile> = Vec::new();
    for record in out.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let record = String::from_utf8_lossy(record);
        let Some((meta, path)) = record.split_once('\t') else { continue };
        let mut parts = meta.split(' ');
        let (Some(mode), Some(oid), Some(stage)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let Ok(stage @ 1..=3) = stage.parse::<usize>() else { continue };
        let entry = Stage { mode: mode.to_string(), oid: oid.to_string() };
        match files.last_mut() {
            Some(last) if last.path == path => last.stages[stage - 1] = Some(entry),
            _ => {
                let mut stages = [None, None, None];
                stages[stage - 1] = Some(entry);
                files.push(ConflictFile { path: path.to_string(), kind: ConflictKind::BothModified, stages });
            }
        }
    }
    for file in &mut files {
        file.kind = match (&file.stages[0], &file.stages[1], &file.stages[2]) {
            (Some(_), Some(_), Some(_)) => ConflictKind::BothModified,
            (None, Some(_), Some(_)) => ConflictKind::BothAdded,
            (Some(_), Some(_), None) => ConflictKind::DeletedByThem,
            (Some(_), None, Some(_)) => ConflictKind::DeletedByUs,
            (None, Some(_), None) => ConflictKind::AddedByUs,
            (None, None, Some(_)) => ConflictKind::AddedByThem,
            _ => ConflictKind::BothDeleted,
        };
    }
    files
}

/// A ref or commit named as people know it: the branch, else a short id.
fn name_of(dir: &Path, rev: &str) -> String {
    let short = || git_text(dir, &["rev-parse", "--short", rev]).unwrap_or_else(|_| rev.chars().take(7).collect());
    git_text(dir, &["name-rev", "--name-only", "--no-undefined", "--exclude=tags/*", rev])
        .ok()
        .filter(|n| !n.is_empty() && !n.contains('~') && !n.contains('^'))
        .unwrap_or_else(short)
}

fn read(path: PathBuf) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// The conflicted files of the repository at `dir`, with what Git is doing and names
/// for local, base and remote. An empty list when nothing conflicts.
pub fn repo_conflicts(dir: &Path) -> Result<RepoConflicts> {
    let workdir =
        workdir(dir).ok_or_else(|| Error::Unsupported(format!("{} is not in a Git work tree", dir.display())))?;
    let git_dir = PathBuf::from(git_text(&workdir, &["rev-parse", "--absolute-git-dir"])?);
    let files = parse_unmerged(&git(&workdir, &["ls-files", "-u", "-z"])?);
    let head = git_text(&workdir, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| name_of(&workdir, "HEAD"));
    let (operation, summary, labels) = if git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir()
    {
        // During a rebase, "ours" is the branch rebased onto and "theirs" the commit replayed.
        let state = if git_dir.join("rebase-merge").is_dir() { "rebase-merge" } else { "rebase-apply" };
        let branch = read(git_dir.join(state).join("head-name"))
            .map(|h| h.trim_start_matches("refs/heads/").to_string())
            .unwrap_or_else(|| "detached HEAD".into());
        let onto =
            read(git_dir.join(state).join("onto")).map(|o| name_of(&workdir, &o)).unwrap_or_else(|| "upstream".into());
        let commit =
            read(git_dir.join("REBASE_HEAD")).map(|c| c.chars().take(7).collect::<String>()).unwrap_or_default();
        (
            Operation::Rebase,
            format!("Rebasing {branch} onto {onto}"),
            Labels {
                local: format!("{onto} (upstream)"),
                base: if commit.is_empty() { "base".into() } else { format!("parent of {commit}") },
                remote: if commit.is_empty() {
                    format!("{branch} (your commit)")
                } else {
                    format!("{commit} ({branch})")
                },
            },
        )
    } else if let Some(merge_head) = read(git_dir.join("MERGE_HEAD")) {
        let first = merge_head.lines().next().unwrap_or_default().to_string();
        let message = read(git_dir.join("MERGE_MSG")).unwrap_or_default();
        // "Merge branch 'feature/x'" names the branch as it was given to `git merge`.
        let theirs = message
            .lines()
            .next()
            .and_then(|l| l.split('\'').nth(1))
            .map(str::to_string)
            .unwrap_or_else(|| name_of(&workdir, &first));
        (
            Operation::Merge,
            format!("Merging {theirs} into {head}"),
            Labels { local: head.clone(), base: "merge base".into(), remote: theirs },
        )
    } else if let Some(pick) = read(git_dir.join("CHERRY_PICK_HEAD")) {
        let short: String = pick.chars().take(7).collect();
        (
            Operation::CherryPick,
            format!("Cherry-picking {short} onto {head}"),
            Labels { local: head.clone(), base: format!("parent of {short}"), remote: short },
        )
    } else if let Some(revert) = read(git_dir.join("REVERT_HEAD")) {
        let short: String = revert.chars().take(7).collect();
        (
            Operation::Revert,
            format!("Reverting {short} on {head}"),
            Labels { local: head.clone(), base: short.clone(), remote: format!("parent of {short}") },
        )
    } else {
        (
            Operation::Other,
            format!("Unmerged files on {head}"),
            Labels { local: format!("{head} (ours)"), base: "base".into(), remote: "theirs".into() },
        )
    };
    Ok(RepoConflicts { workdir, operation, summary, labels, files })
}

/// The content of one stage of a conflicted file.
pub fn stage_content(workdir: &Path, stage: &Stage) -> Result<Vec<u8>> {
    StageReader::new(workdir).read(stage)
}

/// Reads the contents of stages from the repository's object database in this
/// process, each object once: starting `git cat-file` for every stage of every
/// file takes seconds for a hundred conflicts.
pub struct StageReader {
    workdir: PathBuf,
    repo: Option<gix::Repository>,
    read: std::collections::HashMap<String, Vec<u8>>,
}

impl StageReader {
    pub fn new(workdir: &Path) -> Self {
        let repo = gix::open(workdir).ok();
        StageReader { workdir: workdir.to_path_buf(), repo, read: Default::default() }
    }

    pub fn read(&mut self, stage: &Stage) -> Result<Vec<u8>> {
        if !stage.oid.chars().all(|c| c.is_ascii_hexdigit()) || stage.oid.is_empty() {
            return Err(Error::Unsupported(format!("not an object id: {}", stage.oid)));
        }
        if let Some(data) = self.read.get(&stage.oid) {
            return Ok(data.clone());
        }
        let in_process = self.repo.as_ref().and_then(|repo| {
            let id = gix::ObjectId::from_hex(stage.oid.as_bytes()).ok()?;
            Some(repo.find_object(id).ok()?.detach().data)
        });
        // Git itself reads what this cannot, such as objects of another hash kind.
        let data = match in_process {
            Some(data) => data,
            None => git(&self.workdir, &["cat-file", "blob", &stage.oid])?,
        };
        self.read.insert(stage.oid.clone(), data.clone());
        Ok(data)
    }
}

/// The stages of one conflicted path, or `None` when it does not conflict: a
/// single `git ls-files` for when the rest of [`repo_conflicts`] is known.
pub fn conflict_file(workdir: &Path, path: &str) -> Result<Option<ConflictFile>> {
    let literal = format!(":(literal){path}");
    let files = parse_unmerged(&git(workdir, &["ls-files", "-u", "-z", "--", &literal])?);
    Ok(files.into_iter().find(|f| f.path == path))
}

/// The kind of file a Git mode records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileMode {
    Regular,
    Executable,
    Symlink,
    /// A submodule's commit.
    Gitlink,
}

impl FileMode {
    pub fn of(mode: &str) -> FileMode {
        match mode {
            "100755" => FileMode::Executable,
            "120000" => FileMode::Symlink,
            "160000" => FileMode::Gitlink,
            _ => FileMode::Regular,
        }
    }
}

impl Stage {
    pub fn file_mode(&self) -> FileMode {
        FileMode::of(&self.mode)
    }
}

/// Puts one version of a conflicted path into the work tree as the kind of file
/// it was: a symbolic link, an executable or a plain file. Submodules are left to
/// Git.
pub fn write_stage(workdir: &Path, path: &str, stage: &Stage) -> Result<()> {
    let target = workdir.join(path);
    match stage.file_mode() {
        FileMode::Gitlink => Err(Error::Unsupported(format!(
            "{path} is a submodule; resolve it with Git (check out the commit you want in the submodule, then git add it)"
        ))),
        FileMode::Symlink => {
            let link = stage_content(workdir, stage)?;
            if let Ok(meta) = std::fs::symlink_metadata(&target) {
                if meta.is_dir() {
                    return Err(Error::Unsupported(format!("{path} is a folder in the work tree")));
                }
                std::fs::remove_file(&target).map_err(|e| Error::io(path, e))?;
            }
            write_link(&target, &link).map_err(|e| Error::io(path, e))
        }
        mode => {
            let data = stage_content(workdir, stage)?;
            // A link in its place is replaced, not followed.
            if std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
                std::fs::remove_file(&target).map_err(|e| Error::io(path, e))?;
            }
            crate::vfs::local::replace_file(&target, &data, None, None).map_err(|e| Error::io(path, e))?;
            set_executable(&target, mode == FileMode::Executable).map_err(|e| Error::io(path, e))
        }
    }
}

#[cfg(unix)]
fn write_link(target: &Path, link: &[u8]) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(link), target)
}

/// Without symbolic links, Git writes the link's target into a plain file; so does this.
#[cfg(not(unix))]
fn write_link(target: &Path, link: &[u8]) -> std::io::Result<()> {
    std::fs::write(target, link)
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    let mode = perms.mode();
    // As Git does: executable for whoever may read it.
    let new = if executable { mode | ((mode & 0o444) >> 2) } else { mode & !0o111 };
    if new != mode {
        perms.set_mode(new);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_: &Path, _: bool) -> std::io::Result<()> {
    Ok(())
}

/// How a conflicted path is resolved when it is staged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// The file in the work tree, as it is now; with the kind of file to record
    /// when it is known (a whole version was taken). File systems without
    /// executable bits or links cannot tell Git that themselves.
    WorkTree(Option<FileMode>),
    /// The path is removed from the index and the work tree.
    Delete,
}

/// Stages the resolution of conflicted paths: `git add` or `git rm`. Only paths that
/// are conflicted now are accepted.
pub fn stage(workdir: &Path, paths: &[(String, Resolution)]) -> Result<()> {
    let conflicted = parse_unmerged(&git(workdir, &["ls-files", "-u", "-z"])?);
    for (path, _) in paths {
        if !conflicted.iter().any(|f| &f.path == path) {
            return Err(Error::Unsupported(format!("{path} is not a conflicted file")));
        }
    }
    let add: Vec<&str> =
        paths.iter().filter(|(_, r)| matches!(r, Resolution::WorkTree(_))).map(|(p, _)| p.as_str()).collect();
    let remove: Vec<&str> = paths.iter().filter(|(_, r)| *r == Resolution::Delete).map(|(p, _)| p.as_str()).collect();
    if !add.is_empty() {
        let mut args = vec!["add", "--"];
        args.extend(&add);
        git(workdir, &args)?;
    }
    for (path, resolution) in paths {
        let Resolution::WorkTree(Some(mode)) = resolution else { continue };
        match mode {
            FileMode::Executable => git(workdir, &["update-index", "--chmod=+x", "--", path]).map(drop)?,
            FileMode::Regular => git(workdir, &["update-index", "--chmod=-x", "--", path]).map(drop)?,
            // Recorded as a link even where the work tree has a plain file instead.
            FileMode::Symlink
                if !std::fs::symlink_metadata(workdir.join(path)).is_ok_and(|m| m.file_type().is_symlink()) =>
            {
                let oid = git_text(workdir, &["hash-object", "-w", "--", path])?;
                git(workdir, &["update-index", "--cacheinfo", &format!("120000,{oid},{path}")])?;
            }
            FileMode::Symlink | FileMode::Gitlink => {}
        }
    }
    if !remove.is_empty() {
        let mut args = vec!["rm", "-q", "--ignore-unmatch", "--"];
        args.extend(remove);
        git(workdir, &args)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(["-c", "user.email=t@example.com", "-c", "user.name=T", "-c", "init.defaultBranch=main"])
            .args(args)
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn parses_stages() {
        let out = [
            "100644 aaaa 1\tsrc/a.c",
            "100644 bbbb 2\tsrc/a.c",
            "100644 cccc 3\tsrc/a.c",
            "100644 dddd 1\tgone.c",
            "100644 eeee 2\tgone.c",
        ]
        .join("\0");
        let out = out.as_bytes();
        let files = parse_unmerged(out);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].kind, ConflictKind::BothModified);
        assert_eq!(files[1].kind, ConflictKind::DeletedByThem);
        assert_eq!(files[0].stages[2].as_ref().unwrap().oid, "cccc");
    }

    /// A real merge with a conflict, a delete/modify conflict and staging.
    #[test]
    fn merge_in_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        if !run(d, &["init", "-q"]) {
            return;
        }
        std::fs::write(d.join("a.txt"), "1\n2\n3\n").unwrap();
        std::fs::write(d.join("b.txt"), "keep\n").unwrap();
        assert!(run(d, &["add", "."]) && run(d, &["commit", "-qm", "base"]));
        assert!(run(d, &["checkout", "-qb", "feature/x"]));
        std::fs::write(d.join("a.txt"), "1\nX\n3\n").unwrap();
        assert!(run(d, &["rm", "-q", "b.txt"]) && run(d, &["commit", "-qam", "feature"]));
        assert!(run(d, &["checkout", "-q", "main"]));
        std::fs::write(d.join("a.txt"), "1\nM\n3\n").unwrap();
        std::fs::write(d.join("b.txt"), "changed\n").unwrap();
        assert!(run(d, &["commit", "-qam", "main"]));
        assert!(!run(d, &["merge", "-q", "feature/x"]));

        let state = repo_conflicts(d).unwrap();
        assert_eq!(state.operation, Operation::Merge);
        assert_eq!(state.summary, "Merging feature/x into main");
        assert_eq!((state.labels.local.as_str(), state.labels.remote.as_str()), ("main", "feature/x"));
        let kinds: Vec<_> = state.files.iter().map(|f| (f.path.as_str(), f.kind)).collect();
        assert_eq!(kinds, vec![("a.txt", ConflictKind::BothModified), ("b.txt", ConflictKind::DeletedByThem)]);
        let a = &state.files[0];
        assert_eq!(stage_content(&state.workdir, a.stages[2].as_ref().unwrap()).unwrap(), b"1\nX\n3\n");
        // Read in this process, each object once, as Git reads it.
        let mut reader = StageReader::new(&state.workdir);
        assert!(reader.repo.is_some());
        for stage in a.stages.iter().flatten() {
            let by_git = git(&state.workdir, &["cat-file", "blob", &stage.oid]).unwrap();
            assert_eq!(reader.read(stage).unwrap(), by_git);
        }
        assert_eq!(reader.read.len(), 3);
        assert!(reader.read(&Stage { mode: "100644".into(), oid: "--help".into() }).is_err());
        let one = conflict_file(&state.workdir, "a.txt").unwrap().unwrap();
        assert_eq!(one.stages[2].as_ref().unwrap().oid, a.stages[2].as_ref().unwrap().oid);
        assert!(conflict_file(&state.workdir, "a.*").unwrap().is_none());

        std::fs::write(d.join("a.txt"), "1\nM and X\n3\n").unwrap();
        stage(&state.workdir, &[("a.txt".into(), Resolution::WorkTree(None)), ("b.txt".into(), Resolution::Delete)])
            .unwrap();
        assert!(repo_conflicts(d).unwrap().files.is_empty());
        assert!(!d.join("b.txt").exists());
        assert!(stage(&state.workdir, &[("a.txt".into(), Resolution::WorkTree(None))]).is_err());
    }

    /// Taking a whole version keeps what kind of file it is: a link stays a link,
    /// the executable bit follows the version taken.
    #[cfg(unix)]
    #[test]
    fn whole_versions_keep_links_and_executable_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        if !run(d, &["init", "-q"]) {
            return;
        }
        std::os::unix::fs::symlink("base-target", d.join("link")).unwrap();
        std::fs::write(d.join("run.sh"), "echo base\n").unwrap();
        std::fs::set_permissions(d.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(run(d, &["add", "."]) && run(d, &["commit", "-qm", "base"]));
        assert!(run(d, &["checkout", "-qb", "feature"]));
        std::fs::remove_file(d.join("link")).unwrap();
        std::os::unix::fs::symlink("remote-target", d.join("link")).unwrap();
        std::fs::write(d.join("run.sh"), "echo remote\n").unwrap();
        assert!(run(d, &["commit", "-qam", "feature"]));
        assert!(run(d, &["checkout", "-q", "main"]));
        std::fs::remove_file(d.join("link")).unwrap();
        std::os::unix::fs::symlink("local-target", d.join("link")).unwrap();
        std::fs::write(d.join("run.sh"), "echo local\n").unwrap();
        std::fs::set_permissions(d.join("run.sh"), std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(run(d, &["commit", "-qam", "main"]));
        assert!(!run(d, &["merge", "-q", "feature"]));

        let state = repo_conflicts(d).unwrap();
        let file = |p: &str| state.files.iter().find(|f| f.path == p).unwrap().clone();
        let (link, script) = (file("link"), file("run.sh"));
        assert_eq!(link.stages[2].as_ref().unwrap().file_mode(), FileMode::Symlink);
        write_stage(d, "link", link.stages[2].as_ref().unwrap()).unwrap();
        assert_eq!(std::fs::read_link(d.join("link")).unwrap(), Path::new("remote-target"));
        let remote = script.stages[2].as_ref().unwrap();
        assert_eq!(remote.file_mode(), FileMode::Executable);
        write_stage(d, "run.sh", remote).unwrap();
        assert_ne!(std::fs::metadata(d.join("run.sh")).unwrap().permissions().mode() & 0o111, 0);
        stage(
            d,
            &[
                ("link".into(), Resolution::WorkTree(Some(FileMode::Symlink))),
                ("run.sh".into(), Resolution::WorkTree(Some(FileMode::Executable))),
            ],
        )
        .unwrap();
        let index = git_text(d, &["ls-files", "-s", "--", "link", "run.sh"]).unwrap();
        assert!(index.lines().any(|l| l.starts_with("120000 ") && l.ends_with("\tlink")), "{index}");
        assert!(index.lines().any(|l| l.starts_with("100755 ") && l.ends_with("\trun.sh")), "{index}");

        // Back to a plain file: the executable bit goes.
        write_stage(d, "run.sh", script.stages[1].as_ref().unwrap()).unwrap();
        assert_eq!(std::fs::metadata(d.join("run.sh")).unwrap().permissions().mode() & 0o111, 0);
    }
}
