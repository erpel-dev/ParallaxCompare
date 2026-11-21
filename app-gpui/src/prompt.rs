//! The app's own dialog for `window.prompt`. GPUI has no platform dialog on
//! Linux, and its fallback clips longer explanations, is white in the dark theme
//! and does not answer Enter or Escape.

use gpui::{
    App, AppContext as _, ClickEvent, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, PromptButton, PromptHandle, PromptLevel, PromptResponse, Render, RenderablePromptHandle,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, relative,
};

use crate::actions::Quit;
use crate::style;

/// Shows every prompt of the app with [`Dialog`].
pub fn install(cx: &mut App) {
    cx.set_prompt_builder(build);
}

fn build(
    level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    actions: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let dialog = cx.new(|cx| Dialog {
        level,
        message: message.to_string(),
        detail: detail.map(str::to_string),
        actions: actions.to_vec(),
        focus: cx.focus_handle(),
    });
    handle.with_view(dialog, window, cx)
}

/// A modal question: the message, an explanation that wraps, and one button per
/// answer. Enter takes the first answer, or for a `Critical` question (one that
/// cannot be undone) the Cancel button; Escape takes Cancel (or the last answer).
/// A key held down from before the dialog opened answers nothing, and clicks on
/// the dimmed window around it do not reach the controls below.
pub struct Dialog {
    level: PromptLevel,
    message: String,
    detail: Option<String>,
    actions: Vec<PromptButton>,
    focus: FocusHandle,
}

impl Dialog {
    fn cancel_index(&self) -> usize {
        self.actions.iter().position(|a| matches!(a, PromptButton::Cancel(_))).unwrap_or(self.actions.len() - 1)
    }

    /// The answer Enter gives.
    fn default_index(&self) -> usize {
        if self.level == PromptLevel::Critical { self.cancel_index() } else { 0 }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        // Key repeat: Enter held down after answering one question must not
        // answer the next one too.
        if !event.is_held {
            match key {
                "enter" => cx.emit(PromptResponse(self.default_index())),
                "escape" => cx.emit(PromptResponse(self.cancel_index())),
                _ => {}
            }
        }
        cx.stop_propagation();
    }
}

impl gpui::EventEmitter<PromptResponse> for Dialog {}

impl Focusable for Dialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Dialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accent = match self.level {
            PromptLevel::Critical => style::removed().1,
            PromptLevel::Warning => style::c(0xd97706),
            _ => style::accent(),
        };
        let default = self.default_index();
        let buttons = self.actions.iter().enumerate().map(|(i, action)| {
            style::button(("answer", i), action.label().clone(), true)
                .px_3()
                .when(i == default, |b| b.border_color(accent))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(PromptResponse(i))))
        });
        let card = div()
            .id("dialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            // The window's actions are not on the dialog's path; without this,
            // Ctrl+Q would reach the app's handler and quit past the question.
            .on_action(|_: &Quit, _, cx| cx.stop_propagation())
            .occlude()
            .w(px(460.))
            .max_w(relative(0.9))
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .rounded(px(8.))
            .border_1()
            .border_color(style::line())
            .bg(style::overlay())
            .text_color(style::fg())
            .shadow_lg()
            .child(
                div().w_full().text_size(px(14.)).font_weight(gpui::FontWeight::SEMIBOLD).child(self.message.clone()),
            )
            .children(
                self.detail.clone().map(|d| div().w_full().text_size(px(13.)).text_color(style::muted()).child(d)),
            )
            .child(div().mt_2().flex().flex_wrap().justify_end().gap_2().children(buttons));
        div()
            .id("backdrop")
            .occlude()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.35))
            .child(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keystroke, TestAppContext};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Blank(FocusHandle);

    impl Render for Blank {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().track_focus(&self.0)
        }
    }

    /// The review's data loss: Enter held down through the trash question also
    /// answered the "Delete permanently" one. Repeats answer nothing, and Enter
    /// on a question that cannot be undone keeps things as they are.
    #[gpui::test]
    fn held_keys_and_questions_that_cannot_be_undone(cx: &mut TestAppContext) {
        let quit = Arc::new(AtomicBool::new(false));
        cx.update(|cx| {
            style::init(cx);
            install(cx);
            cx.bind_keys(crate::actions::bindings());
            let quit = quit.clone();
            cx.on_action(move |_: &Quit, _| quit.store(true, Ordering::Relaxed));
        });
        let (_, cx) = cx.add_window_view(|_, cx| Blank(cx.focus_handle()));
        let mut answer = cx.update(|window, cx| {
            window.prompt(PromptLevel::Critical, "Delete for good?", None, &["Delete permanently", "Keep them"], cx)
        });
        cx.run_until_parked();
        cx.simulate_event(KeyDownEvent { keystroke: Keystroke::parse("enter").unwrap(), is_held: true });
        cx.run_until_parked();
        assert_eq!(answer.try_recv(), Ok(None), "a repeated Enter answers nothing");
        // Ctrl+Q while the question is open does not quit past it.
        cx.simulate_keystrokes("ctrl-q");
        assert!(!quit.load(Ordering::Relaxed));
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(answer.try_recv(), Ok(Some(1)), "Enter keeps them");

        // An ordinary question takes its first answer on Enter.
        let mut answer =
            cx.update(|window, cx| window.prompt(PromptLevel::Warning, "Replace?", None, &["Replace", "Cancel"], cx));
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(answer.try_recv(), Ok(Some(0)));
    }
}
