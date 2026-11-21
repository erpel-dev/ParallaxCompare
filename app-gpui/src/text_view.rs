//! The text comparison: two files side by side in one virtualized list. Each
//! frame asks the core for the rows in view ([`parallax_core::rows::side_by_side`],
//! the same function behind the Tauri app's `file_rows` command) and draws them.
//!
//! Files open in the view the core chooses ([`parallax_core::file_view::choose`],
//! shared with the Tauri app): text, a converted view, a hex dump or the image
//! compare. Editing goes through the core's [`parallax_core::edit::EditSession`]:
//! copying differences, editing a line, undo and redo, saving.

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    ClickEvent, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, Font, Hsla, Image, ImageFormat,
    MouseButton, MouseDownEvent, PromptLevel, ScrollStrategy, ScrollWheelEvent, SharedString, StyledText, Subscription,
    TextRun, UniformListScrollHandle, Window, div, img, point, prelude::*, px, uniform_list,
};
use parallax_core::convert::View;
use parallax_core::edit::Side;
use parallax_core::highlight::Span;
use parallax_core::rows::{LineView, RowView};
use parallax_core::search::{Hit, SearchOptions};
use parallax_core::textdiff::{DiffOptions, RowKind, WhitespaceMode};

use crate::actions::*;
use crate::input::{InputEvent, TextInput};
use crate::menu::{self, Item, Menu};
use crate::session::{self, FileDiff, SIDES, SaveError, Session};
use crate::style;

pub enum TextEvent {
    Back,
    /// A side was saved: the folder view refreshes that path.
    Saved(String),
}

struct Loaded {
    diff: FileDiff,
    /// Left, right and the mask of differing pixels, for the image compare.
    images: [Option<Arc<Image>>; 3],
}

enum State {
    Loading,
    Ready(Box<Loaded>),
    Failed(String),
}

/// Rows selected on one side: from `anchor` to `head`, inclusive.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Selection {
    side: Side,
    anchor: u32,
    head: u32,
}

impl Selection {
    fn rows(&self) -> Range<u32> {
        self.anchor.min(self.head)..self.anchor.max(self.head) + 1
    }
}

struct FindState {
    input: Entity<TextInput>,
    options: SearchOptions,
    hits: Vec<Hit>,
    current: Option<usize>,
    error: Option<String>,
}

pub struct TextView {
    session: Arc<Session>,
    path: String,
    title: SharedString,
    focus: FocusHandle,
    state: State,
    scroll: UniformListScrollHandle,
    /// The difference (section) last jumped to.
    current: Option<usize>,
    /// Opened from a folder comparison, so there is a way back.
    can_go_back: bool,
    opened_in: Option<f32>,
    requested: Option<View>,
    options: DiffOptions,
    selection: Option<Selection>,
    editing: Option<(Side, u32, Entity<TextInput>)>,
    find: Option<FindState>,
    /// Horizontal scroll of the text columns, in pixels.
    h_offset: f32,
    menu: Option<Menu>,
    message: Option<SharedString>,
    show_mask: bool,
    image_mode: ImageMode,
    /// In blink mode: the right image is shown.
    blink_right: bool,
    _subscriptions: Vec<Subscription>,
}

/// How the image compare shows the two images, as in the Svelte app.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ImageMode {
    SideBySide,
    /// The left image faded, with the differing pixels on top.
    Difference,
    /// Left and right in turn, in one place.
    Blink,
}

impl EventEmitter<TextEvent> for TextView {}

impl Focusable for TextView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

fn image(side: Option<&parallax_core::convert::ImageSide>) -> Option<Arc<Image>> {
    side.map(|s| {
        let format = ImageFormat::from_mime_type(&s.mime).unwrap_or(ImageFormat::Png);
        Arc::new(Image::from_bytes(format, s.bytes.clone()))
    })
}

impl TextView {
    pub fn new(
        session: Arc<Session>,
        path: String,
        title: SharedString,
        can_go_back: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = TextView {
            session,
            path,
            title,
            focus: cx.focus_handle(),
            state: State::Loading,
            scroll: UniformListScrollHandle::new(),
            current: None,
            can_go_back,
            opened_in: None,
            requested: None,
            options: DiffOptions::default(),
            selection: None,
            editing: None,
            find: None,
            h_offset: 0.0,
            menu: None,
            message: None,
            show_mask: true,
            image_mode: ImageMode::SideBySide,
            blink_right: false,
            _subscriptions: Vec::new(),
        };
        view.load(cx);
        view
    }

