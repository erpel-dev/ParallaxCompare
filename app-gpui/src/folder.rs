//! The folder comparison: the core's result tree, flattened into rows for the
//! open folders and the chosen filter, in one virtualized list.
//!
//! Unlike the web view, this view holds the core's [`Node`] tree itself, so
//! there is no second copy of the result and no IPC: opening a folder only
//! changes which rows are listed. Folders the compare did not walk (identical by
//! Git tree id, closed archives) are compared when they are opened, with the
//! same core call as the Tauri app's `expand` command. Copy, delete and changes
//! on disk refresh only the paths they touch, as `refresh_paths` does.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{
    ClickEvent, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, MouseButton, MouseDownEvent,
    Pixels, Point, PromptLevel, ScrollStrategy, SharedString, Subscription, UniformListScrollHandle, Window, div,
    point, prelude::*, px, uniform_list,
};
use parallax_core::compare::{self, CompareOptions, Control, Node, Side as Entry, Status, Summary};
use parallax_core::edit::Side;
use parallax_core::ops::Removal;
use parallax_core::tree;
use parallax_core::vfs::EntryKind;

use crate::actions::*;
use crate::input::{InputEvent, TextInput};
use crate::menu::{self, Item, Menu};
use crate::session::{self, Session};
use crate::style;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    All,
    Differences,
    Same,
    LeftOnly,
    RightOnly,
    Errors,
}

impl Filter {
    fn matches(self, status: Status) -> bool {
        match self {
            Filter::All => true,
            Filter::Differences => {
                matches!(status, Status::Different | Status::LeftOnly | Status::RightOnly | Status::Error)
            }
            Filter::Same => matches!(status, Status::Same | Status::Similar),
            Filter::LeftOnly => status == Status::LeftOnly,
            Filter::RightOnly => status == Status::RightOnly,
            Filter::Errors => status == Status::Error,
        }
    }
}

/// Whether `node` or anything below it passes the filter.
fn subtree_matches(node: &Node, filter: Filter) -> bool {
    filter.matches(node.status) || node.children().iter().any(|c| subtree_matches(c, filter))
}

#[derive(Clone, Debug)]
pub struct FlatRow {
    pub path: String,
    pub depth: usize,
    pub name: SharedString,
    pub status: Status,
    pub container: bool,
    pub expanded: bool,
    pub left: Option<Entry>,
    pub right: Option<Entry>,
    pub message: Option<SharedString>,
}

/// "1 entry", "3 entries".
fn entries(n: usize) -> String {
    format!("{n} entr{}", if n == 1 { "y" } else { "ies" })
}

fn trash_name() -> &'static str {
    if cfg!(windows) { "Recycle Bin" } else { "trash" }
}

fn trash_button() -> &'static str {
    if cfg!(windows) { "Move to Recycle Bin" } else { "Move to trash" }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_string() } else { format!("{dir}/{name}") }
}

fn row_of(child: &Node, path: String, depth: usize, expanded: bool, name: String) -> FlatRow {
    FlatRow {
        path,
        depth,
        name: name.into(),
        status: child.status,
        container: child.is_container(),
        expanded,
        left: child.left(),
        right: child.right(),
        message: child.message().map(|m| m.to_string().into()),
    }
}

/// The rows of the open folders.
pub fn flatten(
    node: &Node,
    dir: &str,
    depth: usize,
    expanded: &HashSet<String>,
    filter: Filter,
    out: &mut Vec<FlatRow>,
) {
    for child in node.children() {
        let path = join(dir, child.name());
        let container = child.is_container();
        let shown = if container {
            filter == Filter::All || subtree_matches(child, filter)
        } else {
            filter.matches(child.status)
        };
        if !shown {
            continue;
        }
        let open = container && expanded.contains(&path);
        out.push(row_of(child, path.clone(), depth, open, child.name().to_string()));
        if open {
            flatten(child, &path, depth + 1, expanded, filter, out);
        }
    }
}

/// Every file in one list, named by its path, as the Svelte app's Flat view.
/// Folders and archives that were not walked show as one entry.
pub fn flatten_files(node: &Node, dir: &str, filter: Filter, out: &mut Vec<FlatRow>) {
    for child in node.children() {
        let path = join(dir, child.name());
        if child.is_container() && !child.children().is_empty() {
            flatten_files(child, &path, filter, out);
        } else if (!child.is_container() || child.unexpanded()) && filter.matches(child.status) {
            out.push(row_of(child, path.clone(), 0, false, path));
        }
    }
}

pub enum FolderEvent {
    /// Open the file at this path (relative to both roots) in a text comparison.
    OpenFile(String),
}

enum Progress {
    Comparing { started: Instant, control: Arc<Control> },
    Done,
    Failed(String),
}

struct OptionsPanel {
    includes: Entity<TextInput>,
    excludes: Entity<TextInput>,
}

pub struct FolderView {
    session: Arc<Session>,
    options: CompareOptions,
    focus: FocusHandle,
    progress: Progress,
    root: Option<Node>,
    summary: Summary,
    expanded: HashSet<String>,
    filter: Filter,
    flat: bool,
    rows: Vec<FlatRow>,
    /// The row with the keyboard focus, and where a Shift selection starts.
    cursor: Option<usize>,
    anchor: Option<usize>,
    selected: BTreeSet<String>,
    scroll: UniformListScrollHandle,
    /// Folders whose entries are being compared after being opened.
    loading: HashSet<String>,
    message: Option<SharedString>,
    menu: Option<Menu>,
    panel: Option<OptionsPanel>,
    watching: bool,
    /// The exclusions the watcher ignores changes under.
    watched_excludes: Vec<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FolderEvent> for FolderView {}

impl Focusable for FolderView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Splits a pattern list as the Svelte view's `splitPatterns` does: glob
/// alternatives (`*.{rs,md}`), character classes and quoted names stay whole.
/// File patterns are separated by spaces, commas or semicolons; exclusions only by
/// commas, so they can name folders with spaces.
fn split_patterns(text: &str, excludes: bool) -> Result<Vec<String>, String> {
    struct Token {
        text: String,
        end: usize,
        started: bool,
    }
    impl Token {
        fn append(&mut self, s: &str, literal: bool) {
            let blank = s.trim().is_empty();
            if !self.started && !literal && blank {
                return;
            }
            self.text.push_str(s);
            self.started = true;
            if literal || !blank {
                self.end = self.text.len();
            }
        }
        fn emit(&mut self, out: &mut Vec<String>) {
            if self.end > 0 {
                out.push(self.text[..self.end].to_string());
            }
            *self = Token { text: String::new(), end: 0, started: false };
        }
    }
    let separator = |c: char| if excludes { c == ',' } else { c.is_whitespace() || c == ',' || c == ';' };
    let mut out = Vec::new();
    let mut token = Token { text: String::new(), end: 0, started: false };
    let (mut braces, mut bracket, mut quote) = (0usize, false, None::<char>);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek().is_some() {
            let next = chars.next().unwrap_or_default();
            // Escaped list separators are literal names, but an escaped comma inside
            // a glob alternative belongs to the glob compiler.
            if quote == Some(next) || (!bracket && braces == 0 && separator(next)) {
                token.append(&next.to_string(), true);
            } else {
                token.append(&format!("{c}{next}"), true);
            }
        } else if quote == Some(c) {
            quote = None;
        } else if quote.is_none() && !bracket && !token.started && (c == '"' || c == '\'') {
            quote = Some(c);
            token.started = true;
        } else if c == '[' && !bracket {
            bracket = true;
            token.append("[", true);
        } else if c == ']' && bracket {
            bracket = false;
            token.append("]", true);
        } else if !bracket && c == '{' {
            braces += 1;
            token.append("{", true);
        } else if !bracket && c == '}' {
            braces = braces.saturating_sub(1);
            token.append("}", true);
        } else if quote.is_none() && !bracket && braces == 0 && separator(c) {
            token.emit(&mut out);
        } else {
            token.append(&c.to_string(), quote.is_some() || bracket || braces > 0);
        }
    }
    if quote.is_some() {
        return Err("Close the quote around the file pattern.".into());
    }
    token.emit(&mut out);
    Ok(out)
}

