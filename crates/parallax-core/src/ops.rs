//! File operations on writable sources: copy across sides and delete.

use std::fs;
use std::time::{Duration, SystemTime};

use crate::source::Source;
use crate::vfs::EntryKind;
use crate::{Error, Result};

/// Copies `rel` from `from` to the same relative path in `to`, recursively for
/// folders. Archives are copied as files. Existing files are overwritten.
pub fn copy(from: &Source, to: &Source, rel: &str) -> Result<u64> {
    let entry = from.stat(rel)?;
    let target_path = to.path(rel);
    let target = to
        .vfs
        .local_path(&target_path)
        .ok_or_else(|| Error::Unsupported(format!("{} is read-only", to.info.display)))?;
    if entry.kind == EntryKind::Dir {
        fs::create_dir_all(&target).map_err(|e| Error::io(target.display().to_string(), e))?;
        let mut copied = 0;
        for child in from.vfs.list(&from.path(rel))? {
            copied += copy(from, to, &crate::vfs::join(rel, &child.name))?;
        }
        return Ok(copied);
    }
    let data = from.vfs.read(&from.path(rel))?;
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent.display().to_string(), e))?;
    }
    let io_err = |e| Error::io(target.display().to_string(), e);
    if entry.kind == EntryKind::Symlink {
        // The data of a symlink is its target path.
        if fs::symlink_metadata(&target).is_ok() {
            fs::remove_file(&target).map_err(io_err)?;
        }
        let link_to = String::from_utf8_lossy(&data).into_owned();
        make_symlink(&link_to, &target).map_err(io_err)?;
        return Ok(1);
    }
    fs::write(&target, &*data).map_err(io_err)?;
    if let Some(mtime) = entry.mtime.filter(|t| *t >= 0) {
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(mtime as u64);
        if let Ok(file) = fs::File::options().write(true).open(&target) {
            let _ = file.set_modified(time);
        }
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
