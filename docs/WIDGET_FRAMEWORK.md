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
  - uses an editor-grade `TextDocument` backend for source editing
  - authoritative source buffer with caret and selection state
  - current source-editor path is desktop-first and newline-line-based
- `TextFieldModel` (added 2026-09-11)
  - single-line editable text, built *on* `TextAreaModel` (same document,
    caret, selection, undo) with events filtered on the way in
  - `Enter` emits `WidgetAction::Activate` instead of a newline; `Tab`/`Up`/
    `Down` are left unconsumed for the host; pasted line breaks are stripped

Current core dialogs:
- `FileDialogModel` (added 2026-09-11)
  - modal Open/Save dialog composed from `ButtonModel`, `TextFieldModel`, and
    `ScrollRegionModel`; no OS picker and no platform crate
  - folders first, file-type filter, hidden dot-files, places column, typed
    paths (absolute, relative, `~/`), Save appends the filter's extension and
    asks before replacing an existing file
  - reads the filesystem only through a `DirectorySource` trait, so every rule
    is unit-tested against an in-memory tree; `StdDirectorySource` is `std::fs`
  - **routes keys to the focused control only.** `ButtonModel` activates on
    `Enter` whether or not it is focused, so a composite that broadcasts key
    events to every child fires all of its buttons at once
  - the host must send it *all* input while open: typed letters also arrive as
    `HostKey` events, so app shortcuts would otherwise fire while typing a name

Host input adapter (added 2026-09-11): `InputSnapshot::ui_events()` turns a
frame's mouse, keys, and typed text into `UiEvent`s in delivery order, and
`HostKey::ui_key()` maps one key. Wheel input is deliberately excluded --
`ScrollLines` can't express `mouse_wheel_precise` pixel deltas -- so pass
`mouse_wheel_y`/`mouse_wheel_precise` to the widget directly. **Wheel sign:**
`ui-core` widgets treat a positive `mouse_wheel_y` as scrolling toward the top
(`offset -= wheel_y`), matching `TextAreaModel` and `sng-rusty`; `sng-roguelite`'s
achievements list uses the opposite sign and should be checked by hand.

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
- split/tree/tab direction is documented in [WORKSPACE_LAYOUT.md](WORKSPACE_LAYOUT.md)
- the intended path is:
  - splitter resizing first
  - tab groups as first-class layout nodes
  - docking later, built on the same workspace tree

Control completeness roadmap:
- control-family coverage and missing standard widgets are documented in [CONTROL_ROADMAP.md](CONTROL_ROADMAP.md)
- text-input model details and prior-art review are documented in [TEXT_INPUT_MODEL.md](TEXT_INPUT_MODEL.md)
- editor/document-layer architecture is documented in [TEXT_EDITOR_MODEL.md](TEXT_EDITOR_MODEL.md)
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

File dialog verification harness:
- `cargo run --manifest-path /Users/jay/pudding/loadngo/Cargo.toml -p loadngo-host-desktop --bin file_dialog_harness` (add `-- --save` for Save mode)
- a real-filesystem Open or Save dialog over a dimmed scene; prints each outcome
- verified on macOS 2026-09-11 with real keyboard input: arrow/Enter navigation
  into a folder, and Save-mode typing replacing the selected stem

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
- process lessons and debugging guidance are documented in [TEXT_RENDERING_TROUBLESHOOTING.md](TEXT_RENDERING_TROUBLESHOOTING.md)

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

## Focus Navigation (d-pad / thumbstick)

Added 2026-09-07, after real gamepad playtesting showed every game
hand-rolling menu navigation per screen.

`ui_core::FocusRing` (`ui-core/src/focus_ring.rs`) owns *which* of a set of
widgets holds focus and moves it in response to a `NavDirection`. It
deliberately changes no widget: the pieces it needs already existed.

- Every focusable model (`ButtonModel`, `SliderModel`, `CheckboxModel`,
  `StepperModel`, `TextAreaModel`) already has `focused: bool` driven by
  `UiEvent::FocusChanged(bool)`, and already paints that state.
- `ButtonModel` already activates on `Key::Enter | Key::Space`.
- `SliderModel` already consumes `Key::Left`/`Key::Right` to adjust its
  value, reporting `input_consumed: true`.

So the host loop is:

1. Refresh the ring's entries from this frame's widget rects.
2. Offer the direction to the focused widget **first**.
3. If the response has `input_consumed`, the widget used it (a slider
   adjusting its value) -- leave focus alone.
4. Otherwise `FocusRing::navigate` moves focus; emit `FocusChanged(false)`
   to the widget that lost it and `FocusChanged(true)` to the one that
   gained it.
