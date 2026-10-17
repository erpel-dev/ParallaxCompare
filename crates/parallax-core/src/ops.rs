//! File operations on writable sources: copy across sides and delete.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime};

use rayon::prelude::*;

use crate::source::Source;
use crate::vfs::{Entry, EntryKind, join};
use crate::{Error, Result};

/// Copies `rel` from `from` to the same relative path in `to`, recursively for
/// folders. Archives are copied as files. Existing files are overwritten.
pub fn copy(from: &Source, to: &Source, rel: &str) -> Result<u64> {
    let entry = from.stat(rel)?;
    let target = to
        .vfs
        .local_path(&to.path(rel))
        .ok_or_else(|| Error::Unsupported(format!("{} is read-only", to.info.display)))?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent.display().to_string(), e))?;
    }
    let copied = copy_entry(from, &entry, rel, &target);
    // Also after a partial copy: whatever was written is on disk now.
    to.vfs.changed(&to.path(rel));
    copied
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
    let path = source
        .vfs
        .local_path(&source.path(rel))
        .ok_or_else(|| Error::Unsupported(format!("{} is read-only", source.info.display)))?;
    let meta = fs::symlink_metadata(&path).map_err(|e| Error::io(path.display().to_string(), e))?;
    let result = if meta.is_dir() { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
    source.vfs.changed(&source.path(rel));
    result.map_err(|e| Error::io(path.display().to_string(), e))
}

#[cfg(unix)]
fn make_symlink(link_to: &str, at: &std::path::Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(link_to, at)
}

/// Windows needs developer mode or admin rights for symlinks; the error is reported as is.
#[cfg(windows)]
fn make_symlink(link_to: &str, at: &std::path::Path) -> std::io::Result<()> {
    let target = link_to.replace('/', "\\");
    let resolved = at.parent().map(|p| p.join(&target));
    if resolved.is_some_and(|p| p.is_dir()) {
        std::os::windows::fs::symlink_dir(target, at)
    } else {
        std::os::windows::fs::symlink_file(target, at)
    }
}
