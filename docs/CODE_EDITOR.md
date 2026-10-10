# Code Editor

A general-purpose, multi-file code editor built on loadngo, meant to replace
VS Code for Jay's day-to-day Rust editing. Started 2026-10-10 at Jay's
request. macOS comes first; every other platform must keep building, which
CI checks.

This document records the decisions and the order of work. The editing core
it builds on is described in [`TEXT_EDITOR_MODEL.md`](TEXT_EDITOR_MODEL.md).

## Decisions (Jay, 2026-10-10)

- **Proportional fonts are the default.** Nothing assumes a fixed character
  cell: layout measures text runs, and no feature depends on columns
  lining up (no column selection, alignment guides or ASCII-art rulers).
  Code is for reading, not for its shape.
- **Almost no hotkeys.** The bindings are Save (Cmd-S), **Save All**
  (Cmd-Shift-S), and the platform's own Undo/Redo, Cut/Copy/Paste and
  Select All. Every other command sits on a visible control, never only on
  a key chord. On Windows and Linux, Ctrl replaces Cmd.
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

- The frame demand is `Idle` whenever nothing changes. The caret blinks on a
  host deadline and stops blinking (solid caret) after a few seconds without
  input, so an editor left open costs nothing.
- Reading files, listing folders, saving and checking for changes on disk
  all run through `offload`, and their results are picked up
  on the next frame. Nothing blocks a frame.
- Changes on disk are checked when the window regains focus
  (`HostFrame.focused`) and before a save, never by polling. A clean buffer
  reloads silently. A dirty buffer shows the conflict and keeps the edits.
- Saves use `replace_atomically` and keep the file's line endings: CRLF
  files are edited as `\n` and written back as CRLF.
- The buffer is dirty when its document revision differs from the revision
  that was saved, not when the whole text compares unequal.
- Files that are not UTF-8 open read-only with a note. Binary files are not
  opened.
- The tree hides `target/` and `.git/` and skips any directory carrying a
  `CACHEDIR.TAG`.

## Milestones

**M1, usable for plain editing.** Open a folder (by argument or the file
dialog), browse it in a tree, open files in tabs, edit, undo, Cmd-S and
Cmd-Shift-S, dirty markers, confirm before closing a dirty tab or quitting,
check for changes on disk at focus, find in the current file, go to line, and
reopen the last folder and tabs at launch. Proportional font, current text
path (one `Text` op per visible line).

**M2, highlighting.** A glyph cache with colored runs in `ui-core` and on the
macOS renderer first (other renderers fall back to one `Text` op per run
until they get the cache). A Rust lexer written here: comments, strings,
chars, lifetimes, numbers, keywords, macros, attributes. Re-lexing starts at
the edited line and stops where the lexer state matches again.

**M3, `cargo check`.** Run `cargo check --message-format=json` on save
through a supervised subprocess, show errors and warnings in the gutter and
in a list, and jump to them. One run at a time; a newer save cancels the
older run.

**M4, rust-analyzer.** LSP over stdio: diagnostics as you type, go to
definition, hover, completion and rename, with document sync from the piece
table's edits.

Later, as needed: find in files, splits, file create/rename/delete in the
tree, and running on Linux and Windows.

## Evidence

Each milestone records here what was run and checked: tests, macOS sessions,
idle CPU and wakeups, and the CI run for the other platforms.
