//! One tab: a comparison, a merge, or a repository's conflicts. A comparison
//! opens the two sides, then shows the folder comparison (or, for two files, the
//! text comparison). Opening a file from the folder view, or a conflicted file
//! from the conflicts view, shows it on top; Back returns to the list as it was.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{AnyView, Context, Entity, FocusHandle, Focusable, SharedString, Subscription, Window, div, prelude::*};

use crate::conflicts_view::{ConflictsEvent, ConflictsView};
use crate::folder::{FolderEvent, FolderView};
use crate::merge::MergeSource;
use crate::merge_view::{MergeEvent, MergeView};
use crate::session::Session;
use crate::style;
use crate::text_view::{TextEvent, TextView};

/// What a tab shows.
#[derive(Clone, Debug)]
pub enum Start {
    Compare {
        left: String,
        right: String,
    },
    /// A merge given on the command line (`git mergetool`) or picked by hand.
    Merge {
        source: MergeSource,
        startup: bool,
    },
    /// The conflicted files of the repository at this folder.
    Conflicts {
        dir: PathBuf,
    },
}

enum Screen {
    Folder(Entity<FolderView>),
    Text(Entity<TextView>),
    Merge(Entity<MergeView>),
    Conflicts(Entity<ConflictsView>),
}

impl Screen {
    fn view(&self) -> AnyView {
        match self {
            Screen::Folder(v) => v.clone().into(),
            Screen::Text(v) => v.clone().into(),
            Screen::Merge(v) => v.clone().into(),
            Screen::Conflicts(v) => v.clone().into(),
        }
    }
    fn focus(&self, cx: &gpui::App) -> FocusHandle {
        match self {
            Screen::Folder(v) => v.focus_handle(cx),
            Screen::Text(v) => v.focus_handle(cx),
            Screen::Merge(v) => v.focus_handle(cx),
            Screen::Conflicts(v) => v.focus_handle(cx),
        }
    }
}

pub struct Workspace {
    start: Start,
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
    pub fn new(start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut workspace = Workspace {
            start: start.clone(),
            session: None,
            error: None,
            stack: Vec::new(),
            focus: cx.focus_handle(),
            _subscriptions: Vec::new(),
        };
        match start {
            Start::Compare { left, right } => {
                let work = cx.background_executor().spawn(async move { Session::open(&left, &right) });
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
            }
            Start::Merge { source, startup } => {
                let view = cx.new(|cx| MergeView::new(source, false, startup, cx));
                workspace.push(Screen::Merge(view), window, cx);
            }
            Start::Conflicts { dir } => {
                let view = cx.new(|cx| ConflictsView::new(dir, cx));
                let sub = cx.subscribe_in(&view, window, |this, _, event: &ConflictsEvent, window, cx| match event {
                    ConflictsEvent::Merge { workdir, path } => {
                        this.open_merge(workdir.clone(), path.clone(), window, cx)
                    }
                });
                workspace._subscriptions.push(sub);
                workspace.push(Screen::Conflicts(view), window, cx);
            }
        }
        workspace
    }

    /// Whether a text or merge view in this tab holds edits that are not saved.
    pub fn unsaved(&self, cx: &gpui::App) -> bool {
        self.stack.iter().any(|screen| match screen {
            Screen::Text(v) => v.read(cx).dirty(),
            Screen::Merge(v) => v.read(cx).dirty(),
            Screen::Folder(_) | Screen::Conflicts(_) => false,
        })
    }

    pub fn title(&self, cx: &gpui::App) -> String {
        match &self.start {
            Start::Compare { left, right } => {
                let (l, r) = (short_name(left), short_name(right));
                if l == r { l } else { format!("{l} ↔ {r}") }
            }
            Start::Merge { .. } => match self.stack.first() {
                Some(Screen::Merge(v)) => v.read(cx).title().map_or("Merge".into(), |t| format!("Merge {t}")),
                _ => "Merge".into(),
            },
            Start::Conflicts { .. } => match self.stack.first() {
                Some(Screen::Conflicts(v)) => v.read(cx).title(),
                _ => "Conflicts".into(),
            },
        }
    }

