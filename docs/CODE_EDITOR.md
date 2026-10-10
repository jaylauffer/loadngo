# Code Editor

A general-purpose, multi-file code editor built on loadngo, meant to replace
VS Code for Jay's day-to-day Rust editing. Started 2026-10-10 at Jay's
request. macOS comes first; every other platform must keep building, which
CI checks.

This document records the decisions and the order of work. The editing core
it builds on is described in [`TEXT_EDITOR_MODEL.md`](TEXT_EDITOR_MODEL.md).

## Running it

```
~/pudding/launch-ide.sh                     # the folder open last time
~/pudding/launch-ide.sh ~/pudding/loadngo   # this folder
```

The script (at the pudding root, outside any repository) builds the release
binary when loadngo has changed and then runs it; `--help` lists its options.
The binary is `loadngo/target/release/code_editor [FOLDER]`.

## Decisions (Jay, 2026-10-10)

- **Proportional fonts are the default.** Nothing assumes a fixed character
  cell: layout measures text runs, and no feature depends on columns
  lining up (no column selection, alignment guides or ASCII-art rulers).
  Code is for reading, not for its shape.
- **Almost no hotkeys.** The bindings are Save (Cmd-S), **Save All**
  (Cmd-Shift-S), and the platform's own Undo/Redo, Cut/Copy/Paste and
  Select All. Every other command sits on a visible control, never only on
  a key chord. On Windows and Linux, Ctrl replaces Cmd.
- **Standard menus** (Jay, 2026-10-10), as loadngo's own menu support
  (`docs/MENUS.md`). On macOS that is the system menu bar; elsewhere a menu
  bar drawn in the window. The menus also carry the platform's usual open,
  close, quit and find shortcuts.
- **rust-analyzer runs as an external process** that speaks LSP over stdio.
  It is spawned and supervised like `airplay2-sender`, not linked or
  rewritten.
- **macOS first.** The editor must build on Linux, Windows, iOS and Android,
  but it only has to run well on the Mac until Jay asks for more.

## What already exists

Checked in the code on 2026-10-10 (loadngo `a82e173d`):

| Piece | Where | State |
|---|---|---|
| Piece-table document, undo/redo, revisions | `ui-core/src/text_document.rs` | Usable. Edits and undo are indexed by character. |
| Multiline editing surface | `ui-core/src/text_area.rs` (`TextAreaModel`) | Caret, selection, drag, wheel and scrollbars, line-number gutter, line metrics cache, line starts updated incrementally on edits. Clipboard is left to the app. |
| Split/tab layout | `ui-core/src/{workspace,split,tabs}.rs` | Generic. |
| File dialog | `ui-core/src/file_dialog.rs` | Built in loadngo, no `rfd`. |
| Atomic save | `loadngo-persistence::replace_atomically` | Write-through replace, including on Windows. |
| Off-thread work | `loadngo_host_desktop::offload` | Two workers; results arrive as host-proactor completions. (`load_text` is a blocking `std::fs` read on macOS, so the editor does not use it.) |
| sng-rusty editor | `sng-rusty/src/bin/sng_rusty_editor.rs` | A visual-novel script editor wrapped around `TextAreaModel`. Only its clipboard routing is worth reusing. |

What is missing for code:

- **Colored runs.** `PaintOp::Text` draws a whole string in one color, and
  every distinct string becomes its own texture. Highlighting splits a line
  into many short colored runs, and each edit or scroll would create and
  destroy textures for them (the churn already met on DX12, see
  `TEXT_TEXTURE_LIFECYCLE.md`). Highlighting therefore needs a glyph cache:
  each glyph rasterized once and lines drawn from it as colored runs, with
  advances that serve proportional measurement as well.
- **The editor shell**: folder tree, one tab per file, dirty markers, save and
  save-all, files changed on disk, find, go to line, session restore.
- **Rust support**: highlighting, `cargo check` errors, rust-analyzer.

## Structure

- `code-editor/` (package `loadngo-code-editor`): the library holds the
  editor's state and logic (open buffers, file tree, commands, session) with
  no host dependency, so it can be tested without a window. A thin binary,
  `code_editor`, connects it to `loadngo-host-desktop`.
- General widgets the editor needs (a lazily loaded tree, colored text runs)
  go into `ui-core`, so other loadngo apps get them too.

