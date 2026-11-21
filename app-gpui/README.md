# ParallaxCompare: GPUI prototype

A second desktop front end for ParallaxCompare, written with
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) (the UI
framework of the Zed editor) instead of Tauri and Svelte. It runs on the same
compare core, `crates/parallax-core`, as the Tauri app and the CLI. It exists to
try out the switch on real code: what carries over, what has to be rebuilt, and
what it changes in size, memory and speed.

```sh
cd app-gpui
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

**Folder compare.** Compares the two roots in the background with
`compare::compare` and shows the core's result tree in one virtualized list:
both sides with name, size and modification time, and a status column (`=`
same, `≈` similar, `≠` different, `◀` left only, `▶` right only, `!` error).
After a compare, folders with differences are open, as in the Svelte app.
Filters (All, All differences, Same, Left only, Right only, Errors) with counts.
Folders the compare did not walk (identical by Git tree id, closed archives) are
compared when opened, with `compare::compare_path`. Refresh (F5) compares again.

**Text compare.** Opens from a file in the folder view (double-click or Enter),
or directly when both roots are files. Side by side with line numbers, syntax
colours, changed characters marked within changed lines, and coloured rows for
changed, added and removed lines. Binary files are shown as a hex dump. F7/F8
and the toolbar move between differences; Escape or Back returns to the folders
as they were.

**Keys.** F7/F8 previous/next difference (both views); ↑/↓, Home/End, ←/→ and
Enter in the folder view; F5 refresh; Escape or Alt+← back; Ctrl+Q quit.

## The same core logic

Both front ends call the same core functions. The Tauri app does it inside its
commands and sends the results to the web view as JSON; this app calls them
directly while drawing.

| Step | Core function | Tauri app (`app/src-tauri/src/commands.rs`) | GPUI app |
|---|---|---|---|
| Open the two sides | `source::open_pair` | `open_session` | `Session::open` |
| Compare folders | `compare::compare` | `run_compare` | `FolderView::run` |
| Open a folder that was not walked | `compare::compare_path`, `tree::replace` | `expand` | `FolderView::toggle` |
| Diff two files | `text::decode`, `edit::EditSession` | `open_file_diff` | `session::open_file` |
| Pick a grammar | `highlight::detect_syntax` | `open_file_diff` | `session::open_file` |
| Build the rows in view | `rows::side_by_side` | `file_rows` | `FileDiff::rows` |

`rows::side_by_side` was moved from the Tauri command `file_rows` into the core
for this prototype, so both apps build their rows (syntax colours, changed
characters, long lines cut around the first change) with the same code. Its
JSON shape is unchanged; the Svelte app reads the same fields as before.

## What it does not do

This prototype covers the two main views. The Tauri app also has, and this one
does not:

- editing text, copying differences, undo, saving
- copy and delete of files and folders, the context menu
- converted views (PDF, Office, executables, outlines, pretty XML/JSON …) and the image compare; only text and hex
- three-way merge, conflicts, pull request review
- the new-comparison dialog, tabs, the path bars, recent sources
- find in files, options (excludes, includes, whitespace), dark theme
- watching the folders for changes
- horizontal scrolling of long lines (they are clipped at the window edge)
- text selection and copying to the clipboard
- screen reader support (released GPUI versions have none)

## Measurements

Release builds of both apps on the same Linux machine (4 cores, Xvfb with
software rendering through Mesa llvmpipe, so absolute times on a real GPU are
lower for both). Memory is PSS summed over all processes of the app, 20 s after
start. Time to rows: from launch until rows show in the window, median of 5.

| | Tauri + Svelte | GPUI prototype |
|---|---|---|
| Two 1-million-line files (43 MB each): memory | 638 MiB (3 processes) | 303 MiB (1 process) |
| Two 1-million-line files: time to rows | 2.11 s | 1.30 s |
| Two folders of 40,000 files: memory | 548 MiB | 134 MiB |
| Two folders of 40,000 files: time to rows | 1.95 s | 1.05 s |
| Release binary, stripped | 95.5 MiB | 33.2 MiB |

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
src/workspace.rs   opens the sources; folder view, with the text view on top
src/folder.rs      folder compare: result tree, rows, filters, lazy folders
src/text_view.rs   text compare: rows from the core, text runs, F7/F8
src/session.rs     opening sources and files through the core
src/style.rs       colours, fonts, buttons and chips, number and date formats
src/actions.rs     keyboard actions and bindings
vendor/xattr/      xattr 0.2.3 with the libc fix
```
