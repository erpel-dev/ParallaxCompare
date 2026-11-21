//! The window: a tab per comparison, and the "New comparison" screen where two
//! sources are typed or picked. Follows the system's light or dark appearance;
//! Ctrl+Shift+L switches by hand.

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, PathPromptOptions, PromptLevel, SharedString,
    Subscription, Window, anchored, deferred, div, point, prelude::*, px,
};

use crate::actions::*;
use crate::input::{InputEvent, TextInput};
use crate::style;
use crate::workspace::{Start, Workspace};
use parallax_core::source::{Completion, CompletionKind, complete};

enum Tab {
    New(Entity<NewCompare>),
    Compare(Entity<Workspace>),
}

pub struct Root {
    tabs: Vec<Tab>,
    active: usize,
    focus: FocusHandle,
    palette: Option<Palette>,
    _subscriptions: Vec<Subscription>,
}

/// The command palette: a filter field over every command, with its keys.
struct Palette {
    input: Entity<TextInput>,
    /// Where the keyboard was, to run the command there.
    return_to: Option<FocusHandle>,
}

impl Root {
    pub fn new(start: Option<Start>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        style::follow(window.appearance());
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            style::follow(window.appearance());
            cx.refresh_windows();
        });
        // The window manager's close button and Alt+F4 ask first, as Ctrl+W does.
        let weak = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            weak.update(cx, |this, cx| this.close_window(window, cx)).unwrap_or(true)
        });
        let mut root = Root {
            tabs: Vec::new(),
            active: 0,
            focus: cx.focus_handle(),
            palette: None,
            _subscriptions: vec![appearance],
        };
        match start {
            Some(start) => root.open(start, window, cx),
            None => root.new_tab(window, cx),
        }
        root
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let screen = cx.new(|cx| NewCompare::new(window, cx));
        let sub = cx.subscribe_in(&screen, window, |this, screen, event: &Begin, window, cx| {
            let Begin(start) = event;
            if let Some(i) = this.tabs.iter().position(|t| matches!(t, Tab::New(s) if s == screen)) {
                let view = cx.new(|cx| Workspace::new(start.clone(), window, cx));
                this.tabs[i] = Tab::Compare(view);
                this.activate(i, window, cx);
            }
        });
        self._subscriptions.push(sub);
        self.tabs.push(Tab::New(screen));
        let last = self.tabs.len() - 1;
        self.activate(last, window, cx);
    }

    pub fn open(&mut self, start: Start, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| Workspace::new(start, window, cx));
        self.tabs.push(Tab::Compare(view));
        let last = self.tabs.len() - 1;
        self.activate(last, window, cx);
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = index.min(self.tabs.len().saturating_sub(1));
        match self.tabs.get(self.active) {
            Some(Tab::New(v)) => window.focus(&v.focus_handle(cx)),
            Some(Tab::Compare(v)) => window.focus(&v.focus_handle(cx)),
            None => window.focus(&self.focus),
        }
        cx.notify();
    }

    fn unsaved(tab: &Tab, cx: &gpui::App) -> bool {
        matches!(tab, Tab::Compare(w) if w.read(cx).unsaved(cx))
    }

    /// Closes a tab, after asking when it holds unsaved edits.
    fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else { return };
        let Tab::Compare(workspace) = tab else { return self.remove_tab(index, window, cx) };
        if !Self::unsaved(tab, cx) {
            return self.remove_tab(index, window, cx);
        }
        let workspace = workspace.clone();
        self.activate(index, window, cx);
        let answer = window.prompt(
            PromptLevel::Warning,
            "Close the tab without saving?",
            Some("The changes in this tab have not been saved."),
            &["Close without saving", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update_in(cx, |this, window, cx| {
                    let index = this.tabs.iter().position(|t| matches!(t, Tab::Compare(w) if *w == workspace));
                    if let Some(index) = index {
                        this.remove_tab(index, window, cx);
                    }
                });
            }
        })
        .detach();
    }

    /// Whether the window may close now; with unsaved edits it asks, and closes
    /// once the answer is to discard them.
    fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let unsaved = self.tabs.iter().filter(|t| Self::unsaved(t, cx)).count();
        if unsaved == 0 {
            return true;
        }
        if let Some(first) = self.tabs.iter().position(|t| Self::unsaved(t, cx)) {
            self.activate(first, window, cx);
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &if unsaved == 1 {
                "Quit without saving?".to_string()
            } else {
                format!("Quit without saving {unsaved} tabs?")
            },
            Some("Changes that have not been saved are lost."),
            &["Quit without saving", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |_, cx| {
            if answer.await == Ok(0) {
                let _ = cx.update(|window, _| window.remove_window());
            }
        })
        .detach();
        false
    }

    fn remove_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            return self.new_tab(window, cx);
        }
        let next = if self.active > index { self.active - 1 } else { self.active };
        self.activate(next, window, cx);
    }

    fn cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.tabs.len() as isize;
        if n > 1 {
            let next = (self.active as isize + step).rem_euclid(n) as usize;
            self.activate(next, window, cx);
        }
    }

    fn show_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let return_to = window.focused(cx);
        let input = cx.new(|cx| TextInput::new("Type a command…", cx));
        let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Changed => cx.notify(),
            InputEvent::Tab => {}
            InputEvent::Submit { .. } => {
                let first = this.matching_commands(cx).into_iter().next();
                if let Some(command) = first {
                    this.run_command(command.action, window, cx);
                }
            }
            InputEvent::Cancel => this.close_palette(window, cx),
        });
        self._subscriptions.push(sub);
        window.focus(&input.focus_handle(cx));
        self.palette = Some(Palette { input, return_to });
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(palette) = self.palette.take() {
            match palette.return_to {
                Some(focus) => window.focus(&focus),
                None => self.activate(self.active, window, cx),
            }
        }
        cx.notify();
    }

    fn matching_commands(&self, cx: &gpui::App) -> Vec<Command> {
        let query = self.palette.as_ref().map(|p| p.input.read(cx).text().to_lowercase()).unwrap_or_default();
        commands()
            .into_iter()
            .filter(|c| {
                query
                    .split_whitespace()
                    .all(|w| c.label.to_lowercase().contains(w) || c.group.to_lowercase().contains(w))
            })
            .collect()
    }

    /// Runs a command where the keyboard was before the palette opened.
    fn run_command(&mut self, action: fn() -> Box<dyn gpui::Action>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_palette(window, cx);
        window.dispatch_action(action(), cx);
    }

    fn render_palette(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let palette = self.palette.as_ref()?;
        let width = px(560.);
        let x = (window.viewport_size().width - width) / 2.;
        let items = self.matching_commands(cx).into_iter().enumerate().map(|(i, c)| {
            let action = c.action;
            div()
                .id(("command", i))
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py(px(5.))
                .rounded(px(4.))
                .text_size(px(13.))
                .when(i == 0, |d| d.bg(style::accent_bg()))
                .hover(|s| s.bg(style::hover()))
                .cursor_pointer()
                .child(div().w(px(70.)).text_size(px(11.)).text_color(style::faint()).child(c.group))
                .child(div().flex_1().child(c.label))
                .child(div().text_size(px(12.)).text_color(style::muted()).child(c.keys))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.run_command(action, window, cx)))
        });
        Some(
            deferred(
                anchored().position(point(x, px(70.))).child(
                    div()
                        .id("palette")
                        .occlude()
                        .w(width)
                        .p_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(style::line())
                        .bg(style::overlay())
                        .shadow_lg()
                        .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
                        .child(div().flex().child(palette.input.clone()))
                        .child(div().id("palette-list").max_h(px(420.)).overflow_y_scroll().children(items))
                        .child(
                            div()
                                .px_1()
                                .pt_1()
                                .text_size(px(11.))
                                .text_color(style::faint())
                                .child("Enter runs the first match · Escape closes"),
                        ),
                ),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn zoom(&mut self, percent: Option<i32>, cx: &mut Context<Self>) {
        let next = match percent {
            Some(step) => (style::zoom() as i32 + step).max(0) as u32,
            None => 100,
        };
        style::set_zoom(next);
        cx.refresh_windows();
    }

    fn tab_title(&self, tab: &Tab, cx: &gpui::App) -> String {
        match tab {
            Tab::New(_) => "New comparison".into(),
            Tab::Compare(v) => v.read(cx).title(cx),
        }
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = self.render_palette(window, cx);
        let fonts = style::fonts(cx);
        let tabs = self.tabs.iter().enumerate().map(|(i, tab)| {
            let active = i == self.active;
            div()
                .id(("tab", i))
                .flex()
                .items_center()
                .gap_2()
                .pl_3()
                .pr_1()
                .h_full()
                .max_w(px(280.))
                .border_r_1()
                .border_color(style::line())
                .when(active, |d| d.bg(style::bg()).border_t_2().border_color(style::accent()))
                .when(!active, |d| d.text_color(style::muted()).cursor_pointer().hover(|s| s.bg(style::hover())))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.activate(i, window, cx)))
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_size(px(13.))
                        .child(self.tab_title(tab, cx)),
                )
                .child(
                    div()
                        .id(("close", i))
                        .px_1()
                        .rounded(px(3.))
                        .text_color(style::faint())
                        .cursor_pointer()
                        .hover(|s| s.bg(style::hover()).text_color(style::fg()))
                        .child("×")
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.close(i, window, cx)
                        })),
                )
        });
        let content = match self.tabs.get(self.active) {
            Some(Tab::New(v)) => v.clone().into_any_element(),
            Some(Tab::Compare(v)) => v.clone().into_any_element(),
            None => div().into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .track_focus(&self.focus)
            .bg(style::bg())
            .text_color(style::fg())
            .font_family(fonts.ui.clone())
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(window, cx)))
            .on_action(cx.listener(|this, _: &Quit, window, cx| {
                if this.close_window(window, cx) {
                    cx.quit();
                }
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                let active = this.active;
                this.close(active, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| this.cycle(-1, window, cx)))
            .on_action(cx.listener(|_, _: &ToggleTheme, _, cx| {
                style::set_dark(!style::is_dark());
                cx.refresh_windows();
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom(Some(10), cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom(Some(-10), cx)))
            .on_action(cx.listener(|this, _: &ZoomReset, _, cx| this.zoom(None, cx)))
            .on_action(cx.listener(|this, _: &ShowPalette, window, cx| this.show_palette(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(34.))
                    .bg(style::chrome())
                    .border_b_1()
                    .border_color(style::line())
                    .children(tabs)
                    .child(
                        div()
                            .id("new-tab")
                            .px_3()
                            .h_full()
                            .flex()
                            .items_center()
                            .text_color(style::muted())
                            .cursor_pointer()
                            .hover(|s| s.bg(style::hover()))
                            .child("+")
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.new_tab(window, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("theme")
                            .px_3()
                            .h_full()
                            .flex()
                            .items_center()
                            .text_size(px(12.))
                            .text_color(style::muted())
                            .cursor_pointer()
                            .hover(|s| s.bg(style::hover()))
                            .child(if style::is_dark() { "☀ Light" } else { "☾ Dark" })
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                style::set_dark(!style::is_dark());
                                cx.refresh_windows();
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(content))
            .children(palette)
    }
}

/// What a completion list shows: the part being completed (the last folder or
/// file name, or the ref), since the rest is what was typed.
fn completion_label(c: &Completion) -> String {
    match c.kind {
        CompletionKind::Ref => c.text.rsplit('@').next().unwrap_or(&c.text).to_string(),
        _ => {
            let dir = c.text.ends_with('/');
            let name = c.text.trim_end_matches('/').rsplit(['/', '\\', '!']).next().unwrap_or(&c.text);
            if dir { format!("{name}/") } else { name.to_string() }
        }
    }
}

/// Open this in the tab of the "New comparison" screen.
pub struct Begin(pub Start);

pub struct NewCompare {
    left: Entity<TextInput>,
    right: Entity<TextInput>,
    error: Option<SharedString>,
    /// Completions for the field being typed in (true: the left one).
    suggestions: Option<(bool, Vec<Completion>)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Begin> for NewCompare {}

impl Focusable for NewCompare {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.left.focus_handle(cx)
    }
}

impl NewCompare {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let left = cx.new(|cx| TextInput::new("Folder, file, archive or repo@revision", cx).monospaced());
        let right = cx.new(|cx| TextInput::new("Folder, file, archive or repo@revision", cx).monospaced());
        let mut subs = Vec::new();
        for (input, next, is_left) in [(&left, Some(right.clone()), true), (&right, None, false)] {
            subs.push(cx.subscribe_in(input, window, move |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Submit { .. } => {
                    this.suggestions = None;
                    match &next {
                        Some(next) => window.focus(&next.focus_handle(cx)),
                        None => this.start(cx),
                    }
                }
                // Completions as the Tauri app's path field offers them.
                InputEvent::Changed => {
                    let text = input.read(cx).text().to_string();
                    let found = if text.is_empty() { Vec::new() } else { complete(&text, 8) };
                    this.suggestions = (!found.is_empty()).then_some((is_left, found));
                    cx.notify();
                }
                // Tab takes the first completion.
                InputEvent::Tab => {
                    if let Some((side, list)) = &this.suggestions
                        && *side == is_left
                        && let Some(first) = list.first()
                    {
                        let text = first.text.clone();
                        input.update(cx, |i, cx| i.set_cursor_text(text, cx));
                    }
                }
                InputEvent::Cancel => {
                    this.suggestions = None;
                    cx.notify();
                }
            }));
        }
        NewCompare { left, right, error: None, suggestions: None, _subscriptions: subs }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let (left, right) =
            (self.left.read(cx).text().trim().to_string(), self.right.read(cx).text().trim().to_string());
        if left.is_empty() || right.is_empty() {
            self.error = Some("Enter both sides.".into());
            return cx.notify();
        }
        cx.emit(Begin(Start::Compare { left, right }));
    }

    /// The conflicted files of the repository the left field names.
    fn conflicts(&mut self, cx: &mut Context<Self>) {
        let dir = self.left.read(cx).text().trim().to_string();
        if dir.is_empty() {
            self.error = Some("Enter the repository's folder on the left.".into());
            return cx.notify();
        }
        cx.emit(Begin(Start::Conflicts { dir: dir.into() }));
    }

    fn swap(&mut self, cx: &mut Context<Self>) {
        let (l, r) = (self.left.read(cx).text().to_string(), self.right.read(cx).text().to_string());
        self.left.update(cx, |i, cx| i.set_text(r, cx));
        self.right.update(cx, |i, cx| i.set_text(l, cx));
    }

    fn browse(&mut self, left: bool, folders: bool, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: !folders,
            directories: folders,
            multiple: false,
            prompt: Some(if left { "Left side".into() } else { "Right side".into() }),
        });
        let input = if left { self.left.clone() } else { self.right.clone() };
        cx.spawn(async move |this, cx| {
            let chosen = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(e)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some(format!("No file dialog: {e}. Type the path instead.").into());
                        cx.notify();
                    });
                    None
                }
                _ => None,
            };
            if let Some(path) = chosen {
                let _ = input.update(cx, |i, cx| i.set_text(path.to_string_lossy().into_owned(), cx));
            }
        })
        .detach();
    }
}

