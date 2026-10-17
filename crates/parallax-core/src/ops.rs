//! File operations on writable sources: copy across sides and delete.
//!
//! Folders are written file by file. Inside an archive, all changes of one
//! operation are collected first, since every write rewrites the archive.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;

use crate::progress::Progress;
use crate::source::Source;
use crate::vfs::local::make_symlink;
use crate::vfs::{Change, Data, Entry, EntryKind, join};
use crate::{Error, Result};

/// Copies each of `paths` from `from` to the same path in `to`, recursively for
/// folders, and returns the number of files copied. Archives are copied as files;
/// existing files are overwritten. All writes into one archive are one rewrite of it.
///
/// Cancelling through `progress` stops a copy into folders after the current
/// files, and a copy into an archive before the archive is replaced.
pub fn copy(from: &Source, to: &Source, paths: &[String], progress: &Progress) -> Result<u64> {
    let mut copied = 0;
    let mut into_archives = Vec::new();
    for rel in paths {
        let entry = from.stat(rel)?;
        let Some(target) = to.vfs.local_path(&to.path(rel)) else {
            if !to.vfs.writable() {
                return Err(Error::Unsupported(format!("{} is read-only", to.info.display)));
            }
            into_archives.extend(collect_changes(from, to, rel, entry, progress)?);
            continue;
        };
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent.display().to_string(), e))?;
        }
        if entry.kind != EntryKind::Dir {
            progress.expect(1);
        }
        let result = copy_entry(from, &entry, rel, &target, progress);
        // Also after a partial copy: whatever was written is on disk now.
        to.vfs.changed(&to.path(rel));
        copied += result?;
    }
    if !into_archives.is_empty() {
        copied +=
            into_archives.iter().filter(|c| matches!(c, Change::Put { kind, .. } if *kind != EntryKind::Dir)).count()
                as u64;
        to.vfs.apply(into_archives, progress)?;
    }
    Ok(copied)
}

/// The writes that copy `rel` with everything below it; file contents are read in parallel.
fn collect_changes(from: &Source, to: &Source, rel: &str, entry: Entry, progress: &Progress) -> Result<Vec<Change>> {
    let mut found = vec![(rel.to_string(), entry)];
    let mut i = 0;
    while i < found.len() {
        progress.check()?;
        if found[i].1.kind == EntryKind::Dir {
            let dir = found[i].0.clone();
            for child in from.vfs.list(&from.path(&dir))? {
                found.push((join(&dir, &child.name), child));
            }
        }
        i += 1;
    }
    progress.expect(found.len() as u64);
    found
        .into_par_iter()
        .filter(|(_, e)| e.kind != EntryKind::Submodule)
        .map(|(rel, e)| {
            progress.advance(1)?;
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
fn copy_entry(from: &Source, entry: &Entry, rel: &str, target: &Path, progress: &Progress) -> Result<u64> {
    let io_err = |e| Error::io(target.display().to_string(), e);
    progress.check()?;
    if entry.kind == EntryKind::Dir {
        fs::create_dir_all(target).map_err(io_err)?;
        let children = from.vfs.list(&from.path(rel))?;
        progress.expect(children.iter().filter(|c| c.kind != EntryKind::Dir).count() as u64);
        return children
            .par_iter()
            .map(|child| copy_entry(from, child, &join(rel, &child.name), &target.join(&child.name), progress))
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
    } else {
        let mut file = fs::File::create(target).map_err(io_err)?;
        file.write_all(&data).map_err(io_err)?;
        if let Some(mtime) = entry.mtime.filter(|t| *t >= 0) {
            let _ = file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(mtime as u64));
        }
    }
    progress.advance(1)?;
    Ok(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// Into the Recycle Bin or Trash, where that is possible.
    Trash,
    Permanent,
}

/// Deletes each of `paths` from a writable source, folders with their contents.
/// Entries inside archives are always removed for good, in one rewrite per archive.
///
/// Returns the paths that could not be moved to the trash (for example on a
/// network drive); they are left in place.
pub fn delete(source: &Source, paths: &[String], removal: Removal, progress: &Progress) -> Result<Vec<String>> {
    let mut local: Vec<(String, PathBuf)> = Vec::new();
    let mut in_archives = Vec::new();
    for rel in paths {
        match source.vfs.local_path(&source.path(rel)) {
            Some(path) => local.push((rel.clone(), path)),
            None => in_archives.push(Change::Remove { path: source.path(rel) }),
        }
    }
    progress.expect(local.len() as u64);
    let mut not_trashed = Vec::new();
    for (rel, path) in &local {
        progress.check()?;
        let result = match removal {
            Removal::Trash => match trash::delete(path) {
                Ok(()) => Ok(()),
                Err(_) if fs::symlink_metadata(path).is_ok() => {
                    not_trashed.push(rel.clone());
                    Ok(())
                }
                // Already gone.
                Err(_) => Ok(()),
            },
            Removal::Permanent => match fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
                Ok(_) => fs::remove_file(path),
                Err(e) => Err(e),
            }
            .map_err(|e| Error::io(path.display().to_string(), e)),
        };
        source.vfs.changed(&source.path(rel));
        result?;
        progress.advance(1)?;
    }
    if !in_archives.is_empty() {
        source.vfs.apply(in_archives, progress)?;
    }
    Ok(not_trashed)
}
