//! The three-way merge: local, base and remote side by side on top, the result
//! below. Each conflict is resolved with a version (Alt+arrows, as in the Svelte
//! app), by editing result lines, or with Git's result; Ctrl+S writes the file.

use std::ops::Range;
use std::sync::atomic::{AtomicI32, Ordering};

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, Font, Hsla, MouseButton, MouseDownEvent,
    PromptLevel, ScrollStrategy, SharedString, StyledText, Subscription, UniformListScrollHandle, Window, div,
    prelude::*, px, uniform_list,
};
use parallax_core::merge::BlockKind;
use parallax_core::merge_session::Choice;
use parallax_core::rows::LineView;

use crate::actions::*;
use crate::input::{InputEvent, TextInput};
use crate::merge::{MergeDoc, MergeSource, SaveError};
use crate::style;
use crate::text_view::styled_line;

/// The exit status for `git mergetool`: 0 once the merge started from the command
/// line was saved without conflicts, else 1.
pub static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

pub enum MergeEvent {
    Back,
    /// The result was written; `clean` when it has no conflicts left.
    Saved {
        clean: bool,
    },
}

enum State {
    Loading,
    Ready(Box<MergeDoc>),
    Failed(String),
}

pub struct MergeView {
    focus: FocusHandle,
    state: State,
    inputs: UniformListScrollHandle,
    result: UniformListScrollHandle,
    /// The conflict worked on, by its index among the conflicts.
    current: Option<usize>,
    /// The selected result line.
    selected: Option<u32>,
    editing: Option<(u32, Entity<TextInput>)>,
    can_go_back: bool,
    /// Started by `git mergetool`: saving sets the exit status.
    startup: bool,
    message: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<MergeEvent> for MergeView {}

impl Focusable for MergeView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl MergeView {
    pub fn new(source: MergeSource, can_go_back: bool, startup: bool, cx: &mut Context<Self>) -> Self {
        if startup {
            EXIT_CODE.store(1, Ordering::Relaxed);
        }
        let work = cx.background_executor().spawn(async move { MergeDoc::open(&source) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(doc) => State::Ready(Box::new(doc)),
                    Err(e) => State::Failed(e),
                };
                if this.doc().is_some_and(|d| d.stats().unresolved > 0) {
                    this.jump(true, cx);
                }
                cx.notify();
            });
        })
        .detach();
        MergeView {
            focus: cx.focus_handle(),
            state: State::Loading,
            inputs: UniformListScrollHandle::new(),
            result: UniformListScrollHandle::new(),
            current: None,
            selected: None,
            editing: None,
            can_go_back,
            startup,
            message: None,
            _subscriptions: Vec::new(),
        }
    }

    pub fn title(&self) -> Option<String> {
        self.doc().map(MergeDoc::title)
    }

    fn doc(&self) -> Option<&MergeDoc> {
        match &self.state {
            State::Ready(d) => Some(d),
            _ => None,
        }
    }

    fn doc_mut(&mut self) -> Option<&mut MergeDoc> {
        match &mut self.state {
            State::Ready(d) => Some(d),
            _ => None,
        }
    }

    fn say(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.message = Some(message.into());
        cx.notify();
    }

    /// The next (or previous) conflict, unresolved ones first.
    fn jump(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(doc) = self.doc() else { return };
        let conflicts = doc.conflicts();
        let n = conflicts.len();
        if n == 0 {
            return self.say("This merge has no conflicts.", cx);
        }
        let start = self.current.unwrap_or(if forward { n - 1 } else { 0 });
        let order: Vec<usize> =
            (1..=n).map(|step| if forward { (start + step) % n } else { (start + n - step % n) % n }).collect();
        let next = order.iter().copied().find(|&i| conflicts[i].choice == Choice::Unresolved).unwrap_or(order[0]);
        let c = conflicts[next];
        self.current = Some(next);
        self.inputs.scroll_to_item(c.input_row as usize, ScrollStrategy::Center);
        self.result.scroll_to_item(c.result_row as usize, ScrollStrategy::Center);
        cx.notify();
    }

    fn take(&mut self, choice: Choice, cx: &mut Context<Self>) {
        let current = self.current;
        let Some(doc) = self.doc_mut() else { return };
        let Some(conflict) = current.and_then(|i| doc.conflicts().get(i).copied()) else {
            return self.say("Go to a conflict first (F8).", cx);
        };
        doc.resolve(conflict.block, choice);
        let left = doc.stats().unresolved;
        self.say(
            if left == 0 {
                "All conflicts resolved. Ctrl+S saves.".to_string()
            } else {
                format!("{left} conflicts left.")
            },
            cx,
        );
    }

    fn undo(&mut self, redo: bool, cx: &mut Context<Self>) {
        if let Some(doc) = self.doc_mut()
            && doc.undo(redo)
        {
            self.say(if redo { "Redone." } else { "Undone." }, cx);
        }
    }

    fn save(&mut self, with_markers: bool, overwrite: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc_mut() else { return };
        match doc.save(with_markers, overwrite) {
            Ok(clean) => {
                if self.startup {
                    EXIT_CODE.store(if clean { 0 } else { 1 }, Ordering::Relaxed);
                }
                cx.emit(MergeEvent::Saved { clean });
                let what = if clean { "Saved." } else { "Saved with conflict markers." };
                self.say(what, cx);
            }
            Err(SaveError::Conflicts) => {
                let answer = window.prompt(
                    PromptLevel::Warning,
                    "Conflicts left",
                    Some("Save the file with conflict markers? It stays conflicted for Git."),
                    &["Save with markers", "Keep resolving"],
                    cx,
                );
                cx.spawn_in(window, async move |this, cx| {
                    if answer.await == Ok(0) {
                        let _ = this.update_in(cx, |this, window, cx| this.save(true, overwrite, window, cx));
                    }
                })
                .detach();
            }
            Err(SaveError::Stale) => {
                let answer = window.prompt(
                    PromptLevel::Warning,
                    "File changed on disk",
                    Some("Another program wrote the file since the merge opened. Replace it with this result?"),
                    &["Replace", "Keep the file"],
                    cx,
                );
                cx.spawn_in(window, async move |this, cx| {
                    if answer.await == Ok(0) {
                        let _ = this.update_in(cx, |this, window, cx| this.save(with_markers, true, window, cx));
                    }
                })
                .detach();
            }
            Err(SaveError::Failed(e)) => self.say(e, cx),
        }
    }

    pub fn dirty(&self) -> bool {
        self.doc().is_some_and(|d| d.session.is_dirty())
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_go_back {
            return;
        }
        if !self.doc().is_some_and(|d| d.session.is_dirty()) {
            return cx.emit(MergeEvent::Back);
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "Discard the unsaved merge?",
            Some("The result has not been saved."),
            &["Discard", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |_, cx| cx.emit(MergeEvent::Back));
            }
        })
        .detach();
    }

    fn select_line(&mut self, line: u32, cx: &mut Context<Self>) {
        self.selected = Some(line);
        cx.notify();
    }

    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(doc) = self.doc() else { return };
        let total = doc.session.result().len() as u32;
        if total == 0 {
            return;
        }
        let line = match self.selected {
            None => 0,
            Some(l) if down => (l + 1).min(total - 1),
            Some(l) => l.saturating_sub(1),
        };
        self.selected = Some(line);
        cx.notify();
    }

    fn edit_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(line), Some(doc)) = (self.selected, self.doc()) else {
            return self.say("Select a result line to edit.", cx);
        };
        let text = doc.result_line(line);
        let input = cx.new(|cx| {
            let mut input = TextInput::new("", cx).monospaced();
            input.set_text(text, cx);
            input
        });
        let sub = cx.subscribe_in(&input, window, move |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Submit { .. } => {
                let text = input.read(cx).text().to_string();
                if let Some(doc) = this.doc_mut() {
                    doc.replace_line(line, &text);
                }
                this.editing = None;
                window.focus(&this.focus);
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
        self.editing = Some((line, input));
        self._subscriptions.push(sub);
        cx.notify();
    }

    /// Runs the syntax-aware merge in the background and offers its solutions.
    fn syntax_merge(&mut self, cx: &mut Context<Self>) {
        let Some(inputs) = self.doc().and_then(MergeDoc::syntax_inputs) else {
            return self.say("A side is unchanged: the syntax-aware merge has nothing to add.", cx);
        };
        self.say("Merging by syntax…", cx);
        let work = cx.background_executor().spawn(async move { inputs.run() });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                let message = match result {
                    Err(e) => format!("The syntax-aware merge failed: {e}"),
                    Ok(merged) => {
                        let Some(doc) = this.doc_mut() else { return };
                        let offer = doc.apply_syntax(merged.as_deref());
                        match (offer.outcome, offer.whole.is_some()) {
                            (None, _) => "The syntax-aware merge cannot read this file.".to_string(),
                            (Some(o), false) => format!(
                                "Syntax-aware merge: {} solved, {} made smaller, {} left. Alt+S takes a solution.",
                                o.solved, o.narrowed, o.conflicts
                            ),
                            (Some(o), true) => format!(
                                "The syntax-aware merge changed more than the conflicts ({} conflicts left); its whole result is offered.",
                                o.conflicts
                            ),
                        }
                    }
                };
                this.say(message, cx);
            });
        })
        .detach();
    }

    fn use_syntax_whole(&mut self, cx: &mut Context<Self>) {
        if let Some(doc) = self.doc_mut()
            && doc.use_syntax_whole()
        {
            self.say("Using the syntax-aware result. Ctrl+Z undoes it.", cx);
        }
    }

    fn use_git_result(&mut self, cx: &mut Context<Self>) {
        if let Some(doc) = self.doc_mut()
            && doc.use_git_result()
        {
            self.say("Using the file Git wrote. Ctrl+Z undoes it.", cx);
        }
    }

    /// The conflict holding `block`, as the current one.
    fn focus_block(&mut self, block: u32, cx: &mut Context<Self>) {
        if let Some(doc) = self.doc()
            && let Some(i) = doc.conflicts().iter().position(|c| c.block == block)
        {
            self.current = Some(i);
            cx.notify();
        }
    }

    fn render_inputs(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mono = style::mono(cx);
        let current_block = self.current_block();
        let State::Ready(doc) = &mut self.state else { return Vec::new() };
        let rows = doc.input_rows(range.start as u32, range.len() as u32);
        rows.into_iter()
            .enumerate()
            .map(|(i, row)| {
                let index = range.start + i;
                let block = row.block;
                let current = current_block == Some(block) && row.kind == BlockKind::Conflict;
                let cells = [(&row.local, Column::Local), (&row.base, Column::Base), (&row.remote, Column::Remote)];
                // The font again on the row: see FolderView::render_row.
                div()
                    .id(("in", index))
                    .font(mono.clone())
                    .text_size(px(style::code_size()))
                    .h(px(style::text_row()))
                    .w_full()
                    .flex()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus);
                            this.focus_block(block, cx)
                        }),
                    )
                    .child(div().w(px(3.)).h_full().when(current, |d| d.bg(style::accent())))
                    .children(cells.into_iter().enumerate().map(|(c, (line, column))| {
                        let (bg, mark) = input_colors(row.kind, column);
                        div()
                            .flex_1()
                            .w_0()
                            .min_w_0()
                            .h_full()
                            .when(c > 0, |d| d.border_l_1().border_color(style::line()))
                            .child(line_cell(line.as_ref(), bg, mark, &mono))
                    }))
                    .into_any_element()
            })
            .collect()
    }

    fn current_block(&self) -> Option<u32> {
        let doc = self.doc()?;
        Some(doc.conflicts().get(self.current?)?.block)
    }

    fn render_results(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mono = style::mono(cx);
        let current_block = self.current_block();
        let selected = self.selected;
        let editing = self.editing.as_ref().map(|(l, i)| (*l, i.clone()));
        let State::Ready(doc) = &mut self.state else { return Vec::new() };
        let conflicts = doc.conflicts();
        let rows = doc.result_rows(range.start as u32, range.len() as u32);
        rows.into_iter()
            .enumerate()
            .map(|(i, row)| {
                let index = range.start + i;
                let block = row.block;
                let current = current_block == Some(block);
                let base = div()
                    .id(("out", index))
                    .font(mono.clone())
                    .text_size(px(style::code_size()))
                    .h(px(style::text_row()))
                    .w_full()
                    .flex()
                    .items_center();
                let Some(line) = row.line else {
                    // A conflict's header: what it holds and how to resolve it.
                    let number = conflicts.iter().position(|c| c.block == block).map_or(0, |n| n + 1);
                    let state = match row.choice {
                        Choice::Unresolved => "unresolved",
                        Choice::Local => "local version",
                        Choice::Remote => "remote version",
                        Choice::Base => "base version",
                        Choice::LocalRemote => "both, local first",
                        Choice::RemoteLocal => "both, remote first",
                        Choice::Edited => "edited",
                        Choice::Syntax => "syntax-aware merge",
                        Choice::Auto => "merged",
                    };
                    let chip = |id: usize, label: &'static str, choice: Choice| {
                        style::toggle(("take", index * 8 + id), label, None, row.choice == choice).on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.focus_block(block, cx);
                                this.take(choice, cx)
                            }),
                        )
                    };
                    let unresolved = row.choice == Choice::Unresolved;
                    let suggestion = doc.session.syntax_suggestion(block as usize).is_some();
                    return base
                        .gap_2()
                        .px_2()
                        .bg(if unresolved { style::removed().0 } else { style::chrome() })
                        .child(div().w(px(3.)).h_full().when(current, |d| d.bg(style::accent())))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(if unresolved {
                                    style::status_color(parallax_core::compare::Status::Different)
                                } else {
                                    style::muted()
                                })
                                .child(format!("Conflict {number} · {state}")),
                        )
                        .child(chip(0, "Local", Choice::Local))
                        .child(chip(1, "Base", Choice::Base))
                        .child(chip(2, "Remote", Choice::Remote))
                        .child(chip(3, "Both", Choice::LocalRemote))
                        .child(chip(4, "Both, remote first", Choice::RemoteLocal))
                        .when(suggestion, |d| d.child(chip(5, "Syntax", Choice::Syntax)))
                        .into_any_element();
                };
                let n = line.number;
                let bg = if selected == Some(n) { style::selection() } else { result_color(row.choice) };
                let row_el = style::fill(base, bg)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                            window.focus(&this.focus);
                            this.select_line(n, cx);
                            this.focus_block(block, cx);
                            if e.click_count >= 2 {
                                this.edit_line(window, cx);
                            }
                        }),
                    )
                    .child(div().w(px(3.)).h_full().when(current, |d| d.bg(style::accent())));
                match &editing {
                    Some((l, input)) if *l == n => row_el
                        .child(number_cell(n))
                        .child(div().flex().flex_1().min_w_0().pr_1().child(input.clone()))
                        .into_any_element(),
                    _ => row_el.child(line_cell(Some(&line), bg, bg, &mono)).into_any_element(),
                }
            })
            .collect()
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let doc = self.doc();
        let stats = doc.map(MergeDoc::stats).unwrap_or_default();
        let n = doc.map_or(0, |d| d.conflicts().len());
        let position = match self.current {
            Some(i) => format!("{} / {}", i + 1, n),
            None => format!("– / {n}"),
        };
        let (can_undo, can_redo, dirty) =
            doc.map_or((false, false, false), |d| (d.session.can_undo(), d.session.can_redo(), d.session.is_dirty()));
        let git = doc.is_some_and(|d| d.git_result.is_some());
        let whole = doc.is_some_and(|d| d.syntax_whole.is_some());
        let sep = || div().w(px(1.)).h(px(18.)).bg(style::line());
        let take = |id: &'static str, label: &'static str, choice: Choice| {
            style::button(id, label, n > 0)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.take(choice, cx)))
        };
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
            .child(
                style::button("prev", "‹ Previous", n > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.jump(false, cx))),
            )
            .child(
                style::button("next", "Next conflict ›", n > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.jump(true, cx))),
            )
            .child(div().text_size(px(13.)).text_color(style::muted()).child(position))
            .child(sep())
            .child(take("take-local", "← Local", Choice::Local))
            .child(take("take-base", "↑ Base", Choice::Base))
            .child(take("take-remote", "Remote →", Choice::Remote))
            .child(take("take-both", "↓ Both", Choice::LocalRemote))
            .child(sep())
            .child(
                style::button("undo", "↶ Undo", can_undo)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.undo(false, cx))),
            )
            .child(
                style::button("redo", "↷ Redo", can_redo)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.undo(true, cx))),
            )
            .child(
                style::button("save", if dirty { "● Save" } else { "Save" }, doc.is_some())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.save(false, false, window, cx))),
            )
            .child(
                style::button("syntax", "Syntax-aware merge", n > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.syntax_merge(cx))),
            )
            .when(whole, |d| {
                d.child(
                    style::button("syntax-whole", "Use the syntax-aware result", true)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.use_syntax_whole(cx))),
                )
            })
            .when(git, |d| {
                d.child(
                    style::button("git", "Use Git's result", true)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.use_git_result(cx))),
                )
            })
            .child(div().flex_1())
            .child(div().text_size(px(12.)).text_color(style::muted()).child(format!(
                "{} merged automatically · {} conflicts · {} unresolved",
                stats.automatic, stats.conflicts, stats.unresolved
            )))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Column {
    Local,
    Base,
    Remote,
}

