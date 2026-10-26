//! The local filesystem, optionally filtered by `.gitignore` rules.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parking_lot::{Mutex, RwLock};

use super::{Change, Data, Entry, EntryKind, Vfs, find_entry, root_entry, split_parent};
use crate::{Error, Result};

/// Files below this size are read into memory; larger ones are memory-mapped.
const MMAP_THRESHOLD: u64 = 256 * 1024;

pub struct LocalFs {
    root: PathBuf,
    /// When set, the tree was walked once up front with `.gitignore` rules applied.
    /// Copies and deletes update it through [`Vfs::changed`].
    snapshot: Option<RwLock<Snapshot>>,
    /// Files Git tracks, shown even where ignore rules match, for walking again.
    tracked: Vec<String>,
    /// Ignored files are listed, marked as ignored, instead of left out.
    mark_ignored: bool,
}

impl LocalFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        LocalFs { root: root.into(), snapshot: None, tracked: Vec::new(), mark_ignored: false }
    }

    /// Walks the whole tree in parallel, honouring `.gitignore`, `.git/info/exclude`
    /// and `.ignore` files. `.git` directories are always skipped.
    ///
    /// `tracked` lists files (relative to `root`) that Git tracks: like `git status`,
    /// they are shown even when an ignore rule matches them.
    ///
    /// With `hide` false, what the rules ignore is listed too, marked as
    /// [`Entry::ignored`]. Ignored folders are not walked up front: they are listed
    /// from disk when asked for, so a large build folder costs nothing until then.
    pub fn with_gitignore(root: impl Into<PathBuf>, tracked: &[String], hide: bool) -> Result<Self> {
        let root = root.into();
        let snapshot = snapshot_of(&root, tracked, !hide);
        Ok(LocalFs { root, snapshot: Some(RwLock::new(snapshot)), tracked: tracked.to_vec(), mark_ignored: !hide })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path of `rel` below the root, or `None` when `rel` names something
    /// outside the tree. Every lookup and write goes through here, so a path that
    /// escaped would have to get past `check_rel` first.
    fn abs(&self, path: &str) -> Option<PathBuf> {
        crate::vfs::check_rel(path).ok()?;
        let mut p = self.root.clone();
        p.extend(path.split('/').filter(|part| !part.is_empty() && *part != "."));
        Some(p)
    }

    /// The path of an entry, with the error a lookup or a write returns when `rel`
    /// names something outside the tree.
    fn abs_checked(&self, path: &str) -> Result<PathBuf> {
        self.abs(path).ok_or_else(|| Error::InvalidSource(path.to_string()))
    }
}

/// The tree as walked up front: the entries of each folder, and the folders that
/// could not be read, which list as errors instead of as empty folders.
#[derive(Default)]
struct Snapshot {
    dirs: HashMap<String, Vec<Entry>>,
    unreadable: HashMap<String, (std::io::ErrorKind, String)>,
}

impl Snapshot {
    fn insert(&mut self, walked: Walked) {
        insert_walked(&mut self.dirs, walked.entries);
        for (rel, kind, message) in walked.errors {
            // Only folders: other errors, such as a broken ignore file, leave the listing complete.
            if rel.is_empty() || self.dirs.contains_key(&rel) {
                self.unreadable.insert(rel, (kind, message));
            }
        }
    }
}

fn snapshot_of(root: &Path, tracked: &[String], mark_ignored: bool) -> Snapshot {
    let mut snapshot = Snapshot::default();
    snapshot.dirs.insert(String::new(), Vec::new());
    snapshot.insert(walk(root, root, None));
    add_tracked(root, &mut snapshot.dirs, tracked);
    if mark_ignored {
        let dirs: Vec<String> = snapshot.dirs.keys().cloned().collect();
        add_ignored(root, &mut snapshot, dirs);
    }
    snapshot
}

