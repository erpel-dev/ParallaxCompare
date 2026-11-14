//! Archive comparisons must not repeat backing filesystem metadata for every entry.
use std::io::Write;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use parallax_core::Result;
use parallax_core::compare::{self, CompareOptions, Control};
use parallax_core::source::{Source, SourceInfo, SourceKind, SourceSpec};
use parallax_core::vfs::{Data, Entry, Vfs, local::LocalFs, mount::ArchiveMount};

struct Counted {
    inner: LocalFs,
    stats: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}
impl Vfs for Counted {
    fn label(&self) -> String {
        self.inner.label()
    }
    fn stat(&self, p: &str) -> Result<Entry> {
        self.stats.fetch_add(1, Ordering::Relaxed);
        self.inner.stat(p)
    }
    fn list(&self, p: &str) -> Result<Vec<Entry>> {
        self.inner.list(p)
    }
    fn read(&self, p: &str) -> Result<Data> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.inner.read(p)
    }
    fn local_path(&self, p: &str) -> Option<PathBuf> {
        self.inner.local_path(p)
    }
}

fn zip(entries: usize, content: &str) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for i in 0..entries {
        zip.start_file(format!("{i}.txt"), zip::write::SimpleFileOptions::default()).unwrap();
        zip.write_all(content.as_bytes()).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn metadata_is_bounded_per_comparison_and_local_archives_are_not_read_whole() {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in [("left.zip", "left!"), ("right.zip", "right")] {
        std::fs::write(dir.path().join(path), zip(1000, content)).unwrap();
    }
    let stats = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let source = |path: &str| Source {
        spec: SourceSpec::Local { path: dir.path().join(path).display().to_string() },
        vfs: Arc::new(ArchiveMount::new(
            Arc::new(Counted { inner: LocalFs::new(dir.path()), stats: stats.clone(), reads: reads.clone() }),
            true,
        )),
        root: path.into(),
        watch: None,
        info: SourceInfo {
            kind: SourceKind::Archive,
            display: path.into(),
            rev: None,
            commit: None,
            commit_summary: None,
            writable: false,
        },
    };
    let (left, right) = (source("left.zip"), source("right.zip"));
    for _ in 0..2 {
        stats.store(0, Ordering::Relaxed);
        let result = compare::compare(&left, &right, &CompareOptions::default(), &Control::default()).unwrap();
        assert_eq!(result.summary.files, 1000);
        assert_eq!(result.summary.different, 1000);
        assert!(stats.load(Ordering::Relaxed) <= 8, "metadata must not scale with member count");
        assert_eq!(reads.load(Ordering::Relaxed), 0, "local archive indexing uses seekable input");
    }
    // A new comparison must not retain a cached file/directory decision.
    std::fs::remove_file(dir.path().join("left.zip")).unwrap();
    std::fs::create_dir(dir.path().join("left.zip")).unwrap();
    std::fs::write(dir.path().join("left.zip/only.txt"), "new").unwrap();
    left.vfs.changed("left.zip");
    let result = compare::compare(&left, &right, &CompareOptions::default(), &Control::default()).unwrap();
    assert!(result.root.children().iter().any(|n| n.name() == "only.txt"));
    assert!(!result.root.children().iter().any(|n| n.name() == "0.txt" && n.left().is_some()));
}

/// Counts, per path, the whole-file reads and the stream opens of a folder.
struct Traced {
    inner: LocalFs,
    reads: Arc<parking_lot::Mutex<Vec<String>>>,
    opens: Arc<parking_lot::Mutex<Vec<String>>>,
}
impl Vfs for Traced {
    fn label(&self) -> String {
        self.inner.label()
    }
    fn stat(&self, p: &str) -> Result<Entry> {
        self.inner.stat(p)
    }
    fn list(&self, p: &str) -> Result<Vec<Entry>> {
        self.inner.list(p)
    }
    fn read(&self, p: &str) -> Result<Data> {
        self.reads.lock().push(p.to_string());
        self.inner.read(p)
    }
    fn open(&self, p: &str) -> Result<Box<dyn std::io::Read + Send>> {
        self.opens.lock().push(p.to_string());
        self.inner.open(p)
    }
    fn local_path(&self, p: &str) -> Option<PathBuf> {
        self.inner.local_path(p)
    }
}

/// A file without an extension may be a Conan profile, which only its data
/// tells. That must not make every such file be read whole: a large binary
/// without an extension is compared as a stream, like one with an extension,
/// and a small one is read whole only when its start shows a profile.
#[test]
fn files_without_extension_are_streamed_unless_they_are_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let profile = "[settings]\nos=Linux\narch=x86_64\n[options]\nshared=True\n";
    for (side, first, spaced) in [("l", 0u8, false), ("r", 1u8, true)] {
        let root = dir.path().join(side);
        std::fs::create_dir(&root).unwrap();
        let mut blob = vec![first];
        blob.resize(1 << 20, 7);
        std::fs::write(root.join("backup"), &blob).unwrap();
        std::fs::write(root.join("backup.blob"), &blob).unwrap();
        // Small, without extension, not a profile: its start is looked at, no more.
        blob.truncate(200 * 1024);
        blob.extend_from_slice(if spaced { b"more" } else { b"" });
        std::fs::write(root.join("small"), &blob).unwrap();
        let text = if spaced { profile.replace('=', " = ") } else { profile.to_string() };
        // Padded with comments past the size files on disk are read whole at anyway.
        std::fs::write(root.join("profile"), format!("{text}{}", "# note\n".repeat(20_000))).unwrap();
    }
    let reads = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let opens = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let source = |side: &str| Source {
        spec: SourceSpec::Local { path: dir.path().join(side).display().to_string() },
        vfs: Arc::new(Traced {
            inner: LocalFs::new(dir.path().join(side)),
            reads: reads.clone(),
            opens: opens.clone(),
        }),
        root: String::new(),
        watch: None,
        info: SourceInfo {
            kind: SourceKind::Folder,
            display: side.into(),
            rev: None,
            commit: None,
            commit_summary: None,
            writable: false,
        },
    };
    for strict in [false, true] {
        reads.lock().clear();
        opens.lock().clear();
        let options = CompareOptions { ignore_line_endings: !strict, ..Default::default() };
        let result = compare::compare(&source("l"), &source("r"), &options, &Control::default()).unwrap();
        let status = |name: &str| {
            compare::flatten(&result.root).into_iter().find(|(p, _)| p == name).unwrap_or_else(|| panic!("{name}")).1
        };
        let count =
            |log: &Arc<parking_lot::Mutex<Vec<String>>>, name: &str| log.lock().iter().filter(|p| *p == name).count();
        for name in ["backup", "backup.blob", "small"] {
            assert_eq!(status(name), compare::Status::Different, "{name}, strict: {strict}");
            assert_eq!(count(&reads, name), 0, "whole reads of {name}, strict: {strict}");
        }
        // Equal size: both are opened and compared up to the first difference.
        assert_eq!((count(&opens, "backup"), count(&opens, "backup.blob")), (2, 2), "strict: {strict}");
        assert_eq!(status("profile"), compare::Status::Similar, "strict: {strict}");
        assert_eq!(count(&reads, "profile"), 2, "strict: {strict}");
    }
}
