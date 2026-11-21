//! Keyboard actions, bound like the Svelte app's keys where it has them.

use gpui::{KeyBinding, actions};

actions!(
    parallax,
    [
        NextDifference,
        PreviousDifference,
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        Expand,
        Collapse,
        Open,
        Refresh,
        Back,
        Quit,
    ]
);

pub fn bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("f8", NextDifference, None),
        KeyBinding::new("f7", PreviousDifference, None),
        KeyBinding::new("down", SelectNext, Some("FolderView")),
        KeyBinding::new("up", SelectPrevious, Some("FolderView")),
        KeyBinding::new("home", SelectFirst, Some("FolderView")),
        KeyBinding::new("end", SelectLast, Some("FolderView")),
        KeyBinding::new("right", Expand, Some("FolderView")),
        KeyBinding::new("left", Collapse, Some("FolderView")),
        KeyBinding::new("enter", Open, Some("FolderView")),
        KeyBinding::new("f5", Refresh, Some("FolderView")),
        KeyBinding::new("escape", Back, Some("TextView")),
        KeyBinding::new("alt-left", Back, Some("TextView")),
        KeyBinding::new("ctrl-q", Quit, None),
    ]
}