/// Adds what ignore rules left out of the walked folders `dirs`, marked as ignored.
fn add_ignored(root: &Path, snapshot: &mut Snapshot, dirs: Vec<String>) {
    for dir in dirs {
        if snapshot.unreadable.contains_key(&dir) {
            continue;
        }
        let mut abs = root.to_path_buf();
        abs.extend(dir.split('/').filter(|p| !p.is_empty()));
        let Ok(read) = fs::read_dir(&abs) else { continue };
        let Some(listed) = snapshot.dirs.get_mut(&dir) else { continue };
        let present: HashSet<String> = listed.iter().map(|e| e.name.clone()).collect();
        for dent in read.flatten() {
            let name = dent.file_name().to_string_lossy().into_owned();
            if name == ".git" || present.contains(&name) {
                continue;
            }
            if let Some(mut entry) = entry_from_path(&dent.path(), name) {
                entry.ignored = true;
                listed.push(entry);
            }
        }
    }
}

/// Whether `dir` is an ignored folder or inside one: those are not in the snapshot.
fn in_ignored(snapshot: &Snapshot, dir: &str) -> bool {
    let mut path = dir;
    while !path.is_empty() {
        let (parent, name) = split_parent(path);
        if let Some(listed) = snapshot.dirs.get(parent) {
            return listed.iter().any(|e| e.name == name && e.ignored && e.kind == EntryKind::Dir);
        }
        path = parent;
    }
    false
}

/// What a walk found: entries with their paths relative to the root, and the
/// paths that could not be read, with their error.
#[derive(Default)]
struct Walked {
    entries: Vec<(String, Entry)>,
    errors: Vec<(String, std::io::ErrorKind, String)>,
}

/// The path an error of the walk is about, if it names one.
fn error_path(err: &ignore::Error) -> Option<&Path> {
    match err {
        ignore::Error::WithPath { path, .. } => Some(path),
        ignore::Error::WithDepth { err, .. } | ignore::Error::WithLineNumber { err, .. } => error_path(err),
        _ => None,
    }
}

/// Walks the tree below `start` (a folder inside `root`) in parallel with ignore
/// rules applied, returning each entry with its path relative to `root`.
fn walk(root: &Path, start: &Path, max_depth: Option<usize>) -> Walked {
    let found = Mutex::new(Walked::default());
    let walker = ignore::WalkBuilder::new(start)
        .hidden(false)
        .git_global(false)
        .require_git(true)
        .max_depth(max_depth)
        .filter_entry(|e| e.file_name() != ".git")
        .build_parallel();
    walker.run(|| {
        let found = &found;
        Box::new(move |result| {
            let dent = match result {
                Ok(dent) => dent,
                Err(err) => {
                    // A folder that cannot be read must not look empty (and so equal).
                    if let (Some(io), Some(rel)) =
                        (err.io_error(), error_path(&err).and_then(|p| p.strip_prefix(root).ok()))
                    {
                        found.lock().errors.push((path_to_rel(rel), io.kind(), io.to_string()));
                    }
                    return ignore::WalkState::Continue;
                }
            };
            if dent.depth() == 0 {
                return ignore::WalkState::Continue;
            }
            let Ok(rel) = dent.path().strip_prefix(root) else {
                return ignore::WalkState::Continue;
            };
            if let Some(entry) = entry_from_path(dent.path(), dent.file_name().to_string_lossy().into_owned()) {
                found.lock().entries.push((path_to_rel(rel), entry));
            }
            ignore::WalkState::Continue
        })
    });
    found.into_inner()
}

fn insert_walked(dirs: &mut HashMap<String, Vec<Entry>>, walked: Vec<(String, Entry)>) {
    for (rel, entry) in walked {
        if entry.kind == EntryKind::Dir {
            dirs.entry(rel.clone()).or_default();
        }
        dirs.entry(split_parent(&rel).0.to_string()).or_default().push(entry);
    }
}

