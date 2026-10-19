//! Folder compare engine.
//!
//! Both trees are walked in parallel. Files are compared with the cheapest test
//! that gives a definite answer:
//!
//! 1. Git tree ids: an identical subtree is marked identical without being walked.
//! 2. Git blob ids / archive CRC32 values: equal ids mean equal content.
//! 3. Size (and in quick mode, modification time).
//! 4. Git blob id of a local file, so a worktree file is checked against a
//!    revision without decompressing the Git object.
//! 5. Byte comparison of the contents, optionally ignoring line-ending differences.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::convert;
use crate::source::Source;
use crate::text::is_binary;
use crate::vfs::{Data, Entry, EntryKind, QuickId, join};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum CompareMode {
    /// Size and modification time only; falls back to content where there are no times (Git).
    Quick,
    /// Contents, using stored hashes and CRCs where possible.
    #[default]
    Content,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CompareOptions {
    pub mode: CompareMode,
    /// Text files that differ only in CRLF vs. LF count as the same.
    pub ignore_line_endings: bool,
    /// Glob patterns matched against names and relative paths, e.g. `.git`, `*.o`, `build/**`.
    pub excludes: Vec<String>,
    /// When not empty, only files matching one of these globs are compared and
    /// shown, and folders without such files are left out. A pattern matches the
    /// file name (`*.java`), or the relative path when it has a `/` (`src/**/*.rs`);
    /// a matching folder or archive takes everything in it. Case does not matter.
    pub includes: Vec<String>,
    /// Documents (PDF, Office, OpenDocument) with equal text and images with equal
    /// pixels count as similar even when their bytes differ.
    pub compare_contents: bool,
    /// Archive entries with equal CRC32 and size are decompressed and compared byte
    /// for byte. Off, an equal CRC32 counts as equal content: different data shares
    /// a CRC32 only by a one-in-four-billion chance or by design, and verifying
    /// decompresses every equal entry (a large solid 7z takes seconds instead of a
    /// tenth of one).
    pub verify_checksums: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions {
            mode: CompareMode::Content,
            ignore_line_endings: true,
            excludes: vec![".git".into(), ".svn".into(), ".hg".into()],
            includes: Vec::new(),
            compare_contents: true,
            verify_checksums: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Same,
    /// The bytes differ but the content is equal under the active rules
    /// (line endings, document text, image pixels).
    Similar,
    Different,
    LeftOnly,
    RightOnly,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct Side {
    pub kind: EntryKind,
    pub size: u64,
    pub mtime: Option<i64>,
}

impl From<&Entry> for Side {
    fn from(e: &Entry) -> Self {
        Side { kind: e.kind, size: e.size, mtime: e.mtime }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub name: String,
    pub left: Option<Side>,
    pub right: Option<Side>,
    pub status: Status,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    /// An identical subtree that was not walked; its children are loaded on demand.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unexpanded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub files: u64,
    pub same: u64,
    pub similar: u64,
    pub different: u64,
    pub left_only: u64,
    pub right_only: u64,
    pub errors: u64,
    /// Folders found identical by their Git tree id without being walked.
    pub identical_subtrees: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompareResult {
    pub root: Node,
    pub summary: Summary,
}

/// Include patterns: those without `/` match names, the others relative paths.
pub struct Includes {
    names: GlobSet,
    paths: GlobSet,
}

impl Includes {
    pub fn new(patterns: &[String]) -> Result<Self> {
        let invalid = |e: globset::Error| Error::InvalidSource(format!("file pattern: {e}"));
        let (mut names, mut paths) = (GlobSetBuilder::new(), GlobSetBuilder::new());
        for pattern in patterns.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
            let by_path = pattern.contains('/');
            let glob = globset::GlobBuilder::new(pattern.trim_start_matches('/'))
                .case_insensitive(true)
                .literal_separator(by_path)
                .build()
                .map_err(invalid)?;
            if by_path {
                paths.add(glob)
            } else {
                names.add(glob)
            };
        }
        Ok(Includes { names: names.build().map_err(invalid)?, paths: paths.build().map_err(invalid)? })
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.paths.is_empty()
    }

    /// Whether the entry at `rel`, or a folder above it, matches a pattern.
    pub fn covers(&self, rel: &str) -> bool {
        if self.is_empty() {
            return true;
        }
        let mut end = 0;
        for part in rel.split('/') {
            end += part.len();
            if self.names.is_match(part) || self.paths.is_match(&rel[..end]) {
                return true;
            }
            end += 1;
        }
        false
    }
}

/// Exclude patterns, matched against entry names and relative paths.
pub struct Excludes(GlobSet);

impl Excludes {
    pub fn new(patterns: &[String]) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            builder.add(Glob::new(pattern).map_err(|e| Error::InvalidSource(format!("exclude pattern: {e}")))?);
        }
        builder.build().map(Excludes).map_err(|e| Error::InvalidSource(format!("exclude pattern: {e}")))
    }

    /// Whether the entry at `rel`, named `name`, is excluded.
    pub fn matches(&self, rel: &str, name: &str) -> bool {
        !self.0.is_empty() && (self.0.is_match(name) || self.0.is_match(rel))
    }

    /// Whether `rel` or a folder above it is excluded, so the compare never sees it.
    pub fn covers(&self, rel: &str) -> bool {
        if self.0.is_empty() {
            return false;
        }
        let mut end = 0;
        for part in rel.split('/') {
            end += part.len();
            if self.matches(&rel[..end], part) {
                return true;
            }
            end += 1;
        }
        false
    }
}

/// Which entries a comparison shows, by its exclude and include patterns, so that
/// operations on a folder touch only what was compared.
pub struct Filter {
    excludes: Excludes,
    includes: Includes,
}

impl Filter {
    pub fn new(opts: &CompareOptions) -> Result<Self> {
        Ok(Filter { excludes: Excludes::new(&opts.excludes)?, includes: Includes::new(&opts.includes)? })
    }

    /// Everything.
    pub fn none() -> Self {
        Filter {
            excludes: Excludes::new(&[]).expect("no patterns"),
            includes: Includes::new(&[]).expect("no patterns"),
        }
    }

    /// Whether the entry at `rel` takes part: not excluded, and a folder or a file the
    /// include patterns cover. An archive is copied whole, so it needs to be covered.
    pub fn keeps(&self, rel: &str, entry: &Entry) -> bool {
        !self.excludes.matches(rel, &entry.name) && (entry.kind == EntryKind::Dir || self.includes.covers(rel))
    }
}

/// Cancellation and progress shared with the caller.
#[derive(Debug, Default)]
pub struct Control {
    pub cancel: AtomicBool,
    pub files_done: AtomicU64,
}

#[derive(Default)]
struct Counters {
    same: AtomicU64,
    similar: AtomicU64,
    different: AtomicU64,
    left_only: AtomicU64,
    right_only: AtomicU64,
    errors: AtomicU64,
    identical_subtrees: AtomicU64,
}

struct Engine<'a> {
    left: &'a Source,
    right: &'a Source,
    opts: &'a CompareOptions,
    excludes: Excludes,
    includes: Includes,
    control: &'a Control,
    counters: Counters,
}

/// Compares the two sources completely.
pub fn compare(left: &Source, right: &Source, opts: &CompareOptions, control: &Control) -> Result<CompareResult> {
    compare_path(left, right, "", opts, control)
}

/// Compares the subtree at `rel` (relative to both roots), e.g. to expand an
/// identical Git subtree or to refresh one folder.
pub fn compare_path(
    left: &Source,
    right: &Source,
    rel: &str,
    opts: &CompareOptions,
    control: &Control,
) -> Result<CompareResult> {
    let started = Instant::now();
    let engine = Engine::new(left, right, opts, control)?;
    let l = left.stat(rel).ok();
    let r = right.stat(rel).ok();
    if l.is_none() && r.is_none() {
        return Err(Error::NotFound(rel.to_string()));
    }
    let name = rel.rsplit('/').next().unwrap_or_default().to_string();
    let mut root = engine.node(rel, name, l.map(as_container), r.map(as_container), true);
    if control.cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    root.unexpanded = false;
    let c = &engine.counters;
    let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
    let summary = Summary {
        files: control.files_done.load(Ordering::Relaxed),
        same: get(&c.same),
        similar: get(&c.similar),
        different: get(&c.different),
        left_only: get(&c.left_only),
        right_only: get(&c.right_only),
        errors: get(&c.errors),
        identical_subtrees: get(&c.identical_subtrees),
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    Ok(CompareResult { root, summary })
}

/// Compares the file at `lrel` on the left with the one at `rrel` on the right,
/// e.g. a file that moved to another folder. Returns its status and, when it is
/// similar, why.
pub fn compare_pair(
    left: &Source,
    right: &Source,
    lrel: &str,
    rrel: &str,
    opts: &CompareOptions,
) -> Result<(Status, Option<&'static str>)> {
    let control = Control::default();
    let engine = Engine::new(left, right, opts, &control)?;
    let (l, r) = (left.stat(lrel)?, right.stat(rrel)?);
    if l.kind == EntryKind::Dir || r.kind == EntryKind::Dir {
        return Err(Error::Unsupported("only files are compared across folders".into()));
    }
    engine.compare_files(lrel, rrel, &l, &r)
}

/// A root that is an archive file is compared as a folder.
fn as_container(mut e: Entry) -> Entry {
    if e.kind == EntryKind::File && crate::vfs::archive::detect_format(&e.name).is_some() {
        e.kind = EntryKind::Archive;
    }
    e
}

impl<'a> Engine<'a> {
    fn new(left: &'a Source, right: &'a Source, opts: &'a CompareOptions, control: &'a Control) -> Result<Self> {
        let excludes = Excludes::new(&opts.excludes)?;
        let includes = Includes::new(&opts.includes)?;
        Ok(Engine { left, right, opts, excludes, includes, control, counters: Counters::default() })
    }

    fn excluded(&self, rel: &str, name: &str) -> bool {
        self.excludes.matches(rel, name)
    }

    fn node(&self, rel: &str, name: String, l: Option<Entry>, r: Option<Entry>, is_root: bool) -> Node {
        let mut node = Node {
            name,
            left: l.as_ref().map(Side::from),
            right: r.as_ref().map(Side::from),
            status: Status::Same,
            children: Vec::new(),
            unexpanded: false,
            message: None,
        };
        let result = match (&l, &r) {
            (Some(l), Some(r)) => self.fill_both(&mut node, rel, l, r, is_root),
            (Some(l), None) => self.fill_orphan(&mut node, rel, l, true),
            (None, Some(r)) => self.fill_orphan(&mut node, rel, r, false),
            (None, None) => Ok(()),
        };
        if let Err(e) = result {
            self.counters.errors.fetch_add(1, Ordering::Relaxed);
            node.status = Status::Error;
            node.message = Some(e.to_string());
        }
        node
    }

    fn fill_both(&self, node: &mut Node, rel: &str, l: &Entry, r: &Entry, is_root: bool) -> Result<()> {
        let (lc, rc) = (l.kind.is_container(), r.kind.is_container());
        if lc && rc {
            if !is_root
                && let (Some(a), Some(b)) = (&l.quick_id, &r.quick_id)
                && a.matches(b) == Some(true)
            {
                self.counters.identical_subtrees.fetch_add(1, Ordering::Relaxed);
                node.unexpanded = true;
                return Ok(());
            }
            node.children = self.children(rel, true, true)?;
            node.status = folder_status(&node.children);
        } else if !lc && !rc {
            self.control.files_done.fetch_add(1, Ordering::Relaxed);
            let (status, reason) = self.compare_files(rel, rel, l, r)?;
            node.status = status;
            node.message = reason.map(str::to_string);
            let counter = match status {
                Status::Same => &self.counters.same,
                Status::Similar => &self.counters.similar,
                _ => &self.counters.different,
            };
            counter.fetch_add(1, Ordering::Relaxed);
        } else {
            node.status = Status::Different;
            node.message = Some("a file on one side, a folder on the other".into());
            node.children = self.children(rel, lc, rc)?;
            self.counters.different.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn fill_orphan(&self, node: &mut Node, rel: &str, e: &Entry, left: bool) -> Result<()> {
        node.status = if left { Status::LeftOnly } else { Status::RightOnly };
        if e.kind.is_container() {
            node.children = self.children(rel, left, !left)?;
        } else {
            self.control.files_done.fetch_add(1, Ordering::Relaxed);
            let counter = if left { &self.counters.left_only } else { &self.counters.right_only };
            counter.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn children(&self, rel: &str, has_left: bool, has_right: bool) -> Result<Vec<Node>> {
        if self.control.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut pairs: BTreeMap<String, (Option<Entry>, Option<Entry>)> = BTreeMap::new();
        let mut collect = |source: &Source, is_left: bool| -> Result<()> {
            for entry in source.vfs.list(&source.path(rel))? {
                let child = join(rel, &entry.name);
                // Files outside the include patterns are not compared; folders are
                // walked for files that match.
                if self.excluded(&child, &entry.name) || (!entry.kind.is_container() && !self.includes.covers(&child)) {
                    continue;
                }
                let slot = pairs.entry(entry.name.clone()).or_default();
                if is_left { slot.0 = Some(entry) } else { slot.1 = Some(entry) }
            }
            Ok(())
        };
        if has_left {
            collect(self.left, true)?;
        }
        if has_right {
            collect(self.right, false)?;
        }
        let mut pairs: Vec<_> = pairs.into_iter().collect();
        pairs.sort_by_cached_key(|(name, (l, r))| {
            let container = l.as_ref().or(r.as_ref()).is_some_and(|e| e.kind.is_container());
            (!container, name.to_lowercase(), name.clone())
        });
        let filtered = !self.includes.is_empty();
        Ok(pairs
            .into_par_iter()
            .filter_map(|(name, (l, r))| {
                let child = join(rel, &name);
                let node = self.node(&child, name, l, r, false);
                // With include patterns, folders without matching files are left out.
                let empty = node.children.is_empty() && !node.unexpanded && node.status != Status::Error;
                let container = node.left.as_ref().or(node.right.as_ref()).is_some_and(|s| s.kind.is_container());
                (!(filtered && container && empty && !self.includes.covers(&child))).then_some(node)
            })
            .collect())
    }

    /// Whether equal ids settle equal content: Git ids always, CRC32 unless verifying.
    fn trusts(&self, id: &QuickId) -> bool {
        id.proves_equality() || !self.opts.verify_checksums
    }

    /// `Same`, `Different`, or `Similar` with the reason when the bytes differ but
    /// the active rules consider the content equal.
    fn compare_files(&self, lrel: &str, rrel: &str, l: &Entry, r: &Entry) -> Result<(Status, Option<&'static str>)> {
        const SAME: Result<(Status, Option<&'static str>)> = Ok((Status::Same, None));
        const DIFFERENT: Result<(Status, Option<&'static str>)> = Ok((Status::Different, None));
        // A symlink whose target text equals a file's bytes is still not that file.
        if l.kind != r.kind {
            return Ok((Status::Different, Some(kind_change(l.kind, r.kind))));
        }
        if self.opts.mode == CompareMode::Quick
            && let (Some(a), Some(b)) = (l.mtime, r.mtime)
        {
            return if l.size == r.size && a == b { SAME } else { DIFFERENT };
        }
        let eol = self.opts.ignore_line_endings;
        let content = self.opts.compare_contents && convert::has_content_rule(&l.name);
        // Without content rules, differing ids or sizes settle it without reading.
        let must_read = eol || content;
        if let (Some(a), Some(b)) = (&l.quick_id, &r.quick_id)
            && let Some(equal) = a.matches(b)
        {
            if equal && l.size == r.size && self.trusts(a) {
                return SAME;
            }
            if !equal && !must_read {
                return DIFFERENT;
            }
        }
        if l.size != r.size && !must_read {
            return DIFFERENT;
        }
        let read_left = || self.left.vfs.read(&self.left.path(lrel));
        let read_right = || self.right.vfs.read(&self.right.path(rrel));
        // A Git object or archive entry on one side, a plain file on the other: hash the
        // plain file the way the id was made instead of inflating the stored data.
        let (left_data, right_data) = match (&l.quick_id, &r.quick_id) {
            (Some(id), None) => {
                let data = read_right()?;
                if l.size == r.size && id.matches_data(&data) {
                    // A CRC match still needs the bytes compared below.
                    if self.trusts(id) {
                        return SAME;
                    }
                } else if !must_read {
                    return DIFFERENT;
                }
                (None, Some(data))
            }
            (None, Some(id)) => {
                let data = read_left()?;
                if l.size == r.size && id.matches_data(&data) {
                    // A CRC match still needs the bytes compared below.
                    if self.trusts(id) {
                        return SAME;
                    }
                } else if !must_read {
                    return DIFFERENT;
                }
                (Some(data), None)
            }
            _ => (None, None),
        };
        // Entries that are decompressed to be read (archives) are compared as streams
        // that stop at the first difference; files on disk are mapped, which already
        // reads only as far as the comparison goes. Content rules need all of both.
        let on_disk = |source: &Source, rel: &str| source.vfs.local_path(&source.path(rel)).is_some();
        let streamed = !content && !(on_disk(self.left, lrel) && on_disk(self.right, rrel));
        if streamed {
            let open = |source: &Source, rel: &str, data: Option<Data>| -> Result<Box<dyn std::io::Read + Send>> {
                match data {
                    Some(data) => Ok(Box::new(std::io::Cursor::new(data))),
                    None => source.vfs.open(&source.path(rel)),
                }
            };
            let left_stream = open(self.left, lrel, left_data)?;
            let right_stream = open(self.right, rrel, right_data)?;
            return match compare_streams(left_stream, right_stream, eol).map_err(|e| Error::io(lrel, e))? {
                Streams::Same => SAME,
                Streams::LineEndings => Ok((Status::Similar, Some("only line endings differ"))),
                Streams::Different => DIFFERENT,
            };
        }
        let left_data = match left_data {
            Some(data) => data,
            None => read_left()?,
        };
        let right_data = match right_data {
            Some(data) => data,
            None => read_right()?,
        };
        if *left_data == *right_data {
            return SAME;
        }
        if eol && !is_binary(&left_data) && !is_binary(&right_data) && equal_ignoring_cr(&left_data, &right_data) {
            return Ok((Status::Similar, Some("only line endings differ")));
        }
        if content && let Some(reason) = convert::content_equal(&l.name, &left_data, &right_data) {
            return Ok((Status::Similar, Some(reason)));
        }
        DIFFERENT
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Streams {
    Same,
    /// Equal once `\r\n` counts as `\n`, as [`equal_ignoring_cr`] decides it.
    LineEndings,
    Different,
}

/// Compares two streams, reading only up to the first difference. With
/// `ignore_cr`, text (not binary, judged like [`is_binary`] by its start) that
/// differs only in line endings is [`Streams::LineEndings`].
fn compare_streams(
    a: Box<dyn std::io::Read + Send>,
    b: Box<dyn std::io::Read + Send>,
    ignore_cr: bool,
) -> std::io::Result<Streams> {
    use std::io::{BufRead, BufReader, Cursor, Read};
    const HEAD: u64 = 8000;
    let head = |r: &mut Box<dyn Read + Send>| -> std::io::Result<Vec<u8>> {
        let mut head = Vec::new();
        r.by_ref().take(HEAD).read_to_end(&mut head)?;
        Ok(head)
    };
    let (mut a, mut b) = (a, b);
    let (head_a, head_b) = (head(&mut a)?, head(&mut b)?);
    let ignore_cr = ignore_cr && !is_binary(&head_a) && !is_binary(&head_b);
    let mut a = BufReader::with_capacity(64 << 10, Cursor::new(head_a).chain(a));
    let mut b = BufReader::with_capacity(64 << 10, Cursor::new(head_b).chain(b));
    // Byte for byte while both are equal.
    let mut ends_with_cr = false;
    loop {
        let (x, y) = (a.fill_buf()?, b.fill_buf()?);
        if x.is_empty() && y.is_empty() {
            return Ok(Streams::Same);
        }
        let n = x.len().min(y.len());
        let equal = if x[..n] == y[..n] { n } else { x.iter().zip(y).take_while(|(p, q)| p == q).count() };
        if equal > 0 {
            ends_with_cr = x[equal - 1] == b'\r';
        }
        a.consume(equal);
        b.consume(equal);
        if equal < n || n == 0 {
            break;
        }
    }
    if !ignore_cr {
        return Ok(Streams::Different);
    }
    // From the first difference on, with `\r` before `\n` left out, a chunk at a
    // time. A `\r` that ended the equal part counts by what follows it on each side.
    let pending = ends_with_cr;
    let (mut a, mut b) = (Stripped::new(a, pending), Stripped::new(b, pending));
    loop {
        let (x, y) = (a.available()?, b.available()?);
        if x.is_empty() || y.is_empty() {
            return Ok(if x.is_empty() && y.is_empty() { Streams::LineEndings } else { Streams::Different });
        }
        let n = x.len().min(y.len());
        if x[..n] != y[..n] {
            return Ok(Streams::Different);
        }
        a.take(n);
        b.take(n);
    }
}

/// A stream with every `\r` that comes right before `\n` left out, read in chunks.
struct Stripped<R> {
    r: R,
    /// A `\r` at the end of the last chunk, kept until the next byte shows whether
    /// a `\n` follows.
    held_cr: bool,
    out: Vec<u8>,
    at: usize,
}

impl<R: std::io::BufRead> Stripped<R> {
    fn new(r: R, held_cr: bool) -> Self {
        Stripped { r, held_cr, out: Vec::new(), at: 0 }
    }

    /// The stripped bytes not taken yet; empty at the end of the stream.
    fn available(&mut self) -> std::io::Result<&[u8]> {
        while self.at == self.out.len() {
            self.out.clear();
            self.at = 0;
            let buf = self.r.fill_buf()?;
            if buf.is_empty() {
                if std::mem::take(&mut self.held_cr) {
                    self.out.push(b'\r');
                }
                break;
            }
            let mut rest = buf;
            if std::mem::take(&mut self.held_cr) && rest[0] != b'\n' {
                self.out.push(b'\r');
            }
            while let Some(i) = memchr::memchr(b'\r', rest) {
                self.out.extend_from_slice(&rest[..i]);
                match rest.get(i + 1) {
                    Some(b'\n') => {}
                    Some(_) => self.out.push(b'\r'),
                    None => self.held_cr = true,
                }
                rest = &rest[i + 1..];
            }
            self.out.extend_from_slice(rest);
            let n = buf.len();
            self.r.consume(n);
        }
        Ok(&self.out[self.at..])
    }

    fn take(&mut self, n: usize) {
        self.at += n;
    }
}

fn kind_change(l: EntryKind, r: EntryKind) -> &'static str {
    match (l, r) {
        (EntryKind::Symlink, EntryKind::File) => "a symlink on the left, a file on the right",
        (EntryKind::File, EntryKind::Symlink) => "a file on the left, a symlink on the right",
        _ => "a different kind of entry on each side",
    }
}

fn folder_status(children: &[Node]) -> Status {
    if children.iter().all(|c| c.status == Status::Same) {
        Status::Same
    } else if children.iter().all(|c| matches!(c.status, Status::Same | Status::Similar)) {
        Status::Similar
    } else {
        Status::Different
    }
}

/// Compares two byte strings treating `\r\n` and `\n` as equal.
fn equal_ignoring_cr(a: &[u8], b: &[u8]) -> bool {
    fn strip(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
        data.iter()
            .enumerate()
            .filter(move |&(i, &c)| !(c == b'\r' && data.get(i + 1) == Some(&b'\n')))
            .map(|(_, &c)| c)
    }
    strip(a).eq(strip(b))
}

/// Flattens a result tree into `(path, status)` pairs, for the CLI and tests.
pub fn flatten(node: &Node) -> Vec<(String, Status)> {
    fn walk(node: &Node, prefix: &str, out: &mut Vec<(String, Status)>) {
        for child in &node.children {
            let path = join(prefix, &child.name);
            out.push((path.clone(), child.status));
            walk(child, &path, out);
        }
    }
    let mut out = Vec::new();
    walk(node, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_equality() {
        assert!(equal_ignoring_cr(b"a\r\nb\r\n", b"a\nb\n"));
        assert!(!equal_ignoring_cr(b"a\rb", b"ab"));
        assert!(!equal_ignoring_cr(b"a\n", b"a\nb"));
    }

    /// Hands out data a few bytes at a time, so chunk boundaries fall everywhere.
    struct Trickle(Vec<u8>, usize, u64);

    impl std::io::Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.2 = self.2.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let n = (1 + (self.2 >> 60) as usize).min(buf.len()).min(self.0.len() - self.1);
            buf[..n].copy_from_slice(&self.0[self.1..self.1 + n]);
            self.1 += n;
            Ok(n)
        }
    }

    /// The streaming comparison decides exactly like the one on whole buffers.
    #[test]
    fn streams_compare_like_buffers() {
        let mut seed = 7u64;
        let mut random = move |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let alphabet = b"ab\r\n\r\n\0";
        for round in 0..20_000 {
            let make = |random: &mut dyn FnMut(u64) -> u64| {
                let len = random(12);
                (0..len).map(|_| alphabet[random(alphabet.len() as u64) as usize]).collect::<Vec<u8>>()
            };
            let a = make(&mut random);
            // Often a variant of `a`: CRs added or removed, or a byte changed.
            let b = match round % 4 {
                0 => make(&mut random),
                1 => a.iter().flat_map(|&c| if c == b'\n' { vec![b'\r', b'\n'] } else { vec![c] }).collect(),
                2 => a.iter().copied().filter(|&c| c != b'\r').collect(),
                _ => a.clone(),
            };
            let expected = |ignore_cr: bool| {
                if a == b {
                    Streams::Same
                } else if ignore_cr && !is_binary(&a) && !is_binary(&b) && equal_ignoring_cr(&a, &b) {
                    Streams::LineEndings
                } else {
                    Streams::Different
                }
            };
            for ignore_cr in [false, true] {
                let got = compare_streams(
                    Box::new(Trickle(a.clone(), 0, round)),
                    Box::new(Trickle(b.clone(), 0, round + 1)),
                    ignore_cr,
                )
                .unwrap();
                assert_eq!(got, expected(ignore_cr), "{a:?} vs {b:?}, ignore_cr: {ignore_cr}");
            }
        }
        // Binary data is judged by its start, like is_binary.
        let mut a = vec![b'x'; 9000];
        a.extend_from_slice(b"\0\r\n");
        let b: Vec<u8> = a.iter().copied().filter(|&c| c != b'\r').collect();
        assert_eq!(
            compare_streams(Box::new(std::io::Cursor::new(a)), Box::new(std::io::Cursor::new(b)), true).unwrap(),
            Streams::LineEndings
        );
    }
}
