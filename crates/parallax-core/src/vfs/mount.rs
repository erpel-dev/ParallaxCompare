//! Makes archives inside any tree browsable as folders, including archives nested
//! in archives and archives stored in Git commits.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use super::archive::{ArchiveFs, detect_format};
use super::{Data, Entry, EntryKind, Vfs, join, split_parent};
use crate::Result;

pub struct ArchiveMount {
    base: Arc<dyn Vfs>,
    /// Whether archives found while listing are offered as folders. Paths that
    /// explicitly point into an archive are always resolved.
    expand: bool,
    cache: Mutex<HashMap<String, Arc<ArchiveFs>>>,
}

impl ArchiveMount {
    pub fn new(base: Arc<dyn Vfs>, expand: bool) -> Self {
        ArchiveMount { base, expand, cache: Mutex::new(HashMap::new()) }
    }

    /// Maps a mount path to the provider that owns it and the path inside it.
    /// Returns `depth == 0` when no archive was entered.
    fn resolve(&self, path: &str) -> Result<(Arc<dyn Vfs>, String, usize)> {
        let mut vfs = self.base.clone();
        let mut inner = String::new();
        let mut outer = String::new();
        let mut depth = 0;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let candidate = join(&inner, part);
            outer = join(&outer, part);
            if let Some(format) = detect_format(part) {
                let is_file = vfs.stat(&candidate).map(|e| e.kind == EntryKind::File).unwrap_or(false);
                if is_file {
                    let archive = self.open_archive(&*vfs, &candidate, &outer, format)?;
                    vfs = archive;
                    inner.clear();
                    depth += 1;
                    continue;
                }
            }
            inner = candidate;
        }
        Ok((vfs, inner, depth))
    }

    fn open_archive(
        &self,
        vfs: &dyn Vfs,
        inner: &str,
        outer: &str,
        format: super::archive::ArchiveFormat,
    ) -> Result<Arc<dyn Vfs>> {
        if let Some(archive) = self.cache.lock().get(outer) {
            return Ok(archive.clone());
        }
        let data: Data = vfs.read(inner)?;
        let label = format!("{}/{}", vfs.label(), inner);
        let archive = Arc::new(ArchiveFs::open(data, format, label)?);
        self.cache.lock().insert(outer.to_string(), archive.clone());
        Ok(archive)
    }

    fn mark(&self, mut entry: Entry) -> Entry {
        if self.expand && entry.kind == EntryKind::File && detect_format(&entry.name).is_some() {
            entry.kind = EntryKind::Archive;
        }
        entry
    }
}

impl Vfs for ArchiveMount {
    fn label(&self) -> String {
        self.base.label()
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let (vfs, inner, _) = self.resolve(dir)?;
        Ok(vfs.list(&inner)?.into_iter().map(|e| self.mark(e)).collect())
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        let (parent, name) = split_parent(path);
        let (vfs, inner, _) = self.resolve(parent)?;
        vfs.stat(&join(&inner, name)).map(|e| self.mark(e))
    }

    fn read(&self, path: &str) -> Result<Data> {
        let (parent, name) = split_parent(path);
        let (vfs, inner, _) = self.resolve(parent)?;
        vfs.read(&join(&inner, name))
    }

    fn local_path(&self, path: &str) -> Option<PathBuf> {
        let (parent, _) = split_parent(path);
        match self.resolve(parent) {
            Ok((_, _, 0)) => self.base.local_path(path),
            _ => None,
        }
    }
}
