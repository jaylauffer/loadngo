# Details View Model

## Purpose

This note records the shared scrollable details/inspector component that was
promoted out of `sng_rusty_editor` and into `loadngo/ui-core`.

It exists because the editor had drifted into custom one-off inspector scroll
logic, and the resulting bugs made it clear that this behavior belonged in a
shared widget model.

Related notes:

- [ARCHITECTURE.md](/Users/jay/pudding/loadngo/docs/ARCHITECTURE.md)
- [TEXT_EDITOR_MODEL.md](/Users/jay/pudding/loadngo/docs/TEXT_EDITOR_MODEL.md)
- [RON_FIRST_EDITOR.md](/Users/jay/pudding/sng-rusty/docs/RON_FIRST_EDITOR.md)
- [SCENE_BLOCKS_AND_VALVES.md](/Users/jay/pudding/sng-rusty/docs/SCENE_BLOCKS_AND_VALVES.md)

## Problem Statement

The `Scenes` and inspector panes in `sng_rusty_editor` were previously doing too
much custom work in the app layer:

- section stacking
- wrapped text measurement
- scroll-state persistence
- scrollbar thumb interaction
- clipping to viewport
- gutter reservation

That led to concrete bugs:

- text overflow into the scrollbar gutter
- text painting past the bottom of the panel
- scroll offset resetting across frames
- thumb dragging not working reliably

## Shared Component

The current shared solution is `DetailsViewModel` in
[details_view.rs](/Users/jay/pudding/loadngo/ui-core/src/details_view.rs).

It owns:

- persistent scroll offset
- thumb drag state
- padded content rect
- reserved scrollbar gutter
- section height aggregation
- section layout from scroll offset
- viewport-aware text clipping
- scrollbar painting and interaction
- text section wrapping and measurement through callbacks

It works with `DetailsSection` values rather than ad hoc editor-only tuples.

## Architectural Lesson

This promotion matters because it reinforces a broader `loadngo` rule:

- editor/runtime app code should assemble content
- shared widget code should own generic interaction and layout behavior

In this case:

- `sng_rusty_editor` still chooses what sections to show
- `loadngo/ui-core` now owns how a scrollable text details view behaves

## Current Boundary

Current boundary is intentionally narrow.

`DetailsViewModel` is for:

- structured titled text sections
- multiline wrapping
- scrollable inspection/detail surfaces

It is not yet:

- markdown
- rich inline spans
- a full document layout engine
- a mixed arbitrary-controls container

That narrow boundary is deliberate.

## Current Uses

The promoted details path is now used for editor-side panes that display:

- scene details
- validation details
- CAS details
- voiceover details
- dictation details
- selection/details panes

This keeps those panes behaviorally consistent and reduces custom glue in the
editor.

## Why This Matters For Scene Work

The scene-centric authoring direction in `sng-rusty` depends on shared details
surfaces behaving correctly.

The scene model is already richer now:

- intro kind
- presentation state
- exits
- beats
- pacing

That information is only useful if the inspector path can present it cleanly.

So the `DetailsViewModel` promotion is part of enabling scene-centric authoring,
not a side concern.

## Guidance For Future Work

When a future editor pane needs scrollable multiline details, prefer:

1. build `DetailsSection` values in the app layer
2. use `DetailsViewModel` for layout, clipping, and scroll behavior
3. only extend `DetailsViewModel` when behavior is genuinely reusable

Do not recreate one-off scrollable text stacks in app code unless there is a
very strong reason.
