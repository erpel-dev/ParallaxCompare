//! The window: a tab per comparison, and the "New comparison" screen where two
//! sources are typed or picked. Follows the system's light or dark appearance;
//! Ctrl+Shift+L switches by hand.

use gpui::{
    ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, PathPromptOptions, SharedString, Subscription,
    Window, div, prelude::*, px,
};

use crate::actions::*;
use crate::input::{InputEvent, TextInput};
use crate::style;
use crate::workspace::Workspace;

enum Tab {
    New(Entity<NewCompare>),
    Compare(Entity<Workspace>),
}

pub struct Root {
    tabs: Vec<Tab>,
    active: usize,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Root {
    pub fn new(start: Option<(String, String)>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        style::follow(window.appearance());
        let appearance = cx.observe_window_appearance(window, |_, window, cx| {
            style::follow(window.appearance());
            cx.refresh_windows();
        });
        let mut root = Root { tabs: Vec::new(), active: 0, focus: cx.focus_handle(), _subscriptions: vec![appearance] };
        match start {
            Some((left, right)) => root.open(left, right, window, cx),
            None => root.new_tab(window, cx),
        }
        root
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let screen = cx.new(|cx| NewCompare::new(window, cx));
        let sub = cx.subscribe_in(&screen, window, |this, screen, event: &Start, window, cx| {
            let Start { left, right } = event;
            if let Some(i) = this.tabs.iter().position(|t| matches!(t, Tab::New(s) if s == screen)) {
                let view = cx.new(|cx| Workspace::new(left.clone(), right.clone(), window, cx));
                this.tabs[i] = Tab::Compare(view);
                this.activate(i, window, cx);
            }
        });
        self._subscriptions.push(sub);
        self.tabs.push(Tab::New(screen));
        let last = self.tabs.len() - 1;
        self.activate(last, window, cx);
    }

    pub fn open(&mut self, left: String, right: String, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| Workspace::new(left, right, window, cx));
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

    fn close(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
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

    fn tab_title(&self, tab: &Tab, cx: &gpui::App) -> String {
        match tab {
            Tab::New(_) => "New comparison".into(),
            Tab::Compare(v) => v.read(cx).title(),
        }
    }
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
    }
}

/// Start a comparison of these two sources.
pub struct Start {
    pub left: String,
    pub right: String,
}

pub struct NewCompare {
    left: Entity<TextInput>,
    right: Entity<TextInput>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Start> for NewCompare {}

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
        for (input, next) in [(&left, Some(right.clone())), (&right, None)] {
            subs.push(cx.subscribe_in(input, window, move |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::Submit { .. } = event {
                    match &next {
                        Some(next) => window.focus(&next.focus_handle(cx)),
                        None => this.start(cx),
                    }
                }
            }));
        }
        NewCompare { left, right, error: None, _subscriptions: subs }
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        let (left, right) =
            (self.left.read(cx).text().trim().to_string(), self.right.read(cx).text().trim().to_string());
        if left.is_empty() || right.is_empty() {
            self.error = Some("Enter both sides.".into());
            return cx.notify();
        }
        cx.emit(Start { left, right });
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
            assert_eq!(ws.read(cx).title(), "l ↔ r");
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
}
