# ParallaxCompare: GPUI prototype

A second desktop front end for ParallaxCompare, written with
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) (the UI
framework of the Zed editor) instead of Tauri and Svelte. It runs on the same
compare core, `crates/parallax-core`, as the Tauri app and the CLI. It exists to
try out the switch on real code: what carries over, what has to be rebuilt, and
what it changes in size, memory and speed.

```sh
cd app-gpui
cargo run --release                                 # the "New comparison" screen
cargo run --release -- old/ new/
cargo run --release -- repo@main repo@feature      # Git revisions
cargo run --release -- a.zip b.zip                  # archives
cargo run --release -- left.txt right.txt           # two files: text compare
```

Sources are written as for the `parallax` CLI. On Linux the build needs the
X11/Wayland, Vulkan and fontconfig development packages:

```sh
sudo apt install libxkbcommon-x11-dev libxcb1-dev libwayland-dev libvulkan-dev libfontconfig-dev libfreetype-dev
```

## What it does

**Tabs and new comparisons.** Without arguments the app opens the "New
comparison" screen: type or pick (Folder…, File…) the two sides, swap them, and
Compare. Each comparison is a tab; Ctrl+T opens another, Ctrl+W closes one,
Ctrl+Tab and Ctrl+Shift+Tab move between them.

**Folder compare.** Compares the two roots in the background with
`compare::compare` and shows the core's result tree in one virtualized list:
both sides with name, size and modification time, and a status column (`=`
same, `≈` similar, `≠` different, `◀` left only, `▶` right only, `!` error).
After a compare, folders with differences are open, as in the Svelte app.

- Filters with counts (All, All differences, Same, Left only, Right only, Errors), and a Flat list of all files by path.
- Folders the compare did not walk (identical by Git tree id, closed archives) are compared when opened.
- Selection with click, Shift+click, Ctrl+click, Shift+↑/↓ and Ctrl+A.
- Copy to the other side (Alt+→, Alt+←), asking before files are replaced; delete from one side (Delete), asking first, into the trash where possible. Only the touched paths are compared again.
- Right-click menu: open, copy, delete from the clicked side, copy path.
- Options: include and exclude patterns, line endings, document and image contents, archives, byte-for-byte archive checks.
- Local sides are watched; files changed by other programs are compared again on their own.

**Text compare.** Opens from the folder view (double-click or Enter), or
directly when both roots are files. Side by side with line numbers, syntax
colours, changed characters marked within changed lines, and coloured rows for
changed, added and removed lines.

- Views: files open in the view the core chooses, as in the Tauri app: text, converted views (pretty JSON and XML, outlines, PDF and Office text, executables …), a hex dump for binary files, or the image compare with the differing pixels marked. Files that are ≈ similar open in the view where they are equal, with a note; the view menu switches.
- Copy a difference to the left or right (Alt+←, Alt+→), edit a line (F2, Enter or double-click), undo and redo (Ctrl+Z, Ctrl+Y), save (Ctrl+S), which asks before overwriting a file another program changed, and refreshes the file's row in the folder view.
- Select lines (click, Shift+click, ↑/↓) and copy them (Ctrl+C); right-click for a menu.
- Find in both files (Ctrl+F) with case, whole word and regex; F3 and Enter move through the matches.
- Options: whitespace (exact, normalized, ignored), ignore case, histogram diff.
- Horizontal scrolling of long lines (Shift+wheel, Shift+←/→).

**Theme.** Follows the system's light or dark appearance; Ctrl+Shift+L or the
button in the tab bar switches by hand. Syntax colours switch with it.

## The same core logic

Both front ends call the same core functions. The Tauri app does it inside its
commands and sends the results to the web view as JSON; this app calls them
directly while drawing.

| Step | Core function | Tauri app (`app/src-tauri/src/commands.rs`) | GPUI app |
|---|---|---|---|
| Open the two sides | `source::open_pair` | `open_session` | `Session::open` |
| Compare folders | `compare::compare` | `run_compare` | `FolderView::run` |
| Open a folder that was not walked | `compare::compare_path`, `tree::replace` | `expand` | `FolderView::toggle` |
| Choose a file's view and convert it | `file_view::choose` | `open_file_diff` | `session::open_file` |
| Diff and edit two files | `edit::EditSession` | `open_file_diff`, `copy_rows`, `replace_rows`, `undo` | `FileDiff` |
| Build the rows in view | `rows::side_by_side` | `file_rows` | `FileDiff::rows` |
| Find in both files | `search::pattern`, `search::search` | `search_diff` | `FileDiff::search` |
| Copy, delete, refresh | `ops::copy`, `ops::delete`, `compare::refresh_path` | `copy_entries`, `delete_entries`, `refresh_paths` | `Session::copy`, `delete`, `refresh` |
| Watch for changes | `watch::Watcher` | `start_watching` | `Session::watch` |