    /// Opens the files (again) in the requested view, on a background thread.
    fn load(&mut self, cx: &mut Context<Self>) {
        let started = Instant::now();
        let (session, path, requested, options) =
            (self.session.clone(), self.path.clone(), self.requested, self.options);
        let work =
            cx.background_executor().spawn(async move { session::open_file(&session, &path, requested, options) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.opened_in = Some(started.elapsed().as_secs_f32());
                this.current = None;
                this.selection = None;
                this.editing = None;
                this.state = match result {
                    Ok(diff) => {
                        let images = match &diff.image {
                            Some(d) => [
                                image(d.left.as_ref()),
                                image(d.right.as_ref()),
                                d.mask.as_ref().map(|m| Arc::new(Image::from_bytes(ImageFormat::Png, m.clone()))),
                            ],
                            None => [None, None, None],
                        };
                        State::Ready(Box::new(Loaded { diff, images }))
                    }
                    Err(e) => State::Failed(e),
                };
                this.search(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn diff(&self) -> Option<&FileDiff> {
        match &self.state {
            State::Ready(l) => Some(&l.diff),
            _ => None,
        }
    }

    fn diff_mut(&mut self) -> Option<&mut FileDiff> {
        match &mut self.state {
            State::Ready(l) => Some(&mut l.diff),
            _ => None,
        }
    }

    pub fn dirty(&self) -> bool {
        self.diff().is_some_and(|d| SIDES.iter().any(|&s| d.dirty(s)))
    }

    fn say(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.message = Some(message.into());
        cx.notify();
    }

    fn jump(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(diff) = self.diff() else { return };
        let n = diff.sections().len();
        if n == 0 {
            return;
        }
        let next = match self.current {
            None if forward => 0,
            None => n - 1,
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        };
        let row = diff.sections()[next].start as usize;
        self.current = Some(next);
        self.scroll.scroll_to_item(row, ScrollStrategy::Center);
        cx.notify();
    }

    /// The difference to copy: the one jumped to, or the one holding the selection.
    fn target_section(&self) -> Option<Range<u32>> {
        let diff = self.diff()?;
        if let Some(sel) = self.selection {
            let row = sel.head;
            if let Some(s) = diff.sections().iter().find(|s| s.start <= row && row < s.end) {
                return Some(s.start..s.end);
            }
        }
        let s = diff.sections().get(self.current?)?;
        Some(s.start..s.end)
    }

    fn copy_difference(&mut self, to: Side, cx: &mut Context<Self>) {
        let Some(rows) = self.target_section() else {
            return self.say("Jump to a difference (F7/F8) or select a line in one first.", cx);
        };
        let Some(diff) = self.diff_mut() else { return };
        if !diff.editable[to as usize] {
            return self.say(format!("The {} side cannot be edited.", side_name(to)), cx);
        }
        if diff.copy_rows(rows, to) {
            let n = diff.sections().len();
            self.current = self.current.map(|i| i.min(n.saturating_sub(1))).filter(|_| n > 0);
            self.search_again(cx);
            self.say(format!("Copied the difference to the {} side.", side_name(to)), cx);
        }
    }

    fn undo(&mut self, redo: bool, cx: &mut Context<Self>) {
        let Some(diff) = self.diff_mut() else { return };
        let done = if redo { diff.redo() } else { diff.undo() };
        if done {
            let n = diff.sections().len();
            self.current = self.current.filter(|&i| i < n);
            self.search_again(cx);
            self.say(if redo { "Redone." } else { "Undone." }, cx);
        }
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_sides(
            SaveRun { sides: SIDES.to_vec(), overwrite: None, saved: Vec::new(), kept: Vec::new() },
            window,
            cx,
        );
    }

    /// Saves the sides of `run` in turn. A side changed on disk is asked about
    /// on its own, as in the Svelte view: the answer applies to that file only.
    fn save_sides(&mut self, mut run: SaveRun, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        let Some(diff) = self.diff_mut() else { return };
        while !run.sides.is_empty() {
            let side = run.sides.remove(0);
            if !diff.dirty(side) {
                continue;
            }
            match diff.save(&session, side, run.overwrite == Some(side)) {
                Ok(()) => run.saved.push(side_name(side)),
                Err(SaveError::Stale) => {
                    let answer = window.prompt(
                        PromptLevel::Warning,
                        &format!("The {} file changed on disk", side_name(side)),
                        Some("Another program changed it after it was opened. Replace it with your version, losing those changes?"),
                        &["Replace", "Keep the file"],
                        cx,
                    );
                    cx.spawn_in(window, async move |this, cx| {
                        match answer.await {
                            Ok(0) => {
                                run.sides.insert(0, side);
                                run.overwrite = Some(side);
                            }
                            Ok(_) => {
                                run.kept.push(side_name(side));
                                run.overwrite = None;
                            }
                            // The question went away unanswered (another one
                            // replaced it): stop, writing nothing more.
                            Err(_) => {
                                let _ = this.update(cx, |this, cx| this.finish_save(run, true, cx));
                                return;
                            }
                        }
                        let _ = this.update_in(cx, |this, window, cx| this.save_sides(run, window, cx));
                    })
                    .detach();
                    return;
                }
                Err(SaveError::Failed(e)) => {
                    if !run.saved.is_empty() {
                        cx.emit(TextEvent::Saved(self.path.clone()));
                    }
                    return self.say(e, cx);
                }
            }
        }
        self.finish_save(run, false, cx);
    }

    fn finish_save(&mut self, run: SaveRun, stopped: bool, cx: &mut Context<Self>) {
        let mut message = match run.saved.as_slice() {
            [] if run.kept.is_empty() && !stopped => "Nothing to save.".to_string(),
            [] => String::new(),
            saved => format!("Saved the {} side.", saved.join(" and ")),
        };
        if !run.kept.is_empty() {
            let kept = run.kept.join(" and ");
            message.push_str(&format!(
                "{}Kept the {kept} file as it is on disk; your edits are not saved.",
                if message.is_empty() { "" } else { " " }
            ));
        }
        if stopped {
            message = format!("{message}{}Saving stopped.", if message.is_empty() { "" } else { " " });
        }
        if !run.saved.is_empty() {
            cx.emit(TextEvent::Saved(self.path.clone()));
        }
        self.say(message, cx);
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_go_back {
            return;
        }
        if !self.dirty() {
            return cx.emit(TextEvent::Back);
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "Discard the unsaved changes?",
            Some("The edits in this file have not been saved."),
            &["Discard", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |_, cx| cx.emit(TextEvent::Back));
            }
        })
        .detach();
    }

    fn select_row(&mut self, side: Side, row: u32, extend: bool, cx: &mut Context<Self>) {
        self.selection = Some(match self.selection {
            Some(s) if extend && s.side == side => Selection { head: row, ..s },
            _ => Selection { side, anchor: row, head: row },
        });
        cx.notify();
    }

    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let total = self.diff().map_or(0, FileDiff::rows_total) as u32;
        if total == 0 {
            return;
        }
        let sel = self.selection.unwrap_or(Selection { side: Side::Left, anchor: 0, head: 0 });
        let row = if down { (sel.head + 1).min(total - 1) } else { sel.head.saturating_sub(1) };
        self.selection = Some(Selection { side: sel.side, anchor: row, head: row });
        self.scroll.scroll_to_item(row as usize, ScrollStrategy::Center);
        cx.notify();
    }

    fn copy_lines(&mut self, cx: &mut Context<Self>) {
        let (Some(sel), Some(diff)) = (self.selection, self.diff()) else { return };
        let text = diff.rows_text(sel.side, sel.rows());
        let lines = sel.rows().len();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.say(
            format!("Copied {lines} line{} from the {} side.", if lines == 1 { "" } else { "s" }, side_name(sel.side)),
            cx,
        );
    }

    fn edit_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sel) = self.selection else { return self.say("Select a line to edit.", cx) };
        let row = sel.head;
        let Some(diff) = self.diff_mut() else { return };
        if !diff.editable[sel.side as usize] {
            return self.say(format!("The {} side cannot be edited.", side_name(sel.side)), cx);
        }
        let text = diff.rows_text(sel.side, row..row + 1);
        let text = text.strip_suffix('\n').unwrap_or(&text).trim_end_matches('\r').to_string();
        let input = cx.new(|cx| {
            let mut input = TextInput::new("", cx).monospaced();
            input.set_text(text, cx);
            input
        });
        let side = sel.side;
        let sub = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Submit { .. } => {
                let text = input.read(cx).text().to_string();
                if let Some(diff) = this.diff_mut() {
                    diff.replace_row(side, row, &text);
                }
                this.editing = None;
                window.focus(&this.focus);
                this.search_again(cx);
                this.say("Line changed.", cx);
            }
            InputEvent::Cancel => {
                this.editing = None;
                window.focus(&this.focus);
                cx.notify();
            }
            InputEvent::Changed | InputEvent::Tab => {}
        });
        window.focus(&input.focus_handle(cx));
        self.editing = Some((side, row, input));
        self._subscriptions.push(sub);
        cx.notify();
    }

    fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = &self.find {
            window.focus(&find.input.focus_handle(cx));
            return;
        }
        let input = cx.new(|cx| TextInput::new("Find in both files", cx));
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Changed => this.search(cx),
            InputEvent::Tab => {}
            InputEvent::Submit { shift } => this.next_hit(!shift, cx),
            InputEvent::Cancel => {
                this.find = None;
                window.focus(&this.focus);
                cx.notify();
            }
        });
        window.focus(&input.focus_handle(cx));
        self.find =
            Some(FindState { input, options: SearchOptions::default(), hits: Vec::new(), current: None, error: None });
        self._subscriptions.push(sub);
        cx.notify();
    }

    /// Searches both files with the core's search, as the `search_diff` command
    /// does, and goes to the first hit.
    fn search(&mut self, cx: &mut Context<Self>) {
        self.find_hits(None, cx);
        self.next_hit(true, cx);
    }

    /// After an edit, undo or copy: the hits in the text as it is now, staying
    /// at the hit that was current (or the next one) without moving the view.
    fn search_again(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &self.find else { return };
        let at = find.current.and_then(|i| find.hits.get(i)).map(|h| h.row);
        self.find_hits(at, cx);
        cx.notify();
    }

    /// Runs the search; the current hit becomes the first one at or after row `at`.
    fn find_hits(&mut self, at: Option<u32>, cx: &gpui::App) {
        let Some(find) = &self.find else { return };
        let query = find.input.read(cx).text().to_string();
        let result = self.diff().map(|d| d.search(&query, &find.options)).unwrap_or(Ok(Vec::new()));
        let Some(find) = &mut self.find else { return };
        match result {
            Ok(hits) => {
                find.current = at.and_then(|row| hits.iter().position(|h| h.row >= row).or(hits.len().checked_sub(1)));
                find.hits = hits;
                find.error = None;
            }
            Err(e) => {
                find.hits.clear();
                find.current = None;
                find.error = Some(e);
            }
        }
    }

    fn next_hit(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else { return };
        let n = find.hits.len();
        if n == 0 {
            find.current = None;
            return cx.notify();
        }
        let next = match find.current {
            None => 0,
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
        };
        find.current = Some(next);
        let row = find.hits[next].row as usize;
        self.scroll.scroll_to_item(row, ScrollStrategy::Center);
        cx.notify();
    }

    fn set_options(&mut self, options: DiffOptions, cx: &mut Context<Self>) {
        self.options = options;
        if let Some(diff) = self.diff_mut() {
            diff.set_options(options);
        }
        self.current = None;
        self.search(cx);
        cx.notify();
    }

    fn scroll_by(&mut self, dx: f32, cx: &mut Context<Self>) {
        self.h_offset = (self.h_offset + dx).clamp(0.0, 200_000.0);
        cx.notify();
    }

    fn on_wheel(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(style::text_row()));
        let dx = if event.modifiers.shift { -f32::from(delta.y) } else { -f32::from(delta.x) };
        if dx != 0.0 {
            self.scroll_by(dx, cx);
            cx.stop_propagation();
        }
    }

    fn view_menu(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let Some(diff) = self.diff() else { return };
        let current = diff.view;
        let weak = cx.entity().downgrade();
        let items = diff
            .views
            .iter()
            .map(|&v| {
                let weak = weak.clone();
                Item::run(v.label(), v == current, move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.requested = Some(v);
                        this.state = State::Loading;
                        this.load(cx);
                    });
                })
            })
            .collect();
        self.menu = Some(Menu { position, items });
        cx.notify();
    }

    fn options_menu(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let o = self.options;
        let weak = cx.entity().downgrade();
        let set = move |options: DiffOptions| {
            let weak = weak.clone();
            move |_: &mut Window, cx: &mut gpui::App| {
                let _ = weak.update(cx, |this, cx| this.set_options(options, cx));
            }
        };
        let items = vec![
            Item::run(
                "Every whitespace difference counts",
                o.whitespace == WhitespaceMode::Exact,
                set(DiffOptions { whitespace: WhitespaceMode::Exact, ..o }),
            ),
            Item::run(
                "Ignore leading, trailing and repeated whitespace",
                o.whitespace == WhitespaceMode::Normalize,
                set(DiffOptions { whitespace: WhitespaceMode::Normalize, ..o }),
            ),
            Item::run(
                "Ignore all whitespace",
                o.whitespace == WhitespaceMode::IgnoreAll,
                set(DiffOptions { whitespace: WhitespaceMode::IgnoreAll, ..o }),
            ),
            Item::Separator,
            Item::run("Ignore case", o.ignore_case, set(DiffOptions { ignore_case: !o.ignore_case, ..o })),
            Item::run("Histogram diff", o.histogram, set(DiffOptions { histogram: !o.histogram, ..o })),
        ];
        self.menu = Some(Menu { position, items });
        cx.notify();
    }

    fn row_menu(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
        let editable = self.diff().map_or([false, false], |d| d.editable);
        let side = self.selection.map_or(Side::Left, |s| s.side);
        let in_section = self.target_section().is_some();
        let items = vec![
            Item::action("Copy lines", "Ctrl+C", self.selection.is_some(), Box::new(CopyLines)),
            Item::action("Edit line…", "F2", editable[side as usize], Box::new(EditLine)),
            Item::Separator,
            Item::action(
                "Copy difference to the left",
                "Alt+←",
                in_section && editable[0],
                Box::new(CopyDifferenceLeft),
            ),
            Item::action(
                "Copy difference to the right",
                "Alt+→",
                in_section && editable[1],
                Box::new(CopyDifferenceRight),
            ),
            Item::Separator,
            Item::action("Find in both files…", "Ctrl+F", true, Box::new(Find)),
        ];
        self.menu = Some(Menu { position, items });
        cx.notify();
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mono = style::mono(cx);
        let selection = self.selection;
        let offset = self.h_offset;
        let editing = self.editing.as_ref().map(|(s, r, input)| (*s, *r, input.clone()));
        let (hits, current_hit) = match &self.find {
            Some(f) => {
                let first = f.hits.partition_point(|h| (h.row as usize) < range.start);
                let last = f.hits.partition_point(|h| (h.row as usize) < range.end);
                (f.hits[first..last].to_vec(), f.current.and_then(|i| f.hits.get(i).cloned()))
            }
            None => (Vec::new(), None),
        };
        let current = self.current;
        let State::Ready(loaded) = &mut self.state else { return Vec::new() };
        let diff = &mut loaded.diff;
        let current = current.and_then(|i| diff.sections().get(i)).map(|s| s.start as usize..s.end as usize);
        let start = range.start;
        let rows = diff.rows(range.start, range.len());
        rows.into_iter()
            .enumerate()
            .map(|(i, row)| {
                let index = start + i;
                let ctx = RowContext {
                    index: index as u32,
                    current: current.as_ref().is_some_and(|c| c.contains(&index)),
                    selection,
                    hits: hits.iter().filter(|h| h.row as usize == index).cloned().collect(),
                    current_hit: current_hit.clone().filter(|h| h.row as usize == index),
                    editing: editing.clone().filter(|(_, r, _)| *r as usize == index).map(|(s, _, input)| (s, input)),
                    offset,
                };
                self.render_row(&row, ctx, &mono, cx)
            })
            .collect()
    }

    fn render_row(&self, row: &RowView, ctx: RowContext, mono: &Font, cx: &mut Context<Self>) -> gpui::AnyElement {
        // The font again on the row: see FolderView::render_row.
        div()
            .id(("row", ctx.index as usize))
            .h(px(style::text_row()))
            .w_full()
            .flex()
            .font(mono.clone())
            .text_size(px(style::code_size()))
            .child(div().w(px(3.)).h_full().when(ctx.current, |d| d.bg(style::accent())))
            .child(self.render_side(row.left.as_ref(), row.kind, Side::Left, &ctx, mono, cx))
            .child(div().w(px(1.)).h_full().bg(style::line()))
            .child(self.render_side(row.right.as_ref(), row.kind, Side::Right, &ctx, mono, cx))
            .into_any_element()
    }

    fn render_side(
        &self,
        line: Option<&LineView>,
        kind: RowKind,
        side: Side,
        ctx: &RowContext,
        mono: &Font,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let (mut background, mark) = side_colors(kind, side == Side::Left);
        let selected = ctx.selection.is_some_and(|s| s.side == side && s.rows().contains(&ctx.index));
        if selected {
            background = style::selection();
        }
        let row = ctx.index;
        let base = style::fill(div(), background)
            .id(("side", row as usize * 2 + side as usize))
            .flex_1()
            .w_0()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    this.select_row(side, row, e.modifiers.shift, cx);
                    if e.click_count >= 2 {
                        this.edit_line(window, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus);
                    if !this.selection.is_some_and(|s| s.side == side && s.rows().contains(&row)) {
                        this.select_row(side, row, false, cx);
                    }
                    this.row_menu(e.position, cx);
                }),
            );
        let number = div()
            .w(px(64.))
            .flex_none()
            .flex()
            .justify_end()
            .pr_2()
            .text_color(style::faint())
            .child(line.map(|l| (l.number + 1).to_string()).unwrap_or_default());
        if let Some((editing_side, input)) = &ctx.editing
            && *editing_side == side
        {
            return base
                .child(number)
                .child(div().flex().flex_1().min_w_0().pr_1().child(input.clone()))
                .into_any_element();
        }
        let Some(line) = line else {
            let filler = if selected { style::selection() } else { style::chrome() };
            return base.bg(filler).into_any_element();
        };
        let s = side as u8;
        let hits: Vec<(u32, u32, bool)> = ctx
            .hits
            .iter()
            .filter(|h| h.side == s)
            .map(|h| (h.start, h.end, ctx.current_hit.as_ref() == Some(h)))
            .collect();
        let (text, runs) = styled_line(line, mono, mark, &hits);
        base.child(number)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(div().ml(px(-ctx.offset)).child(StyledText::new(text).with_runs(runs))),
            )
            .into_any_element()
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let diff = self.diff();
        let sections = diff.map_or(0, |d| d.sections().len());
        let position = match (diff, self.current) {
            (Some(_), Some(i)) => format!("{} / {}", i + 1, sections),
            (Some(_), None) => format!("– / {sections}"),
            _ => String::new(),
        };
        let stats = diff
            .map(|d| {
                let s = d.stats();
                format!("{} changed · {} added · {} removed", s.changed, s.added, s.removed)
            })
            .unwrap_or_default();
        let view_label = diff.map(|d| d.view.label()).unwrap_or("Text");
        let editable = diff.map_or([false, false], |d| d.editable);
        let (can_undo, can_redo) = diff.map_or((false, false), |d| (d.edit.can_undo(), d.edit.can_redo()));
        let dirty = self.dirty();
        let sep = || div().w(px(1.)).h(px(18.)).bg(style::line());
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
            .when(self.can_go_back, |d| {
                d.child(
                    style::button("back", "← Back", true)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.back(window, cx))),
                )
                .child(sep())
            })
            .child(style::button("view", format!("{view_label} ▾"), diff.is_some()).on_click(cx.listener(
                |this, e: &ClickEvent, _, cx| {
                    let p = e.position();
                    this.view_menu(point(p.x, p.y + px(12.)), cx)
                },
            )))
            .child(
                style::button("prev", "‹ Previous", sections > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.jump(false, cx))),
            )
            .child(
                style::button("next", "Next difference ›", sections > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.jump(true, cx))),
            )
            .child(div().text_size(px(13.)).text_color(style::muted()).child(position))
            .child(sep())
            .child(
                style::button("copy-left", "← Copy to left", editable[0] && sections > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_difference(Side::Left, cx))),
            )
            .child(
                style::button("copy-right", "Copy to right →", editable[1] && sections > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.copy_difference(Side::Right, cx))),
            )
            .child(
                style::button("undo", "↶ Undo", can_undo)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.undo(false, cx))),
            )
            .child(
                style::button("redo", "↷ Redo", can_redo)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.undo(true, cx))),
            )
            .child(
                style::button("save", if dirty { "● Save" } else { "Save" }, dirty)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(window, cx))),
            )
            .child(sep())
            .child(
                style::button("find", "Find", diff.is_some())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.open_find(window, cx))),
            )
            .child(style::button("options", "Options ▾", diff.is_some()).on_click(cx.listener(
                |this, e: &ClickEvent, _, cx| {
                    let p = e.position();
                    this.options_menu(point(p.x, p.y + px(12.)), cx)
                },
            )))
            .child(div().w(px(8.)))
            .child(div().text_size(px(13.)).font_weight(gpui::FontWeight::SEMIBOLD).child(self.title.clone()))
            .child(div().flex_1())
            .child(div().text_size(px(12.)).text_color(style::muted()).child(stats))
    }

    fn find_bar(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let find = self.find.as_ref()?;
        let summary = match (&find.error, find.hits.len(), find.current) {
            (Some(e), _, _) => e.clone(),
            (None, 0, _) => "No matches".to_string(),
            (None, n, Some(i)) => format!("{} of {}", i + 1, style::group_digits(n as u64)),
            (None, n, None) => style::group_digits(n as u64),
        };
        let o = find.options.clone();
        let toggle = |id: &'static str, label: &'static str, on: bool, f: fn(&mut SearchOptions)| {
            style::toggle(id, label, None, on).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if let Some(find) = &mut this.find {
                    f(&mut find.options);
                }
                this.search(cx);
            }))
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .border_b_1()
                .border_color(style::line())
                .bg(style::chrome())
                .child(div().flex().w(px(320.)).child(find.input.clone()))
                .child(toggle("match-case", "Aa", o.match_case, |o| o.match_case = !o.match_case))
                .child(toggle("whole-word", "Word", o.whole_word, |o| o.whole_word = !o.whole_word))
                .child(toggle("regex", ".*", o.regex, |o| o.regex = !o.regex))
                .child(
                    style::button("hit-prev", "↑", !find.hits.is_empty())
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.next_hit(false, cx))),
                )
                .child(
                    style::button("hit-next", "↓", !find.hits.is_empty())
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.next_hit(true, cx))),
                )
                .child(div().text_size(px(12.)).text_color(style::muted()).child(summary))
                .child(div().flex_1())
                .child(style::button("find-close", "✕", true).on_click(cx.listener(
                    |this, _: &ClickEvent, window, cx| {
                        this.find = None;
                        window.focus(&this.focus);
                        cx.notify();
                    },
                ))),
        )
    }

    fn status(&self) -> impl IntoElement {
        let text = match &self.state {
            State::Loading => "Opening…".to_string(),
            State::Failed(e) => format!("Could not open {}: {e}", self.path),
            State::Ready(loaded) if loaded.diff.view == View::Image => {
                let side = |i: usize| {
                    let s = &loaded.diff.sides[i];
                    if s.exists { style::human_size(s.size) } else { "missing".to_string() }
                };
                format!(
                    "Image compare · Left: {} · Right: {} · opened in {:.2} s",
                    side(0),
                    side(1),
                    self.opened_in.unwrap_or_default()
                )
            }
            State::Ready(loaded) => {
                let diff = &loaded.diff;
                let side = |i: usize| {
                    let s = &diff.sides[i];
                    let dirty = if diff.dirty(SIDES[i]) { " · modified" } else { "" };
                    if s.exists {
                        format!(
                            "{} · {} · {} lines · {}{dirty}",
                            s.encoding,
                            s.line_ending_name(),
                            style::group_digits(s.lines as u64),
                            style::human_size(s.size)
                        )
                    } else {
                        format!("missing{dirty}")
                    }
                };
                format!(
                    "{} rows · {} · Left: {} · Right: {} · opened in {:.2} s",
                    style::group_digits(diff.rows_total() as u64),
                    diff.syntax.clone().unwrap_or_else(|| "Plain text".into()),
                    side(0),
                    side(1),
                    self.opened_in.unwrap_or_default(),
                )
            }
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

    fn set_image_mode(&mut self, mode: ImageMode, cx: &mut Context<Self>) {
        let start_blinking = mode == ImageMode::Blink && self.image_mode != ImageMode::Blink;
        self.image_mode = mode;
        if start_blinking {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(600)).await;
                    let go_on = this
                        .update(cx, |this, cx| {
                            this.blink_right = !this.blink_right;
                            cx.notify();
                            this.image_mode == ImageMode::Blink
                        })
                        .unwrap_or(false);
                    if !go_on {
                        break;
                    }
                }
            })
            .detach();
        }
        cx.notify();
    }

    /// A strip beside the rows with a mark for every difference, where it is in
    /// the file; clicking a mark jumps there.
    fn render_overview(&self, diff: &FileDiff, cx: &mut Context<Self>) -> gpui::AnyElement {
        const BUCKETS: usize = 300;
        let total = diff.rows_total().max(1) as f32;
        let rows = &diff.edit.diff().rows;
        let mut marks: Vec<(usize, f32, f32, Hsla)> = Vec::new();
        let mut last_bucket = usize::MAX;
        for (i, s) in diff.sections().iter().enumerate() {
            let top = s.start as f32 / total;
            let bucket = (top * BUCKETS as f32) as usize;
            let current = self.current == Some(i);
            if bucket == last_bucket && !current {
                continue;
            }
            last_bucket = bucket;
            let color = if current {
                style::accent()
            } else {
                match rows.get(s.start as usize).map(|r| r.kind) {
                    Some(RowKind::Added) => style::added().1,
                    Some(RowKind::Removed) => style::removed().1,
                    Some(RowKind::Minor) => style::minor().1,
                    _ => style::changed().1,
                }
            };
            marks.push((i, top, (s.end - s.start) as f32 / total, color));
        }
        div()
            .w(px(12.))
            .h_full()
            .flex_none()
            .relative()
            .bg(style::chrome())
            .border_l_1()
            .border_color(style::line())
            .children(marks.into_iter().map(|(i, top, height, color)| {
                div()
                    .id(("mark", i))
                    .absolute()
                    .left(px(1.))
                    .right(px(1.))
                    .top(gpui::relative(top))
                    .h(gpui::relative(height))
                    .min_h(px(3.))
                    .bg(color)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.current = Some(i.saturating_sub(1));
                        if i == 0 {
                            this.current = None;
                        }
                        this.jump(true, cx);
                    }))
            }))
            .into_any_element()
    }

    fn render_image(
        &self,
        loaded: &Loaded,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let Some(d) = &loaded.diff.image else { return div().into_any_element() };
        type Side = Option<parallax_core::convert::ImageSide>;
        let dims = |s: &Side| s.as_ref().map(|s| (s.width.max(1) as f32, s.height.max(1) as f32));
        let (left, right) = (dims(&d.left), dims(&d.right));
        let max_w = left.iter().chain(right.iter()).map(|d| d.0).fold(1.0, f32::max);
        let max_h = left.iter().chain(right.iter()).map(|d| d.1).fold(1.0, f32::max);
        // One scale for both images, so they stay comparable, fitted to the room
        // there is: GPUI's image fills the width when given the full size.
        let single = self.image_mode != ImageMode::SideBySide;
        let avail_w = if single { f32::from(viewport.width) - 60.0 } else { f32::from(viewport.width) / 2.0 - 60.0 };
        let avail_h = f32::from(viewport.height) - 260.0;
        let scale = (avail_w / max_w).min(avail_h / max_h).clamp(0.05, 8.0);
        let picture = |i: usize, size: Option<(f32, f32)>, mask: bool, faded: bool| -> gpui::AnyElement {
            let Some((w, h)) = size.map(|(w, h)| (w * scale, h * scale)) else {
                return div().text_color(style::muted()).child("missing").into_any_element();
            };
            div()
                .relative()
                .w(px(w))
                .h(px(h))
                .when_some(loaded.images[i].clone(), |d, image| {
                    d.child(
                        div().absolute().inset_0().when(faded, |d| d.opacity(0.3)).child(img(image).w(px(w)).h(px(h))),
                    )
                })
                .when(mask, |d| {
                    d.when_some(loaded.images[2].clone(), |d, mask| {
                        d.child(
                            div()
                                .absolute()
                                .inset_0()
                                .opacity(if faded { 1.0 } else { 0.6 })
                                .child(img(mask).w(px(w)).h(px(h))),
                        )
                    })
                })
                .into_any_element()
        };
        let panel = |caption: String, picture: gpui::AnyElement| {
            div()
                .flex_1()
                .w_0()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .child(div().text_size(px(12.)).text_color(style::muted()).child(caption))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .overflow_hidden()
                        .border_1()
                        .border_color(style::line())
                        .bg(style::chrome())
                        .child(picture),
                )
        };
        let caption = |label: &str, side: &Side| {
            side.as_ref()
                .map(|s| format!("{label}: {} × {} · {}", s.width, s.height, s.format))
                .unwrap_or_else(|| format!("{label}: missing"))
        };
        let summary = if d.total_pixels > 0 {
            format!(
                "{} of {} pixels differ ({:.2} %)",
                style::group_digits(d.different_pixels),
                style::group_digits(d.total_pixels),
                d.different_pixels as f64 * 100.0 / d.total_pixels as f64
            )
        } else {
            String::new()
        };
        let body = match self.image_mode {
            ImageMode::SideBySide => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(panel(caption("Left", &d.left), picture(0, left, false, false)))
                .child(panel(caption("Right", &d.right), picture(1, right, self.show_mask, false))),
            ImageMode::Blink => {
                let (i, label, side, size) =
                    if self.blink_right { (1, "Right", &d.right, right) } else { (0, "Left", &d.left, left) };
                div().flex_1().min_h_0().flex().child(panel(caption(label, side), picture(i, size, false, false)))
            }
            ImageMode::Difference => div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(panel("Left, faded, with the differing pixels".into(), picture(0, left.or(right), true, true))),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .pt_2()
                    .text_size(px(12.))
                    .child(div().text_color(style::muted()).child(summary))
                    .child(
                        style::toggle(
                            "mask",
                            "Mark differing pixels",
                            Some(style::status_color(parallax_core::compare::Status::Different)),
                            self.show_mask,
                        )
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.show_mask = !this.show_mask;
                            cx.notify();
                        })),
                    )
                    .child(div().w(px(12.)))
                    .children(
                        [
                            ("Side by side", ImageMode::SideBySide),
                            ("Difference", ImageMode::Difference),
                            ("Blink", ImageMode::Blink),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(n, (label, mode))| {
                            style::toggle(("image-mode", n), label, None, self.image_mode == mode)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_image_mode(mode, cx)))
                        }),
                    ),
            )
            .child(body)
            .into_any_element()
    }
}

