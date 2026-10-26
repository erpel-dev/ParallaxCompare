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
    assert!(result.root.children.iter().any(|n| n.name == "only.txt"));
    assert!(!result.root.children.iter().any(|n| n.name == "0.txt" && n.left.is_some()));
}
