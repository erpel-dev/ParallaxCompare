//! Virtual file system: one tree abstraction for folders, archives and Git revisions.
//!
//! Paths are relative, `/`-separated and never start with a slash; `""` is the root.

pub mod archive;
pub mod git;
pub mod local;
pub mod mount;

use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Dir,
    /// A file that can be browsed like a folder (zip, 7z, tar …).
    Archive,
    Symlink,
    /// A Git submodule (gitlink); only its commit id is known.
    Submodule,
}

impl EntryKind {
    /// Whether the entry has children that can be listed.
    pub fn is_container(self) -> bool {
        matches!(self, EntryKind::Dir | EntryKind::Archive)
    }
}

/// A cheap content fingerprint that a provider knows without reading the content.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum QuickId {
    /// Git blob or tree id: equal ids mean equal content, including whole subtrees.
    GitOid(gix::ObjectId),
    /// CRC32 of the uncompressed data, as stored in zip and 7z headers.
    Crc32(u32),
}

impl QuickId {
    /// `Some(true/false)` when both ids are of the same kind and can be compared.
    pub fn matches(&self, other: &QuickId) -> Option<bool> {
        match (self, other) {
            (QuickId::GitOid(a), QuickId::GitOid(b)) => Some(a == b),
            (QuickId::Crc32(a), QuickId::Crc32(b)) => Some(a == b),
            _ => None,
        }
    }

    /// Whether `data` hashes to this id.
    pub fn matches_data(&self, data: &[u8]) -> bool {
        match self {
            QuickId::GitOid(oid) => git::blob_id(data).as_ref() == Some(oid),
            QuickId::Crc32(crc) => crc32fast::hash(data) == *crc,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Modification time in seconds since the Unix epoch, when the source has one.
    pub mtime: Option<i64>,
    #[serde(skip)]
    pub quick_id: Option<QuickId>,
}

/// File contents: owned, memory-mapped or shared with a cache. Cheap to clone.
#[derive(Clone)]
pub struct Data(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl Data {
    pub fn from_vec(bytes: Vec<u8>) -> Self {
        Data(Arc::new(bytes))
    }

    pub fn from_mmap(map: memmap2::Mmap) -> Self {
        Data(Arc::new(map))
    }

    pub fn from_shared(bytes: Arc<Vec<u8>>) -> Self {
        Data(Arc::new(SharedVec(bytes)))
    }
}

struct SharedVec(Arc<Vec<u8>>);

impl AsRef<[u8]> for SharedVec {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Deref for Data {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

impl AsRef<[u8]> for Data {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl fmt::Debug for Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Data({} bytes)", self.len())
    }
}

/// A write to a tree, for [`Vfs::apply`].
#[derive(Debug, Clone)]
pub enum Change {
    /// Creates or replaces a file, a symlink (`data` is its target path) or a folder (no data).
    Put { path: String, kind: EntryKind, data: Data, mtime: Option<i64> },
    /// Deletes an entry, folders with their contents.
    Remove { path: String },
}

impl Change {
    pub fn path(&self) -> &str {
        match self {
            Change::Put { path, .. } | Change::Remove { path } => path,
        }
    }

    pub(crate) fn with_path(mut self, new: String) -> Self {
        match &mut self {
            Change::Put { path, .. } | Change::Remove { path } => *path = new,
        }
        self
    }
}

pub trait Vfs: Send + Sync {
    /// Human-readable description of the tree root.
    fn label(&self) -> String;
    /// Children of a container, unsorted.
    fn list(&self, dir: &str) -> Result<Vec<Entry>>;
    fn stat(&self, path: &str) -> Result<Entry>;
    fn read(&self, path: &str) -> Result<Data>;
    /// Real filesystem path of an entry, for providers that can be written to.
    fn local_path(&self, _path: &str) -> Option<PathBuf> {
        None
    }
    /// Tells a provider that caches its tree that `path` was written or deleted.
    fn changed(&self, _path: &str) {}
    /// Whether [`apply`](Self::apply) can write to this tree.
    fn writable(&self) -> bool {
        false
    }
    /// Writes `changes` in one go; for an archive, that is one rewrite of the archive.
    fn apply(&self, _changes: Vec<Change>) -> Result<()> {
        Err(crate::Error::Unsupported(format!("{} is read-only", self.label())))
    }
}

pub fn join(dir: &str, name: &str) -> String {
    match (dir.is_empty(), name.is_empty()) {
        (true, _) => name.to_string(),
        (_, true) => dir.to_string(),
        _ => format!("{dir}/{name}"),
    }
}

/// Splits `a/b/c` into `("a/b", "c")`.
pub fn split_parent(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

/// Normalizes a path coming from an archive or user input: `\` → `/`, no leading
/// slash, no `.` components. Returns `None` for paths that escape the root.
pub fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// Looks up an entry in a tree index that maps each folder to its children.
pub(crate) fn find_entry(dirs: &HashMap<String, Vec<Entry>>, path: &str) -> Result<Entry> {
    let (parent, name) = split_parent(path);
    dirs.get(parent)
        .and_then(|list| list.iter().find(|e| e.name == name))
        .cloned()
        .ok_or_else(|| crate::Error::NotFound(path.to_string()))
}

/// A root entry used when `stat("")` is called on a tree.
pub(crate) fn root_entry() -> Entry {
    Entry { name: String::new(), kind: EntryKind::Dir, size: 0, mtime: None, quick_id: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_rejects_traversal() {
        assert_eq!(normalize("./a\\b/").as_deref(), Some("a/b"));
        assert_eq!(normalize("/x/./y").as_deref(), Some("x/y"));
        assert_eq!(normalize("a/../b"), None);
    }

    #[test]
    fn join_and_split() {
        assert_eq!(join("", "a"), "a");
        assert_eq!(join("a", "b"), "a/b");
        assert_eq!(split_parent("a/b/c"), ("a/b", "c"));
        assert_eq!(split_parent("c"), ("", "c"));
    }
}
