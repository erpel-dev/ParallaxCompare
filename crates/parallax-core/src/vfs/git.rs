//! Git revisions as read-only trees, read straight from the object database.
//!
//! No checkout happens. Every tree and blob entry carries its object id as a
//! [`QuickId`], so identical subtrees between two revisions are recognised
//! without walking them.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use gix::ObjectId;
use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind as GitEntryKind;
use serde::Serialize;

use super::{Data, Entry, EntryKind, QuickId, Vfs, root_entry, split_parent};
use crate::{Error, Result};

static NEXT_REPO_ID: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// `gix::Repository` is not `Sync`; each worker thread keeps its own handle.
    static LOCAL_REPOS: RefCell<HashMap<usize, gix::Repository>> = RefCell::new(HashMap::new());
}

/// A repository handle that can be shared between threads.
#[derive(Clone)]
pub struct GitRepo {
    id: usize,
    shared: gix::ThreadSafeRepository,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefInfo {
    pub name: String,
    pub short_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitInfo {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    /// Commit time, seconds since the Unix epoch.
    pub time: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefList {
    pub head: Option<String>,
    pub branches: Vec<RefInfo>,
    pub remotes: Vec<RefInfo>,
    pub tags: Vec<RefInfo>,
    pub recent_commits: Vec<CommitInfo>,
}

#[derive(Debug, Clone)]
pub struct ResolvedRev {
    pub commit: Option<CommitInfo>,
    pub tree: ObjectId,
}

impl GitRepo {
    /// Opens the repository containing `path` (discovering upwards).
    pub fn discover(path: impl AsRef<Path>) -> Result<Self> {
        let shared = gix::ThreadSafeRepository::discover(path.as_ref()).map_err(Error::git)?;
        Ok(GitRepo { id: NEXT_REPO_ID.fetch_add(1, Ordering::Relaxed), shared })
    }

    pub fn with<T>(&self, f: impl FnOnce(&gix::Repository) -> Result<T>) -> Result<T> {
        LOCAL_REPOS.with(|repos| {
            let mut repos = repos.borrow_mut();
            let repo = repos.entry(self.id).or_insert_with(|| {
                let mut repo = self.shared.to_thread_local();
                repo.object_cache_size_if_unset(16 * 1024 * 1024);
                repo
            });
            f(repo)
        })
    }

    pub fn workdir(&self) -> Option<PathBuf> {
        self.shared.work_dir().map(Path::to_path_buf)
    }

    /// Paths of all files in the index (tracked files), relative to the worktree root.
    pub fn tracked_paths(&self) -> Result<Vec<String>> {
        self.with(|repo| {
            let index = match repo.open_index() {
                Ok(index) => index,
                // A fresh repository without an index tracks nothing yet.
                Err(_) if !repo.git_dir().join("index").exists() => return Ok(Vec::new()),
                Err(e) => return Err(Error::git(e)),
            };
            Ok(index.entries().iter().map(|e| e.path(&index).to_str_lossy().into_owned()).collect())
        })
    }

    pub fn git_dir(&self) -> PathBuf {
        self.shared.git_dir().to_path_buf()
    }

    pub fn resolve(&self, rev: &str) -> Result<ResolvedRev> {
        self.with(|repo| {
            let id = repo.rev_parse_single(rev).map_err(Error::git)?;
            let object = id.object().map_err(Error::git)?;
            match object.kind {
                gix::objs::Kind::Tree => Ok(ResolvedRev { commit: None, tree: object.id }),
                _ => {
                    let commit = object.peel_to_commit().map_err(Error::git)?;
                    let tree = commit.tree_id().map_err(Error::git)?.detach();
                    Ok(ResolvedRev { commit: Some(commit_info(&commit)), tree })
                }
            }
        })
    }

    /// The best common ancestor of two revisions.
    pub fn merge_base(&self, a: &str, b: &str) -> Result<String> {
        self.with(|repo| {
            let a = repo.rev_parse_single(a).map_err(Error::git)?.detach();
            let b = repo.rev_parse_single(b).map_err(Error::git)?.detach();
            let base = repo.merge_base(a, b).map_err(Error::git)?;
            base.map(|id| id.to_string()).ok_or_else(|| Error::Git("no common ancestor".into()))
        })
    }

    pub fn refs(&self, recent: usize) -> Result<RefList> {
        self.with(|repo| {
            let mut list = RefList {
                head: repo.head_name().ok().flatten().map(|n| n.shorten().to_string()),
                ..Default::default()
            };
            let platform = repo.references().map_err(Error::git)?;
            let collect = |iter: gix::reference::iter::Iter<'_, '_>| -> Vec<RefInfo> {
                let mut out: Vec<RefInfo> = iter
                    .filter_map(|r| r.ok())
                    .filter_map(|mut r| {
                        let name = r.name().shorten().to_string();
                        let id = r.peel_to_id().ok()?;
                        Some(RefInfo { name, short_id: id.shorten_or_id().to_string() })
                    })
                    .filter(|r| !r.name.ends_with("/HEAD"))
                    .collect();
                out.sort_by(|a, b| a.name.cmp(&b.name));
                out
            };
            list.branches = collect(platform.local_branches().map_err(Error::git)?);
            list.remotes = collect(platform.remote_branches().map_err(Error::git)?);
            list.tags = collect(platform.tags().map_err(Error::git)?);
            if recent > 0
                && let Ok(head) = repo.head_id()
            {
                let walk = head.ancestors().all().map_err(Error::git)?;
                for info in walk.take(recent).filter_map(|i| i.ok()) {
                    if let Ok(commit) = repo.find_commit(info.id) {
                        list.recent_commits.push(commit_info(&commit));
                    }
                }
            }
            Ok(list)
        })
    }
}

impl GitRepo {
    /// The commits reachable from `rev` that changed the file at `path` (added,
    /// modified or deleted it), newest first. Renames are not followed. Stops after
    /// `limit` commits found or `scan` commits looked at.
    pub fn file_log(&self, rev: &str, path: &str, limit: usize, scan: usize) -> Result<Vec<CommitInfo>> {
        self.with(|repo| {
            let start = repo.rev_parse_single(rev).map_err(Error::git)?;
            let entry_at = |id: gix::ObjectId| -> Option<gix::ObjectId> {
                let tree = repo.find_commit(id).ok()?.tree().ok()?;
                tree.lookup_entry_by_path(path).ok().flatten().map(|e| e.object_id())
            };
            let mut out = Vec::new();
            let walk = start.ancestors().all().map_err(Error::git)?;
            for info in walk.take(scan).filter_map(|i| i.ok()) {
                let here = entry_at(info.id);
                let parent = info.parent_ids().next().map(|p| entry_at(p.detach()));
                let changed = match parent {
                    Some(before) => before != here,
                    None => here.is_some(),
                };
                if changed && let Ok(commit) = repo.find_commit(info.id) {
                    out.push(commit_info(&commit));
                    if out.len() >= limit {
                        break;
                    }
                }
            }
            Ok(out)
        })
    }
}

fn commit_info(commit: &gix::Commit<'_>) -> CommitInfo {
    let summary = commit.message().map(|m| m.summary().to_str_lossy().into_owned()).unwrap_or_default();
    let time = commit.time().map(|t| t.seconds).unwrap_or(0);
    CommitInfo { id: commit.id.to_string(), short_id: commit.id().shorten_or_id().to_string(), summary, time }
}

/// One Git tree (usually the tree of a commit) as a read-only VFS.
pub struct GitTreeFs {
    repo: GitRepo,
    tree: ObjectId,
    label: String,
}

impl GitTreeFs {
    pub fn new(repo: GitRepo, tree: ObjectId, label: impl Into<String>) -> Self {
        GitTreeFs { repo, tree, label: label.into() }
    }

    fn tree_at<'r>(&self, repo: &'r gix::Repository, dir: &str) -> Result<gix::Tree<'r>> {
        let root = repo.find_tree(self.tree).map_err(Error::git)?;
        if dir.is_empty() {
            return Ok(root);
        }
        let entry =
            root.lookup_entry_by_path(dir).map_err(Error::git)?.ok_or_else(|| Error::NotFound(dir.to_string()))?;
        if !entry.mode().is_tree() {
            return Err(Error::NotFound(format!("{dir} is not a folder")));
        }
        repo.find_tree(entry.object_id()).map_err(Error::git)
    }
}

fn to_entry(repo: &gix::Repository, name: String, kind: GitEntryKind, oid: ObjectId) -> Result<Entry> {
    let (kind, size) = match kind {
        GitEntryKind::Tree => (EntryKind::Dir, 0),
        GitEntryKind::Commit => (EntryKind::Submodule, 0),
        GitEntryKind::Link => (EntryKind::Symlink, repo.find_header(oid).map_err(Error::git)?.size()),
        GitEntryKind::Blob | GitEntryKind::BlobExecutable => {
            (EntryKind::File, repo.find_header(oid).map_err(Error::git)?.size())
        }
    };
    Ok(Entry { name, kind, size, mtime: None, quick_id: Some(QuickId::GitOid(oid)) })
}

impl Vfs for GitTreeFs {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        self.repo.with(|repo| {
            let tree = self.tree_at(repo, dir)?;
            let mut out = Vec::new();
            for entry in tree.iter() {
                let entry = entry.map_err(Error::git)?;
                let name = entry.filename().to_str_lossy().into_owned();
                out.push(to_entry(repo, name, entry.mode().kind(), entry.object_id())?);
            }
            Ok(out)
        })
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        if path.is_empty() {
            let mut root = root_entry();
            root.quick_id = Some(QuickId::GitOid(self.tree));
            return Ok(root);
        }
        self.repo.with(|repo| {
            let root = repo.find_tree(self.tree).map_err(Error::git)?;
            let entry = root
                .lookup_entry_by_path(path)
                .map_err(Error::git)?
                .ok_or_else(|| Error::NotFound(path.to_string()))?;
            let (_, name) = split_parent(path);
            to_entry(repo, name.to_string(), entry.mode().kind(), entry.object_id())
        })
    }

    fn read(&self, path: &str) -> Result<Data> {
        self.repo.with(|repo| {
            let root = repo.find_tree(self.tree).map_err(Error::git)?;
            let entry = root
                .lookup_entry_by_path(path)
                .map_err(Error::git)?
                .ok_or_else(|| Error::NotFound(path.to_string()))?;
            if entry.mode().is_tree() || entry.mode().is_commit() {
                return Err(Error::NotFound(format!("{path} is not a file")));
            }
            let mut blob = repo.find_blob(entry.object_id()).map_err(Error::git)?;
            Ok(Data::from_vec(blob.take_data()))
        })
    }
}

/// The Git blob id a file with this content would get, for comparing local files
/// against Git objects without reading the object.
pub fn blob_id(data: &[u8]) -> Option<ObjectId> {
    gix::objs::compute_hash(gix::hash::Kind::Sha1, gix::objs::Kind::Blob, data).ok()
}
