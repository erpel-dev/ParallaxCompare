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
    fs::write(&target, &*data).map_err(|e| Error::io(target.display().to_string(), e))?;
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
