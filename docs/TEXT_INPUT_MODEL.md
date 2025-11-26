# Text Input Model

## Purpose

This document records the intended foundation for `loadngo` text editing.

It exists so future agents do not treat text input as a grab-bag of ad hoc
editor fixes. The goal is a reusable control model that supports both:

- general desktop text controls
- the `.ron`-first `sng_rusty_editor` plan

## Prior Art Reviewed

The old C++ samples in `/Users/jay/pudding/loadngo-cpp` were reviewed before
starting the Rust `TextAreaModel`.

Most relevant:

- `Essay/TextWindow`
- `Outline/Outline/TextWindow`

Useful but secondary:

- `Text/TextWindow`
- `Text/TextEngine`
- `Xml/Xml/XMLWnd`

## Concepts Carried Forward

From the C++ text windows, the important generic ideas are:

- the authoritative source is the raw text buffer
- caret and selection are source ranges, not paint-layer artifacts
- visual lines are cached separately from source text
- point-to-source hit testing is based on the cached line layout
- up/down movement should preserve a preferred visual x position
- rendering and layout are separate passes

From `Xml/XMLWnd`, the main reusable idea is not text editing itself but the
editor workflow around it:

- source view and structure view should coexist
- split panes and tabs should organize those views
- source selection and derived structure should synchronize

## First Rust Scope

The first Rust `TextAreaModel` intentionally keeps scope narrow:

- multiline editing
- authoritative `String` buffer
- caret as source char index
- selection as source char range
- cached visual lines
- click to place caret
- drag selection
- keyboard navigation:
  - left/right/up/down
  - home/end
  - backspace/delete
  - enter/tab
  - select-all
- vertical scroll offset
- paint output for:
  - background
  - border
  - selection fill
  - text lines
  - caret

This is enough for a real source-editing surface and for the dedicated harness.

## Current Non-Goals

These are explicitly deferred:

- IME/composition
- clipboard integration
- undo/redo history
- spellcheck
- width-aware soft wrap
- syntax coloring
- search/replace
- code folding

Those are important later, but they should build on a stable base model rather
than being mixed into the first control pass.

## Current Layout Model

The current `TextAreaModel` caches line layout from explicit newline boundaries.

That means:

- each source line becomes one visual line
- the model already owns line caching and point hit testing
- soft wrap can be added later without changing the higher-level editing model

This is a deliberate step toward the richer `Essay`/`Outline` model without
blocking the first working control on width-aware wrapping.

## Input Contract

Text editing needs richer host input than button-style widgets.

The current shared contract therefore includes:

- per-frame typed text
- key events with modifiers
- pointer position and button transitions
- wheel deltas

That input path is now exercised by:

- `ui-core/src/text_area.rs`
- `host-desktop/src/bin/text_input_harness.rs`

## Harness

Use the dedicated harness instead of overloading the renderer text harness:

```bash
cargo run --manifest-path /Users/jay/pudding/loadngo/Cargo.toml -p loadngo-host-desktop --bin text_input_harness
```

Purpose:

- verify desktop text entry behavior in isolation
- test pointer placement and selection without `sng_rusty_editor`
- validate shared host input plumbing before editor integration

## Expected Next Steps

1. add `TextFieldModel`
2. add clipboard hooks
3. add undo/redo model
4. add width-aware soft wrap
5. integrate `TextAreaModel` into `sng_rusty_editor` as the authoritative
   `.ron` source view
