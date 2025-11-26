# Workspace Layout Plan

## Purpose

This document defines the intended `loadngo` path from today's editor/task
layouts to a reusable desktop workspace model that can eventually support
docking.

The immediate next need is resizable panes for `sng_rusty_editor`.
The long-term target is not a one-off splitter widget. It is a workspace layout
tree that can grow into tabbed and dockable panes without throwing away the
first implementation.

## Current Evidence In The Repo

The repo already contains splitter-like behavior in Win32 task UI code:

- [day_planner.rs](../task/src/day_planner.rs)
  - `split_percent`
  - `splitter_dragging`
  - min-pane layout constraints
  - drag-to-resize behavior
- [task_list.rs](../task/src/task_list.rs)
  - simple adjust-bar drag/capture pattern

Those implementations are useful references for behavior, but they are not
reusable as `ui-core` primitives because they are coupled to:

- `HWND`
- `WM_*` message routing
- GDI painting
- host-specific mouse capture
- Win32 `Container` / `BufferedWnd`

## Design Direction

The next reusable abstraction should be a workspace layout model in `ui-core`.

It should support:

- split layouts
- tab groups
- stable panel identities
- persistent layout state

It should not initially implement:

- drag-to-dock
- floating windows
- arbitrary tear-off panes

Those are later behaviors built on the same workspace tree.

## Conceptual Model

Use a tree of workspace nodes.

Suggested shape:

- `WorkspaceNode`
  - `Split(SplitNode)`
  - `Tabs(TabGroupModel)`
  - `Leaf(PanelLeaf)`

- `SplitNode`
  - `axis`
  - `bounds`
  - `split_ratio`
  - `min_first`
  - `min_second`
  - `handle_size`
  - `hit_size`
  - derived rects for first, handle, second
  - drag state

- `TabGroupModel`
  - `tabs`
  - `selected`
  - tab strip bounds
  - page content bounds

- `PanelLeaf`
  - stable `panel_id`
  - title metadata
  - optional role/kind metadata

This gives us a layout tree where:

- splitters resize
- tabs switch content
- leaves host actual tool panes

Later docking can mutate the same tree by:

- inserting new split nodes
- moving leaves between tab groups
- creating new tab groups

## Why This Is Better Than A Narrow Splitter Widget

A one-off `SplitContainerModel` would solve the next editor step, but risks
locking us into a dead-end layout primitive.

The workspace-tree approach avoids that churn:

- split resizing is still the first implemented behavior
- tabs are planned from day one
- layout persistence becomes straightforward
- docking can be added later without rewriting the foundation

## Immediate Scope

The first implementation should be intentionally small.

Phase 1:

- add a split model to `ui-core`
- support horizontal and vertical axis
- support ratio-based layout
- support minimum sizes
- support drag begin / drag update / drag end
- compute child rects and handle rect
- paint only the handle/chrome needed for interaction

Current status:

- `ui-core` now has `SplitNodeModel`
- `sng_rusty_editor` now uses split-driven shell layout for
  left/center/right panes and the labels/operations split
- editor pane ratios are persisted as editor settings
- editor window size is also persisted between runs

Phase 2:

- add a tab-group node model
- support selected tab
- expose tab strip bounds and content bounds
- reuse existing tab concepts where possible

Current status:

- `ui-core` now has a reusable `TabGroupModel`
- `ui-core` now has a recursive `WorkspaceNode` tree with:
  - `Split`
  - `Tabs`
  - `Leaf`
- nested split/tab workspace behavior is exercised in
  [workspace_harness.rs](../host-desktop/src/bin/workspace_harness.rs)
- `sng_rusty_editor` now uses a `WorkspaceTabGroup` for the right-side tool pane

Phase 3:

- compose them into a workspace tree
- persist ratios and selected tabs by stable panel id
- use that for desktop editor layout

Phase 4:

- add docking behaviors only if needed
  - drag tab to edge -> new split
  - drag tab into tab strip -> new tab in group
  - optional float/tear-off later

## Proposed `ui-core` API Shape

The exact names can change, but the model should look roughly like this:

```rust
pub enum WorkspaceAxis {
    Horizontal,
    Vertical,
}

pub struct SplitNodeModel {
    pub axis: WorkspaceAxis,
    pub bounds: Rect,
    pub split_ratio: f32,
    pub min_first: f32,
    pub min_second: f32,
    pub handle_size: f32,
    pub hit_size: f32,
}

impl SplitNodeModel {
    pub fn first_rect(&self) -> Rect;
    pub fn handle_rect(&self) -> Rect;
    pub fn second_rect(&self) -> Rect;
    pub fn clamp_ratio(&mut self);
    pub fn update_from_pointer(&mut self, point: Point);
}
```

Then later:

```rust
pub enum WorkspaceNode {
    Split(SplitNode),
    Tabs(TabGroupModel),
    Leaf(PanelLeaf),
}
```

## Input Ownership

`ui-core` should own the splitter interaction semantics:

- hit test
- hover
- drag state
- clamping
- redraw demand

The host/app should only provide:

- pointer events
- bounds
- stored model state

This matches the rest of the widget-framework direction.

## Paint Ownership

`ui-core` should paint:

- splitter handle
- tab strip chrome
- tab selection state

The app should paint:

- pane contents inside the rects it receives

This keeps the workspace system responsible for workspace behavior and chrome,
without forcing app-specific content into `ui-core`.

## Persistence Plan

Workspace state should be serializable by panel id.

Persist:

- split ratios
- selected tab per group
- collapsed state if later added

Do not persist:

- raw screen coordinates
- host/window handles

The persistent format should describe the logical workspace tree, not host
objects.

## What To Reuse From Existing Task UI

Behavior to reuse conceptually:

- ratio-based split state from [day_planner.rs](../task/src/day_planner.rs)
- minimum pane widths from [day_planner.rs](../task/src/day_planner.rs)
- drag capture lifecycle from [day_planner.rs](../task/src/day_planner.rs)
- adjust-bar cursor semantics from [task_list.rs](../task/src/task_list.rs)

Things not to port directly:

- `DayPlanSplitter`
- `Container`
- `BufferedWnd`
- Win32 message handlers

## Editor Migration Plan

For `sng_rusty_editor`, the migration path should be:

1. Replace manual top-level pane math with a split model.
2. Replace the left column's manual label/op split with a nested split model.
3. Keep pane contents as they are initially.
4. Add tab groups where the editor naturally needs them.
5. Only then revisit whether more dynamic workspace movement is necessary.

This means the next editor work should target the workspace foundation, not more
ad hoc pane math in the app.

## Non-Goals For The First Iteration

Do not build all of docking now.

Specifically out of scope for the first implementation:

- floating tool windows
- cross-window dragging
- auto-hide panes
- docking overlays/ghost previews
- arbitrary nested drag-drop rearrangement

Those are separate layers of complexity.

## Summary

The next step is not "just a splitter."

The next step is:

- a workspace layout foundation in `ui-core`
- with splitter resizing as the first implemented behavior
- designed so tabs and later docking fit naturally into the same tree

That is the path that avoids rework while still solving the editor's immediate
desktop layout needs.