impl Render for NewCompare {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let side = |label: &'static str, input: &Entity<TextInput>, left: bool| {
            div()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(div().text_size(px(12.)).text_color(style::muted()).child(label))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(div().flex().flex_1().child(input.clone()))
                        .child(
                            style::button(if left { "browse-left-dir" } else { "browse-right-dir" }, "Folder…", true)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.browse(left, true, cx))),
                        )
                        .child(
                            style::button(if left { "browse-left-file" } else { "browse-right-file" }, "File…", true)
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.browse(left, false, cx))),
                        ),
                )
                .when_some(self.suggestions.as_ref().filter(|(l, _)| *l == left), |d, (_, list)| {
                    d.child(
                        div()
                            .flex()
                            .flex_col()
                            .p_1()
                            .rounded(px(5.))
                            .border_1()
                            .border_color(style::line())
                            .bg(style::overlay())
                            .children(list.iter().enumerate().map(|(i, c)| {
                                let text = c.text.clone();
                                let input = input.clone();
                                let icon = match c.kind {
                                    CompletionKind::Dir => "▣",
                                    CompletionKind::Archive => "▤",
                                    CompletionKind::File => "·",
                                    CompletionKind::Ref => "⑂",
                                };
                                div()
                                    .id(("suggestion", i))
                                    .flex()
                                    .gap_2()
                                    .px_2()
                                    .py(px(2.))
                                    .rounded(px(3.))
                                    .text_size(px(12.5))
                                    .when(i == 0, |d| d.bg(style::accent_bg()))
                                    .hover(|s| s.bg(style::hover()))
                                    .cursor_pointer()
                                    .child(div().w(px(14.)).text_color(style::faint()).child(icon))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .child(completion_label(c)),
                                    )
                                    .child(div().text_color(style::muted()).child(c.detail.clone().unwrap_or_default()))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                        let text = text.clone();
                                        input.update(cx, |i, cx| i.set_cursor_text(text, cx));
                                        window.focus(&input.focus_handle(cx));
                                        this.suggestions = None;
                                    }))
                            }))
                            .child(
                                div()
                                    .px_2()
                                    .pt_1()
                                    .text_size(px(11.))
                                    .text_color(style::faint())
                                    .child("Tab takes the first"),
                            ),
                    )
                })
        };
        div().size_full().flex().justify_center().pt(px(72.)).bg(style::bg()).child(
            div()
                .w(px(720.))
                .flex()
                .flex_col()
                .gap_4()
                .child(div().text_size(px(22.)).font_weight(gpui::FontWeight::SEMIBOLD).child("New comparison"))
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(style::muted())
                        .child("Compare two folders, files, archives (a.zip, b.7z!/dir) or Git revisions (repo@main, repo@HEAD~3:src)."),
                )
                .child(side("Left", &self.left, true))
                .child(div().flex().justify_center().child(style::button("swap", "⇅ Swap", true).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.swap(cx)))))
                .child(side("Right", &self.right, false))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .id("compare")
                                .px_4()
                                .py_1p5()
                                .rounded(px(6.))
                                .bg(style::accent())
                                .text_color(style::bg())
                                .text_size(px(14.))
                                .cursor_pointer()
                                .child("Compare")
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.start(cx))),
                        )
                        .child(
                            style::button("conflicts", "Resolve conflicts in the left folder's repository", true)
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.conflicts(cx))),
                        )
                        .when_some(self.error.clone(), |d, e| d.child(div().text_size(px(13.)).text_color(style::status_color(parallax_core::compare::Status::Error)).child(e))),
                )
                .child(
                    div()
                        .pt_4()
                        .text_size(px(12.))
                        .text_color(style::faint())
                        .child("Ctrl+T new tab · Ctrl+W close tab · Ctrl+Tab next tab · Ctrl+Shift+L light or dark"),
                ),
        )
    }
}

