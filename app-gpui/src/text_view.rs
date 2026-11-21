//! The text comparison: two files side by side in one virtualized list. Each
//! frame asks the core for the rows in view ([`parallax_core::rows::side_by_side`],
//! the same function behind the Tauri app's `file_rows` command) and draws them.
//! There is no row cache and no placeholder: building ~45 rows takes well under
//! a frame, and the core's highlighter keeps its parse state between calls.

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    ClickEvent, Context, EventEmitter, FocusHandle, Focusable, Font, Hsla, ScrollStrategy, SharedString, StyledText,
    TextRun, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use parallax_core::highlight::Span;
use parallax_core::rows::{LineView, RowView};
use parallax_core::textdiff::RowKind;

use crate::actions::*;
use crate::session::{self, FileDiff, Session};
use crate::style::{self, TEXT_ROW};

pub enum TextEvent {
    Back,
}

enum State {
    Loading,
    Ready(Box<FileDiff>),
    Failed(String),
}

pub struct TextView {
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
}

impl EventEmitter<TextEvent> for TextView {}

impl Focusable for TextView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TextView {
    pub fn new(
        session: Arc<Session>,
        path: String,
        title: SharedString,
        can_go_back: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let started = Instant::now();
        let work = cx.background_executor().spawn({
            let path = path.clone();
            async move { session::open_file(&session, &path) }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.opened_in = Some(started.elapsed().as_secs_f32());
                this.state = match result {
                    Ok(diff) => State::Ready(Box::new(diff)),
                    Err(e) => State::Failed(e),
                };
                cx.notify();
            });
        })
        .detach();
        TextView {
            path,
            title,
            focus: cx.focus_handle(),
            state: State::Loading,
            scroll: UniformListScrollHandle::new(),
            current: None,
            can_go_back,
            opened_in: None,
        }
    }

    fn jump(&mut self, forward: bool, cx: &mut Context<Self>) {
        let State::Ready(diff) = &self.state else { return };
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
        self.current = Some(next);
        let row = diff.sections()[next].start as usize;
        self.scroll.scroll_to_item(row, ScrollStrategy::Center);
        cx.notify();
    }

    fn render_rows(&mut self, range: std::ops::Range<usize>, mono: &Font) -> Vec<gpui::AnyElement> {
        let State::Ready(diff) = &mut self.state else { return Vec::new() };
        let start = range.start;
        let current = self.current.and_then(|i| diff.sections().get(i)).map(|s| s.start as usize..s.end as usize);
        diff.rows(range.start, range.len())
            .into_iter()
            .enumerate()
            .map(|(i, row)| render_row(&row, current.as_ref().is_some_and(|c| c.contains(&(start + i))), mono))
            .collect()
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (stats, position, sections) = match &self.state {
            State::Ready(diff) => {
                let s = diff.stats();
                let n = diff.sections().len();
                let position = match self.current {
                    Some(i) => format!("{} / {}", i + 1, n),
                    None => format!("– / {n}"),
                };
                (format!("{} changed · {} added · {} removed", s.changed, s.added, s.removed), position, n)
            }
            _ => (String::new(), String::new(), 0),
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
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.emit(TextEvent::Back))),
                )
            })
            .child(
                style::button("prev", "‹ Previous", sections > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.jump(false, cx))),
            )
            .child(
                style::button("next", "Next difference ›", sections > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.jump(true, cx))),
            )
            .child(div().text_size(px(13.)).text_color(style::muted()).child(position))
            .child(div().w(px(12.)))
            .child(div().text_size(px(13.)).font_weight(gpui::FontWeight::SEMIBOLD).child(self.title.clone()))
            .child(div().flex_1())
            .child(div().text_size(px(12.)).text_color(style::muted()).child(stats))
    }

    fn status(&self) -> impl IntoElement {
        let text = match &self.state {
            State::Loading => "Opening…".to_string(),
            State::Failed(e) => format!("Could not open {}: {e}", self.path),
            State::Ready(diff) => {
                let side = |i: usize| {
                    let s = &diff.sides[i];
                    if s.exists {
                        format!(
                            "{} · {} · {} lines · {}",
                            s.encoding,
                            s.line_ending_name(),
                            style::group_digits(s.lines as u64),
                            style::human_size(s.size)
                        )
                    } else {
                        "missing".to_string()
                    }
                };
                format!(
                    "{} rows · {} · Left: {} · Right: {} · opened in {:.2} s{}",
                    style::group_digits(diff.rows_total() as u64),
                    diff.syntax.clone().unwrap_or_else(|| "Plain text".into()),
                    side(0),
                    side(1),
                    self.opened_in.unwrap_or_default(),
                    diff.note.as_ref().map(|n| format!(" · {n}")).unwrap_or_default(),
                )
            }
        };
        div()
            .px_3()
            .py_1()
            .border_t_1()
            .border_color(style::line())
            .bg(style::chrome())
            .text_size(px(12.))
            .text_color(style::muted())
            .child(text)
    }
}