/// Background of an input cell, and of its changed characters.
fn input_colors(kind: BlockKind, column: Column) -> (Hsla, Hsla) {
    match (kind, column) {
        (BlockKind::Unchanged, _) | (_, Column::Base) => (style::bg(), style::bg()),
        (BlockKind::Local, Column::Local) | (BlockKind::Remote, Column::Remote) => style::added(),
        (BlockKind::Same, Column::Local | Column::Remote) => style::minor(),
        (BlockKind::Conflict, Column::Local | Column::Remote) => style::removed(),
        _ => (style::bg(), style::bg()),
    }
}

fn result_color(choice: Choice) -> Hsla {
    match choice {
        Choice::Auto => style::bg(),
        Choice::Unresolved => style::removed().0,
        Choice::Edited | Choice::Syntax => style::minor().0,
        _ => style::added().0,
    }
}

fn number_cell(n: u32) -> impl IntoElement {
    div().w(px(52.)).flex_none().flex().justify_end().pr_2().text_color(style::faint()).child((n + 1).to_string())
}

fn line_cell(line: Option<&LineView>, bg: Hsla, mark: Hsla, mono: &Font) -> gpui::AnyElement {
    let Some(line) = line else {
        return div().size_full().bg(style::chrome()).into_any_element();
    };
    let (text, runs) = styled_line(line, mono, mark, &[]);
    style::fill(div(), bg)
        .size_full()
        .flex()
        .items_center()
        .child(number_cell(line.number))
        .child(
            div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().child(StyledText::new(text).with_runs(runs)),
        )
        .into_any_element()
}

