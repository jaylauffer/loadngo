# Widget Framework

`loadngo` owns widget behavior.

That means:
- layout policy
- interaction semantics
- input consumption
- redraw demand
- paint generation

It does not mean the visual novel runtime owns per-widget mouse and touch rules.

## Ownership Split

`sng-rusty` should own:
- visual novel script execution
- scene/dialogue/menu state as data
- music and voice policy
- save/load policy

`loadngo` should own:
- buttons, menus, sliders, lists, tabs
- scroll regions and scroll indicators
- desktop vs touch interaction behavior
- widget paint output
- widget input consumption
- widget redraw demand

## Coordinate Model

Widget and layout space should use logical coordinates, not integer pixel space.

Current policy:
- widget geometry uses `f32`
- pointer positions use `f32`
- paint/layout bounds use `f32`
- backend rasterization may quantize if needed at the final execution boundary

This keeps:
- DPI scaling sane
- host/runtime geometry consistent
- hit-testing and layout free from premature snapping

Integer snapping belongs at the backend/raster edge, not in the widget model.

## Text Contract

Widget text is part of the framework contract, not ad hoc caller math.

Current shared text style semantics:
- horizontal alignment: `Left`, `Center`, `Right`
- vertical alignment: `Top`, `Middle`, `Bottom`
- layout mode: `SingleLine`, `MultiLine`
- single-line overflow: `Clip`, `EllipsisEnd`, `EllipsisMiddle`

Rules:
- alignment is defined against the displayed text box that users actually see, not ad hoc caller offsets
- the shared single-line line-box contract is `ui_core::single_line_text_box_height(font_size)`
- multiline vertical progression should use `ui_core::multiline_line_step(font_size)` until explicit line spacing becomes part of `TextStyle`
- `SingleLine` text must resolve overflow deterministically before rasterization
- `MultiLine` text may contain explicit newlines and should report a logical height
  based on all rendered lines
- widget callers should not fake vertical centering by hardcoded pixel offsets once
  alignment exists in the shared text contract
- widget callers still need to allocate a sane line box; packing 18pt text into an 18px-tall panel is a layout bug, not a renderer feature
- current desktop backend behavior:
  - layout still reserves the shared single-line line box
  - final `Top` / `Middle` / `Bottom` placement is resolved against the displayed opaque text bounds so clip rects do not shave glyph tops

Implementation guidance:
- `LabelModel` should emit a single-line text rect using the shared line-box contract rather than ad hoc caller math
- list rows should reserve a shared single-line text box inside row chrome instead of deriving text height from arbitrary per-panel padding
- platform backends must preserve the caller's reserved line box, but place the rendered image using the displayed text bounds so the final result stays visually aligned inside clipped widgets

Recommended usage:
- buttons, tab captions, compact value fields:
  - `SingleLine`
  - `Center`/`Middle`
- labels and inspector-style fields:
  - `SingleLine`
  - `Left` with explicit `Top`/`Middle` depending on the widget
- text blocks:
  - `MultiLine`
  - usually `Left`/`Top`

Current core text widgets:
- `LabelModel`
  - single-line or compact text inside a bounded rect
- `TextBlockModel`
  - multiline static text inside a bounded rect
  - callers may pre-wrap by inserting `\n` until shared width-aware wrapping exists
- `TextAreaModel`
  - multiline editable text surface
  - authoritative source buffer with caret and selection state
  - first phase uses explicit newline-based visual lines

Current core composition widgets:
- `PanelModel`
  - panel chrome plus content bounds
- `VerticalStackModel`
  - vertical child slot layout with padding and gap
- `ScrollContainerModel`
  - padded scroll viewport plus shared scrollbar indicator
- `SplitNodeModel`
  - ratio-based split layout with min-size clamping and draggable handle state
- `TabGroupModel`
  - tab strip plus shared content rect for the selected page
- `ListRowModel`
  - reusable row chrome and content-slot layout for richer list items
- `WorkspaceNode`
  - recursive split/tab/leaf layout tree for desktop workspaces

Workspace layout roadmap:
- split/tree/tab direction is documented in [WORKSPACE_LAYOUT.md](/Users/jay/pudding/loadngo/docs/WORKSPACE_LAYOUT.md)
- the intended path is:
  - splitter resizing first
  - tab groups as first-class layout nodes
  - docking later, built on the same workspace tree

Control completeness roadmap:
- control-family coverage and missing standard widgets are documented in [CONTROL_ROADMAP.md](/Users/jay/pudding/loadngo/docs/CONTROL_ROADMAP.md)
- text-input model details and prior-art review are documented in [TEXT_INPUT_MODEL.md](/Users/jay/pudding/loadngo/docs/TEXT_INPUT_MODEL.md)
- especially important missing families:
  - text input
  - radio/grouped exclusive selection
  - spin box
  - richer combo variants
  - scribble pad

Desktop verification harness:
- `cargo run --manifest-path /Users/jay/pudding/loadngo/Cargo.toml -p loadngo-host-desktop --bin text_harness`
- this renders one desktop window with:
  - direct `RenderOp::Text` samples
  - centered `ButtonModel` samples
  - fixed-height `ListRowModel` + `LabelModel` samples
  - a `TextBlockModel` multiline sample
- use it before changing shared desktop text placement so runtime/editor regressions are caught in one place

Workspace verification harness:
- `cargo run --manifest-path /Users/jay/pudding/loadngo/Cargo.toml -p loadngo-host-desktop --bin workspace_harness`
- this renders one desktop window with:
  - nested split handles
  - tab groups
  - visible selected leaf panes
  - app-owned content inside workspace-managed layout rects
