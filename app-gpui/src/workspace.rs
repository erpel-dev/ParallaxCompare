//! One comparison (one tab): opens the two sides, then shows the folder
//! comparison (or, for two files, the text comparison). Opening a file from the
//! folder view shows its text comparison on top; Back returns to the folders as
//! they were, and a save there refreshes the file's row.

use std::sync::Arc;

use gpui::{AnyView, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, Window, div, prelude::*};

use crate::folder::{FolderEvent, FolderView};
use crate::session::Session;
use crate::style;
use crate::text_view::{TextEvent, TextView};

enum Screen {
    Folder(Entity<FolderView>),
    Text(Entity<TextView>),
}

impl Screen {
    fn view(&self) -> AnyView {
        match self {
            Screen::Folder(v) => v.clone().into(),
            Screen::Text(v) => v.clone().into(),
        }
    }
    fn focus(&self, cx: &gpui::App) -> FocusHandle {
        match self {
            Screen::Folder(v) => v.focus_handle(cx),
            Screen::Text(v) => v.focus_handle(cx),
        }
    }
}

pub struct Workspace {
    pub labels: [SharedString; 2],
    session: Option<Arc<Session>>,
    error: Option<String>,
    stack: Vec<Screen>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.stack.last().map_or_else(|| self.focus.clone(), |s| s.focus(cx))
    }
}

/// The last part of a path or source, for tab titles.
pub fn short_name(text: &str) -> String {
    let trimmed = text.trim_end_matches(['/', '\\']);
    trimmed.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or(trimmed).to_string()
}

impl Workspace {
    pub fn new(left: String, right: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let work = cx.background_executor().spawn({
            let (left, right) = (left.clone(), right.clone());
            async move { Session::open(&left, &right) }
        });
        // Finishing needs the window, to focus the first view.
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(session) => this.opened(session, window, cx),
                Err(e) => {
                    this.error = Some(e);
                    cx.notify();
                }
            });
        })
        .detach();
        Workspace {
            labels: [left.into(), right.into()],
            session: None,
            error: None,
            stack: Vec::new(),
            focus: cx.focus_handle(),
            _subscriptions: Vec::new(),
        }
    }

    pub fn title(&self) -> String {
        let (l, r) = (short_name(&self.labels[0]), short_name(&self.labels[1]));
        if l == r { l } else { format!("{l} ↔ {r}") }
    }

    fn opened(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let session = Arc::new(session);
        self.session = Some(session.clone());
        if session.is_file {
            let title = self.title().into();
            let view = cx.new(|cx| TextView::new(session, String::new(), title, false, cx));
            self.push(Screen::Text(view), window, cx);
        } else {
            let view = cx.new(|cx| FolderView::new(session, cx));
            let sub = cx.subscribe_in(&view, window, |this, _, event: &FolderEvent, window, cx| match event {
                FolderEvent::OpenFile(path) => this.open_file(path.clone(), window, cx),
            });
            self._subscriptions.push(sub);
            self.push(Screen::Folder(view), window, cx);
        }
    }

    fn open_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else { return };
        let title = SharedString::from(path.clone());
        let view = cx.new(|cx| TextView::new(session, path, title, true, cx));
        let sub = cx.subscribe_in(&view, window, |this, _, event: &TextEvent, window, cx| match event {
            TextEvent::Back => this.back(window, cx),
            TextEvent::Saved(path) => {
                if let Some(Screen::Folder(folder)) = this.stack.first() {
                    folder.update(cx, |folder, cx| folder.refresh_paths(vec![path.clone()], cx));
                }
            }
        });
        self._subscriptions.push(sub);
        self.push(Screen::Text(view), window, cx);
    }

    fn push(&mut self, screen: Screen, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&screen.focus(cx));
        self.stack.push(screen);
        cx.notify();
    }

    fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.stack.len() > 1 {
            self.stack.pop();
            if let Some(top) = self.stack.last() {
                window.focus(&top.focus(cx));
            }
            cx.notify();
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full().flex().flex_col().track_focus(&self.focus);
        match (self.stack.last(), &self.error) {
            (Some(screen), _) => root.child(div().flex_1().min_h_0().child(screen.view())),
            (None, Some(e)) => root.child(
                div()
                    .p_4()
                    .text_color(style::status_color(parallax_core::compare::Status::Error))
                    .child(format!("Could not open the comparison: {e}")),
            ),
            (None, None) => root.child(div().p_4().text_color(style::muted()).child("Opening the sources…")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::short_name;

    #[test]
    fn short_names_for_tabs() {
        assert_eq!(short_name("/home/me/project/"), "project");
        assert_eq!(short_name(r"C:\work\old"), "old");
        assert_eq!(short_name("repo@main"), "repo@main");
        assert_eq!(short_name("/"), "");
    }
}