## Runtime rules

These are the workspace's demand-driven rules applied to the editor:

- The frame demand is `Idle` whenever nothing changes, and
  `IdleUntil(deadline)` while the caret blinks or a backup or session write
  is due. `IdleUntil` is new in host-core for this: like `Idle`, input brings
  the frame at once, but no later than the deadline. `After` would not do:
  it is paced on every host, so input waits for its timer, and a blinking
  caret would delay typing by up to half a blink. The caret stops blinking
  (solid caret) 10 s after the last input, so an editor left open draws
  nothing.
- Reading files, listing folders, saving and checking for changes on disk
  all run through `offload`. On macOS a finished job now posts an
  application-defined event, so the result is dispatched and an idle frame
  runs at once. Before, it waited for the next window event. On the other
  hosts it still waits for the next frame. Nothing blocks a frame.
- Changes on disk are checked when the window regains focus
  (`HostFrame.focused`) and before a save, never by polling. A clean buffer
  reloads silently. A dirty buffer shows the conflict and keeps the edits.
- Saves use `replace_atomically` and keep the file's line endings: CRLF
  files are edited as `\n` and written back as CRLF.
- The buffer is dirty when its document revision differs from the revision
  that was saved, not when the whole text compares unequal.
- Files that are not UTF-8, and binary files (a NUL in the first 8 KiB),
  are not opened; the status line says why.
- Unsaved edits are backed up 2 s after typing stops
  (`backups/<hash of path>.json` in the app data folder) and the open folder,
  tabs, carets and expanded folders 2 s after they change (`session.json`).
  A loadngo host cannot veto closing the window, so these backups are what
  make closing, a crash or a power cut lose at most 2 s of typing. At launch
  the tabs come back, and a backup is applied as one undoable edit over the
  file's text, so Cmd-Z returns to what is on disk.
- The tree hides `target/` and `.git/` and skips any directory carrying a
  `CACHEDIR.TAG`.

## Milestones

**M1, usable for plain editing.** Open a folder (by argument or the file
dialog), browse it in a tree, open files in tabs, edit, undo, Cmd-S and
Cmd-Shift-S, dirty markers, confirm before closing a dirty tab, check for
changes on disk at focus, find in the current file, go to line, and reopen
the last folder and tabs at launch. Proportional font, current text path
(one `Text` op per visible line).

Done 2026-10-10. File menu: Open Folder… (Cmd-O, the file dialog's new
folder mode), Refresh Folder, Save, Save All, Close Tab (Cmd-W), and Quit
on hosts with no application menu. Edit menu: Undo, Redo, Cut, Copy, Paste,
Select All, Find… (Cmd-F), Go to Line…. Quit, from the menu or Cmd-Q, asks
first when files are unsaved: Save All and Quit, Quit Without Saving
(which also drops their backups), or Cancel. The app stops only once the
saves and the session write have landed. A new folder keeps the open tabs;
nothing is closed by switching. Along the way, every `TextAreaModel`
gained:

- undo by word: typing groups until whitespace is followed by a new word,
  deletions group with deletions, and any caret move ends a group. Undo
  and redo put the caret at the change.
- the platform's word and line moves: Option (Ctrl elsewhere) with the arrows
  and Backspace/Delete works by word, Cmd with Left/Right/Backspace by
  line, and Cmd with Up/Down jumps to the document's start or end.
- `auto_indent` (Enter keeps the indentation, plus one level after an
  opening bracket) and `tab_text`.
- a gap between the line-number gutter and the text.
- only lines on screen are measured. Measuring a line takes one text
  measurement per character, and the first layout used to measure every
  line: opening a 4,328-line file blocked one frame for 1,043 ms. Other
  lines carry an estimated width until they scroll into view, and the
  cache of measured lines is bounded.

**M2, highlighting.** Done 2026-10-10 for Rust (`.rs` files), without the
glyph cache this plan first called for. Each colored token is drawn as its
own `Text` op at the offset `TextAreaModel` already measures for the caret
(`TextAreaModel::paint_with_runs`), so every renderer draws it with the text
caches it already has. Tokens such as `let`, `fn` and `(` repeat, so most
come from cache; an edit creates textures only for the tokens it changes.
Measured below: about 2.6 times the text ops of plain drawing, with typing
still at about 1.3 ms of work per frame. A glyph cache stays available if a
platform shows text-texture churn.

