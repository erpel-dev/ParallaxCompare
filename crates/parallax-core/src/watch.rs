//! Watching compared folders and files for changes made outside the app.
//!
//! Events are collected until the file system is quiet for a moment (or a second
//! has passed), mapped to compare paths, filtered, and merged: a burst of
//! thousands of changes becomes a few folders to compare again, or one request
//! to compare everything. Writes the app makes itself are announced with
//! [`Watcher::writing`] and not reported back.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher as _};
use parking_lot::Mutex;

use crate::source::WatchRoot;
use crate::{Error, Result};

/// Wait this long without events before reporting.
const QUIET: Duration = Duration::from_millis(200);
/// But report at least this often during a long burst.
const MAX_DELAY: Duration = Duration::from_secs(1);
/// More distinct paths than this are merged into their folders.
const MAX_PATHS: usize = 200;
/// How long after the app's own write its events are ignored.
const OWN_WRITE_GRACE: Duration = Duration::from_secs(2);

/// Changed compare paths, or `full` when everything should be compared again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub paths: Vec<String>,
    pub full: bool,
}

type Ignore = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub struct Watcher {
    _inner: Arc<Mutex<notify::RecommendedWatcher>>,
    own: Arc<Mutex<OwnWrites>>,
}

/// Paths the app is writing, or wrote until a moment ago.
#[derive(Default)]
struct OwnWrites {
    next_id: u64,
    /// Per write: its id, its paths, and when it stops counting (`None` while running).
    writes: Vec<(u64, Vec<String>, Option<Instant>)>,
}

impl OwnWrites {
    /// The paths of writes still counting; `""` for writes to a watched file root.
    fn active(&mut self) -> Vec<String> {
        let now = Instant::now();
        self.writes.retain(|(_, _, until)| until.is_none_or(|t| t > now));
        self.writes.iter().flat_map(|(_, paths, _)| paths.iter().cloned()).collect()
    }
}

/// Keeps the app's own write from being reported; ends a moment after it is dropped.
pub struct WriteGuard {
    own: Arc<Mutex<OwnWrites>>,
    id: u64,
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        let until = Instant::now() + OWN_WRITE_GRACE;
        for (id, _, end) in &mut self.own.lock().writes {
            if *id == self.id {
                *end = Some(until);
            }
        }
    }
}

impl Watcher {
    /// Watches the roots of both sides; `ignore` filters compare paths (excludes).
    /// `on_change` runs on a background thread. Watching stops when this is dropped.
    pub fn new(
        roots: &[Option<WatchRoot>],
        ignore: Ignore,
        on_change: impl Fn(Batch) + Send + 'static,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let inner =
            Arc::new(Mutex::new(notify::recommended_watcher(tx).map_err(|e| Error::Unsupported(e.to_string()))?));
        let roots: Vec<WatchRoot> = roots.iter().flatten().cloned().collect();
        for root in &roots {
            match root {
                WatchRoot::Tree { dir, gitignore } => watch_tree(&inner, dir, dir, *gitignore, &ignore)?,
                // The folder holding the file: editors and our own saves replace
                // files by renaming, which a watch on the file itself loses.
                WatchRoot::File(file) => {
                    let dir = file.parent().unwrap_or(Path::new("."));
                    watch_dir(&inner, dir, RecursiveMode::NonRecursive)?;
                }
            }
        }
        let watching = inner.clone();
        let own_writes: Arc<Mutex<OwnWrites>> = Arc::default();
        let own = own_writes.clone();
        std::thread::Builder::new()
            .name("watch".into())
            .spawn(move || {
                // Ends when the watcher, and with it the sender, is dropped.
                while let Ok(first) = rx.recv() {
                    let mut events = vec![first];
                    let started = Instant::now();
                    loop {
                        match rx.recv_timeout(QUIET) {
                            Ok(event) => events.push(event),
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                        if started.elapsed() > MAX_DELAY {
                            break;
                        }
                    }
                    let changes: Vec<notify::Event> = events.into_iter().flatten().filter(|e| !is_noise(e)).collect();
                    if PER_FOLDER {
                        watch_new_folders(&watching, &roots, &changes, &ignore);
                    }
                    let paths = changes.into_iter().flat_map(|e| e.paths);
                    if let Some(batch) = to_batch(&roots, paths, &*ignore, &own) {
                        on_change(batch);
                    }
                }
            })
            .map_err(|e| Error::io("watch thread", e))?;
        Ok(Watcher { _inner: inner, own: own_writes })
    }

    /// The app writes these compare paths until the guard is dropped (and their
    /// events arrive a moment later): they are not reported.
    pub fn writing(&self, paths: &[String]) -> WriteGuard {
        let mut own = self.own.lock();
        own.next_id += 1;
        let id = own.next_id;
        own.writes.push((id, paths.to_vec(), None));
        WriteGuard { own: self.own.clone(), id }
    }
}

/// Linux watches each folder on its own (inotify has no recursive watch), so the
/// folders that are not compared are left out; elsewhere one watch covers a tree.
const PER_FOLDER: bool = cfg!(target_os = "linux");

fn watch_dir(inner: &Mutex<notify::RecommendedWatcher>, dir: &Path, mode: RecursiveMode) -> Result<()> {
    inner.lock().watch(dir, mode).map_err(|e| Error::Unsupported(format!("cannot watch {}: {e}", dir.display())))
}

/// Watches the folder `start` inside the tree at `root`, and the folders below it
/// that the compare shows: not excluded, not hidden by `.gitignore`.
fn watch_tree(
    inner: &Mutex<notify::RecommendedWatcher>,
    root: &Path,
    start: &Path,
    gitignore: bool,
    ignore: &Ignore,
) -> Result<()> {
    if !PER_FOLDER {
        return watch_dir(inner, start, RecursiveMode::Recursive);
    }
    let dirs = Mutex::new(Vec::new());
    let (root, ignore) = (root.to_path_buf(), ignore.clone());
    ignore::WalkBuilder::new(start)
        .hidden(false)
        .git_global(false)
        .git_ignore(gitignore)
        .git_exclude(gitignore)
        .ignore(gitignore)
        .require_git(true)
        .filter_entry(move |e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            let rel = e.path().strip_prefix(&root).map(compare_path).unwrap_or_default();
            is_dir && e.file_name() != ".git" && (rel.is_empty() || !ignore(&rel))
        })
        .build_parallel()
        .run(|| {
            let dirs = &dirs;
            Box::new(move |entry| {
                if let Ok(entry) = entry {
                    dirs.lock().push(entry.into_path());
                }
                ignore::WalkState::Continue
            })
        });
    for dir in dirs.into_inner() {
        watch_dir(inner, &dir, RecursiveMode::NonRecursive)?;
    }
    Ok(())
}

