# Desktop Platform Roadmap

## Status and purpose

Status: **roadmap only, nothing here is scheduled or decided**. Written
2026-08-30 after playtesting `sng-roguelite` on `dolores` (Raspberry Pi,
keyboard+mouse attached for the first time) and on macOS in the same
session. Three real gaps surfaced. This document records what was found
and lays out candidate priority/sequencing for a future dedicated design
conversation — it is deliberately not an implementation plan.

This is `loadngo`-scoped, not `sng-roguelite`-scoped, because two of the
three findings are platform/engine concerns that affect every game built
on `loadngo`, not this one game. See each finding for the one item that
is actually `sng-roguelite`-specific.

## Finding 1: Linux X11 present-latency growth (blocks a Linux release)

Already fully documented, unresolved, with next steps already scoped:
[LINUX_X11_PRESENT_LATENCY.md](LINUX_X11_PRESENT_LATENCY.md). Restated
here only for roadmap visibility: `present()`'s software/X11 path grows
from ~13ms to ~150-195ms over a run's first 6 seconds, independent of
input, on `dolores`'s `labwc`/XWayland/v3d stack. Leading hypothesis is
something in the Xwayland/compositor/GPU-driver chain outside our own
code, not confirmed.

**This blocks putting a Linux build on itch.io** — confirmed directly by
playtesting on `dolores` today; not a theoretical concern. A Linux
release should not happen until this is at least characterized well
enough to know whether it's fixable, worked around, or specific to this
one Pi/compositor combination (see that doc's "Suggested next steps" for
what a follow-up investigation session should try first — GLES backend
instead of the software path, and testing without XWayland/labwc in the
loop, would meaningfully narrow this).

## Finding 2: desktop mouse clicks don't reach several `sng-roguelite` screens

This is a `sng-roguelite`-specific adoption gap, not a missing `loadngo`
capability — confirmed by reading both sides directly:

- `loadngo/ui-core/src/button.rs`'s `ButtonModel` already fully supports
  mouse (`PointerPressed`/`PointerReleased`/`PointerMoved` with
  hover/press/focus state) **and** keyboard (`Enter`/`Space` activation),
  with real behavioral tests covering both paths. This already is the
  "basic UI framework that supports keyboard and mouse" — it exists and
  works.
- `sng-roguelite/crates/game-app/src/lib.rs`'s achievements-screen close
  button and end-run-summary screen buttons (`achievements_close_requested`,
  `run_summary_touch_action`) don't use it. They hand-roll their own
  rect-hit-testing against `input.touches` only (`TouchPhase::Started`) —
  a real mouse click never populates that field, so these specific
  interactive elements are structurally unreachable by mouse, regardless
  of platform. Keyboard partly works today because these same functions
  separately check for `Space`/`R`/`T` key presses alongside the
  touch-only hit test — mouse has no equivalent fallback.
- Scroll-wheel input on the achievements screen already works because
  scrolling is handled through a different, non-touch-only path — this is
  why the user saw scrolling work but clicking fail in the same session.

**This is a fix-in-place task, not new infrastructure**: migrate these
specific screens' interactive elements onto `loadngo_ui_core::Button`
(or extend the existing touch-hit-test functions to also accept a mouse
click at the same screen position), rather than building anything new.
Worth checking whether `sng-rusty` has the same touch-only pattern
anywhere before treating this as `sng-roguelite`-only — not yet checked.

## Finding 3: no physical gamepad/controller abstraction exists in `loadngo`

Confirmed via direct search — the only joystick-related code anywhere in
`loadngo` is `touch/src/joystick.rs`, an on-screen **virtual** joystick
for touch input. There is no physical controller/gamepad input path at
all: no `gilrs`-style cross-platform gamepad polling, no per-platform
`GameController`/`XInput`/`evdev` backend, nothing in `host-desktop`'s
input layer that recognizes a connected DualShock/DualSense or any other
pad. This is a genuinely new capability, not a gap in an existing system
— unlike Finding 2.

Real design work this needs before any implementation, none of it
resolved here:

- Where the abstraction lives (a new `loadngo` crate vs. folding into
  `host-desktop`'s existing input layer).
- Which platforms get real backends first, and what each one costs:
  Linux (`evdev`/`udev` or a crate like `gilrs`), macOS (`GameController`
  framework), Windows (`XInput`/`DirectInput`), mobile (Android's
  `InputDevice` gamepad APIs, iOS's `GameController` framework — both
  exist but are separate work from desktop).
- Whether to hand-roll per-platform backends (consistent with this
  project's stated preference, seen in Android/iOS packaging, for owning
  platform integration directly rather than delegating to a black-box
  crate) or accept a cross-platform crate dependency for this specific
  subsystem — an explicit tradeoff to weigh, not a default.
- How button/axis mapping surfaces to a game: a raw per-controller-model
  event stream, or a normalized abstraction (face buttons, sticks,
  triggers) games consume uniformly regardless of pad — probably the
  latter, given every other `loadngo` input surface (`UiEvent`, touch)
  already normalizes across raw platform input, but not decided.

## Proposed priority ordering (open for discussion, not decided)

1. **Finding 1 (Linux latency)** first, and only if a Linux release stays
   a real near-term goal — it's a hard blocker for that specific goal,
   but affects nothing else (Android/iOS/macOS desktop are unaffected).
2. **Finding 2 (button adoption)** is small, low-risk, and independently
   valuable for anyone playing on a desktop with a mouse regardless of
   platform or the Linux question — no reason to wait on anything else.
3. **Finding 3 (gamepad abstraction)** is the largest undertaking of the
   three and stands alone; sequencing it relative to the other two is a
   capacity/priority call, not a technical dependency — nothing here
   blocks or is blocked by Findings 1 or 2.

## Explicitly not decided yet

- Whether Finding 1 turns out to be fixable in this repo at all, versus
  an upstream `labwc`/`wlroots`/Mesa `v3d` issue to work around or route
  around (e.g. defaulting the GLES backend on Linux instead of software).
- Any technical approach for Finding 3 (crate choice, abstraction shape,
  platform rollout order).
- Timing/scheduling for any of this relative to other `loadngo`/
  `sng-roguelite` work.

## Related docs

- [LINUX_X11_PRESENT_LATENCY.md](LINUX_X11_PRESENT_LATENCY.md) — full
  Finding 1 detail, instrumentation, and reproduction steps.
- [CONTROL_ROADMAP.md](CONTROL_ROADMAP.md) — the sibling roadmap for
  `ui-core`'s editor/desktop-tooling widget completeness (text input,
  radio buttons, spin boxes, combos, scribble pad); different scope from
  this document — that one is about which controls *exist*, this one is
  partly about which controls *games actually use* (Finding 2) plus two
  platform-level gaps.
- `sng-roguelite/docs/BUILD_RELEASE_PIPELINE.md` — itch.io release
  process; a Linux release target isn't in it yet, pending Finding 1.