/// The text for a pattern box, quoting a pattern only where splitting would
/// otherwise change it.
fn join_patterns(patterns: &[String], excludes: bool) -> String {
    patterns
        .iter()
        .map(|p| {
            if split_patterns(p, excludes).is_ok_and(|split| split == std::slice::from_ref(p)) {
                p.clone()
            } else {
                format!("\"{}\"", p.replace('"', "\\\""))
            }
        })
        .collect::<Vec<_>>()
        .join(if excludes { ", " } else { " " })
}

impl FolderView {
    pub fn new(session: Arc<Session>, cx: &mut Context<Self>) -> Self {
        let mut view = FolderView {
            session,
            options: CompareOptions::default(),
            focus: cx.focus_handle(),
            progress: Progress::Done,
            root: None,
            summary: Summary::default(),
            expanded: HashSet::new(),
            filter: Filter::All,
            flat: false,
            rows: Vec::new(),
            cursor: None,
            anchor: None,
            selected: BTreeSet::new(),
            scroll: UniformListScrollHandle::new(),
            loading: HashSet::new(),
            message: None,
            menu: None,
            panel: None,
            watching: false,
            watched_excludes: Vec::new(),
            _subscriptions: Vec::new(),
        };
        view.run(cx);
        view
    }

    /// Compares both roots on a background thread; the view keeps showing the
    /// previous result, if any, until the new one is there.
    pub fn run(&mut self, cx: &mut Context<Self>) {
        if let Progress::Comparing { control, .. } = &self.progress {
            control.cancel.store(true, Ordering::Relaxed);
        }
        let control = Arc::new(Control::default());
        self.progress = Progress::Comparing { started: Instant::now(), control: control.clone() };
        let (session, options) = (self.session.clone(), self.options.clone());
        let work = cx.background_executor().spawn({
            let control = control.clone();
            async move {
                // A refresh: archives and folder snapshots may be out of date.
                session.left.vfs.changed(&session.left.path(""));
                session.right.vfs.changed(&session.right.path(""));
                compare::compare(&session.left, &session.right, &options, &control).map_err(|e| e.to_string())
            }
        });
        // Counters while it runs, as the web view gets them from progress events.
        cx.spawn({
            let control = control.clone();
            async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(120)).await;
                    let running = this
                        .update(cx, |this, cx| {
                            let running =
                                matches!(&this.progress, Progress::Comparing { control: c, .. } if Arc::ptr_eq(c, &control));
                            if running {
                                cx.notify();
                            }
                            running
                        })
                        .unwrap_or(false);
                    if !running {
                        break;
                    }
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                // A newer compare replaced this one.
                if !matches!(&this.progress, Progress::Comparing { control: c, .. } if Arc::ptr_eq(c, &control)) {
                    return;
                }
                match result {
                    Ok(result) => {
                        let first = this.root.is_none();
                        this.summary = result.summary;
                        this.root = Some(result.root);
                        this.progress = Progress::Done;
                        if first {
                            this.open_differing_folders();
                        }
                        this.rebuild();
                        this.start_watching(cx);
                    }
                    Err(e) => this.progress = Progress::Failed(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Watches local sides and refreshes what changed, as the Tauri app does with
    /// its `paths-changed` events.
    fn start_watching(&mut self, cx: &mut Context<Self>) {
        if self.watching {
            return;
        }
        let Some(rx) = self.session.watch(&self.options.excludes) else { return };
        self.watching = true;
        self.watched_excludes = self.options.excludes.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let mut full = false;
                let mut paths = BTreeSet::new();
                loop {
                    match rx.try_recv() {
                        Ok(batch) => {
                            full |= batch.full;
                            paths.extend(batch.paths);
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                    }
                }
                if !full && paths.is_empty() {
                    continue;
                }
                let alive = this
                    .update(cx, |this, cx| {
                        if full {
                            this.run(cx);
                        } else {
                            this.refresh_paths(paths.into_iter().collect(), cx);
                        }
                    })
                    .is_ok();
                if !alive {
                    return;
                }
            }
        })
        .detach();
    }

    /// Compares `paths` again (the outermost folder still in the tree for each)
    /// and puts the fresh nodes in place.
    pub fn refresh_paths(&mut self, paths: Vec<String>, cx: &mut Context<Self>) {
        let Some(root) = &self.root else { return };
        let mut targets: Vec<String> = paths.iter().map(|p| tree::refresh_target(root, p)).collect();
        targets.sort();
        targets.dedup();
        if targets.iter().any(String::is_empty) {
            return self.run(cx);
        }
        let outer: Vec<String> =
            targets.iter().filter(|p| !targets.iter().any(|q| p.starts_with(&format!("{q}/")))).cloned().collect();
        let (session, options) = (self.session.clone(), self.options.clone());
        let work = cx.background_executor().spawn({
            let outer = outer.clone();
            async move { session.refresh(&outer, &options) }
        });
        cx.spawn(async move |this, cx| {
            let nodes = work.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(root) = &mut this.root {
                    session::apply_refresh(root, &outer, nodes);
                    this.summary = session::summarize(root, this.summary.elapsed_ms);
                }
                this.rebuild();
                cx.notify();
            });
        })
        .detach();
    }

