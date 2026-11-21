//! A popup menu at a point: right-click menus and the view picker. Each item
//! dispatches an action to the focused view, so menus and keys run the same
//! code; items can also run a closure.

use std::rc::Rc;

use gpui::{
    Action, AnyElement, App, ElementId, Pixels, Point, SharedString, Window, anchored, deferred, div, prelude::*, px,
};

use crate::style;

type Run = Rc<dyn Fn(&mut Window, &mut App)>;

pub enum Item {
    Entry { label: SharedString, keys: &'static str, enabled: bool, run: Run, checked: bool },
    Separator,
}

impl Item {
    /// An item that dispatches `action`, as its key would.
    pub fn action(label: impl Into<SharedString>, keys: &'static str, enabled: bool, action: Box<dyn Action>) -> Item {
        let run: Run = Rc::new(move |window, cx| window.dispatch_action(action.boxed_clone(), cx));
        Item::Entry { label: label.into(), keys, enabled, run, checked: false }
    }

    /// An item that runs `run`.
    pub fn run(label: impl Into<SharedString>, checked: bool, run: impl Fn(&mut Window, &mut App) + 'static) -> Item {
        Item::Entry { label: label.into(), keys: "", enabled: true, run: Rc::new(run), checked }
    }
}

pub struct Menu {
    pub position: Point<Pixels>,
    pub items: Vec<Item>,
}

/// The menu, drawn above everything; `close` runs after an item and on a click outside.
pub fn render(id: impl Into<ElementId>, menu: &Menu, close: impl Fn(&mut Window, &mut App) + 'static) -> AnyElement {
    let close: Run = Rc::new(close);
    let outside = close.clone();
    let items = menu.items.iter().enumerate().map(|(i, item)| match item {
        Item::Separator => div().my_1().h(px(1.)).bg(style::line()).into_any_element(),
        Item::Entry { label, keys, enabled, run, checked } => {
            let (run, close) = (run.clone(), close.clone());
            div()
                .id(("item", i))
                .flex()
                .items_center()
                .gap_4()
                .px_3()
                .py(px(4.))
                .rounded(px(4.))
                .text_size(px(13.))
                .text_color(if *enabled { style::fg() } else { style::faint() })
                .child(div().w(px(12.)).child(if *checked { "✓" } else { "" }))
                .child(div().flex_1().child(label.clone()))
                .child(div().text_color(style::faint()).text_size(px(12.)).child(*keys))
                .when(*enabled, |d| {
                    d.cursor_pointer().hover(|s| s.bg(style::hover())).on_click(move |_, window, cx| {
                        close(window, cx);
                        run(window, cx);
                    })
                })
                .into_any_element()
        }
    });
    deferred(
        anchored().position(menu.position).snap_to_window_with_margin(px(8.)).child(
            div()
                .id(id)
                .occlude()
                .min_w(px(220.))
                .p_1()
                .rounded(px(6.))
                .border_1()
                .border_color(style::line())
                .bg(style::overlay())
                .shadow_lg()
                .on_mouse_down_out(move |_, window, cx| outside(window, cx))
                .children(items),
        ),
    )
    .with_priority(1)
    .into_any_element()
}
