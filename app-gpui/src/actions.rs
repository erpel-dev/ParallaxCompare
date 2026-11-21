//! Keyboard actions, bound to the same keys as in the Svelte app.

use gpui::{Action, KeyBinding, actions};

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
        // Merge view.
        TakeLocal,
        TakeRemote,
        TakeBase,
        TakeBoth,
        TakeBothRemoteFirst,
        TakeSyntax,
        // Window.
        NewTab,
        CloseTab,
        NextTab,
        PreviousTab,
        ToggleTheme,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        ShowPalette,
        Quit,
    ]
);

pub fn bindings() -> Vec<KeyBinding> {
    let folder = Some("FolderView");
    let text = Some("TextView");
    let merge = Some("MergeView");
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
        KeyBinding::new("alt-left", TakeLocal, merge),
        KeyBinding::new("alt-right", TakeRemote, merge),
        KeyBinding::new("alt-up", TakeBase, merge),
        KeyBinding::new("alt-down", TakeBoth, merge),
        KeyBinding::new("alt-shift-down", TakeBothRemoteFirst, merge),
        KeyBinding::new("alt-s", TakeSyntax, merge),
        KeyBinding::new("down", SelectNext, merge),
        KeyBinding::new("up", SelectPrevious, merge),
        KeyBinding::new("enter", EditLine, merge),
        KeyBinding::new("f2", EditLine, merge),
        KeyBinding::new("ctrl-z", Undo, merge),
        KeyBinding::new("ctrl-y", Redo, merge),
        KeyBinding::new("ctrl-shift-z", Redo, merge),
        KeyBinding::new("ctrl-s", Save, merge),
        KeyBinding::new("escape", Back, merge),
        KeyBinding::new("ctrl-t", NewTab, None),
        KeyBinding::new("ctrl-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new("ctrl-shift-l", ToggleTheme, None),
        KeyBinding::new("ctrl-=", ZoomIn, None),
        KeyBinding::new("ctrl-+", ZoomIn, None),
        KeyBinding::new("ctrl--", ZoomOut, None),
        KeyBinding::new("ctrl-0", ZoomReset, None),
        KeyBinding::new("ctrl-shift-p", ShowPalette, None),
        KeyBinding::new("f1", ShowPalette, None),
        KeyBinding::new("ctrl-q", Quit, None),
    ]
}

/// A command for the palette: where it applies, what it does, its keys.
pub struct Command {
    pub group: &'static str,
    pub label: &'static str,
    pub keys: &'static str,
    pub action: fn() -> Box<dyn Action>,
}

macro_rules! command {
    ($group:literal, $label:literal, $keys:literal, $action:expr) => {
        Command { group: $group, label: $label, keys: $keys, action: || Box::new($action) }
    };
}

/// Every command, for the palette and as the keyboard shortcut list.
pub fn commands() -> Vec<Command> {
    vec![
        command!("Window", "New comparison", "Ctrl+T", NewTab),
        command!("Window", "Close tab", "Ctrl+W", CloseTab),
        command!("Window", "Next tab", "Ctrl+Tab", NextTab),
        command!("Window", "Previous tab", "Ctrl+Shift+Tab", PreviousTab),
        command!("Window", "Light or dark", "Ctrl+Shift+L", ToggleTheme),
        command!("Window", "Larger text", "Ctrl+=", ZoomIn),
        command!("Window", "Smaller text", "Ctrl+-", ZoomOut),
        command!("Window", "Text size 100 %", "Ctrl+0", ZoomReset),
        command!("Window", "Quit", "Ctrl+Q", Quit),
        command!("Compare", "Next difference", "F8", NextDifference),
        command!("Compare", "Previous difference", "F7", PreviousDifference),
        command!("Compare", "Compare again", "F5", Refresh),
        command!("Folder", "Open comparison", "Enter", Open),
        command!("Folder", "Copy to the right", "Alt+→", CopyRight),
        command!("Folder", "Copy to the left", "Alt+←", CopyLeft),
        command!("Folder", "Delete…", "Delete", DeleteSelected),
        command!("Folder", "Select all rows", "Ctrl+A", SelectAll),
        command!("Folder", "Menu for the selection", "Shift+F10", ShowMenu),
        command!("Text", "Copy difference to the left", "Alt+←", CopyDifferenceLeft),
        command!("Text", "Copy difference to the right", "Alt+→", CopyDifferenceRight),
        command!("Text", "Edit line", "F2", EditLine),
        command!("Text", "Copy lines", "Ctrl+C", CopyLines),
        command!("Text", "Find in both files", "Ctrl+F", Find),
        command!("Text", "Next match", "F3", FindNext),
        command!("Text", "Previous match", "Shift+F3", FindPrevious),
        command!("Text", "Undo", "Ctrl+Z", Undo),
        command!("Text", "Redo", "Ctrl+Y", Redo),
        command!("Text", "Save", "Ctrl+S", Save),
        command!("Text", "Back to the folders", "Escape", Back),
        command!("Merge", "Take local", "Alt+←", TakeLocal),
        command!("Merge", "Take remote", "Alt+→", TakeRemote),
        command!("Merge", "Take base", "Alt+↑", TakeBase),
        command!("Merge", "Take both, local first", "Alt+↓", TakeBoth),
        command!("Merge", "Take both, remote first", "Alt+Shift+↓", TakeBothRemoteFirst),
        command!("Merge", "Take the syntax-aware solution", "Alt+S", TakeSyntax),
    ]
}
