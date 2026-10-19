//! File operations on writable sources: copy across sides and delete.
//!
//! Folders are written file by file. Inside an archive, all changes of one
//! operation are collected first, since every write rewrites the archive.

use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::compare::Filter;
use crate::progress::Progress;
use crate::source::Source;
use crate::vfs::local::{make_symlink, replace_file, same_file};
use crate::vfs::{Change, Data, Entry, EntryKind, join};
use crate::{Error, Result};

/// Copies each of `paths` from `from` to the same path in `to`, recursively for
/// folders, and returns the number of files copied. Archives are copied as files;
/// existing files are overwritten. All writes into one archive are one rewrite of it.
/// Inside folders, only the entries `filter` keeps are copied, as the comparison
/// showed them; `paths` themselves always are.
///
/// Cancelling through `progress` stops a copy into folders after the current
/// files, and a copy into an archive before the archive is replaced.
pub fn copy(from: &Source, to: &Source, paths: &[String], filter: &Filter, progress: &Progress) -> Result<u64> {
    let mut copied = 0;
    let mut into_archives = Vec::new();
    for rel in paths {
        let entry = from.stat(rel)?;
        let Some(target) = to.vfs.local_path(&to.path(rel)) else {
            if !to.vfs.writable() {
                return Err(Error::Unsupported(format!("{} is read-only", to.info.display)));
            }
            into_archives.extend(collect_changes(from, to, rel, entry, filter, progress)?);
            continue;
        };
        // A folder copied into itself would copy its own copies, without end.
        if entry.kind == EntryKind::Dir
            && let Some(source) = from.vfs.local_path(&from.path(rel))
            && inside(&target, &source)
        {
            return Err(Error::Unsupported(format!(
                "{} cannot be copied into itself ({})",
                source.display(),
                target.display()
            )));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent.display().to_string(), e))?;
        }
        if entry.kind != EntryKind::Dir {
            progress.expect(1);
        }
        let result = copy_entry(from, &entry, rel, &target, filter, progress);
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

/// Copies the file at `src` in `from` over the file at `dst` in `to`, e.g. to
/// a file that moved to another folder on the other side.
pub fn copy_file_as(from: &Source, to: &Source, src: &str, dst: &str, progress: &Progress) -> Result<u64> {
    let entry = from.stat(src)?;
    if entry.kind == EntryKind::Dir {
        return Err(Error::Unsupported("only files are copied onto another path".into()));
    }
    progress.expect(1);
    let Some(target) = to.vfs.local_path(&to.path(dst)) else {
        if !to.vfs.writable() {
            return Err(Error::Unsupported(format!("{} is read-only", to.info.display)));
        }
        let data = from.vfs.read(&from.path(src))?;
        let kind = if entry.kind == EntryKind::Symlink { EntryKind::Symlink } else { EntryKind::File };
        to.vfs.apply(vec![Change::Put { path: to.path(dst), kind, data, mtime: entry.mtime }], progress)?;
        progress.advance(1)?;
        return Ok(1);
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent.display().to_string(), e))?;
    }
    let result = copy_entry(from, &entry, src, &target, &Filter::none(), progress);
    to.vfs.changed(&to.path(dst));
    result
}

/// The writes that copy `rel` with everything below it; file contents are read in parallel.
fn collect_changes(
    from: &Source,
    to: &Source,
    rel: &str,
    entry: Entry,
    filter: &Filter,
    progress: &Progress,
) -> Result<Vec<Change>> {
    let mut found = vec![(rel.to_string(), entry)];
    let mut i = 0;
    while i < found.len() {
        progress.check()?;
        if found[i].1.kind == EntryKind::Dir {
            let dir = found[i].0.clone();
            for child in from.vfs.list(&from.path(&dir))? {
                let rel = join(&dir, &child.name);
                if filter.keeps(&rel, &child) {
                    found.push((rel, child));
                }
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
fn copy_entry(
    from: &Source,
    entry: &Entry,
    rel: &str,
    target: &Path,
    filter: &Filter,
    progress: &Progress,
) -> Result<u64> {
    let io_err = |e| Error::io(target.display().to_string(), e);
    progress.check()?;
    if entry.kind == EntryKind::Dir {
        fs::create_dir_all(target).map_err(io_err)?;
        let mut children = from.vfs.list(&from.path(rel))?;
        children.retain(|c| filter.keeps(&join(rel, &c.name), c));
        progress.expect(children.iter().filter(|c| c.kind != EntryKind::Dir).count() as u64);
        return children
            .par_iter()
            .map(|child| copy_entry(from, child, &join(rel, &child.name), &target.join(&child.name), filter, progress))
            .try_reduce(|| 0, |a, b| Ok(a + b));
    }
    let source_path = from.vfs.local_path(&from.path(rel));
    // The same file on both sides (one folder opened twice, a hard link): nothing to copy.
    // Writing it would truncate the file being read.
    if entry.kind != EntryKind::Symlink && source_path.as_deref().is_some_and(|p| same_file(p, target)) {
        progress.advance(1)?;
        return Ok(0);
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
        // Written next to the target and renamed over it: a symlink there is replaced, not
        // followed, and the target is only replaced once the copy is complete. The file
        // keeps the source's permissions (the executable bit), when the source is on disk.
        let permissions = source_path.and_then(|p| fs::metadata(p).ok()).map(|m| m.permissions());
        replace_file(target, &data, entry.mtime, permissions).map_err(io_err)?;
    }
    progress.advance(1)?;
    Ok(1)
}

/// Whether `path` is `folder` or inside it, also through symlinks. The parts of
/// `path` that do not exist yet are taken as written.
fn inside(path: &Path, folder: &Path) -> bool {
    let Ok(folder) = fs::canonicalize(folder) else { return false };
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while fs::symlink_metadata(&existing).is_err() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_owned());
                existing = parent.to_path_buf();
            }
            _ => return false,
        }
    }
    let Ok(mut resolved) = fs::canonicalize(&existing) else { return false };
    resolved.extend(rest.iter().rev());
    resolved.starts_with(&folder)
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