/// What a row needs besides its lines: where it is and what is marked in it.
struct RowContext {
    index: u32,
    current: bool,
    selection: Option<Selection>,
    hits: Vec<Hit>,
    current_hit: Option<Hit>,
    editing: Option<(Side, Entity<TextInput>)>,
    offset: f32,
}

/// Where a save is: the sides still to save, the one the user agreed to
/// overwrite, and what was saved or kept so far.
struct SaveRun {
    sides: Vec<Side>,
    overwrite: Option<Side>,
    saved: Vec<&'static str>,
    kept: Vec<&'static str>,
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Left => "left",
        Side::Right => "right",
    }
}

/// Background of a row's side by kind, and of its changed characters.
fn side_colors(kind: RowKind, left: bool) -> (Hsla, Hsla) {
    match (kind, left) {
        (RowKind::Equal, _) => (style::bg(), style::bg()),
        (RowKind::Minor, _) => style::minor(),
        (RowKind::Changed, _) => style::changed(),
        (RowKind::Removed, true) => style::removed(),
        (RowKind::Added, false) => style::added(),
        _ => (style::chrome(), style::chrome()),
    }
}

const TAB: usize = 4;

/// The line as GPUI draws it: tabs expanded, cut ends marked, and text runs for
/// the syntax colours (`spans`), changed characters (`marks`) and search hits,
/// which the core gives in UTF-16 units.
pub(crate) fn styled_line(
    line: &LineView,
    mono: &Font,
    mark: Hsla,
    hits: &[(u32, u32, bool)],
) -> (SharedString, Vec<TextRun>) {
    let src = line.text.as_str();
    // UTF-16 unit -> byte offset in `src`.
    let mut unit_to_byte = Vec::with_capacity(src.len() + 1);
    for (b, ch) in src.char_indices() {
        for _ in 0..ch.len_utf16() {
            unit_to_byte.push(b);
        }
    }
    unit_to_byte.push(src.len());
    let byte = |u: u32| unit_to_byte[(u as usize).min(unit_to_byte.len() - 1)];

    // Expand tabs; `out_at[b]` is where source byte `b` lands in the output.
    let prefix = if line.cut_start.is_some() { "…" } else { "" };
    let mut out = String::with_capacity(src.len() + prefix.len() + 4);
    out.push_str(prefix);
    let mut out_at = vec![0usize; src.len() + 1];
    let mut column = 0usize;
    for (b, ch) in src.char_indices() {
        for k in 0..ch.len_utf8() {
            out_at[b + k] = out.len();
        }
        if ch == '\t' {
            let width = TAB - column % TAB;
            out.extend(std::iter::repeat_n(' ', width));
            column += width;
        } else {
            out.push(ch);
            column += 1;
        }
    }
    out_at[src.len()] = out.len();
    let body_end = out.len();
    if line.cut_end.is_some() {
        out.push('…');
    }
    // Search hits are given for the whole line; on a cut line they shift with it.
    let shift = line.cut_start.unwrap_or(0);
    let spans: Vec<(usize, usize, Hsla)> =
        line.spans.iter().map(|&Span(a, b, color)| (out_at[byte(a)], out_at[byte(b)], style::c(color))).collect();
    let marks: Vec<(usize, usize)> = line.marks.iter().map(|&(a, b)| (out_at[byte(a)], out_at[byte(b)])).collect();
    let hits: Vec<(usize, usize, bool)> = hits
        .iter()
        .filter(|h| h.1 > shift)
        .map(|&(a, b, current)| (out_at[byte(a.saturating_sub(shift))], out_at[byte(b - shift)], current))
        .collect();
    let mut cuts = vec![0, prefix.len(), body_end, out.len()];
    for &(a, b, _) in &spans {
        cuts.extend([a, b]);
    }
    for &(a, b) in &marks {
        cuts.extend([a, b]);
    }
    for &(a, b, _) in &hits {
        cuts.extend([a, b]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut runs: Vec<TextRun> = Vec::with_capacity(cuts.len());
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b {
            continue;
        }
        let in_body = a >= prefix.len() && a < body_end;
        let color = if in_body {
            spans.iter().find(|s| s.0 <= a && a < s.1).map_or(style::fg(), |s| s.2)
        } else {
            style::faint()
        };
        let hit = hits.iter().find(|h| h.0 <= a && a < h.1);
        let background = match hit {
            Some(&(_, _, true)) if in_body => Some(style::accent_bg()),
            Some(_) if in_body => Some(style::search_hit()),
            _ => (in_body && marks.iter().any(|m| m.0 <= a && a < m.1)).then_some(mark),
        };
        match runs.last_mut() {
            Some(last) if last.color == color && last.background_color == background => last.len += b - a,
            _ => runs.push(TextRun {
                len: b - a,
                font: mono.clone(),
                color,
                background_color: background,
                underline: None,
                strikethrough: None,
            }),
        }
    }
    (out.into(), runs)
}

impl Render for TextView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let viewport = window.viewport_size();
        let mono = style::mono(cx);
        let body = match &self.state {
            State::Loading => {
                div().flex_1().p_4().text_color(style::muted()).child("Opening the files…").into_any_element()
            }
            State::Failed(e) => div()
                .flex_1()
                .p_4()
                .text_color(style::status_color(parallax_core::compare::Status::Error))
                .child(e.clone())
                .into_any_element(),
            State::Ready(loaded) if loaded.diff.view == View::Image => self.render_image(loaded, viewport, cx),
            State::Ready(loaded) => {
                let overview = self.render_overview(&loaded.diff, cx);
                let rows = loaded.diff.rows_total();
                let note = loaded
                    .diff
                    .note
                    .clone()
                    .or_else(|| loaded.diff.identical.then(|| "The files are identical.".to_string()));
                let equal_view = loaded.diff.equal_view;
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .when_some(note, |d, note| {
                        d.child(
                            div()
                                .px_3()
                                .py_1()
                                .text_size(px(12.))
                                .bg(style::accent_bg())
                                .text_color(style::fg())
                                .child(note),
                        )
                    })
                    .when_some(equal_view, |d, v| {
                        d.child(
                            div()
                                .id("equal-view")
                                .px_3()
                                .py_1()
                                .text_size(px(12.))
                                .bg(style::accent_bg())
                                .text_color(style::accent())
                                .cursor_pointer()
                                .child(format!("The files are equal in “{}”. Show them there.", v.label()))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    this.requested = Some(v);
                                    this.state = State::Loading;
                                    this.load(cx);
                                })),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .child(
                                uniform_list(
                                    "text-rows",
                                    rows,
                                    cx.processor(|this, range: Range<usize>, _window, cx| this.render_rows(range, cx)),
                                )
                                .track_scroll(self.scroll.clone())
                                .flex_1()
                                .w_0()
                                .h_full()
                                .font(mono)
                                .text_size(px(style::code_size()))
                                .on_scroll_wheel(cx.listener(Self::on_wheel)),
                            )
                            .child(overview),
                    )
                    .into_any_element()
            }
        };
        let find_bar = self.find_bar(cx);
        let menu = self.menu.as_ref().map(|m| {
            let weak = cx.entity().downgrade();
            menu::render("text-menu", m, move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.menu = None;
                    cx.notify();
                });
            })
        });
        div()
            .key_context("TextView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NextDifference, _, cx| this.jump(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousDifference, _, cx| this.jump(false, cx)))
            .on_action(cx.listener(|this, _: &Back, window, cx| this.back(window, cx)))
            .on_action(cx.listener(|this, _: &CopyDifferenceLeft, _, cx| this.copy_difference(Side::Left, cx)))
            .on_action(cx.listener(|this, _: &CopyDifferenceRight, _, cx| this.copy_difference(Side::Right, cx)))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.undo(false, cx)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.undo(true, cx)))
            .on_action(cx.listener(|this, _: &Save, window, cx| this.save(window, cx)))
            .on_action(cx.listener(|this, _: &CopyLines, _, cx| this.copy_lines(cx)))
            .on_action(cx.listener(|this, _: &EditLine, window, cx| this.edit_line(window, cx)))
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find(window, cx)))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.next_hit(true, cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.next_hit(false, cx)))
            .on_action(cx.listener(|this, _: &ScrollLeft, _, cx| this.scroll_by(-80.0, cx)))
            .on_action(cx.listener(|this, _: &ScrollRight, _, cx| this.scroll_by(80.0, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(false, cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                this.state = State::Loading;
                this.load(cx)
            }))
            .size_full()
            .flex()
            .flex_col()
            .text_color(style::fg())
            .child(self.header(cx))
            .children(find_bar)
            .child(body)
            .child(self.status())
            .children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::font;

    fn line(text: &str, spans: Vec<Span>, marks: Vec<(u32, u32)>) -> LineView {
        LineView::new(0, text, spans, marks)
    }

    #[test]
    fn runs_cover_the_text_and_map_utf16_positions_to_bytes() {
        let mono = font("monospace");
        let mark = style::c(0xff0000);
        // "é" is one UTF-16 unit but two bytes; the emoji is two units and four bytes.
        let l = line("é😀ab", vec![Span(0, 1, 0x0000ff)], vec![(3, 4)]);
        let (text, runs) = styled_line(&l, &mono, mark, &[]);
        assert_eq!(text.as_ref(), "é😀ab");
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), text.len());
        assert_eq!(runs[0].len, 2);
        assert_eq!(runs[0].color, style::c(0x0000ff));
        let marked: Vec<_> = runs.iter().filter(|r| r.background_color.is_some()).map(|r| r.len).collect();
        assert_eq!(marked, [1], "only `a` is marked");
    }

    #[test]
    fn tabs_expand_and_shift_later_ranges() {
        let mono = font("monospace");
        let l = line("\tx", Vec::new(), vec![(1, 2)]);
        let (text, runs) = styled_line(&l, &mono, style::c(0xff0000), &[]);
        assert_eq!(text.as_ref(), "    x");
        let mut at = 0;
        for run in &runs {
            if run.background_color.is_some() {
                assert_eq!(&text[at..at + run.len], "x");
            }
            at += run.len;
        }
    }

    #[test]
    fn cut_lines_are_marked_at_both_ends() {
        let mono = font("monospace");
        let long = format!("{}needle{}", "a".repeat(20_000), "b".repeat(20_000));
        let l = LineView::window(0, &long, Vec::new(), vec![(20_000, 20_006)], None);
        let (text, runs) = styled_line(&l, &mono, style::c(0xff0000), &[]);
        assert!(text.starts_with('…') && text.ends_with('…'));
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), text.len());
    }

    #[test]
    fn search_hits_get_their_own_background_and_the_current_one_stands_out() {
        let mono = font("monospace");
        let l = line("find me, find you", Vec::new(), Vec::new());
        let (text, runs) = styled_line(&l, &mono, style::c(0xff0000), &[(0, 4, false), (9, 13, true)]);
        let mut at = 0;
        let mut seen = Vec::new();
        for run in &runs {
            if let Some(bg) = run.background_color {
                seen.push((&text[at..at + run.len], bg == style::accent_bg()));
            }
            at += run.len;
        }
        assert_eq!(seen, [("find", false), ("find", true)]);
    }

    #[test]
    fn selections_cover_rows_in_either_direction() {
        let down = Selection { side: Side::Left, anchor: 3, head: 6 };
        let up = Selection { side: Side::Left, anchor: 6, head: 3 };
        assert_eq!(down.rows(), 3..7);
        assert_eq!(up.rows(), 3..7);
    }
}

