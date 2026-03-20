# Control Roadmap

## Purpose

This document tracks the intended completeness of `loadngo` interactive controls.

The goal is not just to satisfy the current editor. The goal is to reach a
stable, modern desktop UI foundation that covers the common control set expected
of application tooling.

This is the control-level counterpart to:

- [WIDGET_FRAMEWORK.md](/Users/jay/pudding/loadngo/docs/WIDGET_FRAMEWORK.md)
- [WORKSPACE_LAYOUT.md](/Users/jay/pudding/loadngo/docs/WORKSPACE_LAYOUT.md)
- [TEXT_INPUT_MODEL.md](/Users/jay/pudding/loadngo/docs/TEXT_INPUT_MODEL.md)

## Current `ui-core` Coverage

Already present in `ui-core`:

- button
  - [button.rs](/Users/jay/pudding/loadngo/ui-core/src/button.rs)
- checkbox
  - [checkbox.rs](/Users/jay/pudding/loadngo/ui-core/src/checkbox.rs)
- slider
  - [slider.rs](/Users/jay/pudding/loadngo/ui-core/src/slider.rs)
- stepper
  - [stepper.rs](/Users/jay/pudding/loadngo/ui-core/src/stepper.rs)
- list / selection state
  - [list.rs](/Users/jay/pudding/loadngo/ui-core/src/list.rs)
- combo/list combo
  - [combo.rs](/Users/jay/pudding/loadngo/ui-core/src/combo.rs)
- tree control / tree combo
  - [tree.rs](/Users/jay/pudding/loadngo/ui-core/src/tree.rs)
- tabs
  - [tabs.rs](/Users/jay/pudding/loadngo/ui-core/src/tabs.rs)
- static text
  - [label.rs](/Users/jay/pudding/loadngo/ui-core/src/label.rs)
  - [text_block.rs](/Users/jay/pudding/loadngo/ui-core/src/text_block.rs)
- images
  - [bitmap.rs](/Users/jay/pudding/loadngo/ui-core/src/bitmap.rs)

Also present as supporting composition:

- panel
- vertical stack
- scroll region / scroll container
- list row

## Missing Core Controls

The following controls are still missing or incomplete.

### Text Input

Still missing or incomplete:

- single-line editable text field
- clipboard semantics
- undo/redo
- IME/composition
- width-aware soft wrap
- spellcheck hooks

This is still the largest incomplete standard control family.

If `loadngo` is intended to support serious editor workflows, text input is
first-class and should not stay app-local.

Current foundation:

- `TextAreaModel` now exists in [text_area.rs](/Users/jay/pudding/loadngo/ui-core/src/text_area.rs)
- the dedicated desktop validation surface is
  [text_input_harness.rs](/Users/jay/pudding/loadngo/host-desktop/src/bin/text_input_harness.rs)
- the carried-forward design concepts from the old C++ editors are documented in
  [TEXT_INPUT_MODEL.md](/Users/jay/pudding/loadngo/docs/TEXT_INPUT_MODEL.md)
- current supported behavior:
  - authoritative multiline source buffer
  - piece-table-backed document storage
  - undo/redo and revision tracking
  - caret and selection model
  - focus and keyboard navigation
  - pointer placement and drag selection
- editor/document-layer architecture is documented in
  [TEXT_EDITOR_MODEL.md](/Users/jay/pudding/loadngo/docs/TEXT_EDITOR_MODEL.md)

### Radio Buttons / Grouped Exclusive Selection

Missing:

- radio button model
- radio group semantics
- keyboard navigation within a group

Checkboxes already exist, but exclusive single-choice groups are still absent.

### Spin Box

`StepperModel` exists, but a real spin box is still missing.

Desired control:

- numeric text field plus increment/decrement affordance
- optional min/max/step constraints
- keyboard editing support

This should likely compose:

- text input
- stepper behavior

### Combo / Dropdown Completeness

`ListCombo` exists, but the control family is not complete yet.

Future variants:

