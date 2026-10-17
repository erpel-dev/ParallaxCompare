//! The local filesystem, optionally filtered by `.gitignore` rules.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parking_lot::{Mutex, RwLock};

use super::{Data, Entry, EntryKind, Vfs, find_entry, root_entry, split_parent};
use crate::{Error, Result};

/// Files below this size are read into memory; larger ones are memory-mapped.
const MMAP_THRESHOLD: u64 = 256 * 1024;

pub struct LocalFs {
    root: PathBuf,
    /// When set, the tree was walked once up front with `.gitignore` rules applied.
    /// Copies and deletes update it through [`Vfs::changed`].
    snapshot: Option<RwLock<HashMap<String, Vec<Entry>>>>,
}

impl LocalFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalFs { root: root.into(), snapshot: None }
    }

    /// Walks the whole tree in parallel, honouring `.gitignore`, `.git/info/exclude`
    /// and `.ignore` files. `.git` directories are always skipped.
    ///
    /// `tracked` lists files (relative to `root`) that Git tracks: like `git status`,
    /// they are shown even when an ignore rule matches them.
    pub fn with_gitignore(root: impl Into<PathBuf>, tracked: &[String]) -> Result<Self> {
        let root = root.into();
        let mut dirs: HashMap<String, Vec<Entry>> = HashMap::new();
        dirs.insert(String::new(), Vec::new());
        insert_walked(&mut dirs, walk(&root, &root, None));
        add_tracked(&root, &mut dirs, tracked);
        Ok(LocalFs { root, snapshot: Some(RwLock::new(dirs)) })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn abs(&self, path: &str) -> PathBuf {
        let mut p = self.root.clone();
        p.extend(path.split('/').filter(|p| !p.is_empty()));
        p
    }
}

/// Walks the tree below `start` (a folder inside `root`) in parallel with ignore
/// rules applied, returning each entry with its path relative to `root`.
fn walk(root: &Path, start: &Path, max_depth: Option<usize>) -> Vec<(String, Entry)> {
    let found = Mutex::new(Vec::new());
    let walker = ignore::WalkBuilder::new(start)
        .hidden(false)
        .git_global(false)
        .require_git(true)
        .max_depth(max_depth)
        .filter_entry(|e| e.file_name() != ".git")
        .build_parallel();
    walker.run(|| {
        let found = &found;
        Box::new(move |result| {
            let Ok(dent) = result else {
                return ignore::WalkState::Continue;
            };
            if dent.depth() == 0 {
                return ignore::WalkState::Continue;
            }
            let Ok(rel) = dent.path().strip_prefix(root) else {
                return ignore::WalkState::Continue;
            };
            if let Some(entry) = entry_from_path(dent.path(), dent.file_name().to_string_lossy().into_owned()) {
                found.lock().push((path_to_rel(rel), entry));
            }
            ignore::WalkState::Continue
        })
    });
    found.into_inner()
}

fn insert_walked(dirs: &mut HashMap<String, Vec<Entry>>, walked: Vec<(String, Entry)>) {
    for (rel, entry) in walked {
        if entry.kind == EntryKind::Dir {
            dirs.entry(rel.clone()).or_default();
        }
        dirs.entry(split_parent(&rel).0.to_string()).or_default().push(entry);
    }
}

/// Adds tracked files (and their folders) that ignore rules removed from the walk.
fn add_tracked(root: &Path, dirs: &mut HashMap<String, Vec<Entry>>, tracked: &[String]) {
    let mut present: HashSet<String> = HashSet::with_capacity(tracked.len());
    for (dir, entries) in dirs.iter() {
        for entry in entries {
            present.insert(super::join(dir, &entry.name));
        }
    }
    for path in tracked {
        if present.contains(path) {
            continue;
        }
        // Make sure every folder on the way exists, then the file itself.
        let mut parent = String::new();
        for part in path.split('/') {
            let child = super::join(&parent, part);
            if !present.contains(&child) {
                let mut abs = root.to_path_buf();
                abs.extend(child.split('/'));
                let Some(entry) = entry_from_path(&abs, part.to_string()) else { break };
                if entry.kind == EntryKind::Dir {
                    dirs.entry(child.clone()).or_default();
                }
                dirs.entry(parent.clone()).or_default().push(entry);
                present.insert(child.clone());
            }
            parent = child;
        }
    }
}

fn path_to_rel(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

fn to_unix(time: std::io::Result<SystemTime>) -> Option<i64> {
    let time = time.ok()?;
    Some(match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    })
}