`rows::side_by_side` and `file_view::choose` were moved from the Tauri commands
`file_rows` and `open_file_diff` into the core, so both apps build their rows
and choose views with the same code. The JSON the Svelte app reads is unchanged.

## What it does not do yet

- three-way merge and conflict resolution (`--merge`, the conflicts view), and pull request review
- typing directly into the text view: lines are edited one at a time in a field
- saving converted views that write back into their file (ECUC values) is wired but untested
- path completion, recent sources and the source pickers for Git revisions and archives (sources are typed)
- the overview strip beside the text, wrapping long lines, the image compare's blink and overlay modes
- preferences (text size), keyboard-shortcut help, the command palette
- screen reader support (released GPUI versions have none)

## Tests

`cargo test` runs unit tests and headless UI tests. The UI tests open the views
in GPUI's test window and drive them like a user: F8 to a difference, Alt+→ to
copy it, Delete and the answer to the prompt, F2 and typing to edit a line,
Ctrl+S, Ctrl+F and Enter, Ctrl+T and Ctrl+W, and check the files on disk and the
view's state afterwards.

## Measurements

Release builds of both apps on the same Linux machine (4 cores, Xvfb with
software rendering through Mesa llvmpipe, so absolute times on a real GPU are
lower for both). Memory is PSS summed over all processes of the app, 20 s after
start. Time to rows: from launch until rows show in the window, median of 5.

| | Tauri + Svelte | GPUI app |
|---|---|---|
| Two 1-million-line files (43 MB each): memory | 638 MiB (3 processes) | 318 MiB (1 process) |
| Two 1-million-line files: time to rows | 2.11 s | 1.76 s |
| Two folders of 40,000 files: memory | 548 MiB | 143 MiB |
| Two folders of 40,000 files: time to rows | 1.95 s | 1.54 s |
| Release binary, stripped | 95.5 MiB | 35.7 MiB |

Both apps open a file through the same `file_view::choose`, which also checks
whether differing files are ≈ similar in a converted view; that check is part of
both times. The GPUI times also include the tab bar and opening the sources in
the background before the folder or text view starts.

The binary sizes are not a fair comparison yet: the GPUI app links only the
parts of the core it uses. The core with everything, as in the CLI, is
85.7 MiB, so a GPUI app with all features would be about the size of today's.
On Linux the Tauri app also needs WebKitGTK (about 121 MiB for its two main
libraries), which this one does not.

The folder view also differs in how rows appear: the Svelte view shows entries
while the compare runs; this one shows the tree once the compare is done, with
a counter until then.

## Notes

- **Own workspace.** GPUI brings several hundred crates. `app-gpui` has its own
  `Cargo.lock` (started as a copy of the main one, so the core builds with the
  same versions) and is excluded from the main workspace, so `cargo test` and
  CI there are unchanged.
- **gpui 0.2.2.** The last GPUI release on crates.io is from October 2025;
  newer GPUI (wgpu renderer on Linux, AccessKit) exists only in Zed's
  repository. That release needs `xattr 0.2.3`, which no longer builds on Linux
  against current `libc`; `vendor/xattr` is that release with a one-line fix
  (`ENOATTR` is `ENODATA` on Linux and Android), applied through
  `[patch.crates-io]`.
- **Headless runs.** On X11 GPUI starts drawing once the window is reported
  visible. Under a bare Xvfb that never happens; run a window manager (for
  example `openbox`) on the display.

## Layout

```
src/main.rs        command line, window, key bindings
src/root.rs        tabs, the "New comparison" screen, light and dark
src/workspace.rs   one comparison: folder view, with the text view on top
src/folder.rs      folder compare: rows, filters, Flat, selection, copy and delete, options, watching
src/text_view.rs   text compare: rows from the core, views, image compare, editing, find
src/session.rs     sources, files, saving, search and file operations through the core
src/input.rs       a single-line text field (GPUI has none)
src/menu.rs        popup menus
src/style.rs       light and dark palettes, fonts, buttons and chips, number and date formats
src/actions.rs     keyboard actions and bindings
vendor/xattr/      xattr 0.2.3 with the libc fix
```
