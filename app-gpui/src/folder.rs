//! The folder comparison: the core's result tree, flattened into rows for the
//! open folders and the chosen filter, in one virtualized list.
//!
//! Unlike the web view, this view holds the core's [`Node`] tree itself, so
//! there is no second copy of the result and no IPC: opening a folder only
//! changes which rows are listed. Folders the compare did not walk (identical by
//! Git tree id, closed archives) are compared when they are opened, with the
//! same core call as the Tauri app's `expand` command.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gpui::{
    ClickEvent, Context, EventEmitter, FocusHandle, Focusable, ScrollStrategy, SharedString, UniformListScrollHandle,
    Window, div, prelude::*, px, uniform_list,
};
use parallax_core::compare::{self, CompareOptions, Control, Node, Side, Status, Summary};
use parallax_core::tree;
use parallax_core::vfs::EntryKind;

use crate::actions::*;
use crate::session::Session;
use crate::style::{self, ROW};

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

#[derive(Clone)]
struct FlatRow {
    path: String,
    depth: usize,
    name: SharedString,
    status: Status,
    container: bool,
    expanded: bool,
    left: Option<Side>,
    right: Option<Side>,
    message: Option<SharedString>,
}

fn flatten(node: &Node, dir: &str, depth: usize, expanded: &HashSet<String>, filter: Filter, out: &mut Vec<FlatRow>) {
    for child in node.children() {
        let path = if dir.is_empty() { child.name().to_string() } else { format!("{dir}/{}", child.name()) };
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
        out.push(FlatRow {
            path: path.clone(),
            depth,
            name: child.name().to_string().into(),
            status: child.status,
            container,
            expanded: open,
            left: child.left(),
            right: child.right(),
            message: child.message().map(|m| m.to_string().into()),
        });
        if open {
            flatten(child, &path, depth + 1, expanded, filter, out);
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

pub struct FolderView {
    session: Arc<Session>,
    options: CompareOptions,
    focus: FocusHandle,
    progress: Progress,
    root: Option<Node>,
    summary: Summary,
    expanded: HashSet<String>,
    filter: Filter,
    rows: Vec<FlatRow>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    /// Folders whose entries are being compared after being opened.
    loading: HashSet<String>,
    error: Option<SharedString>,
}

impl EventEmitter<FolderEvent> for FolderView {}

impl Focusable for FolderView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
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
            rows: Vec::new(),
            selected: None,
            scroll: UniformListScrollHandle::new(),
            loading: HashSet::new(),
            error: None,
        };
        view.run(cx);
        view
    }

    /// Compares both roots on a background thread; the view keeps showing the
    /// previous result, if any, until the new one is there.
    fn run(&mut self, cx: &mut Context<Self>) {
        if let Progress::Comparing { control, .. } = &self.progress {
            control.cancel.store(true, Ordering::Relaxed);
        }
        let control = Arc::new(Control::default());
        self.progress = Progress::Comparing { started: Instant::now(), control: control.clone() };
        self.error = None;
        let (session, options) = (self.session.clone(), self.options.clone());
        let work = cx.background_executor().spawn({
            let control = control.clone();
            async move { compare::compare(&session.left, &session.right, &options, &control).map_err(|e| e.to_string()) }
        });
        // Counters while it runs, as the web view gets them from progress events.
        cx.spawn({
            let control = control.clone();
            async move |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_millis(120)).await;
                    let running = this
                        .update(cx, |this, cx| {
                            let running = matches!(&this.progress, Progress::Comparing { control: c, .. } if Arc::ptr_eq(c, &control));
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
                    }
                    Err(e) => this.progress = Progress::Failed(e),
                }
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
                    let path =
                        if dir.is_empty() { child.name().to_string() } else { format!("{dir}/{}", child.name()) };
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
        let selected = self.selected.and_then(|i| self.rows.get(i)).map(|r| r.path.clone());
        self.rows.clear();
        if let Some(root) = &self.root {
            flatten(root, "", 0, &self.expanded, self.filter, &mut self.rows);
        }
        self.selected = selected.and_then(|p| self.rows.iter().position(|r| r.path == p));
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.rebuild();
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.rows.len() {
            self.selected = Some(index);
            self.scroll.scroll_to_item(index, ScrollStrategy::Center);
            cx.notify();
        }
    }

    fn toggle(&mut self, index: usize, open: Option<bool>, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else { return };
        if !row.container {
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
                    Err(e) => this.error = Some(format!("{}: {e}", row.path).into()),
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
        if row.container {
            self.toggle(index, None, cx);
        } else {
            cx.emit(FolderEvent::OpenFile(row.path.clone()));
        }
    }

    /// The next (or previous) differing file among the rows, from the selection.
    fn next_difference(&mut self, forward: bool, cx: &mut Context<Self>) {
        let n = self.rows.len();
        if n == 0 {
            return;
        }
        let differs = |r: &FlatRow| !r.container && Filter::Differences.matches(r.status);
        let start = self.selected.unwrap_or(if forward { n - 1 } else { 0 });
        for step in 1..=n {
            let i = if forward { (start + step) % n } else { (start + n - step % n) % n };
            if differs(&self.rows[i]) {
                self.select(i, cx);
                return;
            }
        }
    }

    fn on_click(&mut self, index: usize, event: &ClickEvent, cx: &mut Context<Self>) {
        self.selected = Some(index);
        if event.click_count() >= 2 {
            self.open(index, cx);
        }
        cx.notify();
    }

    fn render_side(
        &self,
        row: &FlatRow,
        side: Option<Side>,
        index: usize,
        chevron: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(side) = side else {
            return div().flex_1().min_w_0().h_full().bg(style::chrome()).into_any_element();
        };
        let color = style::status_color(row.status);
        let is_dir = side.kind == EntryKind::Dir || row.container;
        let loading = self.loading.contains(&row.path);
        let marker = if !row.container {
            " "
        } else if loading {
            "…"
        } else if row.expanded {
            "▾"
        } else {
            "▸"
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
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
                            .id(("chevron", index * 2 + usize::from(chevron)))
                            .w(px(14.))
                            .text_color(style::muted())
                            .child(marker)
                            .when(row.container, |d| {
                                d.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.selected = Some(index);
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

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let row = &self.rows[index];
        let selected = self.selected == Some(index);
        let count_note = row.message.clone();
        div()
            .id(("row", index))
            .h(px(ROW))
            .w_full()
            .flex()
            .items_center()
            .text_size(px(13.))
            .border_b_1()
            .border_color(gpui::transparent_black())
            .when(selected, |d| d.bg(style::accent_bg()))
            .when(!selected, |d| d.hover(|s| s.bg(style::hover())))
            .on_click(cx.listener(move |this, e: &ClickEvent, _, cx| this.on_click(index, e, cx)))
            .child(self.render_side(row, row.left, index, false, cx))
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
                    .when_some(count_note, |d, m| d.tooltip(move |_, cx| cx.new(|_| Tooltip(m.clone())).into())),
            )
            .child(self.render_side(row, row.right, index, true, cx))
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                "Showing {} rows · {} files compared in {} ms: {} same, {} similar, {} different, {} left only, {} right only, {} errors",
                style::group_digits(self.rows.len() as u64),
                style::group_digits(s.files),
                s.elapsed_ms,
                style::group_digits(s.same),
                style::group_digits(s.similar),
                style::group_digits(s.different),
                style::group_digits(s.left_only),
                style::group_digits(s.right_only),
                style::group_digits(s.errors),
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
            .child(text)
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_color(style::status_color(Status::Error)).child(e))
            })
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
                .flex()
                .gap_2()
                .px_2()
                .child(div().flex_1().child(label))
                .child(div().w(px(72.)).flex().justify_end().child("Size"))
                .child(div().w(px(124.)).flex().justify_end().child("Modified"))
        };
        let count = self.rows.len();
        div()
            .key_context("FolderView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| {
                let next = this.selected.map_or(0, |i| i + 1);
                this.select(next, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| {
                let prev = this.selected.map_or(0, |i| i.saturating_sub(1));
                this.select(prev, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select(0, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                let last = this.rows.len().saturating_sub(1);
                this.select(last, cx)
            }))
            .on_action(cx.listener(|this, _: &Expand, _, cx| {
                if let Some(i) = this.selected {
                    this.toggle(i, Some(true), cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Collapse, _, cx| {
                if let Some(i) = this.selected {
                    this.toggle(i, Some(false), cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Open, _, cx| {
                if let Some(i) = this.selected {
                    this.open(i, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &NextDifference, _, cx| this.next_difference(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousDifference, _, cx| this.next_difference(false, cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.run(cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(style::bg())
            .text_color(style::fg())
            .child(
                div()
                    .flex()
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
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(style::muted())
                            .child("F7/F8 differences · Enter opens · ←/→ folders"),
                    ),
            )
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
    }
}