    /// As the web view does after a compare: folders with differences are open,
    /// unless the tree is huge.
    fn open_differing_folders(&mut self) {
        let Some(root) = &self.root else { return };
        if self.summary.files > 20_000 {
            return;
        }
        fn walk(node: &Node, dir: &str, out: &mut HashSet<String>, budget: &mut usize) {
            for child in node.children() {
                if *budget == 0 {
                    return;
                }
                if child.is_container() && child.status != Status::Same && !child.unexpanded() {
                    let path = join(dir, child.name());
                    *budget -= 1;
                    walk(child, &path, out, budget);
                    out.insert(path);
                }
            }
        }
        let mut budget = 400;
        walk(root, "", &mut self.expanded, &mut budget);
    }

    fn rebuild(&mut self) {
        // The cursor, the Shift anchor and the selection follow their paths into
        // the new rows; indices into the old rows would point anywhere.
        let path_at = |i: Option<usize>| i.and_then(|i| self.rows.get(i)).map(|r| r.path.clone());
        let (cursor, anchor) = (path_at(self.cursor), path_at(self.anchor));
        self.rows.clear();
        if let Some(root) = &self.root {
            if self.flat {
                flatten_files(root, "", self.filter, &mut self.rows);
            } else {
                flatten(root, "", 0, &self.expanded, self.filter, &mut self.rows);
            }
        }
        let (mut kept, mut new_cursor, mut new_anchor) = (BTreeSet::new(), None, None);
        for (i, row) in self.rows.iter().enumerate() {
            if cursor.as_ref() == Some(&row.path) {
                new_cursor = Some(i);
            }
            if anchor.as_ref() == Some(&row.path) {
                new_anchor = Some(i);
            }
            if !self.selected.is_empty() && self.selected.contains(&row.path) {
                kept.insert(row.path.clone());
            }
        }
        self.selected = kept;
        self.cursor = new_cursor;
        self.anchor = new_anchor;
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.rebuild();
        cx.notify();
    }

    fn set_flat(&mut self, flat: bool, cx: &mut Context<Self>) {
        self.flat = flat;
        self.rebuild();
        cx.notify();
    }

    fn say(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.message = Some(message.into());
        cx.notify();
    }

    /// Moves the cursor; with `extend`, selects from the anchor to it.
    fn select(&mut self, index: usize, extend: bool, cx: &mut Context<Self>) {
        if index >= self.rows.len() {
            return;
        }
        if extend {
            let valid = |i: &usize| *i < self.rows.len();
            let anchor = self.anchor.filter(valid).or(self.cursor.filter(valid)).unwrap_or(index);
            let (a, b) = (anchor.min(index), anchor.max(index));
            self.selected = self.rows[a..=b].iter().map(|r| r.path.clone()).collect();
            self.anchor = Some(anchor);
        } else {
            self.selected = BTreeSet::from([self.rows[index].path.clone()]);
            self.anchor = Some(index);
        }
        self.cursor = Some(index);
        self.scroll.scroll_to_item(index, ScrollStrategy::Center);
        cx.notify();
    }

