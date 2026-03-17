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
- composition-host tests
  - model-to-widget mapping
  - action aggregation
  - input consumption aggregation
  - redraw aggregation
  - fractional-rect hit testing
  - half-open edge behavior
  - desktop and touch interaction paths
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