- non-editable dropdown
- editable combo box
- searchable combo

These should share a consistent popup/list interaction model.

### Scribble Pad

Missing:

- simple stroke capture area
- single-pen input
- recorded stroke output

Immediate goal:

- a bounded control that records a single stream of pen/mouse/touch points
- no paint program features yet
- enough for signatures, initials, simple markings

Later it may grow into a richer paint canvas, but that should be a separate
expansion, not the first version.

## Control Families And Expected Standards

The intended baseline is roughly "HTML form completeness for desktop app
tooling," plus a few editor-oriented extras.

Expected baseline families:

- buttons
- checkboxes
- radio buttons
- text input: single-line
- text input: multiline
- labels / static text
- sliders
- spin boxes / steppers
- dropdown / combo boxes
- list and tree selection controls
- tabs
- scroll containers

Editor-oriented extension:

- scribble pad
- richer canvas later

## Recommended Build Order

### Phase 1: Text Input Foundation

Build:

- `TextFieldModel`
- `TextAreaModel`

Why first:

- many later controls depend on text entry
- spin box and editable combo both build on it
- editors cannot stay credible without first-class text editing
- `sng_rusty_editor` is now explicitly blocked on multiline `.ron` editing, so
  `TextAreaModel` is the immediate next practical dependency

Immediate priority within Phase 1:

- build `TextAreaModel` first
- then add `TextFieldModel`

Reason:

- multiline source editing is required to make `sng_rusty_editor` a real
  authoring tool
- the `.ron`-first editor plan is documented in
  [RON_FIRST_EDITOR.md](/Users/jay/pudding/sng-rusty/docs/RON_FIRST_EDITOR.md)

Key requirements:

- caret model
- selection model
- focus behavior
- arrow/home/end navigation
- backspace/delete
- insertion/replacement
- clipboard hooks later
- desktop text-input harness with deterministic verification
- piece-table-backed document model for editor-grade surfaces
- incremental line index maintenance as the next performance step

### Phase 2: Radio And Group Semantics

Build:

- `RadioButtonModel`
- `RadioGroupModel` or equivalent grouped selection policy

Why:

- small surface area
- standard desktop semantics
- fills an obvious hole in the control set

### Phase 3: Proper Spin Box

Build:

- `SpinBoxModel`

Why:

- stepper logic already exists
- numeric editing is common in desktop tools

### Phase 4: Combo Family Expansion

Expand:

- readonly dropdown
- editable combo
- searchable combo

Why:

- reuse list + popup + text input once those foundations exist

### Phase 5: Scribble Pad

Build:

- `ScribblePadModel`

First version scope:

- single pen/brush
- stroke capture only
- clear/reset
- emit recorded stroke data

Out of scope initially:

- multiple layers
- shape tools
- eraser tools
- paint bucket
- arbitrary raster editing

## Scribble Pad Design Notes

The scribble pad should be treated as an input control first, not a full
graphics editor.

Suggested first model:

- bounds
- stroke width
- stroke color
- active pointer id
- current stroke points
- committed strokes

Required semantics:

- mouse, pen, and touch support through normalized pointer input
- one active stroke at a time for the first version
- redraw while drawing
- export captured stroke data in logical coordinates

That is enough for:

- signatures
- initials
- approvals
- simple mark-up

Later, if needed, a richer `CanvasModel` can exist beside it rather than forcing
all future paint features into the scribble control.

## Requirements For Future Implementations

Each new control should include:

- model-level tests
- input behavior tests
- paint contract tests
- keyboard behavior tests where relevant
- explicit ownership of focus semantics

For text entry and scribble input, tests should emphasize deterministic state
transitions rather than screenshot-only verification.

## Summary

The next major completeness milestone for `loadngo` controls is:

1. text input
2. radio/grouped exclusive selection
3. spin box
4. combo family expansion
5. scribble pad

This gets `loadngo` much closer to a real desktop application toolkit, not just
a small set of ad hoc editor widgets.