    fn toggle(&mut self, index: usize, open: Option<bool>, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else { return };
        if !row.container || self.flat {
            return;
        }
        let open = open.unwrap_or(!row.expanded);
        if !open {
            self.expanded.remove(&row.path);
            self.rebuild();
            cx.notify();
            return;
        }
        let unexpanded = self.root.as_ref().and_then(|r| tree::find(r, &row.path)).is_some_and(Node::unexpanded);
        if !unexpanded {
            self.expanded.insert(row.path);
            self.rebuild();
            cx.notify();
            return;
        }
        if !self.loading.insert(row.path.clone()) {
            return;
        }
        let (session, options, path) = (self.session.clone(), self.options.clone(), row.path.clone());
        let work = cx.background_executor().spawn(async move {
            compare::compare_path(&session.left, &session.right, &path, &options, &Control::default())
                .map(|r| r.root)
                .map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            let fresh = work.await;
            let _ = this.update(cx, |this, cx| {
                this.loading.remove(&row.path);
                match fresh {
                    Ok(node) => {
                        if let Some(root) = &mut this.root {
                            tree::replace(root, &row.path, Some(node));
                        }
                        this.expanded.insert(row.path);
                    }
                    Err(e) => this.message = Some(format!("{}: {e}", row.path).into()),
                }
                this.rebuild();
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else { return };
        if row.container && !self.flat {
            self.toggle(index, None, cx);
        } else if !row.container {
            cx.emit(FolderEvent::OpenFile(row.path.clone()));
        }
    }

    /// The next (or previous) differing file among the rows, from the cursor.
    fn next_difference(&mut self, forward: bool, cx: &mut Context<Self>) {
        let n = self.rows.len();
        if n == 0 {
            return;
        }
        let differs = |r: &FlatRow| !r.container && Filter::Differences.matches(r.status);
        let start = self.cursor.unwrap_or(if forward { n - 1 } else { 0 });
        for step in 1..=n {
            let i = if forward { (start + step) % n } else { (start + n - step % n) % n };
            if differs(&self.rows[i]) {
                self.select(i, false, cx);
                return;
            }
        }
    }

    fn selected_rows(&self) -> Vec<FlatRow> {
        self.rows.iter().filter(|r| self.selected.contains(&r.path)).cloned().collect()
    }

    /// Archives at or below `paths` on the side copied from: they are copied whole,
    /// with entries the patterns or the status filter hide.
    fn archives_copied(&self, paths: &[String], to: Side) -> usize {
        fn walk(node: &Node, to: Side, n: &mut usize) {
            let from = if to == Side::Right { node.left() } else { node.right() };
            if from.is_some_and(|e| e.kind == EntryKind::Archive) {
                *n += 1;
            } else {
                node.children().iter().for_each(|c| walk(c, to, n));
            }
        }
        let Some(root) = &self.root else { return 0 };
        let mut n = 0;
        for path in paths {
            if let Some(node) = tree::find(root, path) {
                walk(node, to, &mut n);
            }
        }
        n
    }

    /// Copies the selected entries to one side, then refreshes them.
    fn copy_selected(&mut self, to: Side, window: &mut Window, cx: &mut Context<Self>) {
        let from_present = |r: &FlatRow| if to == Side::Right { r.left.is_some() } else { r.right.is_some() };
        let rows: Vec<FlatRow> = self.selected_rows().into_iter().filter(from_present).collect();
        if rows.is_empty() {
            return self.say("Select entries that exist on the side to copy from.", cx);
        }
        if !self.session.writable(to) {
            return self.say(format!("The {} side cannot be written.", side_name(to)), cx);
        }
        let replaces =
            rows.iter().filter(|r| if to == Side::Right { r.right.is_some() } else { r.left.is_some() }).count();
        let paths: Vec<String> = rows.into_iter().map(|r| r.path).collect();
        let archives = self.archives_copied(&paths, to);
        let go = move |this: &mut Self, cx: &mut Context<Self>| this.run_copy(paths.clone(), to, cx);
        if replaces == 0 && archives == 0 {
            return go(self, cx);
        }
        let side = side_name(to);
        let message = if replaces > 0 {
            format!("Replace {} on the {side} side?", entries(replaces))
        } else {
            format!(
                "Copy {} to the {side} side?",
                if archives == 1 { "the archive".into() } else { format!("{archives} archives") }
            )
        };
        let mut detail = String::new();
        if replaces > 0 {
            detail.push_str("Files that exist on both sides are overwritten.");
        }
        if archives > 0 {
            detail.push_str(if detail.is_empty() { "" } else { " " });
            detail.push_str(
                "Archives are copied whole, including entries hidden by file patterns, exclusions or the status filter.",
            );
        }
        let ok = if replaces > 0 { "Replace" } else { "Copy" };
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &[ok, "Cancel"], cx);
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, go);
            }
        })
        .detach();
    }

    fn run_copy(&mut self, paths: Vec<String>, to: Side, cx: &mut Context<Self>) {
        let (session, options) = (self.session.clone(), self.options.clone());
        let n = paths.len();
        let work = cx.background_executor().spawn({
            let paths = paths.clone();
            async move { session.copy(&paths, to, &options) }
        });
        self.say(format!("Copying {} to the {} side…", entries(n), side_name(to)), cx);
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.say(format!("Copied {} to the {} side.", entries(n), side_name(to)), cx),
                    Err(e) => this.say(format!("Copy failed: {e}"), cx),
                }
                this.refresh_paths(paths, cx);
            });
        })
        .detach();
    }

    /// Deletes the selected entries from one side, after asking. Only writable
    /// sides are offered: a Git revision cannot be changed.
    fn delete_selected(&mut self, side: Option<Side>, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        if rows.is_empty() {
            return;
        }
        let on = |s: Side| rows.iter().any(|r| if s == Side::Left { r.left.is_some() } else { r.right.is_some() });
        let sides: Vec<Side> = match side {
            Some(s) => vec![s],
            None => [Side::Left, Side::Right].into_iter().filter(|&s| on(s)).collect(),
        };
        let writable: Vec<Side> = sides.iter().copied().filter(|&s| self.session.writable(s)).collect();
        if writable.is_empty() {
            let which = match sides.as_slice() {
                [s] => format!("The {} side cannot be written.", side_name(*s)),
                _ => "Neither side can be written.".to_string(),
            };
            return self.say(which, cx);
        }
        let present = |s: Side| -> Vec<String> {
            rows.iter()
                .filter(|r| if s == Side::Left { r.left.is_some() } else { r.right.is_some() })
                .map(|r| r.path.clone())
                .collect()
        };
        let per_side: Vec<(Side, Vec<String>)> = writable.iter().map(|&s| (s, present(s))).collect();
        let in_archive = |s: Side, paths: &[String]| paths.iter().filter(|p| self.session.inside_archive(s, p)).count();
        let (message, detail, answers): (String, String, Vec<&str>) = match per_side.as_slice() {
            [(s, paths)] => {
                let archived = in_archive(*s, paths);
                let what = entries(paths.len());
                let detail = if archived == 0 {
                    format!("They go to the {}.", trash_name())
                } else if archived == paths.len() {
                    "They are inside an archive and are deleted for good; this cannot be undone.".to_string()
                } else {
                    format!(
                        "{archived} of them are inside an archive and are deleted for good; this cannot be undone. The others go to the {}.",
                        trash_name()
                    )
                };
                let ok = if archived == 0 { trash_button() } else { "Delete" };
                (format!("Delete {what} from the {} side?", side_name(*s)), detail, vec![ok, "Cancel"])
            }
            _ => {
                let archived: usize = per_side.iter().map(|(s, p)| in_archive(*s, p)).sum();
                let detail = if archived == 0 {
                    format!("They go to the {}.", trash_name())
                } else {
                    format!("Entries inside archives are deleted for good. Others go to the {}.", trash_name())
                };
                (format!("Delete {} from which side?", entries(rows.len())), detail, vec!["Left", "Right", "Cancel"])
            }
        };
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &answers[..], cx);
        cx.spawn_in(window, async move |this, cx| {
            let chosen = match (per_side.len(), answer.await) {
                (1, Ok(0)) => per_side.into_iter().next(),
                (2, Ok(i @ (0 | 1))) => per_side.into_iter().nth(i),
                _ => None,
            };
            if let Some((side, paths)) = chosen {
                let _ = this.update_in(cx, |this, window, cx| this.run_delete(paths, side, Removal::Trash, window, cx));
            }
        })
        .detach();
    }

    fn run_delete(
        &mut self,
        paths: Vec<String>,
        side: Side,
        removal: Removal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.session.writable(side) {
            return self.say(format!("The {} side cannot be written.", side_name(side)), cx);
        }
        let session = self.session.clone();
        let work = cx.background_executor().spawn({
            let paths = paths.clone();
            async move { session.delete(&paths, side, removal) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(left_over) => {
                        let deleted = paths.len() - left_over.len();
                        let mut message = format!("Deleted {} from the {} side.", entries(deleted), side_name(side));
                        if !left_over.is_empty() {
                            message.push_str(&format!(
                                " {} could not be moved to the {} and {} still there.",
                                entries(left_over.len()),
                                trash_name(),
                                if left_over.len() == 1 { "is" } else { "are" }
                            ));
                            this.offer_permanent_delete(left_over, side, window, cx);
                        }
                        this.say(message, cx);
                    }
                    Err(e) => this.say(format!("Delete failed: {e}"), cx),
                }
                this.refresh_paths(paths, cx);
            });
        })
        .detach();
    }

    /// For entries the trash refused (on a network drive, for example), as the
    /// Svelte view asks.
    fn offer_permanent_delete(&mut self, paths: Vec<String>, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let first = paths.first().cloned().unwrap_or_default();
        let message = match paths.len() {
            1 => format!("\"{first}\" could not be moved to the {}.", trash_name()),
            n => format!("{n} entries could not be moved to the {}.", trash_name()),
        };
        let answer = window.prompt(
            PromptLevel::Critical,
            &message,
            Some("Delete them permanently? This cannot be undone."),
            &["Delete permanently", "Keep them"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ =
                    this.update_in(cx, |this, window, cx| this.run_delete(paths, side, Removal::Permanent, window, cx));
            }
        })
        .detach();
    }

    fn show_menu(&mut self, position: Point<Pixels>, side: Option<Side>, cx: &mut Context<Self>) {
        let rows = self.selected_rows();
        let one_file = rows.len() == 1 && !rows[0].container;
        let on = |s: Side| rows.iter().any(|r| if s == Side::Left { r.left.is_some() } else { r.right.is_some() });
        let writable = [self.session.writable(Side::Left), self.session.writable(Side::Right)];
        let paths: Vec<String> = rows.iter().map(|r| r.path.clone()).collect();
        let mut items = vec![
            Item::action("Open comparison", "Enter", one_file, Box::new(Open)),
            Item::Separator,
            Item::action("Copy to the right", "Alt+→", on(Side::Left) && writable[1], Box::new(CopyRight)),
            Item::action("Copy to the left", "Alt+←", on(Side::Right) && writable[0], Box::new(CopyLeft)),
            Item::Separator,
        ];
        let weak = cx.entity().downgrade();
        for s in [Side::Left, Side::Right] {
            if side.is_some_and(|clicked| clicked != s) {
                continue;
            }
            let weak = weak.clone();
            let label = format!("Delete from the {} side…", side_name(s));
            let enabled = on(s) && writable[s as usize];
            let item = Item::run(label, false, move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.delete_selected(Some(s), window, cx));
            });
            items.push(match item {
                Item::Entry { label, run, .. } => Item::Entry { label, keys: "", enabled, run, checked: false },
                other => other,
            });
        }
        items.push(Item::Separator);
        items.push(Item::run("Copy path", false, move |_, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(paths.join("\n")))
        }));
        self.menu = Some(Menu { position, items });
        cx.notify();
    }

    fn toggle_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel.take().is_some() {
            window.focus(&self.focus);
            return cx.notify();
        }
        let includes = cx.new(|cx| {
            let mut input = TextInput::new("Only these files, e.g. *.rs src/**", cx).monospaced();
            input.set_text(join_patterns(&self.options.includes, false), cx);
            input
        });
        let excludes = cx.new(|cx| {
            let mut input = TextInput::new("Leave out, separated by commas, e.g. target, *.o", cx).monospaced();
            input.set_text(join_patterns(&self.options.excludes, true), cx);
            input
        });
        for input in [&includes, &excludes] {
            let sub = cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Submit { .. } => this.apply_patterns(cx),
                InputEvent::Cancel => this.toggle_panel(window, cx),
                InputEvent::Changed | InputEvent::Tab => {}
            });
            self._subscriptions.push(sub);
        }
        window.focus(&includes.focus_handle(cx));
        self.panel = Some(OptionsPanel { includes, excludes });
        cx.notify();
    }

    fn apply_patterns(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = &self.panel else { return };
        let split = |input: &Entity<TextInput>, excludes| split_patterns(input.read(cx).text(), excludes);
        let (includes, excludes) = match (split(&panel.includes, false), split(&panel.excludes, true)) {
            (Ok(i), Ok(e)) => (i, e),
            (Err(e), _) | (_, Err(e)) => return self.say(e, cx),
        };
        if let Err(e) = compare::Filter::new(&CompareOptions {
            includes: includes.clone(),
            excludes: excludes.clone(),
            ..CompareOptions::default()
        }) {
            return self.say(format!("Pattern not valid: {e}"), cx);
        }
        self.options.includes = includes;
        self.options.excludes = excludes;
        // The watcher ignores changes under the old exclusions: start it again
        // with the new ones once this compare is done.
        if self.watching && self.watched_excludes != self.options.excludes {
            self.session.stop_watching();
            self.watching = false;
        }
        self.run(cx);
    }

    fn set_option(&mut self, f: impl FnOnce(&mut CompareOptions), cx: &mut Context<Self>) {
        f(&mut self.options);
        self.run(cx);
    }

    fn on_mouse_down(
        &mut self,
        index: usize,
        side: Side,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus);
        if event.button == MouseButton::Right {
            if !self.rows.get(index).is_some_and(|r| self.selected.contains(&r.path)) {
                self.select(index, false, cx);
            }
            self.show_menu(event.position, Some(side), cx);
            return;
        }
        if event.modifiers.control {
            let path = self.rows[index].path.clone();
            if !self.selected.remove(&path) {
                self.selected.insert(path);
            }
            self.cursor = Some(index);
            self.anchor = Some(index);
            return cx.notify();
        }
        self.select(index, event.modifiers.shift, cx);
        if event.click_count >= 2 {
            self.open(index, cx);
        }
    }

    fn render_side(
        &self,
        row: &FlatRow,
        side: Option<Entry>,
        index: usize,
        which: Side,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let base = div()
            .id(("side", index * 2 + which as usize))
            .flex_1()
            .w_0()
            .min_w_0()
            .h_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    this.on_mouse_down(index, which, e, window, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    this.on_mouse_down(index, which, e, window, cx)
                }),
            );
        let Some(side) = side else {
            return base.bg(style::chrome()).into_any_element();
        };
        let color = style::status_color(row.status);
        let is_dir = side.kind == EntryKind::Dir || row.container;
        let loading = self.loading.contains(&row.path);
        let marker = if !row.container || self.flat {
            " "
        } else if loading {
            "…"
        } else if row.expanded {
            "▾"
        } else {
            "▸"
        };
        base.flex()
            .items_center()
            .gap_2()
            .px_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .pl(px(row.depth as f32 * 16.0))
                    .child(
                        div()
                            .id(("chevron", index * 2 + which as usize))
                            .w(px(14.))
                            .text_color(style::muted())
                            .child(marker)
                            .when(row.container && !self.flat, |d| {
                                d.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.cursor = Some(index);
                                    this.toggle(index, None, cx);
                                }))
                            }),
                    )
                    .child(div().w(px(18.)).text_color(style::faint()).child(if is_dir { "▣" } else { "·" }))
                    .child(
                        div().min_w_0().overflow_hidden().whitespace_nowrap().text_color(color).child(row.name.clone()),
                    ),
            )
            .child(div().w(px(72.)).flex().justify_end().text_color(style::muted()).child(if is_dir {
                String::new()
            } else {
                style::human_size(side.size)
            }))
            .child(
                div()
                    .w(px(124.))
                    .flex()
                    .justify_end()
                    .text_color(style::muted())
                    .child(side.mtime.map(style::date_time).unwrap_or_default()),
            )
            .into_any_element()
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let row = &self.rows[index];
        let selected = self.selected.contains(&row.path);
        let cursor = self.cursor == Some(index);
        let note = row.message.clone();
        // GPUI lays out list rows where the inherited text style does not reach
        // (prepaint): each row names its font, or text falls back to GPUI's default.
        div()
            .id(("row", index))
            .h(px(style::row()))
            .w_full()
            .flex()
            .items_center()
            .font_family(style::fonts(cx).ui.clone())
            .text_size(px(style::code_size()))
            .when(selected, |d| d.bg(style::accent_bg()))
            .when(!selected, |d| d.hover(|s| s.bg(style::hover())))
            .when(cursor, |d| d.border_l_2().border_color(style::accent()))
            .child(self.render_side(row, row.left, index, Side::Left, cx))
            .child(
                div()
                    .id(("status", index))
                    .w(px(36.))
                    .h_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_l_1()
                    .border_r_1()
                    .border_color(style::line())
                    .text_color(style::status_color(row.status))
                    .child(style::status_glyph(row.status))
                    .when_some(note, |d, m| d.tooltip(move |_, cx| cx.new(|_| Tooltip(m.clone())).into())),
            )
            .child(self.render_side(row, row.right, index, Side::Right, cx))
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let s = &self.summary;
        let diffs = s.different + s.left_only + s.right_only + s.errors;
        let filters = [
            (Filter::All, "All", s.files, None),
            (Filter::Differences, "All differences", diffs, Some(style::status_color(Status::Different))),
            (Filter::Same, "Same", s.same + s.similar, Some(style::muted())),
            (Filter::LeftOnly, "Left only", s.left_only, Some(style::status_color(Status::LeftOnly))),
            (Filter::RightOnly, "Right only", s.right_only, Some(style::status_color(Status::RightOnly))),
            (Filter::Errors, "Errors", s.errors, Some(style::status_color(Status::Error))),
        ];
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(style::line())
            .child(div().text_size(px(11.)).text_color(style::muted()).child("SHOW"))
            .children(filters.into_iter().enumerate().map(|(i, (filter, label, count, dot))| {
                style::chip(("filter", i), label, count, dot, self.filter == filter)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_filter(filter, cx)))
            }))
    }

    fn render_panel(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let panel = self.panel.as_ref()?;
        let o = &self.options;
        let toggle = |id: &'static str, label: &'static str, on: bool, f: fn(&mut CompareOptions)| {
            style::toggle(id, label, None, on)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_option(f, cx)))
        };
        let label = |text: &'static str| div().w(px(90.)).text_size(px(12.)).text_color(style::muted()).child(text);
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(style::line())
                .bg(style::chrome())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(label("Files"))
                        .child(div().flex().flex_1().child(panel.includes.clone())),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(label("Leave out"))
                        .child(div().flex().flex_1().child(panel.excludes.clone())),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .child(label("Compare"))
                        .child(toggle("eol", "Line endings do not count", o.ignore_line_endings, |o| {
                            o.ignore_line_endings = !o.ignore_line_endings
                        }))
                        .child(toggle("contents", "Same document text or pixels is similar", o.compare_contents, |o| {
                            o.compare_contents = !o.compare_contents
                        }))
                        .child(toggle("archives", "Open archives", o.open_archives, |o| {
                            o.open_archives = !o.open_archives
                        }))
                        .child(toggle("verify", "Verify archive entries byte for byte", o.verify_checksums, |o| {
                            o.verify_checksums = !o.verify_checksums
                        }))
                        .child(toggle("metadata", "Permissions and times count too", o.compare_metadata, |o| {
                            o.compare_metadata = !o.compare_metadata
                        }))
                        .child(div().flex_1())
                        .child(
                            style::button("apply", "Compare with these patterns", true)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.apply_patterns(cx))),
                        ),
                ),
        )
    }

    fn render_status(&self) -> impl IntoElement {
        let s = &self.summary;
        let text = match &self.progress {
            Progress::Comparing { started, control } => format!(
                "Comparing… {} files compared, {} listed · {:.1} s",
                style::group_digits(control.files_done.load(Ordering::Relaxed)),
                style::group_digits(control.files_listed.load(Ordering::Relaxed)),
                started.elapsed().as_secs_f32()
            ),
            Progress::Failed(e) => format!("The comparison failed: {e}"),
            Progress::Done => format!(
                "Showing {} rows · {} files compared in {} ms: {} same, {} similar, {} different, {} left only, {} right only, {} errors{}{}",
                style::group_digits(self.rows.len() as u64),
                style::group_digits(s.files),
                s.elapsed_ms,
                style::group_digits(s.same),
                style::group_digits(s.similar),
                style::group_digits(s.different),
                style::group_digits(s.left_only),
                style::group_digits(s.right_only),
                style::group_digits(s.errors),
                if self.selected.len() > 1 { format!(" · {} selected", self.selected.len()) } else { String::new() },
                if self.watching { " · watching for changes" } else { "" },
            ),
        };
        div()
            .flex()
            .gap_3()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(style::line())
            .bg(style::chrome())
            .text_size(px(12.))
            .text_color(style::muted())
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().child(text))
            .when_some(self.message.clone(), |d, m| d.child(div().text_color(style::fg()).child(m)))
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}

