//! The conflicted files of a repository after a merge, rebase or cherry-pick
//! stopped: merge text files line by line, take a whole side, and stage what is
//! resolved, as the Svelte app's conflicts view does.

use std::collections::BTreeMap;
use std::path::PathBuf;

use gpui::{
    ClickEvent, Context, EventEmitter, FocusHandle, Focusable, PromptLevel, SharedString, Window, div, prelude::*, px,
};
use parallax_core::conflicts::{self, Resolution};

use crate::actions::Refresh;
use crate::merge::{self, RepoState};
use crate::style;

pub enum ConflictsEvent {
    /// Merge this file line by line.
    Merge { workdir: PathBuf, path: String },
}

pub struct ConflictsView {
    dir: PathBuf,
    focus: FocusHandle,
    pub(crate) state: Option<Result<RepoState, String>>,
    /// Files resolved here, and how to stage each.
    pub(crate) resolved: BTreeMap<String, Resolution>,
    message: Option<SharedString>,
}

impl EventEmitter<ConflictsEvent> for ConflictsView {}

impl Focusable for ConflictsView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ConflictsView {
    pub fn new(dir: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut view =
            ConflictsView { dir, focus: cx.focus_handle(), state: None, resolved: BTreeMap::new(), message: None };
        view.load(cx);
        view
    }

    pub(crate) fn load(&mut self, cx: &mut Context<Self>) {
        let dir = self.dir.clone();
        let work = cx.background_executor().spawn(async move { merge::repo_state(&dir) });
        cx.spawn(async move |this, cx| {
            let state = work.await;
            let _ = this.update(cx, |this, cx| {
                // Files staged or resolved elsewhere (Git, another tool) are no
                // longer listed; their records here would be staged again. A
                // failed read (the repository briefly unavailable) changes nothing.
                if let Ok(state) = &state {
                    this.resolved.retain(|path, _| state.files.iter().any(|f| f.path == *path));
                }
                this.state = Some(state);
                cx.notify();
            });
        })
        .detach();
    }

    pub fn title(&self) -> String {
        format!("Conflicts in {}", crate::workspace::short_name(&self.dir.to_string_lossy()))
    }

    fn workdir(&self) -> Option<PathBuf> {
        self.state.as_ref()?.as_ref().ok().map(|s| s.workdir.clone())
    }

    /// A merge of `path` was saved: clean means it can be staged.
    pub fn merged(&mut self, path: &str, clean: bool, cx: &mut Context<Self>) {
        if clean {
            self.resolved.insert(path.to_string(), Resolution::WorkTree(None));
        } else {
            self.resolved.remove(path);
        }
        cx.notify();
    }

    fn take(&mut self, path: String, remote: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workdir) = self.workdir() else { return };
        let side = if remote { "remote" } else { "local" };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Take the {side} version?"),
            Some(&format!("{path} in the work tree is replaced with the {side} version as a whole.")),
            &["Take it", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let result = merge::take_side(&workdir, &path, remote);
            let _ = this.update(cx, |this, cx| match result {
                Ok(resolution) => {
                    this.resolved.insert(path.clone(), resolution);
                    this.message = Some(format!("{path}: took the {side} version.").into());
                    cx.notify();
                }
                Err(e) => {
                    this.message = Some(e.into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(crate) fn stage(&mut self, cx: &mut Context<Self>) {
        let Some(workdir) = self.workdir() else { return };
        let paths: Vec<(String, Resolution)> = self.resolved.iter().map(|(p, r)| (p.clone(), *r)).collect();
        if paths.is_empty() {
            self.message = Some("Resolve files first: merge them and save, or take a side.".into());
            return cx.notify();
        }
        let n = paths.len();
        match conflicts::stage(&workdir, &paths) {
            Ok(()) => {
                self.resolved.clear();
                self.message = Some(format!("Staged {n} file{}.", if n == 1 { "" } else { "s" }).into());
            }
            // The records stay, to stage again once the problem is solved.
            Err(e) => self.message = Some(format!("Staging failed: {e}").into()),
        }
        self.load(cx);
    }
}

impl Render for ConflictsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .key_context("ConflictsView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.load(cx)))
            .size_full()
            .flex()
            .flex_col()
            .text_color(style::fg());
        let state = match &self.state {
            None => return root.child(div().p_4().text_color(style::muted()).child("Reading the repository…")),
            Some(Err(e)) => {
                return root.child(
                    div()
                        .p_4()
                        .text_color(style::status_color(parallax_core::compare::Status::Error))
                        .child(format!("No conflicts to resolve here: {e}")),
                );
            }
            Some(Ok(state)) => state,
        };
        let resolved = self.resolved.len();
        let header = div()
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
                style::button("refresh", "⟳ Refresh", true)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.load(cx))),
            )
            .child(
                style::button("stage", format!("Stage {resolved} resolved"), resolved > 0)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.stage(cx))),
            )
            .child(div().flex_1())
            .child(div().text_size(px(12.)).text_color(style::muted()).child(state.summary.clone()));
        let rows = state.files.iter().enumerate().map(|(i, file)| {
            let path = file.path.clone();
            let done = self.resolved.contains_key(&file.path);
            let workdir = state.workdir.clone();
            let (merge_path, local_path, remote_path) = (path.clone(), path.clone(), path.clone());
            div()
                .id(("conflict", i))
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .h(px(34.))
                .border_b_1()
                .border_color(style::line())
                .hover(|s| s.bg(style::hover()))
                .child(
                    div()
                        .w(px(18.))
                        .text_color(if done {
                            style::status_color(parallax_core::compare::Status::RightOnly)
                        } else {
                            style::status_color(parallax_core::compare::Status::Different)
                        })
                        .child(if done { "✓" } else { "!" }),
                )
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_size(px(13.)).child(path))
                .child(div().w(px(200.)).text_size(px(12.)).text_color(style::muted()).child(format!(
                    "{}{}",
                    merge::kind_label(file.kind),
                    match file.special {
                        Some(conflicts::FileMode::Symlink) => " · link",
                        Some(conflicts::FileMode::Gitlink) => " · submodule",
                        _ => "",
                    }
                )))
                .child(style::button(("merge", i), "Merge…", file.text).when(file.text, |b| {
                    b.on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                        cx.emit(ConflictsEvent::Merge { workdir: workdir.clone(), path: merge_path.clone() })
                    }))
                }))
                .child(style::button(("local", i), "Take local", true).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.take(local_path.clone(), false, window, cx),
                )))
                .child(style::button(("remote", i), "Take remote", true).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| this.take(remote_path.clone(), true, window, cx),
                )))
        });
        let empty = state.files.is_empty();
        root.child(header)
            .when(empty, |d| d.child(div().p_4().text_color(style::muted()).child("No conflicted files are left.")))
            .child(div().id("conflict-list").flex_1().overflow_y_scroll().children(rows))
            .child(
                div()
                    .flex()
                    .px_3()
                    .py_1()
                    .border_t_1()
                    .border_color(style::line())
                    .bg(style::chrome())
                    .text_size(px(12.))
                    .text_color(style::muted())
                    .child(div().flex_1().child(state.workdir.display().to_string()))
                    .when_some(self.message.clone(), |d, m| d.child(div().text_color(style::fg()).child(m))),
            )
    }
}