/// Adds tracked files (and their folders) that ignore rules removed from the walk.
fn add_tracked(root: &Path, dirs: &mut HashMap<String, Vec<Entry>>, tracked: &[String]) {
    let mut present: HashSet<String> = HashSet::with_capacity(tracked.len());
    for (dir, entries) in dirs.iter() {
        for entry in entries {
            present.insert(super::join(dir, &entry.name));
        }
    }
    for path in tracked {
        if present.contains(path) {
            continue;
        }
        // Make sure every folder on the way exists, then the file itself.
        let mut parent = String::new();
        for part in path.split('/') {
            let child = super::join(&parent, part);
            if !present.contains(&child) {
                let mut abs = root.to_path_buf();
                abs.extend(child.split('/'));
                let Some(entry) = entry_from_path(&abs, part.to_string()) else { break };
                if entry.kind == EntryKind::Dir {
                    dirs.entry(child.clone()).or_default();
                }
                dirs.entry(parent.clone()).or_default().push(entry);
                present.insert(child.clone());
            }
            parent = child;
        }
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
    let meta = fs::symlink_metadata(path).ok()?;
    // Symlinks are not followed: like Git and tar, a link is compared by its target
    // path. Following them would also loop forever on links to a parent folder.
    let (kind, size) = if meta.file_type().is_symlink() {
        (EntryKind::Symlink, link_target(path).map_or(0, |t| t.len() as u64))
    } else if meta.is_dir() {
        (EntryKind::Dir, 0)
    } else {
        (EntryKind::File, meta.len())
    };
    Some(Entry { name, kind, size, mtime: to_unix(meta.modified()), quick_id: None, ignored: false })
}

fn special_kind(kind: fs::FileType) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if kind.is_fifo() {
            return "named pipe";
        }
        if kind.is_socket() {
            return "socket";
        }
        if kind.is_block_device() || kind.is_char_device() {
            return "device";
        }
    }
    let _ = kind;
    "special file"
}

/// A symlink's target as Git stores it: the raw path text, with `/` separators.
fn link_target(path: &Path) -> std::io::Result<Vec<u8>> {
    let target = fs::read_link(path)?.to_string_lossy().into_owned();
    Ok(if cfg!(windows) { target.replace('\\', "/") } else { target }.into_bytes())
}

impl Vfs for LocalFs {
    fn label(&self) -> String {
        self.root.display().to_string()
    }

    fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        if let Some(snapshot) = &self.snapshot {
            let snapshot = snapshot.read();
            if let Some((kind, message)) = snapshot.unreadable.get(dir) {
                let abs = self.abs_checked(dir)?;
                return Err(Error::io(abs.display().to_string(), std::io::Error::new(*kind, message.clone())));
            }
            if let Some(listed) = snapshot.dirs.get(dir) {
                return Ok(listed.clone());
            }
            if !(self.mark_ignored && in_ignored(&snapshot, dir)) {
                return Err(Error::NotFound(dir.to_string()));
            }
            drop(snapshot);
            // Inside an ignored folder: everything is ignored, and read from disk.
            let listed = self.list_disk(dir)?;
            return Ok(listed.into_iter().filter(|e| e.name != ".git").map(|e| Entry { ignored: true, ..e }).collect());
        }
        self.list_disk(dir)
    }

    fn stat(&self, path: &str) -> Result<Entry> {
        if path.is_empty() {
            return Ok(root_entry());
        }
        if let Some(snapshot) = &self.snapshot {
            let snapshot = snapshot.read();
            let found = find_entry(&snapshot.dirs, path);
            if found.is_ok() || !(self.mark_ignored && in_ignored(&snapshot, split_parent(path).0)) {
                return found;
            }
        }
        let (_, name) = split_parent(path);
        let abs = self.abs_checked(path)?;
        // Past the snapshot only for what is inside an ignored folder.
        let ignored = self.snapshot.is_some();
        entry_from_path(&abs, name.to_string())
            .map(|e| Entry { ignored, ..e })
            .ok_or_else(|| Error::NotFound(path.to_string()))
    }

    fn read(&self, path: &str) -> Result<Data> {
        self.read_disk(path)
    }

    fn local_path(&self, path: &str) -> Option<PathBuf> {
        self.abs(path)
    }

    fn writable(&self) -> bool {
        true
    }

    fn apply(&self, changes: Vec<Change>, progress: &crate::progress::Progress) -> Result<()> {
        self.apply_changes(changes, progress)
    }

    fn changed(&self, path: &str) {
        self.update_snapshot(path)
    }
}

