//! The app's own dialog for `window.prompt`. GPUI has no platform dialog on
//! Linux, and its fallback clips longer explanations, is white in the dark theme
//! and does not answer Enter or Escape.

use gpui::{
    App, AppContext as _, ClickEvent, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, PromptButton, PromptHandle, PromptLevel, PromptResponse, Render, RenderablePromptHandle,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px, relative,
};

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
/// answer. Enter takes the first answer, Escape the Cancel button (or the last).
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

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        match key {
            "enter" => cx.emit(PromptResponse(0)),
            "escape" => cx.emit(PromptResponse(self.cancel_index())),
            _ => {}
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
        let buttons = self.actions.iter().enumerate().map(|(i, action)| {
            style::button(("answer", i), action.label().clone(), true)
                .px_3()
                .when(i == 0, |b| b.border_color(accent))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| cx.emit(PromptResponse(i))))
        });
        let card = div()
            .id("dialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
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
        div().size_full().flex().items_center().justify_center().bg(gpui::black().opacity(0.35)).child(card)
    }
}
