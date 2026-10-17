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
use crate::vfs::{Entry, EntryKind, join};
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
    /// Documents (PDF, Office, OpenDocument) with equal text and images with equal
    /// pixels count as similar even when their bytes differ.
    pub compare_contents: bool,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions {
            mode: CompareMode::Content,
            ignore_line_endings: true,
            excludes: vec![".git".into(), ".svn".into(), ".hg".into()],
            compare_contents: true,
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
        Ok(Engine { left, right, opts, excludes, control, counters: Counters::default() })
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
            let (status, reason) = self.compare_files(rel, l, r)?;
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
                if self.excluded(&child, &entry.name) {
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
        Ok(pairs
            .into_par_iter()
            .map(|(name, (l, r))| {
                let child = join(rel, &name);
                self.node(&child, name, l, r, false)
            })
            .collect())
    }

    /// `Same`, `Different`, or `Similar` with the reason when the bytes differ but
    /// the active rules consider the content equal.
    fn compare_files(&self, rel: &str, l: &Entry, r: &Entry) -> Result<(Status, Option<&'static str>)> {
        const SAME: Result<(Status, Option<&'static str>)> = Ok((Status::Same, None));
        const DIFFERENT: Result<(Status, Option<&'static str>)> = Ok((Status::Different, None));
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
            if equal && l.size == r.size {
                return SAME;
            }
            if !equal && !must_read {
                return DIFFERENT;
            }
        }
        if l.size != r.size && !must_read {
            return DIFFERENT;
        }
        let read_left = || self.left.vfs.read(&self.left.path(rel));
        let read_right = || self.right.vfs.read(&self.right.path(rel));
        // A Git object or archive entry on one side, a plain file on the other: hash the
        // plain file the way the id was made instead of inflating the stored data.
        let (left_data, right_data) = match (&l.quick_id, &r.quick_id) {
            (Some(id), None) => {
                let data = read_right()?;
                if l.size == r.size && id.matches_data(&data) {
                    return SAME;
                }
                if !must_read {
                    return DIFFERENT;
                }
                (read_left()?, data)
            }
            (None, Some(id)) => {
                let data = read_left()?;
                if l.size == r.size && id.matches_data(&data) {
                    return SAME;
                }
                if !must_read {
                    return DIFFERENT;
                }
                (data, read_right()?)
            }
            _ => (read_left()?, read_right()?),
        };
        if *left_data == *right_data {
            return SAME;
        }
        if eol && !is_binary(&left_data) && !is_binary(&right_data) && equal_ignoring_cr(&left_data, &right_data) {
            return Ok((Status::Similar, Some("only line endings differ")));
        }
        if content && convert::content_equal(&l.name, &left_data, &right_data) == Some(true) {
            return Ok((Status::Similar, Some("same content, the file bytes differ")));
        }
        DIFFERENT
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
}
