# iOS Render Command Queueing

Date: 2026-08-29

This records what `host-desktop/src/ios.rs`'s `render_commands` assumes
about its callers, why that assumption broke one real consumer, the fix,
and a known issue that's still open after the fix.

## The Contract, and Who Follows It

Every `render_ops`/`render_widget_paint_ops` call from game code funnels
into `render_commands`, which queues `FrameCommand`s for the next
`flush_selected_backend` (triggered by `WindowEvent::RedrawRequested`).

Two different calling conventions exist among current `loadngo`
consumers:

- **One call per tick, whole scene.** `sng-roguelite` and `sng-zhoenus`
  each build their entire frame's `PaintOp` list in one place and call
  `render_widget_paint_ops` exactly once per tick.
- **Several calls per tick, layered scene.** `sng-rusty`'s widget-based
  UI issues its scene across independent `render_ops`/
  `render_widget_paint_ops` calls — background, then portraits, then
  dialogue/UI overlays each call their own — expecting them to
  accumulate into one frame.

Neither convention is wrong on its own; the bug was `render_commands`
picking one and breaking the other.

## What Broke, and Why

An earlier fix ("Multitouch support for ios") made `render_commands`
unconditionally `.clear()` `queued_commands` before every call. That was
a real fix for a real bug: RedrawRequested delivery through UIKit isn't
guaranteed 1:1 with tick production, so under touch-driven load a stale,
not-yet-flushed tick's commands could still be sitting in the queue when
a newer tick's commands arrived — `extend_from_slice` piled them on top
of each other instead of replacing them, producing a visible double-image
(a thumbstick drawn at both its old and new position at once).
Unconditionally clearing before every call fixed that for the
single-call-per-tick convention, where "every call" and "every tick" are
the same thing.

For the multi-call-per-tick convention, they aren't the same thing: each
of `sng-rusty`'s several same-tick calls wiped out every earlier call in
that tick, silently leaving only whatever was drawn last. Confirmed
on-device: background art and dialogue text were missing entirely, only
button chrome survived (buttons happened to be drawn last). This looked
at first like a rendering bug and a separate, unrelated touch-input bug
(tapping the title screen's only button appeared to do nothing) — it was
one bug. The tap was registering the whole time; the screen just never
visibly changed, because the renderer was still only ever showing that
same last-drawn button regardless of what the game's state actually was.

Isolated via direct A/B testing: reverting just this fix (temporarily,
working-tree only) reproduced the bug against an otherwise-current
`loadngo`; reverting just the unrelated Retina-text fix from the same
investigation (see the entry above) confirmed that one wasn't the cause.
Desktop (same `gfx-metal`) and Android (separate `gfx-gles` renderer)
were both unaffected by the underlying bug, which is what narrowed it to
iOS-specific queueing rather than a `sng-rusty`-side or asset-side bug.

## The Fix

`queued_commands` is now cleared once per new `frame_epoch`, not once
per call. `frame_epoch` only advances *between* ticks — never mid-tick,
since a tick's `render_commands` calls all happen synchronously within
one `pool.run_until_stalled()` pass, before the game ever awaits
`next_frame()` again — so it's the right signal to tell "first call this
tick" (clear stale data from an already-superseded tick) apart from
"another call still in this tick" (accumulate, don't wipe out a sibling
call). A new `queued_commands_epoch: Option<u64>` field on
`HostSharedState` tracks the epoch as of the last clear.

For single-call-per-tick callers this is a no-op by construction — the
epoch has always advanced since the last call, so the clear-branch fires
on literally every call, identical to the old unconditional behavior.
Verified on real hardware: `sng-zhoenus` and `sng-roguelite` render
identically to before. `sng-rusty` now renders backgrounds, portraits,
and dialogue text together correctly for the first time on-device.

## Known Issue (Open): Inconsistent Touch Deeper Into `sng-rusty`

Even with the fix above, touch responsiveness in `sng-rusty` on iOS was
reported as inconsistent after further playtesting: button presses work,
but became less reliable the further into the story the player went, and
the in-story menu button in particular could not be reliably brought up.

Not yet root-caused. What's already ruled out:
- Not the `queued_commands` clearing bug above — that fix is confirmed
  in place and confirmed to have fixed the original "nothing responds"
  symptom (which really was the same bug, per the section above); this
  is a *different*, still-present symptom on top of that fix.
- Not present in `sng-zhoenus`/`sng-roguelite` on the same device.

Candidates worth checking first, not yet investigated:
- Whether `sng-rusty`'s own hit-testing reads button bounds computed
  from a stale layout pass rather than the frame actually on screen
  (the same class of "which tick's state am I reading" bug as the
  rendering issue above, just on the input side).
- Whether deeper scenes issue enough additional `render_commands` calls
  per tick that some are landing in a *different* `frame_epoch` than
  intended (e.g. a slow decode stalling one call past an epoch
  boundary), partially reintroducing the original multi-tick pile-up
  this doc's fix targeted, just less severely.
- Whether iOS's touch delivery itself (`WindowEvent::Touch`,
  `apply_touch_event`) is dropping events under whatever the deeper
  scenes do differently (more active textures, more overlays, heavier
  per-tick work generally).

`LOADNGO_TRACE_WIDGETS=1` (env var, gates `trace_widgets_log` in
`gfx-metal`) and `sng-rusty`'s own `[sng-rusty-trace]` button-state
logging were both useful for the investigation above and are the
starting point for this one too — ideally combined with
`LOADNGO_TRACE_INPUT=1` (referenced in `ios.rs`'s own comments, not yet
exercised for this specific issue) to see whether touch events
themselves are being delivered at all during a failed tap deeper in the
story, versus being delivered but not acted on.
