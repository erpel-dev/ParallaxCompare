//! File operations on writable sources: copy across sides and delete.
//!
//! Folders are written file by file. Inside an archive, all changes of one
//! operation are collected first, since every write rewrites the archive.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use rayon::prelude::*;

use crate::source::Source;
use crate::vfs::local::make_symlink;
use crate::vfs::{Change, Data, Entry, EntryKind, join};
use crate::{Error, Result};

/// Copies `rel` from `from` to the same relative path in `to`, recursively for
/// folders. Archives are copied as files. Existing files are overwritten.
pub fn copy(from: &Source, to: &Source, rel: &str) -> Result<u64> {
    let entry = from.stat(rel)?;
    let Some(target) = to.vfs.local_path(&to.path(rel)) else {
        if !to.vfs.writable() {
            return Err(Error::Unsupported(format!("{} is read-only", to.info.display)));
        }
        let changes = collect_changes(from, to, rel, entry)?;
        let copied =
            changes.iter().filter(|c| matches!(c, Change::Put { kind, .. } if *kind != EntryKind::Dir)).count();
        to.vfs.apply(changes)?;
        return Ok(copied as u64);
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent.display().to_string(), e))?;
    }
    let copied = copy_entry(from, &entry, rel, &target);
    // Also after a partial copy: whatever was written is on disk now.
    to.vfs.changed(&to.path(rel));
    copied
}

/// The writes that copy `rel` with everything below it; file contents are read in parallel.
fn collect_changes(from: &Source, to: &Source, rel: &str, entry: Entry) -> Result<Vec<Change>> {
    let mut found = vec![(rel.to_string(), entry)];
    let mut i = 0;
    while i < found.len() {
        if found[i].1.kind == EntryKind::Dir {
            let dir = found[i].0.clone();
            for child in from.vfs.list(&from.path(&dir))? {
                found.push((join(&dir, &child.name), child));
            }
        }
        i += 1;
    }
    found
        .into_par_iter()
        .filter(|(_, e)| e.kind != EntryKind::Submodule)
        .map(|(rel, e)| {
            let (kind, data) = match e.kind {
                EntryKind::Dir => (EntryKind::Dir, Data::from_vec(Vec::new())),
                EntryKind::Symlink => (EntryKind::Symlink, from.vfs.read(&from.path(&rel))?),
                _ => (EntryKind::File, from.vfs.read(&from.path(&rel))?),
            };
            Ok(Change::Put { path: to.path(&rel), kind, data, mtime: e.mtime })
        })
        .collect()
}

/// Copies one entry to `target`; the children of a folder in parallel.
fn copy_entry(from: &Source, entry: &Entry, rel: &str, target: &Path) -> Result<u64> {
    let io_err = |e| Error::io(target.display().to_string(), e);
    if entry.kind == EntryKind::Dir {
        fs::create_dir_all(target).map_err(io_err)?;
        let children = from.vfs.list(&from.path(rel))?;
        return children
            .par_iter()
            .map(|child| copy_entry(from, child, &join(rel, &child.name), &target.join(&child.name)))
            .try_reduce(|| 0, |a, b| Ok(a + b));
    }
    let data = from.vfs.read(&from.path(rel))?;
    if entry.kind == EntryKind::Symlink {
        // The data of a symlink is its target path.
        if fs::symlink_metadata(target).is_ok() {
            fs::remove_file(target).map_err(io_err)?;
        }
        let link_to = String::from_utf8_lossy(&data).into_owned();
        make_symlink(&link_to, target).map_err(io_err)?;
        return Ok(1);
    }
    let mut file = fs::File::create(target).map_err(io_err)?;
    file.write_all(&data).map_err(io_err)?;
    if let Some(mtime) = entry.mtime.filter(|t| *t >= 0) {
        let _ = file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime as u64));
    }
    Ok(1)
}

/// Deletes `rel` from a writable source (recursively for folders).
pub fn delete(source: &Source, rel: &str) -> Result<()> {
    let Some(path) = source.vfs.local_path(&source.path(rel)) else {
        return source.vfs.apply(vec![Change::Remove { path: source.path(rel) }]);
    };
    let meta = fs::symlink_metadata(&path).map_err(|e| Error::io(path.display().to_string(), e))?;
    let result = if meta.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
    source.vfs.changed(&source.path(rel));
    result.map_err(|e| Error::io(path.display().to_string(), e))
}
