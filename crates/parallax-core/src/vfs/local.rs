//! The local filesystem, optionally filtered by `.gitignore` rules.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parking_lot::Mutex;

use super::{Data, Entry, EntryKind, Vfs, root_entry, split_parent};
use crate::{Error, Result};

/// Files below this size are read into memory; larger ones are memory-mapped.
const MMAP_THRESHOLD: u64 = 256 * 1024;

pub struct LocalFs {
    root: PathBuf,
    /// When set, the tree was walked once up front with `.gitignore` rules applied.
    snapshot: Option<HashMap<String, Vec<Entry>>>,
}

impl LocalFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalFs { root: root.into(), snapshot: None }
    }

    /// Walks the whole tree in parallel, honouring `.gitignore`, `.git/info/exclude`
    /// and `.ignore` files. `.git` directories are always skipped.
    pub fn with_gitignore(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let dirs: Mutex<HashMap<String, Vec<Entry>>> = Mutex::new(HashMap::new());
        dirs.lock().insert(String::new(), Vec::new());

        let walker = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .git_global(false)
            .require_git(true)
            .filter_entry(|e| e.file_name() != ".git")
            .build_parallel();
        walker.run(|| {
            let dirs = &dirs;
            let root = &root;
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
                let rel = path_to_rel(rel);
                let Some(entry) = entry_from_path(dent.path(), dent.file_name().to_string_lossy().into_owned()) else {
                    return ignore::WalkState::Continue;
                };
                let (parent, _) = split_parent(&rel);
                let mut dirs = dirs.lock();
                if entry.kind == EntryKind::Dir {
                    dirs.entry(rel.clone()).or_default();
                }
                dirs.entry(parent.to_string()).or_default().push(entry);
                ignore::WalkState::Continue
            })
        });
        Ok(LocalFs { root, snapshot: Some(dirs.into_inner()) })
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
    let link_meta = fs::symlink_metadata(path).ok()?;
    // Follow symlinks so that linked files and folders compare by their target;
    // dangling links are shown as symlinks.
    let (meta, kind) = if link_meta.file_type().is_symlink() {
        match fs::metadata(path) {
            Ok(m) if m.is_dir() => (m, EntryKind::Dir),
            Ok(m) => (m, EntryKind::File),
            Err(_) => (link_meta, EntryKind::Symlink),
        }
    } else if link_meta.is_dir() {
        (link_meta, EntryKind::Dir)
    } else {
        (link_meta, EntryKind::File)
    };
    let size = if kind == EntryKind::Dir { 0 } else { meta.len() };
    Some(Entry { name, kind, size, mtime: to_unix(meta.modified()), quick_id: None })
}

impl Vfs for LocalFs {
    fn label(&self) -> String {
        self.root.display().to_string()
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        if let Some(snapshot) = &self.snapshot {
            return snapshot.get(dir).cloned().ok_or_else(|| Error::NotFound(dir.to_string()));
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
        if self.snapshot.is_some() {
            return super::stat_via_list(self, path);
        }
        let (_, name) = split_parent(path);
        entry_from_path(&self.abs(path), name.to_string()).ok_or_else(|| Error::NotFound(path.to_string()))
    }

    fn read(&self, path: &str) -> Result<Data> {
        let abs = self.abs(path);
        let err = |e| Error::io(abs.display().to_string(), e);
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
}