/// The view driven like a user would, in a headless window.
#[cfg(test)]
mod ui_tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn copy_a_difference_edit_find_undo_and_save(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (l, r) = (dir.path().join("l.txt"), dir.path().join("r.txt"));
        std::fs::write(&l, "alpha\nbeta\ngamma\n").unwrap();
        std::fs::write(&r, "alpha\nBETA\ngamma\ndelta\n").unwrap();
        let session = Arc::new(Session::open(&l.to_string_lossy(), &r.to_string_lossy()).unwrap());
        cx.update(|cx| {
            style::init(cx);
            cx.bind_keys(crate::actions::bindings());
            cx.bind_keys(crate::input::bindings());
        });
        let (view, cx) = cx.add_window_view(|_, cx| TextView::new(session, String::new(), "l ↔ r".into(), false, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(view.diff().unwrap().sections().len(), 2));
        cx.update(|window, cx| window.focus(&view.read(cx).focus));

        // F8 to the first difference, Alt+← copies it to the left.
        cx.simulate_keystrokes("f8 alt-left");
        view.read_with(cx, |view, _| {
            let diff = view.diff().unwrap();
            assert_eq!(diff.sections().len(), 1);
            assert!(diff.dirty(Side::Left));
        });
        // Ctrl+Z undoes it, Ctrl+Y does it again.
        cx.simulate_keystrokes("ctrl-z");
        view.read_with(cx, |view, _| assert_eq!(view.diff().unwrap().sections().len(), 2));
        cx.simulate_keystrokes("ctrl-y");

        // Edit the first line on the left: select it, F2, type, Enter.
        view.update(cx, |view, cx| view.select_row(Side::Left, 0, false, cx));
        cx.simulate_keystrokes("f2");
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_input("ALPHA");
        cx.simulate_keystrokes("enter");
        // Save writes the left file.
        cx.simulate_keystrokes("ctrl-s");
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(&l).unwrap(), "ALPHA\nBETA\ngamma\n");
        assert_eq!(std::fs::read_to_string(&r).unwrap(), "alpha\nBETA\ngamma\ndelta\n");

        // Find highlights matches on both sides; F3 moves through them.
        cx.simulate_keystrokes("ctrl-f");
        cx.simulate_input("alpha");
        view.read_with(cx, |view, _| {
            let find = view.find.as_ref().unwrap();
            assert_eq!(find.hits.len(), 2, "ALPHA on the left and alpha on the right, ignoring case");
            assert_eq!(find.current, Some(0));
        });
        cx.simulate_keystrokes("enter");
        view.read_with(cx, |view, _| assert_eq!(view.find.as_ref().unwrap().current, Some(1)));
        cx.simulate_keystrokes("escape");
        view.read_with(cx, |view, _| assert!(view.find.is_none()));

        // Ctrl+C copies the selected lines.
        view.update(cx, |view, cx| {
            view.select_row(Side::Right, 2, false, cx);
            view.select_row(Side::Right, 3, true, cx);
        });
        cx.simulate_keystrokes("ctrl-c");
        assert_eq!(cx.read_from_clipboard().and_then(|c| c.text()).as_deref(), Some("gamma\ndelta\n"));
    }

    fn open<'a>(
        cx: &'a mut TestAppContext,
        left: &str,
        right: &str,
    ) -> (tempfile::TempDir, Entity<TextView>, &'a mut gpui::VisualTestContext) {
        let dir = tempfile::tempdir().unwrap();
        let (l, r) = (dir.path().join("l.txt"), dir.path().join("r.txt"));
        std::fs::write(&l, left).unwrap();
        std::fs::write(&r, right).unwrap();
        let session = Arc::new(Session::open(&l.to_string_lossy(), &r.to_string_lossy()).unwrap());
        cx.update(|cx| {
            style::init(cx);
            cx.bind_keys(crate::actions::bindings());
            cx.bind_keys(crate::input::bindings());
        });
        let (view, cx) = cx.add_window_view(|_, cx| TextView::new(session, String::new(), "l ↔ r".into(), false, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.focus(&view.read(cx).focus));
        (dir, view, cx)
    }

    fn edit_line(view: &Entity<TextView>, cx: &mut gpui::VisualTestContext, side: Side, row: u32, text: &str) {
        view.update(cx, |view, cx| view.select_row(side, row, false, cx));
        cx.simulate_keystrokes("f2 ctrl-a");
        cx.simulate_input(text);
        cx.simulate_keystrokes("enter");
    }

    /// The review's data loss: with both files changed on disk, agreeing to
    /// replace one must not replace the other. Each side is asked about.
    #[gpui::test]
    fn overwriting_asks_for_each_side(cx: &mut TestAppContext) {
        let (dir, view, cx) = open(cx, "a\nb\n", "a\nc\n");
        edit_line(&view, cx, Side::Left, 0, "L");
        edit_line(&view, cx, Side::Right, 0, "R");
        let (l, r) = (dir.path().join("l.txt"), dir.path().join("r.txt"));
        std::fs::write(&l, "changed elsewhere, left\n").unwrap();
        std::fs::write(&r, "changed elsewhere, right\n").unwrap();
        cx.simulate_keystrokes("ctrl-s");
        cx.run_until_parked();
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Replace");
        cx.run_until_parked();
        assert!(cx.has_pending_prompt(), "the right file is asked about too");
        cx.simulate_prompt_answer("Keep the file");
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(&l).unwrap(), "L\nb\n");
        assert_eq!(std::fs::read_to_string(&r).unwrap(), "changed elsewhere, right\n");
        view.read_with(cx, |view, _| {
            let diff = view.diff().unwrap();
            assert!(!diff.dirty(Side::Left) && diff.dirty(Side::Right));
            assert!(view.message.as_ref().unwrap().contains("Kept the right file"));
        });
    }

    /// Find counts the text as it is after a copy, an undo or a line edit.
    #[gpui::test]
    fn find_follows_edits(cx: &mut TestAppContext) {
        let (_dir, view, cx) = open(cx, "x\nfoo\n", "x\nbar\n");
        let hits = |view: &Entity<TextView>, cx: &mut gpui::VisualTestContext| {
            view.read_with(cx, |view, _| view.find.as_ref().unwrap().hits.len())
        };
        cx.simulate_keystrokes("ctrl-f");
        cx.simulate_input("foo");
        assert_eq!(hits(&view, cx), 1);
        cx.update(|window, cx| window.focus(&view.read(cx).focus));
        cx.simulate_keystrokes("f8 alt-right");
        assert_eq!(hits(&view, cx), 2, "the copy put foo on the right");
        cx.simulate_keystrokes("ctrl-z");
        assert_eq!(hits(&view, cx), 1);
        edit_line(&view, cx, Side::Left, 0, "foo foo");
        assert_eq!(hits(&view, cx), 3);
    }

    #[gpui::test]
    fn options_and_views(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (l, r) = (dir.path().join("a.json"), dir.path().join("b.json"));
        std::fs::write(&l, r#"{"a":1,"b":"x"}"#).unwrap();
        std::fs::write(&r, "{\n  \"a\": 1,\n  \"b\": \"X\"\n}\n").unwrap();
        let session = Arc::new(Session::open(&l.to_string_lossy(), &r.to_string_lossy()).unwrap());
        cx.update(style::init);
        let (view, cx) = cx.add_window_view(|_, cx| TextView::new(session, String::new(), "json".into(), false, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let diff = view.diff().unwrap();
            // The values differ, so no converted view makes the files equal: text.
            assert_eq!(diff.view, View::Text);
            assert!(diff.views.contains(&View::PrettyJson));
        });
        // Pretty-printed, as the view menu picks it, only the case of one value differs.
        view.update(cx, |view, cx| {
            view.requested = Some(View::PrettyJson);
            view.load(cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let diff = view.diff().unwrap();
            assert_eq!(diff.view, View::PrettyJson);
            assert_eq!(diff.sections().len(), 1);
        });
        // Ignore case: no differences left.
        view.update(cx, |view, cx| view.set_options(DiffOptions { ignore_case: true, ..view.options }, cx));
        view.read_with(cx, |view, _| assert!(view.diff().unwrap().sections().is_empty()));
        // The text view shows the bytes as they are.
        view.update(cx, |view, cx| {
            view.requested = Some(View::Text);
            view.load(cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| assert_eq!(view.diff().unwrap().view, View::Text));
    }

    #[gpui::test]
    fn image_modes_and_blinking(cx: &mut TestAppContext) {
        use image::{ImageBuffer, Rgb};
        let dir = tempfile::tempdir().unwrap();
        let (l, r) = (dir.path().join("a.png"), dir.path().join("b.png"));
        let mut img = ImageBuffer::from_pixel(40, 30, Rgb([250u8, 250, 250]));
        img.save(&l).unwrap();
        for x in 5..15 {
            img.put_pixel(x, 10, Rgb([200, 30, 30]));
        }
        img.save(&r).unwrap();
        let session = Arc::new(Session::open(&l.to_string_lossy(), &r.to_string_lossy()).unwrap());
        cx.update(style::init);
        let (view, cx) = cx.add_window_view(|_, cx| TextView::new(session, String::new(), "png".into(), false, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let diff = view.diff().unwrap();
            assert_eq!(diff.view, View::Image);
            assert_eq!(diff.image.as_ref().unwrap().different_pixels, 10);
        });
        view.update(cx, |view, cx| view.set_image_mode(ImageMode::Blink, cx));
        let first = view.read_with(cx, |view, _| view.blink_right);
        cx.executor().advance_clock(std::time::Duration::from_millis(650));
        cx.run_until_parked();
        assert_ne!(view.read_with(cx, |view, _| view.blink_right), first, "blinks");
        view.update(cx, |view, cx| view.set_image_mode(ImageMode::Difference, cx));
        cx.executor().advance_clock(std::time::Duration::from_millis(1300));
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |view, _| view.image_mode), ImageMode::Difference);
    }
}