fn entry_from_path(path: &Path, name: String) -> Option<Entry> {
    let meta = fs::symlink_metadata(path).ok()?;
    // Symlinks are not followed: like Git and tar, a link is compared by its target
    // path. Following them would also loop forever on links to a parent folder.
    let (kind, size) = if meta.file_type().is_symlink() {
        (EntryKind::Symlink, link_target(path).map_or(0, |t| t.len() as u64))
    } else if meta.is_dir() {
        (EntryKind::Dir, 0)
    } else {
        (EntryKind::File, meta.len())
    };
    Some(Entry { name, kind, size, mtime: to_unix(meta.modified()), quick_id: None })
}

/// A symlink's target as Git stores it: the raw path text, with `/` separators.
fn link_target(path: &Path) -> std::io::Result<Vec<u8>> {
    let target = fs::read_link(path)?.to_string_lossy().into_owned();
    Ok(if cfg!(windows) { target.replace('\\', "/") } else { target }.into_bytes())
}

impl Vfs for LocalFs {
    fn label(&self) -> String {
        self.root.display().to_string()
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        if let Some(snapshot) = &self.snapshot {
            return snapshot.read().get(dir).cloned().ok_or_else(|| Error::NotFound(dir.to_string()));
        }
        let abs = self.abs(dir);
        let rd = fs::read_dir(&abs).map_err(|e| Error::io(abs.display().to_string(), e))?;
        let mut out = Vec::new();
        for dent in rd {
            let dent = dent.map_err(|e| Error::io(abs.display().to_string(), e))?;
            let name = dent.file_name().to_string_lossy().into_owned();
            if let Some(entry) = entry_from_path(&dent.path(), name) {
                out.push(entry);
            }
        }
        Ok(out)
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        if path.is_empty() {
            return Ok(root_entry());
        }
        if let Some(snapshot) = &self.snapshot {
            return find_entry(&snapshot.read(), path);
        }
        let (_, name) = split_parent(path);
        entry_from_path(&self.abs(path), name.to_string()).ok_or_else(|| Error::NotFound(path.to_string()))
    }

    fn read(&self, path: &str) -> Result<Data> {
        let abs = self.abs(path);
        let err = |e| Error::io(abs.display().to_string(), e);
        if fs::symlink_metadata(&abs).map_err(err)?.file_type().is_symlink() {
            return link_target(&abs).map(Data::from_vec).map_err(err);
        }
        let file = fs::File::open(&abs).map_err(err)?;
        let len = file.metadata().map_err(err)?.len();
        if len < MMAP_THRESHOLD {
            let mut buf = Vec::with_capacity(len as usize);
            std::io::Read::read_to_end(&mut &file, &mut buf).map_err(err)?;
            return Ok(Data::from_vec(buf));
        }
        // SAFETY: the map is read-only; if another process truncates the file while
        // it is mapped, reads may fault, which is the accepted trade-off of every
        // mmap-based diff tool.
        let map = unsafe { memmap2::Mmap::map(&file) }.map_err(err)?;
        Ok(Data::from_mmap(map))
    }

    fn local_path(&self, path: &str) -> Option<PathBuf> {
        Some(self.abs(path))
    }

    /// Updates the snapshot at `path` from disk, with the same ignore rules as the first walk.
    fn changed(&self, path: &str) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if path.is_empty() {
            return;
        }
        let (parent, name) = split_parent(path);
        let was_listed = {
            let dirs = snapshot.read();
            if !dirs.contains_key(parent) {
                // The parent folder was created as well.
                drop(dirs);
                return self.changed(parent);
            }
            dirs[parent].iter().any(|e| e.name == name)
        };
        // Listing the parent tells whether ignore rules hide `path`. An entry that was
        // already shown stays (Git-tracked files are shown even when ignored).
        let entry = walk(&self.root, &self.abs(parent), Some(1))
            .into_iter()
            .find(|(rel, _)| rel == path)
            .map(|(_, e)| e)
            .or_else(|| was_listed.then(|| entry_from_path(&self.abs(path), name.to_string())).flatten());
        let below = match &entry {
            Some(e) if e.kind == EntryKind::Dir => walk(&self.root, &self.abs(path), None),
            _ => Vec::new(),
        };

        let mut dirs = snapshot.write();
        let prefix = format!("{path}/");
        dirs.retain(|dir, _| dir != path && !dir.starts_with(&prefix));
        let siblings = dirs.entry(parent.to_string()).or_default();
        siblings.retain(|e| e.name != name);
        if let Some(entry) = entry {
            insert_walked(&mut dirs, vec![(path.to_string(), entry)]);
            insert_walked(&mut dirs, below);
        }
    }
}