- use it before changing split/tree/tab workspace behavior so editor-shell regressions are caught outside `sng_rusty_editor`

Text-input verification harness:
- `cargo run --manifest-path /Users/jay/pudding/loadngo/Cargo.toml -p loadngo-host-desktop --bin text_input_harness`
- this renders one desktop window with:
  - a live `TextAreaModel`
  - keyboard entry, caret movement, selection, and scroll behavior
  - a focused validation surface for host text-input plumbing
- use it before moving multiline source editing into `sng_rusty_editor`

Text-metrics verification harness:
- `cargo run --manifest-path /Users/jay/pudding/loadngo/Cargo.toml -p loadngo-host-desktop --bin text_metrics_harness`
- this renders one desktop window with:
  - side-by-side `LogicalLineBox` vs `VisibleInk` single-line samples
  - the strings `123`, `...`, `ooo`, `Ops(`, `gggg`, `T`, `MMMMM`, and `WWWWW`
  - a three-tab comparison row so sibling control alignment is obvious
- use it when changing baseline, line-box, or vertical centering behavior in the shared renderer
- process lessons and debugging guidance are documented in [TEXT_RENDERING_TROUBLESHOOTING.md](/Users/jay/pudding/loadngo/docs/TEXT_RENDERING_TROUBLESHOOTING.md)

This is the boundary that desktop backends must preserve so editor and runtime UI
do not drift into separate text-layout worlds.

In practical terms:
- `sng-rusty` builds models such as `RuntimeGlobalMenuModel`
- `loadngo`-owned hosts/composition layers turn those models into:
  - widget trees
  - paint ops
  - semantic actions
  - input-consumed / redraw results

`sng-rusty` should not be manually reproducing button press/release logic once a
widget host exists for that surface.

## WidgetResponse

Widgets communicate through `ui_core::WidgetResponse`.

Current fields:
- `request_redraw`
- `request_focus`
- `input_consumed`
- `action`

Contract:
- `request_redraw=true` means visible widget state changed
- `input_consumed=true` means the event must not bleed into higher-level runtime actions
- `action=Some(...)` means the widget emitted a semantic action

Discrete widgets such as buttons can usually express their semantic result as an
action.

Continuous widgets such as sliders are different:
- the widget still owns interaction semantics and paint generation
- the composition host reports value changes upward
- the runtime should consume those value changes as data, not fake them as button actions

Scroll regions follow the same principle:
- the widget owns viewport/content/offset math
- the runtime supplies scroll deltas and content extent
- the widget reports clamped state and paint ops for indicators
- the runtime should not reimplement scrollbar math per panel

Examples:
- button press inside bounds:
  - consumes input
  - requests redraw
- button release inside bounds:
  - consumes input
  - requests redraw
  - emits activation
- button release outside after a captured press:
  - consumes input
  - requests redraw
  - emits no activation
- slider drag:
  - consumes input
  - requests redraw
  - updates the widget-owned value continuously
  - reports changed values upward through the composition host

## Redraw Policy

Widgets do not schedule frames directly.

They report redraw demand.

The runtime or host should convert that into `FrameDemand`:
- static unchanged UI: `Idle`
- animated or time-based UI: `After(duration)`

This keeps rendering demand-driven without pushing platform scheduling into widget code.

Composition layers should aggregate widget responses upward instead of hiding
them. That means an overlay host should report:
- semantic actions
- whether any widget consumed the event
- whether any widget requested redraw

That aggregate response is the contract the runtime should use to:
- suppress story advancement on consumed input
- request another frame only when something visible changed

## Platform Semantics

Desktop and touch are not interchangeable.

Desktop typically wants:
- click-release activation
- pointer hover
- focus semantics

Touch typically wants:
- direct press feedback
- larger hit targets
- outside-dismiss overlays
- minimal move/hover dependency

Those differences should live in `loadngo` composition layers such as `loadngo-touch`, not in `sng-rusty`.

The same applies to overlay dismissal and back-navigation semantics:
- desktop overlays should respect click-release behavior
- touch overlays should support outside-dismiss where appropriate
- submenu transitions must consume the opening/closing gesture so it cannot leak
  into the underlying story/runtime input path

Those are widget-framework rules, not VN runtime rules.

## Test Strategy

The minimum useful test coverage is:
- widget unit tests
  - press/release activation
  - release-outside cancellation
  - hover/focus redraw behavior
  - paint contract
  - text alignment and overflow contract
- composition-host tests
  - model-to-widget mapping
  - action aggregation
  - input consumption aggregation
  - redraw aggregation
  - fractional-rect hit testing
  - half-open edge behavior
  - desktop and touch interaction paths
  - deterministic text placement for top/middle/bottom alignment
  - deterministic single-line overflow behavior
- runtime/editor validation
  - confirm `sng-rusty` runtime button labels stay vertically centered
  - confirm `sng_rusty_editor` headers, list rows, and inspector labels are not clipped
- runtime model tests
  - widget-key/action round-trips
  - menu/button model contents

The goal is to prove widget behavior at the `loadngo` boundary so runtime bugs do
not have to be debugged indirectly through end-to-end UI behavior.

## Current Direction

The current migration path is:
1. move Global Menu interaction onto `loadngo` widget hosts
2. move submenu Back/SaveLoad/Sound/Input controls onto the same model
3. let `sng-rusty` consume only semantic widget actions
4. remove runtime-local click bookkeeping that duplicates widget behavior