impl LocalFs {
    /// The entries of `dir` as they are on disk.
    fn list_disk(&self, dir: &str) -> Result<Vec<Entry>> {
        let abs = self.abs_checked(dir)?;
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

    fn read_disk(&self, path: &str) -> Result<Data> {
        let abs = self.abs_checked(path)?;
        let err = |e| Error::io(abs.display().to_string(), e);
        let kind = fs::symlink_metadata(&abs).map_err(err)?.file_type();
        if kind.is_symlink() {
            return link_target(&abs).map(Data::from_vec).map_err(err);
        }
        // Opening a named pipe waits for a writer, and devices never end: only files are read.
        if !kind.is_file() && !kind.is_dir() {
            return Err(Error::Unsupported(format!("{}: not a regular file ({})", abs.display(), special_kind(kind))));
        }
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

    fn apply_changes(&self, changes: Vec<Change>, progress: &crate::progress::Progress) -> Result<()> {
        progress.expect(changes.len() as u64);
        for change in changes {
            progress.check()?;
            let path = change.path().to_string();
            let abs = self.abs_checked(&path)?;
            let io_err = |e| Error::io(abs.display().to_string(), e);
            let result = match change {
                Change::Put { kind: EntryKind::Dir, .. } => fs::create_dir_all(&abs).map_err(io_err),
                Change::Put { kind: EntryKind::Symlink, data, .. } => {
                    if fs::symlink_metadata(&abs).is_ok() {
                        fs::remove_file(&abs).map_err(io_err)?;
                    }
                    make_symlink(&String::from_utf8_lossy(&data), &abs).map_err(io_err)
                }
                Change::Put { data, mtime, .. } => replace_file(&abs, &data, mtime, None).map_err(io_err),
                Change::Remove { .. } => match fs::symlink_metadata(&abs) {
                    Ok(meta) if meta.is_dir() => fs::remove_dir_all(&abs).map_err(io_err),
                    Ok(_) => fs::remove_file(&abs).map_err(io_err),
                    Err(_) => Ok(()),
                },
            };
            self.changed(&path);
            result?;
            progress.advance(1)?;
        }
        Ok(())
    }

    /// Updates the snapshot at `path` from disk, with the same ignore rules as the first walk.
    fn update_snapshot(&self, path: &str) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if path.is_empty() {
            *snapshot.write() = snapshot_of(&self.root, &self.tracked, self.mark_ignored);
            return;
        }
        let (parent, name) = split_parent(path);
        // Whether `path` was listed, and as ignored.
        let was_listed = {
            let snapshot = snapshot.read();
            if !snapshot.dirs.contains_key(parent) {
                // The parent folder was created as well, or `path` is inside an ignored folder.
                drop(snapshot);
                return self.update_snapshot(parent);
            }
            snapshot.dirs[parent].iter().find(|e| e.name == name).map(|e| e.ignored)
        };
        let (Some(parent_abs), Some(path_abs)) = (self.abs(parent), self.abs(path)) else {
            return;
        };
        // Listing the parent tells whether ignore rules hide `path`. An entry that was
        // already shown stays (Git-tracked files are shown even when ignored); others
        // are shown as ignored, or left out.
        let entry = walk(&self.root, &parent_abs, Some(1))
            .entries
            .into_iter()
            .find(|(rel, _)| rel == path)
            .map(|(_, e)| e)
            .or_else(|| {
                let on_disk = || entry_from_path(&path_abs, name.to_string());
                match was_listed {
                    Some(false) => on_disk(),
                    _ if self.mark_ignored && name != ".git" => on_disk().map(|e| Entry { ignored: true, ..e }),
                    _ => None,
                }
            });
        // Ignored folders are listed from disk when asked for, not walked.
        let walked_dir = entry.as_ref().is_some_and(|e| e.kind == EntryKind::Dir && !e.ignored);
        let below = if walked_dir { walk(&self.root, &path_abs, None) } else { Walked::default() };
        let mut new_dirs: Vec<String> = if walked_dir { vec![path.to_string()] } else { Vec::new() };
        new_dirs.extend(below.entries.iter().filter(|(_, e)| e.kind == EntryKind::Dir).map(|(rel, _)| rel.clone()));

        let mut snapshot = snapshot.write();
        let prefix = format!("{path}/");
        snapshot.dirs.retain(|dir, _| dir != path && !dir.starts_with(&prefix));
        snapshot.unreadable.retain(|dir, _| dir != path && !dir.starts_with(&prefix));
        let siblings = snapshot.dirs.entry(parent.to_string()).or_default();
        siblings.retain(|e| e.name != name);
        match entry {
            Some(entry) if entry.ignored => siblings.push(entry),
            Some(entry) => {
                insert_walked(&mut snapshot.dirs, vec![(path.to_string(), entry)]);
                snapshot.insert(below);
                if self.mark_ignored {
                    add_ignored(&self.root, &mut snapshot, new_dirs);
                }
            }
            None => {}
        }
    }
}

/// Writes a file through a temporary file in the same folder and renames it over
/// the old one, so a failed write leaves the old file intact, and a symlink at
/// `path` is replaced rather than written through. The file gets `permissions`,
/// or else those of the file it replaces.
pub fn replace_file(
    path: &Path,
    data: &[u8],
    mtime: Option<i64>,
    permissions: Option<fs::Permissions>,
) -> std::io::Result<()> {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // A name of its own for every write, created exclusively: two writers never share it.
    let (temp, mut file) = loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = path.with_file_name(format!(".{name}.{}-{n}.parallax-tmp", std::process::id()));
        match fs::OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(data)?;
        if let Some(mtime) = mtime.filter(|t| *t >= 0) {
            file.set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(mtime as u64))?;
        }
        file.sync_all()?;
        if let Some(permissions) = permissions.or_else(|| fs::metadata(path).ok().map(|m| m.permissions())) {
            file.set_permissions(permissions)?;
        }
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Whether `a` and `b` are the same file on disk, also through hard links,
/// symlinked folders or other spellings of the path. A symlink at `b` itself
/// counts as its own entry.
pub(crate) fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(meta_a), Ok(meta_b)) = (fs::metadata(a), fs::symlink_metadata(b)) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = (a, b);
        meta_a.dev() == meta_b.dev() && meta_a.ino() == meta_b.ino()
    }
    // Without stable file ids: the same path once links are resolved (hard links are not seen).
    #[cfg(not(unix))]
    {
        let _ = meta_a;
        !meta_b.is_symlink() && fs::canonicalize(a).ok().is_some_and(|p| fs::canonicalize(b).ok() == Some(p))
    }
}

