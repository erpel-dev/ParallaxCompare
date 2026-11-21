//! The window's content: opens the two sides, then shows the folder comparison
//! (or, for two files, the text comparison). Opening a file from the folder view
//! shows its text comparison on top; Back returns to the folders as they were.

use std::sync::Arc;

use gpui::{AnyView, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, Window, div, prelude::*, px};

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
    labels: [SharedString; 2],
    session: Option<Arc<Session>>,
    error: Option<String>,
    stack: Vec<Screen>,
    _subscriptions: Vec<Subscription>,
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
            _subscriptions: Vec::new(),
        }
    }

    fn opened(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let session = Arc::new(session);
        self.session = Some(session.clone());
        if session.is_file {
            // The window header already shows both paths; the view names the files.
            let name = |s: &SharedString| s.rsplit(['/', '\\']).next().unwrap_or_default().to_string();
            let title = format!("{}  ↔  {}", name(&self.labels[0]), name(&self.labels[1])).into();
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let fonts = style::fonts(cx);
        let root =
            div().size_full().flex().flex_col().bg(style::bg()).text_color(style::fg()).font_family(fonts.ui.clone());
        let tabs = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .h(px(32.))
            .border_b_1()
            .border_color(style::line())
            .text_size(px(13.))
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("{}  ↔  {}", self.labels[0], self.labels[1])),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(style::faint())
                    .child("GPUI prototype · same core as the Tauri app"),
            );
        match (self.stack.last(), &self.error) {
            (Some(screen), _) => root.child(tabs).child(div().flex_1().min_h_0().child(screen.view())),
            (None, Some(e)) => root.child(tabs).child(
                div().p_4().text_color(style::status_color(parallax_core::compare::Status::Error)).child(e.clone()),
            ),
            (None, None) => {
                root.child(tabs).child(div().p_4().text_color(style::muted()).child("Opening the sources…"))
            }
        }
    }
}
