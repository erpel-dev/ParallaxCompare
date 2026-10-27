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
use crate::vfs::{Change, Data, Entry, EntryKind, check_rel, join};
use crate::{Error, Result};

/// Every path must name an entry inside the tree it is applied to, never the tree
/// itself: the UI and the CLI reach this code with paths they built as strings.
fn check_paths(paths: &[String]) -> Result<()> {
    paths.iter().try_for_each(|path| check_rel(path))
}

/// The same for paths that are written to or deleted: `""` is the compared root.
fn check_writable_path(paths: &[String]) -> Result<()> {
    check_paths(paths)?;
    if paths.iter().any(String::is_empty) {
        return Err(Error::InvalidSource("the compared folder itself".into()));
    }
    Ok(())
}

/// Copies each of `paths` from `from` to the same path in `to`, recursively for
/// folders, and returns the number of files copied. Archives are copied as files;
/// existing files are overwritten. All writes into one archive are one rewrite of it.
/// Inside folders, only the entries `filter` keeps are copied, as the comparison
/// showed them; `paths` themselves always are.
///
/// Cancelling through `progress` stops a copy into folders after the current
/// files, and a copy into an archive before the archive is replaced.
pub fn copy(from: &Source, to: &Source, paths: &[String], filter: &Filter, progress: &Progress) -> Result<u64> {
    check_writable_path(paths)?;
    let mut copied = 0;
    let mut into_archives = Vec::new();
    for rel in paths {
        let entry = from.stat(rel)?;
        let plan = plan_entry(from, to, rel, entry.clone(), true, filter, progress)?.expect("selected entry");
        let Some(target) = to.vfs.local_path(&to.path(rel)) else {
            if !to.vfs.writable() {
                return Err(Error::Unsupported(format!("{} is read-only", to.info.display)));
            }
            into_archives.extend(collect_changes(from, to, plan, progress)?);
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
        let result = copy_entry(from, &plan, &target, progress);
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
    check_writable_path(&[src.to_string(), dst.to_string()])?;
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
    let result = copy_entry(from, &CopyEntry { path: src.to_string(), entry, children: Vec::new() }, &target, progress);
    to.vfs.changed(&to.path(dst));
    result
}

/// A pruned source tree, built once before writing any of its directories.
struct CopyEntry {
    path: String,
    entry: Entry,
    children: Vec<CopyEntry>,
}

fn plan_entry(
    from: &Source,
    to: &Source,
    rel: &str,
    entry: Entry,
    selected: bool,
    filter: &Filter,
    progress: &Progress,
) -> Result<Option<CopyEntry>> {
    progress.check()?;
    if !selected && filter.excluded(rel) {
        return Ok(None);
    }
    let mut keep = selected || filter.included(rel);
    let mut children = Vec::new();
    if entry.kind == EntryKind::Dir {
        children = from
            .vfs
            .list(&from.path(rel))?
            .into_par_iter()
            .map(|child| {
                let path = join(rel, &child.name);
                plan_entry(from, to, &path, child, false, filter, progress)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        keep |= !children.is_empty();
    } else if !keep && entry.kind == EntryKind::Archive {
        // The comparison retains an archive when either side has a matching
        // member. Its displayed source archive is then copied as a whole.
        keep = archive_has_match(from, rel, filter, progress)?;
        if !keep {
            match to.stat(rel) {
                Ok(target) if target.kind == EntryKind::Archive => {
                    keep = archive_has_match(to, rel, filter, progress)?;
                }
                Err(Error::NotFound(_)) | Ok(_) => {}
                Err(e) => return Err(e),
            }
        }
    }
    Ok(keep.then(|| CopyEntry { path: rel.to_string(), entry, children }))
}

fn archive_has_match(from: &Source, dir: &str, filter: &Filter, progress: &Progress) -> Result<bool> {
    for child in from.vfs.list(&from.path(dir))? {
        progress.check()?;
        let rel = join(dir, &child.name);
        if filter.excluded(&rel) {
            continue;
        }
        if filter.included(&rel)
            || (matches!(child.kind, EntryKind::Dir | EntryKind::Archive)
                && archive_has_match(from, &rel, filter, progress)?)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Turns the same pruned plan into a single archive rewrite; reads run in parallel.
fn collect_changes(from: &Source, to: &Source, plan: CopyEntry, progress: &Progress) -> Result<Vec<Change>> {
    let mut pending = vec![plan];
    let mut found = Vec::new();
    while let Some(plan) = pending.pop() {
        progress.check()?;
        found.push((plan.path, plan.entry));
        pending.extend(plan.children);
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
fn copy_entry(from: &Source, plan: &CopyEntry, target: &Path, progress: &Progress) -> Result<u64> {
    let entry = &plan.entry;
    let rel = &plan.path;
    let io_err = |e| Error::io(target.display().to_string(), e);
    progress.check()?;
    if entry.kind == EntryKind::Dir {
        fs::create_dir_all(target).map_err(io_err)?;
        progress.expect(plan.children.iter().filter(|c| c.entry.kind != EntryKind::Dir).count() as u64);
        return plan
            .children
            .par_iter()
            .map(|child| copy_entry(from, child, &target.join(&child.entry.name), progress))
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
    check_writable_path(paths)?;
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