struct Tooltip(SharedString);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(px(4.))
            .bg(style::fg())
            .text_color(style::bg())
            .text_size(px(12.))
            .child(self.0.clone())
    }
}

impl Render for FolderView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let comparing = matches!(self.progress, Progress::Comparing { .. });
        let side_header = |label: &'static str| {
            div()
                .flex_1()
                .w_0()
                .flex()
                .gap_2()
                .px_2()
                .child(div().flex_1().child(label))
                .child(div().w(px(72.)).flex().justify_end().child("Size"))
                .child(div().w(px(124.)).flex().justify_end().child("Modified"))
        };
        let count = self.rows.len();
        let any = !self.selected.is_empty();
        let menu = self.menu.as_ref().map(|m| {
            let weak = cx.entity().downgrade();
            menu::render("folder-menu", m, move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.menu = None;
                    cx.notify();
                });
            })
        });
        let panel = self.render_panel(cx);
        div()
            .key_context("FolderView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                let next = this.cursor.map_or(0, |i| i + 1);
                this.select(next, false, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                let prev = this.cursor.map_or(0, |i| i.saturating_sub(1));
                this.select(prev, false, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendNext, _, cx| {
                let next = this.cursor.map_or(0, |i| i + 1);
                this.select(next, true, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendPrevious, _, cx| {
                let prev = this.cursor.map_or(0, |i| i.saturating_sub(1));
                this.select(prev, true, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select(0, false, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                let last = this.rows.len().saturating_sub(1);
                this.select(last, false, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                this.selected = this.rows.iter().map(|r| r.path.clone()).collect();
                cx.notify()
            }))
            .on_action(cx.listener(|this, _: &Expand, _, cx| {
                if let Some(i) = this.cursor {
                    this.toggle(i, Some(true), cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| {
                if let Some(i) = this.cursor {
                    this.toggle(i, Some(false), cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Open, _, cx| {
                if let Some(i) = this.cursor {
                    this.open(i, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &CopyRight, window, cx| this.copy_selected(Side::Right, window, cx)))
            .on_action(cx.listener(|this, _: &CopyLeft, window, cx| this.copy_selected(Side::Left, window, cx)))
            .on_action(cx.listener(|this, _: &DeleteSelected, window, cx| this.delete_selected(None, window, cx)))
            .on_action(cx.listener(|this, _: &ShowMenu, _, cx| {
                let at = point(px(240.), px(160.) + px(style::row()) * this.cursor.unwrap_or(0).min(20) as f32);
                this.show_menu(at, None, cx)
            }))
            .on_action(cx.listener(|this, _: &NextDifference, _, cx| this.next_difference(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousDifference, _, cx| this.next_difference(false, cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.run(cx)))
            .size_full()
            .flex()
            .flex_col()
            // No background of its own: the window's (Root) shows through. Every
            // full-window quad is shaded again on each frame.
            .text_color(style::fg())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(style::line())
                    .bg(style::chrome())
                    .child(
                        style::button("refresh", "⟳ Refresh", !comparing)
                            .when(!comparing, |b| b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.run(cx)))),
                    )
                    .child(
                        style::button("prev", "‹ Previous", count > 0)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.next_difference(false, cx))),
                    )
                    .child(
                        style::button("next", "Next difference ›", count > 0)
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.next_difference(true, cx))),
                    )
                    .child(div().w(px(1.)).h(px(18.)).bg(style::line()))
                    .child(style::button("to-right", "Copy to right →", any).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.copy_selected(Side::Right, window, cx)),
                    ))
                    .child(style::button("to-left", "← Copy to left", any).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.copy_selected(Side::Left, window, cx)),
                    ))
                    .child(style::button("delete", "Delete…", any).on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.delete_selected(None, window, cx)),
                    ))
                    .child(div().w(px(1.)).h(px(18.)).bg(style::line()))
                    .child(style::toggle("flat", "Flat", None, self.flat).on_click(cx.listener(
                        |this, _: &ClickEvent, _, cx| {
                            let flat = !this.flat;
                            this.set_flat(flat, cx)
                        },
                    )))
                    .child(
                        style::button("options", if self.panel.is_some() { "Options ▴" } else { "Options ▾" }, true)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_panel(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(style::muted())
                            .child("F7/F8 differences · Enter opens · Alt+←/→ copies · right-click for more"),
                    ),
            )
            .children(panel)
            .child(self.render_filters(cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(26.))
                    .text_size(px(12.))
                    .text_color(style::muted())
                    .border_b_1()
                    .border_color(style::line())
                    .child(side_header("Name"))
                    .child(div().w(px(36.)))
                    .child(side_header("Name")),
            )
            .child(
                uniform_list(
                    "folder-rows",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                        range.map(|i| this.render_row(i, cx).into_any_element()).collect::<Vec<_>>()
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1(),
            )
            .child(self.render_status())
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Node {
        let dir = tempfile::tempdir().unwrap();
        for (side, files) in [
            ("l", vec![("a/x.txt", "1"), ("a/same.txt", "s"), ("only-left.txt", "l")]),
            ("r", vec![("a/x.txt", "2"), ("a/same.txt", "s"), ("b/new.txt", "n")]),
        ] {
            for (path, content) in files {
                let p = dir.path().join(side).join(path);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, content).unwrap();
            }
        }
        let p = |s: &str| dir.path().join(s).to_string_lossy().into_owned();
        let session = Session::open(&p("l"), &p("r")).unwrap();
        compare::compare(&session.left, &session.right, &CompareOptions::default(), &Control::default()).unwrap().root
    }

    #[test]
    fn rows_follow_open_folders_and_filters() {
        let root = tree();
        let mut rows = Vec::new();
        flatten(&root, "", 0, &HashSet::new(), Filter::All, &mut rows);
        assert_eq!(rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), ["a", "b", "only-left.txt"]);
        rows.clear();
        flatten(&root, "", 0, &HashSet::from(["a".to_string()]), Filter::Differences, &mut rows);
        assert_eq!(rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), ["a", "a/x.txt", "b", "only-left.txt"]);
        rows.clear();
        flatten(&root, "", 0, &HashSet::new(), Filter::LeftOnly, &mut rows);
        assert_eq!(rows.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), ["only-left.txt"]);
    }

    #[test]
    fn flat_rows_list_every_file_by_path() {
        let root = tree();
        let mut rows = Vec::new();
        flatten_files(&root, "", Filter::All, &mut rows);
        assert_eq!(
            rows.iter().map(|r| r.name.to_string()).collect::<Vec<_>>(),
            ["a/same.txt", "a/x.txt", "b/new.txt", "only-left.txt"]
        );
        rows.clear();
        flatten_files(&root, "", Filter::Same, &mut rows);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn patterns_split_on_semicolons_and_commas() {
        fn split(text: &str, excludes: bool) -> Vec<String> {
            split_patterns(text, excludes).unwrap()
        }
        assert_eq!(split(" *.rs; target ,, build/** ", false), ["*.rs", "target", "build/**"]);
        assert!(split("  ", false).is_empty());
        // Glob alternatives, classes and quotes stay whole, as in the Svelte view.
        assert_eq!(split("*.{txt,md} [a,b]*.c 'my file.txt'", false), ["*.{txt,md}", "[a,b]*.c", "my file.txt"]);
        // Exclusions are split at commas only: folder names may have spaces.
        assert_eq!(split("build output, *.{o,obj}, a\\,b", true), ["build output", "*.{o,obj}", "a,b"]);
        assert!(split_patterns("\"open", false).is_err());
        for (list, excludes) in [(vec!["*.{a,b}", "x y", "q\"uote"], false), (vec!["a,b", "c d"], true)] {
            let list: Vec<String> = list.into_iter().map(String::from).collect();
            assert_eq!(split(&join_patterns(&list, excludes), excludes), list);
        }
    }
}

/// The view driven like a user would: keys, actions and answers to prompts, in a
/// headless window.
#[cfg(test)]
mod ui_tests {
    use super::*;
    use gpui::TestAppContext;

    fn sides() -> (tempfile::TempDir, Arc<Session>) {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in [
            ("l/a/x.txt", "1"),
            ("l/a/same.txt", "s"),
            ("l/only-left.txt", "l"),
            ("r/a/x.txt", "2"),
            ("r/a/same.txt", "s"),
            ("r/b/new.txt", "n"),
        ] {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let p = |s: &str| dir.path().join(s).to_string_lossy().into_owned();
        let session = Arc::new(Session::open(&p("l"), &p("r")).unwrap());
        (dir, session)
    }

    fn paths(view: &FolderView) -> Vec<String> {
        view.rows.iter().map(|r| r.path.clone()).collect()
    }

    #[gpui::test]
    fn keyboard_copy_and_delete(cx: &mut TestAppContext) {
        let (dir, session) = sides();
        cx.update(|cx| cx.bind_keys(crate::actions::bindings()));
        cx.update(style::init);
        let (view, cx) = cx.add_window_view(|_, cx| FolderView::new(session, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(paths(view), ["a", "a/same.txt", "a/x.txt", "b", "b/new.txt", "only-left.txt"]);
            assert_eq!(
                (view.summary.files, view.summary.different, view.summary.left_only, view.summary.right_only),
                (4, 1, 1, 1)
            );
        });
        cx.update(|window, cx| window.focus(&view.read(cx).focus));

        // F8 goes to the first difference; F8 again to the next.
        cx.simulate_keystrokes("f8");
        view.read_with(cx, |view, _| assert_eq!(view.rows[view.cursor.unwrap()].path, "a/x.txt"));
        cx.simulate_keystrokes("f8 f8");
        view.read_with(cx, |view, _| assert_eq!(view.rows[view.cursor.unwrap()].path, "only-left.txt"));

        // Alt+→ copies a left-only file without asking.
        cx.simulate_keystrokes("alt-right");
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(dir.path().join("r/only-left.txt")).unwrap(), "l");
        view.read_with(cx, |view, _| assert_eq!(view.summary.left_only, 0));

        // Replacing a file on the other side asks first.
        cx.simulate_keystrokes("home down down alt-right");
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Replace");
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(dir.path().join("r/a/x.txt")).unwrap(), "1");
        view.read_with(cx, |view, _| assert_eq!(view.summary.different, 0));

        // Delete asks; the right-only file goes from the right side.
        let new = view.read_with(cx, |view, _| view.rows.iter().position(|r| r.path == "b/new.txt").unwrap());
        view.update(cx, |view, cx| view.select(new, false, cx));
        cx.simulate_keystrokes("delete");
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer(trash_button());
        cx.run_until_parked();
        assert!(!dir.path().join("r/b/new.txt").exists());
        view.read_with(cx, |view, _| assert_eq!(view.summary.right_only, 0));
    }

    #[gpui::test]
    fn filters_flat_view_and_folders(cx: &mut TestAppContext) {
        let (_dir, session) = sides();
        cx.update(|cx| cx.bind_keys(crate::actions::bindings()));
        cx.update(style::init);
        let (view, cx) = cx.add_window_view(|_, cx| FolderView::new(session, cx));
        cx.run_until_parked();
        view.update(cx, |view, cx| view.set_filter(Filter::Differences, cx));
        view.read_with(cx, |view, _| assert_eq!(paths(view), ["a", "a/x.txt", "b", "b/new.txt", "only-left.txt"]));
        view.update(cx, |view, cx| view.set_flat(true, cx));
        view.read_with(cx, |view, _| assert_eq!(paths(view), ["a/x.txt", "b/new.txt", "only-left.txt"]));
        view.update(cx, |view, cx| {
            view.set_flat(false, cx);
            view.set_filter(Filter::All, cx);
        });
        // ← closes the folder at the cursor, → opens it again.
        cx.update(|window, cx| window.focus(&view.read(cx).focus));
        cx.simulate_keystrokes("home left");
        view.read_with(cx, |view, _| assert_eq!(paths(view), ["a", "b", "b/new.txt", "only-left.txt"]));
        cx.simulate_keystrokes("right");
        view.read_with(cx, |view, _| assert_eq!(paths(view).len(), 6));
    }
    /// The review's crash: Shift+click after a filter dropped the row the
    /// selection started from. The anchor follows its path, or is gone.
    #[gpui::test]
    fn shift_selection_after_the_rows_changed(cx: &mut TestAppContext) {
        let (_dir, session) = sides();
        cx.update(style::init);
        let (view, cx) = cx.add_window_view(|_, cx| FolderView::new(session, cx));
        cx.run_until_parked();
        let selected = |view: &FolderView| view.selected.iter().cloned().collect::<Vec<_>>();
        view.update(cx, |view, cx| {
            let at = |p: &str| view.rows.iter().position(|r| r.path == p).unwrap();
            // The anchor's row is filtered out: Shift+click selects just the row.
            let new = at("b/new.txt");
            view.select(new, false, cx);
            view.set_filter(Filter::LeftOnly, cx);
            assert_eq!(paths(view), ["only-left.txt"]);
            view.select(0, true, cx);
            assert_eq!(selected(view), ["only-left.txt"]);
            // The anchor's row moves: the range starts where it is now.
            view.set_filter(Filter::All, cx);
            let b = view.rows.iter().position(|r| r.path == "b").unwrap();
            view.select(b, false, cx);
            view.set_filter(Filter::Differences, cx);
            let last = view.rows.len() - 1;
            view.select(last, true, cx);
            assert_eq!(selected(view), ["b", "b/new.txt", "only-left.txt"]);
        });
    }

    /// Changed exclusions apply to the watcher too: it starts again with them.
    #[gpui::test]
    fn the_watcher_follows_changed_exclusions(cx: &mut TestAppContext) {
        let (_dir, session) = sides();
        cx.update(style::init);
        let (view, cx) = cx.add_window_view(|_, cx| FolderView::new(session, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert!(view.watching && view.watched_excludes == view.options.excludes));
        cx.update(|window, cx| view.update(cx, |view, cx| view.toggle_panel(window, cx)));
        let excludes = view.read_with(cx, |view, _| view.panel.as_ref().unwrap().excludes.clone());
        excludes.update(cx, |input, cx| input.set_text("b, *.{md,txt}", cx));
        view.update(cx, |view, cx| view.apply_patterns(cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.options.excludes, ["b", "*.{md,txt}"]);
            assert!(view.watching);
            assert_eq!(view.watched_excludes, view.options.excludes);
            assert_eq!(paths(view), ["a"]);
        });
    }
}
