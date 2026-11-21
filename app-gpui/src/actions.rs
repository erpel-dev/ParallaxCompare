//! Keyboard actions, bound to the same keys as in the Svelte app.

use gpui::{KeyBinding, actions};

actions!(
    parallax,
    [
        // Both comparison views.
        NextDifference,
        PreviousDifference,
        Refresh,
        // Folder view.
        SelectNext,
        SelectPrevious,
        SelectFirst,
        SelectLast,
        ExtendNext,
        ExtendPrevious,
        SelectAll,
        Expand,
        Collapse,
        Open,
        CopyRight,
        CopyLeft,
        DeleteSelected,
        ShowMenu,
        // Text view.
        Back,
        CopyDifferenceLeft,
        CopyDifferenceRight,
        Undo,
        Redo,
        Save,
        CopyLines,
        EditLine,
        Find,
        FindNext,
        FindPrevious,
        ScrollLeft,
        ScrollRight,
        // Window.
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        ToggleTheme,
        Quit,
    ]
);

pub fn bindings() -> Vec<KeyBinding> {
    let folder = Some("FolderView");
    let text = Some("TextView");
    vec![
        KeyBinding::new("f8", NextDifference, None),
        KeyBinding::new("f7", PreviousDifference, None),
        KeyBinding::new("alt-down", NextDifference, text),
        KeyBinding::new("alt-up", PreviousDifference, text),
        KeyBinding::new("f5", Refresh, None),
        KeyBinding::new("down", SelectNext, folder),
        KeyBinding::new("up", SelectPrevious, folder),
        KeyBinding::new("shift-down", ExtendNext, folder),
        KeyBinding::new("shift-up", ExtendPrevious, folder),
        KeyBinding::new("home", SelectFirst, folder),
        KeyBinding::new("end", SelectLast, folder),
        KeyBinding::new("ctrl-a", SelectAll, folder),
        KeyBinding::new("right", Expand, folder),
        KeyBinding::new("left", Collapse, folder),
        KeyBinding::new("enter", Open, folder),
        KeyBinding::new("alt-right", CopyRight, folder),
        KeyBinding::new("alt-left", CopyLeft, folder),
        KeyBinding::new("delete", DeleteSelected, folder),
        KeyBinding::new("shift-f10", ShowMenu, folder),
        KeyBinding::new("menu", ShowMenu, folder),
        KeyBinding::new("down", SelectNext, text),
        KeyBinding::new("up", SelectPrevious, text),
        KeyBinding::new("escape", Back, text),
        KeyBinding::new("alt-left", CopyDifferenceLeft, text),
        KeyBinding::new("alt-right", CopyDifferenceRight, text),
        KeyBinding::new("ctrl-z", Undo, text),
        KeyBinding::new("ctrl-y", Redo, text),
        KeyBinding::new("ctrl-shift-z", Redo, text),
        KeyBinding::new("ctrl-s", Save, text),
        KeyBinding::new("ctrl-c", CopyLines, text),
        KeyBinding::new("enter", EditLine, text),
        KeyBinding::new("f2", EditLine, text),
        KeyBinding::new("ctrl-f", Find, text),
        KeyBinding::new("f3", FindNext, text),
        KeyBinding::new("shift-f3", FindPrevious, text),
        KeyBinding::new("shift-left", ScrollLeft, text),
        KeyBinding::new("shift-right", ScrollRight, text),
        KeyBinding::new("ctrl-t", NewTab, None),
        KeyBinding::new("ctrl-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new("ctrl-shift-l", ToggleTheme, None),
        KeyBinding::new("ctrl-q", Quit, None),
    ]
}