The lexer (`code-editor/src/rust_lexer.rs`) is written here: keywords,
types (capitalized and primitive), function definitions and calls, macros,
lifetimes, strings (including raw and multi-line), chars, numbers,
constants, comments, doc comments and attributes. It lexes one line at a
time; what continues past a line (a block comment, a string) is a small
state. The highlighter keeps each line's colors and lexes only down to the
bottom of the screen. An edit drops what was kept from its line on, so
typing re-lexes from the edited line to the bottom of the screen. A line
holding tabs is drawn plain.

**M3, `cargo check`.** Done 2026-10-10. Saving a `.rs` file, the Check
button or Build > Check runs `cargo check --all-targets
--message-format=json` in the package holding the file (the nearest
`Cargo.toml` above it). One check runs at a time; a newer one kills the
older one, and its late result is ignored. The check runs on a thread of
its own, since it can take minutes and the two offload workers serve the
editor's file I/O. Its result comes back through the host's new
`completion()`: a `Completer` another thread finishes, read like an
offloaded job, and on macOS it wakes an idle frame.

Errors and warnings show as a bar in the gutter and an underline under the
primary span; the caret's line shows its message in the status bar; and the
count in the status bar opens the Problems panel (also Build > Show or Hide
Problems), whose rows open the file at the line and column. Marks refer to
the file as last saved and can drift while you edit, until the next save
checks again. cargo is taken from `PATH`, else `~/.cargo/bin`, so a Finder
launch finds it too.

**M4, rust-analyzer.** Done 2026-10-11. rust-analyzer (`rustup component
add rust-analyzer`; found on `PATH` or in `~/.cargo/bin`) starts for the
open folder when the first Rust file loads, as a child process speaking LSP
over stdio. A writer, a reader and a stderr thread keep the frame thread
from ever waiting on it. The reader wakes the frame through the host's new
`frame_waker()` after each message. The protocol logic (`lsp/session.rs`)
has no process in it, so its tests play the server. Positions are UTF-32,
the editor's own character indices, which rust-analyzer agrees to.
rust-analyzer's own cargo check is off: the editor runs one on save (M3),
and two would fight over the build lock.

- Every open Rust file is kept in step: opened, its whole text sent once
  per change (sent before any request, so an answer is never about older
  text), saved and closed.
- rust-analyzer's own diagnostics (syntax errors, and what it finds without
  cargo) show live as you type, with cargo check's, in the gutter, the status
  bar and the Problems panel. Its state ("indexing…", "ready") is in the status
  bar.