    fn opened(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let session = Arc::new(session);
        self.session = Some(session.clone());
        if session.is_file {
            let title = self.title(cx).into();
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

    fn open_merge(&mut self, workdir: PathBuf, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let source = MergeSource::Repo { workdir, path: path.clone() };
        let view = cx.new(|cx| MergeView::new(source, true, false, cx));
        let sub = cx.subscribe_in(&view, window, move |this, _, event: &MergeEvent, window, cx| match event {
            MergeEvent::Back => this.back(window, cx),
            MergeEvent::Saved { clean } => {
                if let Some(Screen::Conflicts(list)) = this.stack.first() {
                    list.update(cx, |list, cx| list.merged(&path, *clean, cx));
                }
            }
        });
        self._subscriptions.push(sub);
        self.push(Screen::Merge(view), window, cx);
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
    use super::*;
    use gpui::TestAppContext;

    #[test]
    fn short_names_for_tabs() {
        assert_eq!(short_name("/home/me/project/"), "project");
        assert_eq!(short_name(r"C:\work\old"), "old");
        assert_eq!(short_name("repo@main"), "repo@main");
        assert_eq!(short_name("/"), "");
    }

    #[gpui::test]
    fn conflicts_merge_save_and_stage(cx: &mut TestAppContext) {
        use crate::conflicts_view::ConflictsEvent;
        let repo = crate::merge::tests::conflicted_repo();
        cx.update(|cx| {
            style::init(cx);
            cx.bind_keys(crate::actions::bindings());
        });
        let dir = repo.path().to_path_buf();
        let (ws, cx) = cx.add_window_view(|window, cx| Workspace::new(Start::Conflicts { dir }, window, cx));
        cx.run_until_parked();
        let Some(Screen::Conflicts(list)) = ws.read_with(cx, |ws, _| {
            ws.stack.first().map(|s| match s {
                Screen::Conflicts(l) => Screen::Conflicts(l.clone()),
                _ => panic!("the conflicts list first"),
            })
        }) else {
            panic!("a conflicts list")
        };
        assert_eq!(
            ws.read_with(cx, |ws, cx| ws.title(cx)),
            format!("Conflicts in {}", short_name(&repo.path().to_string_lossy()))
        );
        // "Merge…" on a.txt opens the merge on top.
        let workdir = std::fs::canonicalize(repo.path()).unwrap();
        list.update(cx, |_, cx| cx.emit(ConflictsEvent::Merge { workdir, path: "a.txt".into() }));
        cx.run_until_parked();
        assert_eq!(ws.read_with(cx, |ws, _| ws.stack.len()), 2);
        // Alt+→ takes the remote side; Ctrl+S saves; the list marks a.txt resolved.
        cx.simulate_keystrokes("alt-right ctrl-s");
        cx.run_until_parked();
        assert_eq!(std::fs::read_to_string(repo.path().join("a.txt")).unwrap(), "one\nTWO (feature)\nthree\n");
        cx.simulate_keystrokes("escape");
        assert_eq!(ws.read_with(cx, |ws, _| ws.stack.len()), 1);
        list.update(cx, |list, cx| list.stage(cx));
        cx.run_until_parked();
        let left = crate::merge::repo_state(repo.path()).unwrap();
        assert_eq!(left.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["b.txt"], "a.txt is staged");
    }

    /// Records of files no longer conflicted (staged elsewhere) are dropped
    /// when the list is read again, so Stage does not stage them a second time.
    #[gpui::test]
    fn refreshing_the_conflicts_drops_stale_records(cx: &mut TestAppContext) {
        use parallax_core::conflicts::Resolution;
        let repo = crate::merge::tests::conflicted_repo();
        cx.update(style::init);
        let dir = repo.path().to_path_buf();
        let (list, cx) = cx.add_window_view(|_, cx| ConflictsView::new(dir, cx));
        cx.run_until_parked();
        list.update(cx, |list, cx| {
            list.merged("a.txt", true, cx);
            list.resolved.insert("staged-elsewhere.txt".into(), Resolution::WorkTree(None));
            list.load(cx);
        });
        cx.run_until_parked();
        list.read_with(cx, |list, _| assert_eq!(list.resolved.keys().collect::<Vec<_>>(), ["a.txt"]));
    }
}