/// Adds watches for folders that were created or moved into a watched tree.
fn watch_new_folders(
    inner: &Mutex<notify::RecommendedWatcher>,
    roots: &[WatchRoot],
    events: &[notify::Event],
    ignore: &Ignore,
) {
    use notify::EventKind::{Create, Modify};
    use notify::event::ModifyKind::Name;
    for event in events {
        if !matches!(event.kind, Create(_) | Modify(Name(_))) {
            continue;
        }
        for path in event.paths.iter().filter(|p| p.is_dir()) {
            for root in roots {
                if let WatchRoot::Tree { dir, gitignore } = root
                    && path.starts_with(dir)
                    && let Err(e) = watch_tree(inner, dir, path, *gitignore, ignore)
                {
                    eprintln!("{e}");
                }
            }
        }
    }
}

fn compare_path(rel: &Path) -> String {
    rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// Events that change nothing compared: opening and reading (a write also comes as
/// a modify event), and a folder's own modification, which Windows reports when
/// its contents change; the contents report themselves.
fn is_noise(event: &notify::Event) -> bool {
    use notify::EventKind::{Access, Modify};
    use notify::event::ModifyKind::Name;
    match event.kind {
        Access(_) => true,
        Modify(Name(_)) => false,
        Modify(_) => event.paths.iter().all(|p| p.is_dir()),
        _ => false,
    }
}

/// Maps file system paths to a batch of compare paths.
fn to_batch(
    roots: &[WatchRoot],
    paths: impl Iterator<Item = PathBuf>,
    ignore: &dyn Fn(&str) -> bool,
    own: &Mutex<OwnWrites>,
) -> Option<Batch> {
    let own = own.lock().active();
    let mut found = BTreeSet::new();
    for path in paths {
        for root in roots {
            match root {
                // A watched archive or file changes as a whole: any write of ours is it.
                WatchRoot::File(file) if path == *file && own.is_empty() => {
                    return Some(Batch { paths: vec![String::new()], full: true });
                }
                WatchRoot::File(_) => {}
                WatchRoot::Tree { dir, .. } => {
                    let Ok(rel) = path.strip_prefix(dir) else { continue };
                    let rel = compare_path(rel);
                    // The root folder's own metadata, our temporary files, and Git's churn.
                    if rel.is_empty()
                        || rel.ends_with(".parallax-tmp")
                        || rel.split('/').any(|p| p == ".git")
                        || ignore(&rel)
                        || own.iter().any(|p| overlaps(p, &rel))
                    {
                        continue;
                    }
                    found.insert(rel);
                }
            }
        }
    }
    if found.is_empty() {
        return None;
    }
    Some(match coalesce(found.into_iter().collect(), MAX_PATHS) {
        Some(paths) => Batch { paths, full: false },
        None => Batch { paths: Vec::new(), full: true },
    })
}

/// Whether one path is the other or inside it.
fn overlaps(a: &str, b: &str) -> bool {
    let within = |x: &str, y: &str| x == y || x.strip_prefix(y).is_some_and(|r| r.starts_with('/'));
    within(a, b) || within(b, a)
}

/// The outermost of `paths`, merged into their folders until at most `limit`
/// remain; `None` when even the top-level entries are too many.
pub fn coalesce(paths: Vec<String>, limit: usize) -> Option<Vec<String>> {
    let mut paths = outermost(paths);
    while paths.len() > limit {
        let parents: Vec<String> =
            paths.iter().map(|p| p.rsplit_once('/').map_or(p.as_str(), |(dir, _)| dir).to_string()).collect();
        if parents == paths {
            return None;
        }
        paths = outermost(parents);
    }
    Some(paths)
}

/// `paths` without duplicates and without those inside another one.
fn outermost(mut paths: Vec<String>) -> Vec<String> {
    paths.sort();
    paths.dedup();
    let all: std::collections::HashSet<&str> = paths.iter().map(String::as_str).collect();
    let inside = |p: &str| p.match_indices('/').any(|(i, _)| all.contains(&p[..i]));
    paths.iter().filter(|p| !inside(p)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalescing() {
        let v = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(coalesce(v(&["a/b", "a", "a/b/c", "b"]), 10), Some(v(&["a", "b"])));
        // "a/b" sorts before "a.txt" and "a/b/c" — inner paths still fold away.
        assert_eq!(coalesce(v(&["a/b/c", "a.txt", "a/b"]), 10), Some(v(&["a.txt", "a/b"])));
        let many: Vec<String> = (0..50).map(|i| format!("src/gen/f{i}")).chain(["docs/x".into()]).collect();
        assert_eq!(coalesce(many, 10), Some(v(&["docs", "src/gen"])));
        let top: Vec<String> = (0..50).map(|i| format!("f{i}")).collect();
        assert_eq!(coalesce(top, 10), None);
    }

    #[test]
    fn batches_skip_excluded_own_and_temporary_paths() {
        let root = PathBuf::from("/data/tree");
        let roots =
            [WatchRoot::Tree { dir: root.clone(), gitignore: false }, WatchRoot::File(PathBuf::from("/data/pack.zip"))];
        let own = Mutex::new(OwnWrites { next_id: 1, writes: vec![(1, vec!["written/file.txt".to_string()], None)] });
        let ignore = |rel: &str| rel.starts_with("build");
        let paths =
            ["a.txt", ".git/index", "build/out.o", "written/file.txt", "written", ".x.123.parallax-tmp", "src/b.rs"]
                .map(|p| root.join(p));
        let batch = to_batch(&roots, paths.into_iter(), &ignore, &own).unwrap();
        assert_eq!(batch, Batch { paths: vec!["a.txt".into(), "src/b.rs".into()], full: false });
        assert_eq!(to_batch(&roots, [PathBuf::from("/data/pack.zip")].into_iter(), &ignore, &own), None);
        let none = Mutex::new(OwnWrites::default());
        let batch = to_batch(&roots, [PathBuf::from("/data/pack.zip")].into_iter(), &ignore, &none).unwrap();
        assert!(batch.full);
        assert_eq!(to_batch(&roots, [root.join(".git/HEAD")].into_iter(), &ignore, &own), None);
    }

    #[test]
    fn reports_changes_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let (tx, rx) = mpsc::channel();
        let roots = [Some(WatchRoot::Tree { dir: dir.path().to_path_buf(), gitignore: false })];
        let watcher = Watcher::new(&roots, Arc::new(|_: &str| false), move |b| tx.send(b).unwrap()).unwrap();
        let guard = watcher.writing(&["mine.txt".to_string()]);
        std::fs::write(dir.path().join("mine.txt"), "x").unwrap();
        drop(guard);
        std::fs::write(dir.path().join("sub/new.txt"), "x").unwrap();
        let batch = rx.recv_timeout(Duration::from_secs(10)).expect("an event");
        assert_eq!(batch.paths, vec!["sub/new.txt".to_string()], "{batch:?}");
        // A new folder is watched too, and so are folders inside it.
        std::fs::create_dir_all(dir.path().join("made/deeper")).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        while rx.try_recv().is_ok() {}
        std::fs::write(dir.path().join("made/deeper/x.txt"), "x").unwrap();
        let batch = rx.recv_timeout(Duration::from_secs(10)).expect("an event");
        assert_eq!(batch.paths, vec!["made/deeper/x.txt".to_string()], "{batch:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ignored_folders_are_not_watched() {
        let dir = tempfile::tempdir().unwrap();
        let git =
            |args: &[&str]| std::process::Command::new("git").args(args).current_dir(dir.path()).output().unwrap();
        git(&["init", "-q"]);
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let (tx, rx) = mpsc::channel();
        let roots = [Some(WatchRoot::Tree { dir: dir.path().to_path_buf(), gitignore: true })];
        let _watcher = Watcher::new(&roots, Arc::new(|_: &str| false), move |b| tx.send(b).unwrap()).unwrap();
        std::fs::write(dir.path().join("target/debug/out.o"), "x").unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "x").unwrap();
        let batch = rx.recv_timeout(Duration::from_secs(10)).expect("an event");
        assert_eq!(batch.paths, vec!["src/main.rs".to_string()], "{batch:?}");
    }
}