/// Background of a row's side by kind; `None` for a filler where the side has no line.
fn side_colors(kind: RowKind, left: bool) -> (Hsla, Hsla) {
    // (line background, changed characters)
    match (kind, left) {
        (RowKind::Equal, _) => (style::bg(), style::bg()),
        (RowKind::Minor, _) => (style::c(0xeef3fb), style::c(0xd6e2f7)),
        (RowKind::Changed, _) => (style::c(0xfff3d6), style::c(0xf6c978)),
        (RowKind::Removed, true) => (style::c(0xfde4e1), style::c(0xf4aaa3)),
        (RowKind::Added, false) => (style::c(0xe2f4e4), style::c(0x9fd7a8)),
        _ => (style::chrome(), style::chrome()),
    }
}

fn render_row(row: &RowView, current: bool, mono: &Font) -> gpui::AnyElement {
    div()
        .h(px(TEXT_ROW))
        .w_full()
        .flex()
        .child(div().w(px(3.)).h_full().when(current, |d| d.bg(style::accent())))
        .child(render_side(row.left.as_ref(), row.kind, true, mono))
        .child(div().w(px(1.)).h_full().bg(style::line()))
        .child(render_side(row.right.as_ref(), row.kind, false, mono))
        .into_any_element()
}

fn render_side(line: Option<&LineView>, kind: RowKind, left: bool, mono: &Font) -> impl IntoElement {
    let (background, mark) = side_colors(kind, left);
    let Some(line) = line else {
        return div().flex_1().min_w_0().h_full().bg(style::chrome()).into_any_element();
    };
    let (text, runs) = styled_line(line, mono, mark);
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .items_center()
        .bg(background)
        .child(
            div()
                .w(px(64.))
                .flex_none()
                .flex()
                .justify_end()
                .pr_2()
                .text_color(style::faint())
                .child((line.number + 1).to_string()),
        )
        .child(
            div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().child(StyledText::new(text).with_runs(runs)),
        )
        .into_any_element()
}

const TAB: usize = 4;

/// The line as GPUI draws it: tabs expanded, cut ends marked, and text runs for
/// the syntax colours (`spans`) and changed characters (`marks`), which the core
/// gives in UTF-16 units.
fn styled_line(line: &LineView, mono: &Font, mark: Hsla) -> (SharedString, Vec<TextRun>) {
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

    let spans: Vec<(usize, usize, Hsla)> =
        line.spans.iter().map(|&Span(a, b, color)| (out_at[byte(a)], out_at[byte(b)], style::c(color))).collect();
    let marks: Vec<(usize, usize)> = line.marks.iter().map(|&(a, b)| (out_at[byte(a)], out_at[byte(b)])).collect();
    let mut cuts = vec![0, prefix.len(), body_end, out.len()];
    for &(a, b, _) in &spans {
        cuts.extend([a, b]);
    }
    for &(a, b) in &marks {
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
        let background = (in_body && marks.iter().any(|m| m.0 <= a && a < m.1)).then_some(mark);
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            State::Ready(diff) => {
                let rows = diff.rows_total();
                let row_font = mono.clone();
                uniform_list(
                    "text-rows",
                    rows,
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, _cx| {
                        this.render_rows(range, &row_font)
                    }),
                )
                .track_scroll(self.scroll.clone())
                .flex_1()
                .font(mono)
                .text_size(px(13.))
                .into_any_element()
            }
        };
        div()
            .key_context("TextView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &NextDifference, _, cx| this.jump(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousDifference, _, cx| this.jump(false, cx)))
            .on_action(cx.listener(|this, _: &Back, _, cx| {
                if this.can_go_back {
                    cx.emit(TextEvent::Back)
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(style::bg())
            .text_color(style::fg())
            .child(self.header(cx))
            .child(body)
            .child(self.status())
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
        let (text, runs) = styled_line(&l, &mono, mark);
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
        let (text, runs) = styled_line(&l, &mono, style::c(0xff0000));
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
        let (text, runs) = styled_line(&l, &mono, style::c(0xff0000));
        assert!(text.starts_with('…') && text.ends_with('…'));
        assert_eq!(runs.iter().map(|r| r.len).sum::<usize>(), text.len());
    }
}
