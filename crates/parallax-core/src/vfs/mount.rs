//! Makes archives inside any tree browsable as folders, including archives nested
//! in archives and archives stored in Git commits.
//!
//! Writes into an archive rewrite it and store the new archive file in the tree it
//! lives in; for a nested archive, that is a write into the outer archive.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use super::archive::{ArchiveFs, detect_format};
use super::{Change, Data, Entry, EntryKind, Vfs, join, split_parent};
use crate::progress::Progress;
use crate::{Error, Result};

/// Archive writes run one at a time in the whole process, whichever source they
/// come from: two rewrites of one archive from the same old version would lose the
/// first one's changes.
static WRITING: Mutex<()> = Mutex::new(());

pub struct ArchiveMount {
    base: Arc<dyn Vfs>,
    /// Whether archives found while listing are offered as folders. Paths that
    /// explicitly point into an archive are always resolved.
    expand: bool,
    cache: Arc<Mutex<HashMap<String, Arc<ArchiveFs>>>>,
    // Present only on a comparison-scoped view. No network metadata lookup per member.
    types: Option<Mutex<HashMap<String, bool>>>,
}

impl ArchiveMount {
    pub fn new(base: Arc<dyn Vfs>, expand: bool) -> Self {
        ArchiveMount { base, expand, cache: Arc::new(Mutex::new(HashMap::new())), types: None }
    }

    /// Maps a mount path to the provider that owns it and the path inside it.
    /// Returns `depth == 0` when no archive was entered.
    fn resolve(&self, path: &str) -> Result<(Arc<dyn Vfs>, String, usize)> {
        crate::vfs::check_rel(path)?;
        let mut vfs = self.base.clone();
        let mut inner = String::new();
        let mut outer = String::new();
        let mut depth = 0;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let candidate = join(&inner, part);
            outer = join(&outer, part);
            if let Some(format) = detect_format(part) {
                let stat = || vfs.stat(&candidate).is_ok_and(|e| e.kind == EntryKind::File);
                let is_file = match &self.types {
                    Some(types) => *types.lock().entry(outer.clone()).or_insert_with(stat),
                    None => stat(),
                };
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
    ) -> Result<Arc<ArchiveFs>> {
        // Serialize first opens too: parallel member reads share one parsed index.
        let mut cache = self.cache.lock();
        if let Some(archive) = cache.get(outer) {
            return Ok(archive.clone());
        }
        let label = format!("{}/{}", vfs.label(), inner);
        let archive = Arc::new(match vfs.local_path(inner) {
            Some(path) => ArchiveFs::open_file(&path, format, label)?,
            None => ArchiveFs::open(vfs.read(inner)?, format, label)?,
        });
        cache.insert(outer.to_string(), archive.clone());
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
    fn read_operation(&self) -> Option<Arc<dyn Vfs>> {
        Some(Arc::new(Self {
            base: self.base.clone(),
            expand: self.expand,
            cache: self.cache.clone(),
            types: Some(Mutex::new(HashMap::new())),
        }))
    }

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

    fn open(&self, path: &str) -> Result<Box<dyn std::io::Read + Send>> {
        let (parent, name) = split_parent(path);
        let (vfs, inner, _) = self.resolve(parent)?;
        vfs.open(&join(&inner, name))
    }

    fn slow_to_read(&self, path: &str) -> bool {
        let (parent, name) = split_parent(path);
        self.resolve(parent).is_ok_and(|(vfs, inner, _)| vfs.slow_to_read(&join(&inner, name)))
    }

    fn warm(&self, path: &str) {
        let (parent, name) = split_parent(path);
        if let Ok((vfs, inner, _)) = self.resolve(parent) {
            vfs.warm(&join(&inner, name));
        }
    }

    fn local_path(&self, path: &str) -> Option<PathBuf> {
        let (parent, _) = split_parent(path);
        match self.resolve(parent) {
            Ok((_, _, 0)) => self.base.local_path(path),
            _ => None,
        }
    }

    fn changed(&self, path: &str) {
        // An archive that was overwritten or deleted, or one inside a changed folder,
        // is opened again on next use.
        let prefix = format!("{path}/");
        self.cache.lock().retain(|outer, _| !path.is_empty() && outer != path && !outer.starts_with(&prefix));
        if let Some(types) = &self.types {
            types.lock().clear();
        }
        self.base.changed(path);
    }

    fn writable(&self) -> bool {
        self.base.writable()
    }

    fn apply(&self, changes: Vec<Change>, progress: &Progress) -> Result<()> {
        if !self.base.writable() {
            return Err(Error::Unsupported(format!("{} is read-only", self.label())));
        }
        let _writing = WRITING.lock();
        let (mut direct, nested) = split_by_archive(&*self.base, changes);
        for (outer, inner) in nested {
            // Always reopen the current container before a rewrite. Size/mtime can
            // stay unchanged when another tab or program replaces it.
            self.changed(&outer);
            let archive = self.open_archive(&*self.base, &outer, &outer, archive_format(&outer))?;
            let data = Data::from_vec(rewrite(&archive, inner, progress)?);
            direct.push(Change::Put { path: outer, kind: EntryKind::File, data, mtime: None });
        }
        let paths: Vec<String> = direct.iter().map(|c| c.path().to_string()).collect();
        let result = self.base.apply(direct, progress);
        for path in &paths {
            self.changed(path);
        }
        result
    }
}

fn archive_format(path: &str) -> super::archive::ArchiveFormat {
    detect_format(split_parent(path).1).expect("archive paths come from split_by_archive")
}

/// Sorts changes into those on `vfs` itself and those inside archives stored in it,
/// grouped by the archive's path, with paths relative to the archive.
fn split_by_archive(vfs: &dyn Vfs, changes: Vec<Change>) -> (Vec<Change>, BTreeMap<String, Vec<Change>>) {
    let mut direct = Vec::new();
    let mut nested: BTreeMap<String, Vec<Change>> = BTreeMap::new();
    let mut is_archive: HashMap<String, bool> = HashMap::new();
    for change in changes {
        let parts: Vec<&str> = change.path().split('/').collect();
        let mut prefix = String::new();
        let mut target = None;
        // The last part is what is written; an archive there is replaced as a file.
        for (i, part) in parts.iter().enumerate().take(parts.len().saturating_sub(1)) {
            prefix = join(&prefix, part);
            if detect_format(part).is_some()
                && *is_archive
                    .entry(prefix.clone())
                    .or_insert_with(|| vfs.stat(&prefix).is_ok_and(|e| e.kind == EntryKind::File))
            {
                target = Some((prefix.clone(), parts[i + 1..].join("/")));
                break;
            }
        }
        match target {
            Some((outer, rest)) => nested.entry(outer).or_default().push(change.with_path(rest)),
            None => direct.push(change),
        }
    }
    (direct, nested)
}

/// The bytes of `archive` with `changes` applied, which may enter nested archives.
fn rewrite(archive: &ArchiveFs, changes: Vec<Change>, progress: &Progress) -> Result<Vec<u8>> {
    let (mut direct, nested) = split_by_archive(archive, changes);
    for (outer, inner) in nested {
        let label = join(&archive.label(), &outer);
        let child = ArchiveFs::open(archive.read(&outer)?, archive_format(&outer), label)?;
        let data = Data::from_vec(rewrite(&child, inner, progress)?);
        direct.push(Change::Put { path: outer, kind: EntryKind::File, data, mtime: None });
    }
    archive.rewritten(direct, progress)
}