impl Render for MergeView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mono = style::mono(cx);
        let header = self.header(cx);
        let body = match &self.state {
            State::Loading => div().flex_1().p_4().text_color(style::muted()).child("Merging…").into_any_element(),
            State::Failed(e) => div()
                .flex_1()
                .p_4()
                .text_color(style::status_color(parallax_core::compare::Status::Error))
                .child(e.clone())
                .into_any_element(),
            State::Ready(doc) => {
                let inputs = doc.session.input_row_count() as usize;
                let results = doc.session.result_row_count() as usize;
                let [local, base, remote] = doc.labels.clone();
                let output = doc.output.display().to_string();
                let label = |text: String| {
                    div().flex_1().w_0().min_w_0().px_2().overflow_hidden().whitespace_nowrap().child(text)
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .h(px(24.))
                            .items_center()
                            .text_size(px(12.))
                            .text_color(style::muted())
                            .border_b_1()
                            .border_color(style::line())
                            .child(div().w(px(3.)))
                            .child(label(format!("Local: {local}")))
                            .child(label(format!("Base: {base}")))
                            .child(label(format!("Remote: {remote}"))),
                    )
                    .child(
                        uniform_list(
                            "merge-inputs",
                            inputs,
                            cx.processor(|this, range: Range<usize>, _, cx| this.render_inputs(range, cx)),
                        )
                        .track_scroll(self.inputs.clone())
                        .flex_1()
                        .font(mono.clone())
                        .text_size(px(style::code_size())),
                    )
                    .child(
                        div()
                            .flex()
                            .h(px(24.))
                            .items_center()
                            .px_2()
                            .gap_2()
                            .text_size(px(12.))
                            .border_t_1()
                            .border_b_1()
                            .border_color(style::line())
                            .bg(style::chrome())
                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Result"))
                            .child(div().text_color(style::muted()).child(output)),
                    )
                    .child(
                        uniform_list(
                            "merge-result",
                            results,
                            cx.processor(|this, range: Range<usize>, _, cx| this.render_results(range, cx)),
                        )
                        .track_scroll(self.result.clone())
                        .flex_1()
                        .font(mono)
                        .text_size(px(style::code_size())),
                    )
                    .into_any_element()
            }
        };
        let status = match &self.state {
            State::Ready(doc) => format!(
                "{} · {} lines in the result{}{}",
                doc.syntax.clone().unwrap_or_else(|| "Plain text".into()),
                style::group_digits(doc.session.result().len() as u64),
                if doc.session.is_dirty() { " · modified" } else { "" },
                doc.repo.as_ref().map(|(w, p)| format!(" · {p} in {}", w.display())).unwrap_or_default(),
            ),
            _ => String::new(),
        };
        div()
            .key_context("MergeView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NextDifference, _, cx| this.jump(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousDifference, _, cx| this.jump(false, cx)))
            .on_action(cx.listener(|this, _: &TakeLocal, _, cx| this.take(Choice::Local, cx)))
            .on_action(cx.listener(|this, _: &TakeRemote, _, cx| this.take(Choice::Remote, cx)))
            .on_action(cx.listener(|this, _: &TakeBase, _, cx| this.take(Choice::Base, cx)))
            .on_action(cx.listener(|this, _: &TakeBoth, _, cx| this.take(Choice::LocalRemote, cx)))
            .on_action(cx.listener(|this, _: &TakeBothRemoteFirst, _, cx| this.take(Choice::RemoteLocal, cx)))
            .on_action(cx.listener(|this, _: &TakeSyntax, _, cx| {
                let current = this.current_block();
                let has =
                    this.doc().zip(current).is_some_and(|(d, b)| d.session.syntax_suggestion(b as usize).is_some());
                if has {
                    this.take(Choice::Syntax, cx)
                } else {
                    this.say("Run the syntax-aware merge first; this conflict has no solution from it.", cx)
                }
            }))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.undo(false, cx)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.undo(true, cx)))
            .on_action(cx.listener(|this, _: &Save, window, cx| this.save(false, false, window, cx)))
            .on_action(cx.listener(|this, _: &Back, window, cx| this.back(window, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(true, cx)))
            .on_action(cx.listener(|this, _: &SelectPrevious, _, cx| this.move_selection(false, cx)))
            .on_action(cx.listener(|this, _: &EditLine, window, cx| this.edit_line(window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .text_color(style::fg())
            .child(header)
            .child(body)
            .child(
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
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().child(status))
                    .when_some(self.message.clone(), |d, m| d.child(div().text_color(style::fg()).child(m))),
            )
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn resolve_conflicts_with_keys_and_save(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_string_lossy().into_owned();
        // Conflicts closer than a few lines are joined into one; these two are apart.
        let mid = "c1\nc2\nc3\nc4\nc5\n";
        std::fs::write(p("base"), format!("a\nb\n{mid}d\ne\n")).unwrap();
        std::fs::write(p("local"), format!("a\nB-local\n{mid}D-local\ne\n")).unwrap();
        std::fs::write(p("remote"), format!("a\nB-remote\n{mid}D-remote\ne\n")).unwrap();
        let source = MergeSource::Files { base: p("base"), local: p("local"), remote: p("remote"), output: p("out") };
        cx.update(|cx| {
            style::init(cx);
            cx.bind_keys(crate::actions::bindings());
            cx.bind_keys(crate::input::bindings());
        });
        let (view, cx) = cx.add_window_view(|_, cx| MergeView::new(source, false, true, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let doc = view.doc().unwrap();
            assert_eq!(doc.stats().unresolved, 2);
            assert_eq!(view.current, Some(0), "opens on the first unresolved conflict");
        });
        assert_eq!(EXIT_CODE.load(Ordering::Relaxed), 1, "not saved yet");
        cx.update(|window, cx| window.focus(&view.read(cx).focus));
        // Alt+← takes the local version; F8 goes to the next unresolved conflict.
        cx.simulate_keystrokes("alt-left f8 alt-right");
        view.read_with(cx, |view, _| assert_eq!(view.doc().unwrap().stats().unresolved, 0));
        // Ctrl+Z, then Alt+↓ takes both.
        cx.simulate_keystrokes("ctrl-z alt-down");
        cx.simulate_keystrokes("ctrl-s");
        cx.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(p("out")).unwrap(),
            "a\nB-local\nc1\nc2\nc3\nc4\nc5\nD-local\nD-remote\ne\n"
        );
        assert_eq!(EXIT_CODE.load(Ordering::Relaxed), 0, "saved without conflicts");
        // Edit the first result line.
        view.update(cx, |view, cx| view.select_line(0, cx));
        cx.simulate_keystrokes("f2 ctrl-a");
        cx.simulate_input("A");
        cx.simulate_keystrokes("enter ctrl-s");
        cx.run_until_parked();
        assert!(std::fs::read_to_string(p("out")).unwrap().starts_with("A\nB-local\n"));
    }

    #[gpui::test]
    fn saving_with_conflicts_asks_first(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n).to_string_lossy().into_owned();
        std::fs::write(p("base"), "x\n").unwrap();
        std::fs::write(p("local"), "l\n").unwrap();
        std::fs::write(p("remote"), "r\n").unwrap();
        let source = MergeSource::Files { base: p("base"), local: p("local"), remote: p("remote"), output: p("out") };
        cx.update(|cx| {
            style::init(cx);
            cx.bind_keys(crate::actions::bindings());
        });
        let (view, cx) = cx.add_window_view(|_, cx| MergeView::new(source, false, false, cx));
        cx.run_until_parked();
        cx.update(|window, cx| window.focus(&view.read(cx).focus));
        cx.simulate_keystrokes("ctrl-s");
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Save with markers");
        cx.run_until_parked();
        let saved = std::fs::read_to_string(p("out")).unwrap();
        assert!(saved.contains("<<<<<<<") && saved.contains(">>>>>>>"), "{saved}");
    }
}