5. Confirm by forwarding `Key::Enter` to the focused widget and reading the
   `WidgetAction` back, so activation follows the same contract a mouse
   click does.

This is why `input_consumed` matters beyond "don't bleed into runtime
actions": it is what lets one navigation loop drive mixed widget types
without knowing which is which. Up/down walking a settings screen while
left/right adjusts whichever slider is focused falls out of the existing
contract with no per-widget special-casing in the host.

### Hover and focus are the same question, answered per input device

A screen can have a mouse hovering one control and a gamepad focused on
another. Drawing both leaves the player guessing which one will fire, so
callers should **arbitrate on `loadngo_touch::InputMethod`** — show hover
when the player is on keyboard/mouse, focus when they're on a gamepad —
rather than rendering whichever happens to be set. `sng-roguelite`'s
`RunSummaryButtonState::highlighted_id` is the reference shape: one
`Option<WidgetId>`, resolved on the input side, handed to the renderer.

### A single-control screen shows no focus indicator

`FocusRing::should_indicate_focus()` is false for rings with fewer than two
entries, and `indicated_id()` returns `None` for them. A focus highlight
exists to answer "which of these will I get?", and a dismissable popup
whose only control is Close has no such question — highlighting it is
noise. Renderers should read `indicated_id()`, not `focused_id()`.

Navigation is **spatial**, computed from entry rects (nearest neighbour
along the travel axis, off-axis drift as tie-breaker), not declaration
order -- so a row or grid behaves the way it looks, and relayout preserves
focus by widget id. The ring starts **disarmed**: a screen must observe its
controls released before acting, so a button still held from the previous
screen cannot confirm the instant a new screen appears.

Translating hardware into a `NavDirection` is one layer up, in
`loadngo_touch::NavRepeat` (`touch/src/nav_input.rs`), because `ui-core`
sits below `host-core` and cannot see a `GamepadSnapshot`. `NavRepeat`
covers both the d-pad and the left stick and owns hold-to-repeat timing
(immediate first step, then a delay, then a repeat interval) -- without
which a held stick would step focus once per frame.

First adopter is `sng-roguelite` (reward draft, run summary, achievements,
sound settings). `sng-rusty`'s `LoadngoButtonHost` is the natural next
adopter, since it already manages button collections and calls `paint()`.

## Known Gaps

- ~~**`sng-roguelite`'s mouse adapter never sends `UiEvent::PointerMoved`.**~~
  **Fixed 2026-09-07.** `button_activated_this_frame` now sends
  `PointerMoved` every frame, the same way `slider_changed_this_frame`
  always did, so `ButtonModel::hover` is finally live for desktop mouse
  users rather than dead code. Found originally 2026-09-06 via
  `sng-bass-blaster`; fixed at the source when the same screens gained
  gamepad focus and needed hover and focus to coexist.

- ~~**Discrete scroll input moved the view in whole jumps.**~~ **Fixed
  2026-09-08.** `ScrollRegionModel` now eases toward a target offset
  (`glide_scroll_delta` + `advance`), so a detented wheel notch, arrow key,
  or gamepad step reads as motion rather than a teleport. Reported on Linux,
  where X11 delivers whole notches; macOS hid it because trackpads report
  pixel deltas that were already continuous. Input that is *already*
  continuous — touch drags, scrollbar-thumb drags, precise pixel deltas —
  still applies instantly via `apply_scroll_delta`, since gliding those
  would feel like lag under the finger. The glide is framerate-independent
  and deltas accumulate, so spinning a wheel fast travels further instead of
  restarting. **Callers must call `advance` every frame, not only on frames
  with input**, or a glide stalls the moment the wheel stops turning.

- **No theming/skin system.** Every widget's `paint()` hardcodes its colors
  (see `ui-core/src/button.rs`'s fill/border literals), and the button style
  is a light fill with dark text. That is why `sng-roguelite` still paints
  its own dark `RenderOp`s and adopts these widgets for logic only: adopting
  `ButtonModel::paint()` today would drop a light button into a dark game.
  A palette/theme threaded through `paint()` is the real enabler for games
  converging on shared widget visuals, and is deliberately *not* part of the
  focus-navigation work above. Until it exists, a game can still show focus
  by reading `focused` and painting its own highlight, which is what
  `sng-roguelite` does (`push_focusable_button_frame`).

## Current Direction

The current migration path is:
1. move Global Menu interaction onto `loadngo` widget hosts
2. move submenu Back/SaveLoad/Sound/Input controls onto the same model
3. let `sng-rusty` consume only semantic widget actions
4. remove runtime-local click bookkeeping that duplicates widget behavior