#[cfg(unix)]
pub(crate) fn make_symlink(link_to: &str, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(link_to, at)
}

/// Windows needs developer mode or admin rights for symlinks; the error is reported as is.
#[cfg(windows)]
pub(crate) fn make_symlink(link_to: &str, at: &Path) -> std::io::Result<()> {
    let target = link_to.replace('/', "\\");
    let resolved = at.parent().map(|p| p.join(&target));
    if resolved.is_some_and(|p| p.is_dir()) {
        std::os::windows::fs::symlink_dir(target, at)
    } else {
        std::os::windows::fs::symlink_file(target, at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::Progress;

    /// Two writers at once each get their own temporary file: both succeed, and the
    /// file ends up as one of them wrote it, whole.
    #[test]
    fn concurrent_replacements_stay_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        let (a, b) = (vec![b'A'; 1 << 20], vec![b'B'; 1 << 21]);
        for _ in 0..10 {
            let barrier = std::sync::Barrier::new(2);
            let (ra, rb) = std::thread::scope(|s| {
                let wa = s.spawn(|| {
                    barrier.wait();
                    replace_file(&target, &a, None, None)
                });
                let wb = s.spawn(|| {
                    barrier.wait();
                    replace_file(&target, &b, None, None)
                });
                (wa.join().unwrap(), wb.join().unwrap())
            });
            ra.unwrap();
            rb.unwrap();
            let got = fs::read(&target).unwrap();
            assert!(got == a || got == b, "mixed content of {} bytes", got.len());
        }
        let left: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left, ["target.txt"], "temporary files left behind");
    }

    /// Every lookup and write is confined to the root, also when the path contains
    /// `..` or is absolute.
    #[test]
    fn paths_cannot_leave_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/inside.txt"), b"in").unwrap();
        fs::write(dir.path().join("outside.txt"), b"out").unwrap();
        let vfs = LocalFs::new(&root);

        assert_eq!(&*vfs.read("sub/inside.txt").unwrap(), b"in");
        for path in ["../outside.txt", "sub/../../outside.txt", "/etc/passwd", ".."] {
            assert!(matches!(vfs.read(path), Err(Error::InvalidSource(_))), "{path}");
            assert!(matches!(vfs.stat(path), Err(Error::InvalidSource(_))), "{path}");
            assert!(matches!(vfs.list(path), Err(Error::InvalidSource(_))), "{path}");
            assert!(vfs.local_path(path).is_none(), "{path}");
        }

        let write = |path: &str| {
            vfs.apply(
                vec![Change::Put {
                    path: path.to_string(),
                    kind: EntryKind::File,
                    data: Data::from_vec(b"written".to_vec()),
                    mtime: None,
                }],
                &Progress::default(),
            )
        };
        assert!(matches!(write("../outside.txt"), Err(Error::InvalidSource(_))));
        assert!(write("sub/../../outside.txt").is_err());
        assert_eq!(fs::read(dir.path().join("outside.txt")).unwrap(), b"out");
        write("sub/new.txt").unwrap();
        assert_eq!(fs::read(root.join("sub/new.txt")).unwrap(), b"written");
    }

    /// A folder the walk cannot read lists as an error, not as an empty folder that
    /// would compare equal to anything empty.
    #[cfg(unix)]
    #[test]
    fn unreadable_folders_are_errors_in_the_snapshot() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(std::process::Command::new("git").arg("init").arg("-q").arg(root).status().unwrap().success());
        fs::create_dir(root.join("restricted")).unwrap();
        fs::write(root.join("restricted/secret.txt"), b"secret").unwrap();
        fs::write(root.join("visible.txt"), b"visible").unwrap();
        fs::set_permissions(root.join("restricted"), fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_dir(root.join("restricted")).is_ok();
        let tracked = ["restricted/secret.txt".to_string(), "visible.txt".to_string()];
        let vfs = LocalFs::with_gitignore(root, &tracked, true).unwrap();
        let listed = vfs.list("restricted");
        fs::set_permissions(root.join("restricted"), fs::Permissions::from_mode(0o755)).unwrap();
        // Root reads every folder: there is nothing to test then.
        if readable {
            return;
        }
        assert!(matches!(listed, Err(Error::Io { .. })), "{listed:?}");
        assert_eq!(vfs.stat("restricted").unwrap().kind, EntryKind::Dir);
        assert!(vfs.list("").unwrap().iter().any(|e| e.name == "visible.txt"));
        // Once readable again, a refresh lists it.
        vfs.changed("restricted");
        assert_eq!(vfs.list("restricted").unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn named_pipes_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("mkfifo").arg(dir.path().join("pipe")).status().unwrap();
        assert!(status.success());
        let fs = LocalFs::new(dir.path().to_path_buf());
        let Err(err) = fs.read("pipe") else { panic!("a pipe is not read") };
        assert!(err.to_string().contains("named pipe"), "{err}");
    }
}