#[cfg(test)]
mod ui_tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn new_comparison_from_typed_paths_and_tabs(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        for side in ["l", "r"] {
            std::fs::create_dir_all(dir.path().join(side)).unwrap();
            std::fs::write(dir.path().join(side).join("f.txt"), side).unwrap();
        }
        cx.update(|cx| {
            style::init(cx);
            cx.bind_keys(crate::actions::bindings());
            cx.bind_keys(crate::input::bindings());
        });
        let (root, cx) = cx.add_window_view(|window, cx| Root::new(None, window, cx));
        root.read_with(cx, |root, _| assert!(matches!(root.tabs[..], [Tab::New(_)])));
        // Typed into the left field, Enter moves to the right one, Enter compares.
        cx.simulate_input(&dir.path().join("l").to_string_lossy());
        cx.simulate_keystrokes("enter");
        cx.simulate_input(&dir.path().join("r").to_string_lossy());
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        root.read_with(cx, |root, cx| {
            let [Tab::Compare(ws)] = &root.tabs[..] else { panic!("one comparison tab") };
            assert_eq!(ws.read(cx).title(cx), "l ↔ r");
        });
        // Ctrl+T adds a tab, Ctrl+W closes it, Ctrl+Shift+L switches the theme.
        cx.simulate_keystrokes("ctrl-t");
        root.read_with(cx, |root, _| assert_eq!((root.tabs.len(), root.active), (2, 1)));
        cx.simulate_keystrokes("ctrl-w");
        root.read_with(cx, |root, _| assert_eq!(root.tabs.len(), 1));
        let dark = style::is_dark();
        cx.simulate_keystrokes("ctrl-shift-l");
        assert_eq!(style::is_dark(), !dark);
        style::set_dark(dark);
    }

    /// Ctrl+W on a tab with unsaved edits asks; Cancel keeps the tab.
    #[gpui::test]
    fn closing_a_tab_with_unsaved_edits_asks(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let (l, r) = (dir.path().join("l.txt"), dir.path().join("r.txt"));
        std::fs::write(&l, "a\n").unwrap();
        std::fs::write(&r, "b\n").unwrap();
        cx.update(|cx| {
            style::init(cx);
            cx.bind_keys(crate::actions::bindings());
        });
        let start = Start::Compare { left: l.to_string_lossy().into(), right: r.to_string_lossy().into() };
        let (root, cx) = cx.add_window_view(|window, cx| Root::new(Some(start), window, cx));
        cx.run_until_parked();
        // F8 and Alt+→ change the right side in the text view.
        cx.simulate_keystrokes("f8 alt-right");
        root.read_with(cx, |root, cx| assert!(Root::unsaved(&root.tabs[0], cx)));
        cx.simulate_keystrokes("ctrl-w");
        assert!(cx.has_pending_prompt());
        cx.simulate_prompt_answer("Cancel");
        cx.run_until_parked();
        root.read_with(cx, |root, _| assert!(matches!(root.tabs[..], [Tab::Compare(_)])));
        cx.simulate_keystrokes("ctrl-w");
        cx.simulate_prompt_answer("Close without saving");
        cx.run_until_parked();
        root.read_with(cx, |root, _| assert!(matches!(root.tabs[..], [Tab::New(_)])));
        assert_eq!(std::fs::read_to_string(&r).unwrap(), "b\n");
    }

    #[gpui::test]
    fn palette_zoom_and_path_completion(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("release-1.4")).unwrap();
        std::fs::create_dir_all(dir.path().join("release-1.5")).unwrap();
        cx.update(|cx| {
            style::init(cx);
            cx.bind_keys(crate::actions::bindings());
            cx.bind_keys(crate::input::bindings());
        });
        let (root, cx) = cx.add_window_view(|window, cx| Root::new(None, window, cx));
        // Typing a path offers the entries that start with it; Tab takes the first.
        let partial = format!("{}/rel", dir.path().display());
        cx.simulate_input(&partial);
        let Tab::New(screen) = root.read_with(cx, |root, _| match &root.tabs[0] {
            Tab::New(s) => Tab::New(s.clone()),
            Tab::Compare(_) => panic!("the new comparison screen"),
        }) else {
            unreachable!()
        };
        screen.read_with(cx, |screen, _| {
            let (left, list) = screen.suggestions.as_ref().expect("completions");
            assert!(*left);
            assert_eq!(list.len(), 2);
        });
        cx.simulate_keystrokes("tab");
        let text = screen.read_with(cx, |screen, cx| screen.left.read(cx).text().to_string());
        assert!(text.ends_with("release-1.4/"), "{text}");

        // The palette runs a command by name; F1 shows it too.
        let zoom = style::zoom();
        cx.simulate_keystrokes("ctrl-shift-p");
        assert!(root.read_with(cx, |root, _| root.palette.is_some()));
        cx.simulate_input("larger");
        let first = root.read_with(cx, |root, cx| root.matching_commands(cx).first().map(|c| c.label));
        assert_eq!(first, Some("Larger text"));
        cx.simulate_keystrokes("enter");
        assert!(root.read_with(cx, |root, _| root.palette.is_none()));
        assert_eq!(style::zoom(), zoom + 10);
        cx.simulate_keystrokes("ctrl-0");
        assert_eq!(style::zoom(), 100);
        cx.simulate_keystrokes("f1");
        assert_eq!(root.read_with(cx, |root, cx| root.matching_commands(cx).len()), commands().len());
        cx.simulate_keystrokes("escape");
        assert!(root.read_with(cx, |root, _| root.palette.is_none()));
    }
}
