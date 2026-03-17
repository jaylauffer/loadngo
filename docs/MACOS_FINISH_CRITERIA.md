# macOS Finish Criteria

This document defines when the macOS path is finished enough to stop consuming
engineering time and shift focus back to Android.

## Goal

`loadngo` should fully own the active macOS host and rendering path for
`sng-rusty`.

That means:
- no polling sleep loop
- no Macroquad/miniquad dependency in the active macOS path
- no runtime-local widget semantics for the primary overlay flow
- stable Metal rendering for the current VN command set

## Required Conditions

### 1. Host Scheduling

macOS is finished only when:
- host scheduling is proactor-driven
- there is no fixed sleep/tick loop in the active host path
- static VN screens can idle without continuous synthetic frame scheduling
- animated states still request paced frames when needed

Current status:
- proactor-driven host wakeups are in place
- fixed sleep loop is removed
- runtime still computes frame demand in `sng-rusty`

### 2. Rendering Ownership

macOS is finished only when:
- `loadngo-host-desktop` owns the window/event path
- `loadngo-gfx-metal` owns the active render backend
- the current `sng-rusty` render command set used on macOS is handled by
  `loadngo`
- `macroquad` and `miniquad` are absent from the macOS dependency path

### 3. Widget Ownership

macOS is finished only when the primary overlay path is owned by `loadngo`
widget hosts instead of ad hoc runtime-local button logic.

Minimum required overlay coverage:
- Global Menu buttons
- submenu `Back` buttons
- overlay input consumption must prevent story/input bleed-through

Preferred next coverage before declaring macOS complete:
- Save/Load action buttons
- Sound submenu controls
- Input submenu controls

This is the boundary where `sng-rusty` should only provide UI state and consume
semantic actions.

### 4. Manual Acceptance

The macOS path is finished only when these manual checks pass consistently:
- app launches with the native `loadngo` host
- Metal initializes and renders real frames
- opening Global Menu works reliably
- navigating to Sound/Save/Input submenus works reliably
- `Back` returns to Global Menu without closing everything or advancing story
- outside click dismissal works as intended
- resize keeps content and UI stable
- background/image/text rendering remain correct through normal VN progression

## Explicit Non-Goals For macOS Finish

These should not block the macOS finish decision:
- Android touch layout quality
- Android renderer performance
- Android lifecycle/audio bring-up
- Windows host completion

## Current Remaining macOS Work

At the time of writing, the remaining architectural items are:
- move more submenu controls onto `loadngo` widget hosts
- move redraw demand ownership upward from runtime-local code toward widget
  response aggregation
- reduce the remaining runtime-local overlay assembly in `sng-rusty`

When the required conditions and manual acceptance list are satisfied, macOS is
finished enough to freeze except for regressions and return focus to Android.