- Hover: rest the pointer on a name for 450 ms.
- Go to Definition: Cmd-click (Ctrl elsewhere) or Navigate > Go to
  Definition; Navigate > Back (Cmd-[) returns.
- Completion: typing a name, `.` or `::` lists completions under the caret,
  narrowed as you type; Up/Down, Enter or Tab, or a click inserts one with
  any `use` line it brings; Escape or any other key closes it.
- Rename: Navigate > Rename Symbol… renames across the workspace; files not
  open are opened, and every changed file is left unsaved for Save All.

A server that stops is reported and not restarted until the folder is
opened again; switching folders or quitting stops it, with its proc-macro
servers.

Later, as needed: find in files, splits, file create/rename/delete in the
tree, and running on Linux and Windows.

## Evidence

Each milestone records here what was run and checked: tests, macOS sessions,
idle CPU and wakeups, and the CI run for the other platforms.

**M1 (2026-10-10, Mac mini).** `cargo test -p loadngo-code-editor`: 27
tests, 15 of them whole flows on real temp folders (save, Save All, CRLF,
close prompts, disk conflicts both ways, restart with backups, find, go to
line, clipboard, idle). ui-core tests cover undo grouping, word and line
moves, auto-indent and the lazy layout. Workspace fmt and strict clippy
pass on macOS. iOS, Android (host library and editor) and Windows (type
check, `blake3` pure) builds pass from macOS; Linux is left to CI.

In the release build on macOS, with `CODE_EDITOR_TRACE=1` printing each
frame's work:

- the 4,328-line `archive_cas_browser.rs`: the frame that first laid it
  out (restored at launch, with its backup applied) took 9.0 ms; before lazy
  layout, opening it took 1,043 ms. Typing stays at or under 1.2 ms per
  frame, jumping to the end of the file 10.5 ms, undo 3.2 ms.
- a folder listing arrives 0.6 ms after the click that asked for it, as its
  own frame.
- idle with a file open: 0 frames in 15 s once the caret stops blinking,
  0.0% CPU, 9 threads, 39 MB.
- after the editor was killed with unsaved edits, the next launch restored
  both tabs and the unsaved text.
- `sng_rusty_editor`'s source pane, which uses the same `TextAreaModel`, was
  checked by screenshot after these changes.

**Open Folder and menus (2026-10-10, Mac mini).** 34 editor tests (new: system
menu commands and republished enabled states, Quit with nothing unsaved,
Quit asking then Save All and Quit, Quit Without Saving dropping backups,
Open Folder by typed path, Escape closing the dialog, the drawn menu bar by
click and keys), 6 menu-model tests and 3 folder-mode dialog tests. In the
release build: the system menu bar shows the application, File, Edit and
Window menus with the right items disabled; Cmd-O opened the dialog; picking
a folder and Open switched the tree; Cmd-Q with an unsaved file showed the
prompt; Escape cancelled it; Cmd-S saved through the menu's key equivalent;
Cmd-Q then quit and the session recorded the new folder. The drawn bar was
checked on macOS with `CODE_EDITOR_DRAWN_MENU=1`. `sng_rusty_editor` now
gets the standard application and Window menus, and Cmd-Q quits it. iOS,
Android (all binaries, now that a file dialog's source is `Send`) and Windows
(type check) builds pass from macOS.

**Highlighting (2026-10-10, Mac mini).** 47 editor tests, among them every
`.rs` file in the loadngo workspace lexed with no gaps or overlaps and no
file left inside a comment or string (0.55 s). Release build with
`CODE_EDITOR_TRACE=1` on the 4,328-line `archive_cas_browser.rs`, with
highlighting: typing at most 1.28 ms of frame work (1.2 ms without), the
jump to the end, which lexes all 4,328 lines, 12.3 ms (10.5 ms without),
undo 0.95 ms; about 275 paint ops per frame against about 105.

**cargo check (2026-10-10, Mac mini).** 56 editor tests. Among them:
cargo's JSON parsed at the primary span, duplicate messages from bin and test
targets reported once, paths given the way the editor opened the folder
(cargo reports `/private/var/…` for `/var/…`), a cancelled check ending
quietly, and the real cargo run on a broken crate both directly and through
the editor from save to the marked line. Host: `completion()` delivers a
result finished on another thread only through the proactor, and a dropped
`Completer` delivers an error. In the release build: Check on a crate with
a type error showed the gutter bar, the underline under `"no"`, "1 error" in
the status bar, the message on the caret's line, and the Problems row;
fixing it and pressing Cmd-S re-checked by itself and showed the three
warnings rustc reports once there is no error.

**rust-analyzer (2026-10-11, Mac mini).** 77 editor tests, among them:
- the session's protocol logic against a played server;
- a run against the real rust-analyzer 1.99.0, skipped where it is not
  installed, as on CI: a syntax error reported on its line and a hover
  answered, 2.7 s;
- editor flows for sync order, live diagnostics, hover, Cmd-click and Back,
  completion with an import, and rename across an open and an unopened
  file, replaying rust-analyzer's real reply.

Release build: hover on a function; Cmd-click to its definition and back; a
live syntax error marked unsaved; `hel` completed to `helper`; `helper`
renamed to `assist` in two files, saved, and cargo check clean. Quitting
left no rust-analyzer process.

The rename found a `TextAreaModel` bug: when several edits came before a
layout, the lines changed by all but the last kept showing their old text,
though the text itself was right (saving would have written `fn assist`
under a displayed `fn helper`). Completions with an import hit it too. Now
any second edit before a layout re-lays out from the first changed line
(ui-core test, which fails without the fix). Testing through synthetic input
also showed a test-tool artifact: Cmd chords sent without the Cmd key's
release made later clicks arrive as Cmd-clicks. The tool now releases it.
