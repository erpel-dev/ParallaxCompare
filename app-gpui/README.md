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
cargo run --release -- path/to/repo                 # the repository's merge conflicts
cargo run --release -- --merge BASE LOCAL REMOTE -o OUT   # as git mergetool runs it
```

Sources are written as for the `parallax` CLI. On Linux the build needs the
X11/Wayland, Vulkan and fontconfig development packages:

```sh
sudo apt install libxkbcommon-x11-dev libxcb1-dev libwayland-dev libvulkan-dev libfontconfig-dev libfreetype-dev
```

## AppImage

Releases (and the AppImage workflow on pull requests) also ship the app as
`ParallaxCompare-GPUI-…-linux-x86_64.AppImage`. It bundles only the libraries
the binary links (xcb-xkb, xkbcommon and their few dependencies) and uses the
system's Vulkan driver, Wayland and fonts, as on any desktop; it does not start
without a Vulkan driver (on machines without a GPU, Mesa's `lavapipe`, package
`mesa-vulkan-drivers`). To build one locally:

```sh
cargo build --release
../scripts/appimage-gpui.sh ParallaxCompare-GPUI-x86_64.AppImage
../scripts/appimage-gpui-self-test.sh ParallaxCompare-GPUI-x86_64.AppImage
```

`parallax-gpui --self-test [REPO_URL]` runs the same checks as the Tauri app's
(`git`, `gh` and `xdg-open` start as the app starts them; with a URL, `git
ls-remote` reaches it) and, with a display, opens a window and waits for its
first frame. The self-test script starts Xvfb and openbox when there is no
display (GPUI draws only once a window manager reports the window visible).

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

**Merge.** A three-way merge of one file: local, base and remote side by side,
the result below. Each conflict is resolved with a version (Alt+← local, Alt+→
remote, Alt+↑ base, Alt+↓ both, Alt+Shift+↓ both remote first, as in the Svelte
app) or with the chips on its header row; result lines are edited with F2;
Ctrl+Z/Ctrl+Y undo and redo. The syntax-aware merge (the built-in engine,
weave) solves conflicts the line merge cannot, offered per conflict (Alt+S) or
as a whole result. "Use Git's result" takes the file Git wrote. Ctrl+S saves,
asking before writing conflict markers or overwriting a file another program
changed. As `git mergetool` (`--merge BASE LOCAL REMOTE -o OUT`), the exit status
is 0 once the result was saved without conflicts, else 1.

**Conflicts.** With a repository folder (or from the "New comparison" screen),
the conflicted files of a merge, rebase or cherry-pick that stopped: Merge…
opens a text file in the merge view, Take local / Take remote resolve a file as
a whole, and Stage adds what is resolved to the index.

**Overview, zoom, palette, completion.**

- The text view has an overview strip beside the rows: a mark for every difference, where it is in the file; a click jumps there.
- The image compare shows the images side by side, as the differing pixels over the faded left image, or blinking between left and right.
- Ctrl+= and Ctrl+- make file contents larger or smaller, Ctrl+0 resets.
- Ctrl+Shift+P or F1 opens the command palette: every command with its keys, filtered as you type; it is also the list of keyboard shortcuts.
- The source fields of "New comparison" complete folders, files, archives and `repo@` refs as you type, with the same core function as the Tauri app's path field; Tab takes the first.

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
| Read a conflicted file's versions | `conflicts::merge_inputs`, `conflicts::conflict_of` | `open_merge` | `MergeDoc::open` |
| Merge, resolve, undo | `merge_session::MergeSession` | `merge_resolve`, `merge_edit`, `merge_undo` | `MergeDoc` |
| Build the merge rows in view | `rows::merge_input_rows`, `rows::merge_result_rows` | `merge_input_rows`, `merge_result_rows` | `MergeDoc::input_rows`, `result_rows` |
| Offer a syntax-aware merge | `syntax_merge::Engine`, `MergeSession::offer_syntax` | `merge_structured` | `MergeDoc::apply_syntax` |
| List, take and stage conflicts | `conflicts::repo_conflicts`, `write_stage`, `stage` | `repo_conflicts`, `take_conflict_side`, `stage_conflicts` | `merge::repo_state`, `take_side` |
| Complete a source being typed | `source::complete` | `complete_path` | the "New comparison" fields |

`rows::side_by_side`, `file_view::choose`, the merge row builders,
`conflicts::merge_inputs` and `MergeSession::offer_syntax` were moved from the
Tauri commands into the core, so both apps build their rows, choose views, read
conflicts and offer syntax-aware solutions with the same code. The JSON the
Svelte app reads is unchanged.

## What it does not do yet

- pull request review (comments, findings, CI annotations, writing reviews)
- typing directly into the text and merge views: lines are edited one at a time in a field
- choosing mergiraf as the syntax-aware engine (the built-in engine runs)
- matching moved and renamed files in the Flat view, as the Svelte app's Flat view does
- the version pickers beside the source fields (branches, tags, history); `repo@` completion lists refs
- wrapping long lines (they scroll sideways)
- remembering preferences (theme, text size) between runs
- screen reader support (released GPUI versions have none)

## Tests

`cargo test` runs 34 unit and headless UI tests. The UI tests open the views in
GPUI's test window and drive them like a user: F8 to a difference, Alt+→ to
copy it, Delete and the answer to the prompt, F2 and typing to edit a line,
Ctrl+S, Ctrl+F and Enter, Ctrl+T and Ctrl+W, Alt+arrows in a merge, the command
palette, path completion with Tab, blinking images. One test goes from a
repository stopped in a merge through the conflicts list, the merge, saving and
staging, and checks the index. Others cover the review findings: Shift+click
after a filter changed the rows, a save that asks about each file changed on
disk, Ctrl+W on a tab with unsaved edits, Find after edits, a watcher that
follows new exclusions, and conflict records dropped on refresh. The files on
disk and the views' state are checked afterwards.

## Measurements

Release builds of both apps on the same Linux machine (4 cores, Xvfb with
software rendering through Mesa llvmpipe, so absolute times on a real GPU are
lower for both). Memory is PSS summed over all processes of the app, 20 s after
start. Time to rows: from launch until rows show in the window, median of 5.

| | Tauri + Svelte | GPUI app |
|---|---|---|
| Idle window: memory | 402 MiB (3 processes) | 127 MiB (1 process) |
| Idle window: time to first UI | 1.46 s | 0.45 s |
| Two 1-million-line files (43 MB each): memory | 615 MiB | 324 MiB |
| Two 1-million-line files: time to rows | 2.37 s | 1.75 s |
| Two folders of 40,000 files: memory | 554 MiB | 144 MiB |
| Two folders of 40,000 files: time to rows | 2.24 s | 1.63 s |
| Three-way merge of 200,000 lines (133 conflicts): memory | 607 MiB | 233 MiB |
| Three-way merge of 200,000 lines: time to rows | 1.96 s | 1.35 s |
| Release binary, stripped | 95.5 MiB | 102.9 MiB |

Both apps open a file through the same `file_view::choose`, which also checks
whether differing files are ≈ similar in a converted view; that check is part of
both times. The GPUI times also include the tab bar and opening the sources in
the background before the folder or text view starts.

The GPUI binary now links nearly all of the core (converted views, merge,
the syntax-aware engine) plus GPUI and is slightly larger than the Tauri
binary. On Linux the Tauri app also needs WebKitGTK (about 121 MiB for its two
main libraries) at run time, which this one does not.

### Reaction times

From X11 input to the first changed pixel in the result area: Xvfb at
1400 × 900, software rendering through Mesa llvmpipe, both apps pinned to two
CPUs, 20 trials each, after warm-up. Milliseconds, median / 90th percentile.

| | Tauri + Svelte | GPUI before | GPUI now |
|---|---|---|---|
| Mouse-wheel step, 40,000 files | 62 / 82 | 66 / 82 | 67 / 96 |
| Filter All ↔ Left only, 40,000 files | 90 / 111 | 76 / 97 | 60 / 73 |
| F8 / F7, two 1-million-line files | 137 / 161 | 214 / 250 | 70 / 103 |
| Mouse-wheel step, with `RUST_BACKTRACE=1` | – | 340 / 382 | 66 / 88 |
| F8 / F7, with `RUST_BACKTRACE=1` | – | 230 / 255 | 70 / 89 |

What made the GPUI app slow, found with `perf`:

- **Fonts.** `TextSystem::all_font_names` also lists GPUI's own fallback
  families ("Segoe UI", "Helvetica" …) whether they are installed or not, so on
  Linux the app picked "Segoe UI". Every text run then tried it and walked the
  fallback list, eight failed lookups before DejaVu Sans, each building an
  `anyhow` error; with `RUST_BACKTRACE=1` each also captured a backtrace, two
  thirds of the main thread's time. The app now checks which family a name
  really resolves to (and the interface is drawn in Inter as meant).
- **Rows in lists.** `uniform_list` lays rows out during prepaint, where the
  text style of the list and its parents does not apply; rows now name their
  font and size themselves instead of falling back to GPUI's default font.
- **Unsaved state.** `EditSession::is_dirty` and `MergeSession::is_dirty` in
  the core hashed the whole text on each call, and the views ask on every
  frame: about 60 ms per frame for two 43 MB files. The answer is now kept
  until the text changes.
- **Overdraw.** The views no longer paint full-window backgrounds over the
  window's own, and unchanged rows no longer paint the window colour again.

What remains is mostly rendering: GPUI redraws the whole window on every frame
through Vulkan, and with llvmpipe that rasterising takes 80–90 % of the CPU
time of an interaction. WebKit repaints only the changed parts on the CPU. With
a GPU a GPUI frame takes a few milliseconds, so these timings are the worst
case for GPUI and say little about a desktop with graphics drivers.

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
src/main.rs        command line (compare, conflicts, --merge), window, key bindings
src/root.rs        tabs, the "New comparison" screen with completion, the command palette, zoom, light and dark
src/workspace.rs   one comparison: folder view, with the text view on top
src/folder.rs      folder compare: rows, filters, Flat, selection, copy and delete, options, watching
src/text_view.rs   text compare: rows from the core, views, image compare, editing, find
src/session.rs     sources, files, saving, search and file operations through the core
src/merge.rs       a three-way merge and a repository's conflicts through the core
src/merge_view.rs  the merge view
src/conflicts_view.rs  the conflicted files of a repository
src/prompt.rs      the app's dialog for questions (GPUI's own clips text on Linux)
src/input.rs       a single-line text field (GPUI has none)
src/menu.rs        popup menus
src/style.rs       light and dark palettes, fonts, buttons and chips, number and date formats
src/actions.rs     keyboard actions and bindings
vendor/xattr/      xattr 0.2.3 with the libc fix
```
