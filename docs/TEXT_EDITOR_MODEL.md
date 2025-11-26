# Text Editor Model

## Purpose

This document defines the intended boundary between:

- generic text input controls
- editor-grade text document behavior

`loadngo` needs both.

## Core Separation

The important split is:

- `TextAreaModel`
  - the UI control
  - owns bounds, selection, caret, scrollbars, paint, and input routing
- `TextDocument`
  - the editor-grade document model
  - owns text storage, revision tracking, distributed edits, and undo/redo

This avoids conflating:

- "multiline text can be entered"
- "the editor has a scalable source buffer model"

Those are related, but they are not the same subsystem.

## Current Direction

Current source-editor direction:

- `TextDocument` is piece-table-backed
- `TextAreaModel` uses that document model
- source-editor dirty tracking should use document revision, not whole-text equality
- undo/redo should live in the document model, not in widget-local ad hoc stacks

That is the minimum viable foundation for:

- direct `.ron` editing
- distributed edits
- later undo/redo batching

## Why Piece Table

The current preferred long-term direction is a piece-table family document model.

Reasons:

- distributed edits are cheap
- undo/redo is natural
- original file content can stay immutable
- append-only add buffer works well with desktop editor workflows
- this is closer to modern editor architecture than a plain mutable `String`

This does not prevent later use of:

- memory-mapped original buffers
- tree indexing over pieces
- more advanced span/line indexes

## What A Basic Multiline Control Needs

A basic multiline control should provide:

- caret
- selection
- insertion/deletion/replacement
- focus behavior
- pointer placement/drag selection
- vertical and horizontal scrolling
- clipboard hooks

It does not need to own the full editor feature set.

## What An Editor-Grade Text Surface Needs

An editor-grade text surface needs additional structure:

- scalable document storage
- undo/redo
- revision tracking
- incremental line index maintenance
- visible-range layout
- source span mapping
- later:
  - gutters
  - syntax highlighting
  - diagnostics
  - search
  - command system

That is why the document model must be explicit.

## Intended End State

The intended final architecture is:

1. `TextDocument`
   - piece-table-backed
   - undo/redo
   - revisioned
   - incremental line index support
2. `TextAreaModel`
   - generic editable text surface
   - can host any `TextDocument`
3. higher-level editor surface
   - source editor shell
   - line numbers/gutter
   - diagnostics
   - jump/navigation commands
   - source/preview/structure coordination

In other words:

- `TextAreaModel` should remain reusable
- editor behavior should grow around it, not be baked into every multiline text field

## Performance Roadmap

### Phase 1

- piece-table storage
- undo/redo
- revision tracking
- visible-slice layout

### Phase 2

- incremental line-start maintenance after edits
- stop rebuilding whole-document line indexes on every change

### Phase 3

- visible-range remeasurement from first affected line
- keep offscreen line metrics stable unless invalidated

### Phase 4

- editor affordances
  - line-number gutter
  - source spans
  - diagnostics decorations

### Phase 5

- richer command/editor features
  - search
  - jump history
  - code-like navigation
  - undo coalescing / transaction grouping

## Guidance For Future Work

- do not put editor-only policy into low-level widget paint math
- do not use whole-text equality to detect source-editor dirty state
- do not regress back to a plain mutable `String` as the primary editor buffer
- keep deterministic tests near the document model
- validate text-input behavior in `text_input_harness`
- validate source-editor behavior in `sng_rusty_editor`
- add and prefer a dedicated editor harness for source-specific behavior
  - line numbers/gutter
  - large-file load
  - undo/redo
  - vertical navigation across empty lines
  - line merge/split edits
  - clipboard shortcuts
  - save/dirty workflow stubs

## Immediate Next Improvements

Near-term work should stay focused on turning the current source surface into a
real editor, not a richer demo control.

Priority order:

1. source-editor workflow integration
   - parse-on-debounce from the authoritative source buffer
   - keep the raw buffer editable on parse failure
   - refresh derived views from the latest successful parse
2. editor coordination
   - source-to-structure navigation
   - source-driven preview selection
   - diagnostics mapped back to source spans
3. editor affordances
   - current-line highlight
   - gutter polish
   - keyboard shortcuts and desktop menu integration
   - dedicated `text_editor_harness` on top of `TextAreaModel` + `TextDocument`
4. text-system completeness
   - single-line `TextFieldModel`
   - IME/composition
   - clipboard parity across desktop backends
   - inactive-window caret policy
5. scaling
   - incremental remeasurement from the first affected line
   - viewport-only decorations
   - line-level diagnostics/highlighting without full relayout

Process guidance:

- when text layout or placement changes, validate first in the dedicated
  harnesses, then in `sng-rusty` and `sng_rusty_editor`
- use explicit overlays and stdout metrics in harnesses before drawing visual
  conclusions from screenshots
- keep one live process per target while debugging so observations stay tied to
  the current build
