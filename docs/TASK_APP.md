# Task app

The Task window: a day plan and a project plan over one plan file. It came
from the C++ `loadngo-cpp/Task` (Windows) by way of a Rust Win32 port in
`task/src/` (`task_window.rs`, `day_planner.rs`, `task_list.rs`, ...). Since
2026-10-13 the same app also runs on macOS and Linux, on `ui-core` and
`loadngo-host-desktop`.

## Running it

    cargo run -p task --release

`task --help` describes it. On Windows the binary is still the Win32 port;
everywhere else it is the new front end.

## Layout of the code

- `task/src/lib.rs` and its modules: the app with no platform in it.
  - `plan.rs`: tasks and time entries, every edit the window makes, undo and
    redo (whole snapshots, 100 steps; a drag is one step), and the JSON the
    Win32 Task reads and writes.
  - `clock.rs`: times are local wall-clock 100 ns ticks since 1601 (a local
    FILETIME), as the C++ and Win32 Task store them.
  - `store.rs`: loading and saving, as `IoRequest`/`IoResponse`.
  - `tree_layout.rs`: the project hierarchy's boxes and lines.
  - `app.rs`: `TaskApp`, the window: layout, input, drag and drop, painting
    to `PaintOp`s, menus.
- `task/src/main.rs`: on Windows the Win32 port; elsewhere the `desktop`
  module, which runs `TaskApp` on `loadngo-host-desktop`.

## The proactor and idle

The window draws only on input or a finished job (`FrameDemand::Idle`).
Loading and saving run on the host's offload workers (`loadngo_host_desktop::offload`)
and come back as host-proactor completions; the window never waits on the
disk. The only deadline is a save that is due, as `FrameDemand::idle_until`.
There is no timer thread and no caret blink.

Measured 2026-10-13 on the Mac mini (release build, a plan of 11 tasks and 7
entries): idle 0.0% CPU, CPU time flat over 15 s, 9 threads, about 30 MB;
two drags with the mouse cost 0.11 s of CPU in all, then nothing.

## Files

The plan lives in the app data folder (`~/Library/Application Support/loadngo-task`
on macOS; `TASK_STATE_DIR` overrides it, for test runs):

- `user_plan.json`: tasks
- `user_plan-entries.json`: time entries
- `config.json`: settings (string values; unknown keys are kept)

These are the Win32 Task's file names and formats (its `taskdata` folder), so
that folder can be copied here, and a test opens a Windows entries file on
its day. Each file is replaced atomically (`loadngo_persistence::replace_atomically`).
A plan that cannot be read is left alone: the window opens read-only and saves
nothing.

The plan is saved one second after each change, and before Quit. The Win32
Task saved every 12 minutes, on Save and on close; the window's close button
on macOS closes at once, so waiting would lose work.

## What it does

Day Plan:

- The date banner (past, today and future in different colours); `<` and `>`
  or the arrow keys change the day, Today returns.
- Two columns, Planned and Actual, in 15-minute slots. Click a slot and type
  a task (matching tasks are offered; Up/Down and Enter, or click one); a new
  name makes a new task. Drag a task from the list into a slot. Drag an entry
  to move it, its bottom edge to change its length. Delete or Backspace
  deletes the selected entry.
- The list on the right: tasks with no subtasks, all or within one project
  (the menu at its top), with their priority marks; "Add a task" adds one
  (into the chosen project). Its left edge, and the line between the
  columns, can be dragged; both widths are kept.

Project Plan:

- Projects (top-level tasks) on the right; the chosen one's tasks as a tree
  on the left, each task centred over its subtasks. The button under a box
  folds its subtasks. Drag a box onto another to move it there, onto the
  project list to make it a project.
- The details of the selected task: its name, priority (none, or 1 to 5),
  where it is, Add Subtask and Delete. Deleting a task moves its subtasks up
  to its parent; its time entries stay.

Both: drag anything onto the bin at the top right to delete it. Undo and Redo
(Edit menu, Cmd-Z) cover every edit. New Task (Cmd-N) goes to the list's add
field. The menus are the macOS menu bar, or drawn in the window elsewhere.

## Differences from the Win32 Task

- The `<` and `>` buttons work (the Win32 banner only drew them), and the
  banner shows the day being planned, not always today.
- Rename, priority, subtasks, moving tasks and deleting by drag or key are
  new; the Win32 port had a placeholder for task details and for new
  entities.
- Unset priority shows no mark (the Win32 list drew it as priority 1, red).
  Lists sort by priority, then name (the Win32 comparator's due date and
  start date are never set by either app).
- A project's list in the day plan includes all its tasks with no subtasks,
  not only its direct children.
- Planned entries are no longer titled "(suppressed) ..."; entries show their
  task's current name.

## Not ported

- The Schedule tab, Report and Print: placeholders in the Win32 port.
- Sync: the Win32 button sends a `RequestUserTaskSynch` to the configured
  multicast targets (none by default), and nothing in the workspace answers
  it. Plan sync needs a responder first; the place for it is the resident
  loadngo node (`docs/LOADNGO_NODE.md`).
- The chat strip, and files or text dropped from other apps (the Win32 port
  only logged them).
- Remembering the window's size and position.

## Not yet checked

- Linux and Windows desktops have not run the window (Linux: gates on
  dolores; Windows: CI). On Windows the binary is still the Win32 port; the
  new front end compiles there too and could replace it.
- Input was driven on macOS with real events (tab, list and slot clicks,
  typing, drags into slots and between rows); Cmd-Z, the bin and the
  hierarchy drags were checked by tests only.
