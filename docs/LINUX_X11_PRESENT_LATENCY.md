# Linux X11 Present Latency Growth (Open Issue)

## Status

Unresolved, but substantially re-scoped. Documented 2026-08-26 during an
input-lag investigation on `sng-roguelite` so the finding survives across
sessions. Confirmed again 2026-08-30 during a real keyboard+mouse
playtest on `dolores` — this is now the tracked blocker for a Linux
itch.io release; see [DESKTOP_PLATFORM_ROADMAP.md](DESKTOP_PLATFORM_ROADMAP.md).
A follow-up investigation the same day (2026-08-30, see "What was ruled
out") directly disproved the original leading hypothesis (see below) —
the growth is **not** specific to the software/SHM present path after
all, and is **not** CPU frequency scaling or thermal throttling either.
Root cause is still unknown; the search space is narrower than before but
the fix is not yet in sight.

## Summary

On the Linux desktop backend (`host-desktop/src/linux.rs`), the
wall-clock cost of `present()` grows steadily over a run's lifetime,
independent of user input, **regardless of which render backend is
active** (see 2026-08-30 update below — this was originally thought to be
software-path-specific and is not):

| elapsed run time | approx. tick | `present()` duration |
|---|---|---|
| ~0.1s  | tick 5   | ~13 ms |
| ~1-2s  | tick 40  | ~44-48 ms |
| ~3-4s  | tick 90  | ~104-125 ms |
| ~6s    | tick 140 | ~148-195 ms |

This was measured with a single, cleanly-tracked `sng-roguelite-game`
process, zero keyboard/mouse events, on a Raspberry Pi running a `labwc`
Wayland session with the game forced onto the X11/XWayland backend
(`WINIT_UNIX_BACKEND=x11`). Simulation (`session.advance`) and render-op
building/queuing stayed flat at well under a millisecond the entire time;
all of the growth is inside `present()`.

This is a separate finding from (and was discovered while investigating)
the input-driven frame-pacing bug described below, which **is** fixed.

## Related, already-fixed bug (for context)

`window_event` handlers in `linux.rs` used to call `publish_frame()` (bump
`frame_epoch`, notify the `Condvar` that `next_frame()` awaits) on every raw
OS event, including keyboard auto-repeat. Holding a key down drove the main
game loop at OS key-repeat rate instead of the caller's requested
`FrameDemand` cadence, degrading both input responsiveness and simulation
tick pacing the longer a key stayed held. Fixed by having `window_event`
handlers set `pending_redraw` directly instead of bumping `frame_epoch`;
only the `next_frame` timer thread, the initial `resumed()` kick, and the
explicit `wake_host()` API advance the frame clock now. See
`advance_frame_clock`, `publish_frame`, and `NextFrameFuture` in
`host-desktop/src/linux.rs`.

That fix is unrelated to the present-latency growth documented here: the
growth reproduces with zero input events at all.

## What was ruled out

Checked directly, with evidence, before writing this up:

- **Growing OS thread count.** Sampled `/proc/<pid>/status` `Threads:`
  every second across a 12s idle run: flat at 3 threads throughout. Not a
  `next_frame()` thread-spawn leak.
- **Redraw-request flooding from a busy-polling event loop.** Added atomic
  counters (`REDRAW_REQUEST_COUNT`, `ABOUT_TO_WAIT_COUNT`) around
  `request_redraw_if_needed()` and `about_to_wait()`. Both stayed flat
  (~2 redraw requests and 1 `about_to_wait` call per tick) across the
  entire run. `ControlFlow::Poll` is not busy-spinning.
- **Growing render-op / command list.** `ops_len` from `build_render_ops`
  stayed at 45-62 for the whole run; `build_ms` and `render_ops_ms` (queuing
  cost, not raster cost) stayed under ~0.15 ms throughout.
- **Correlation with input.** Reproduces identically with zero keyboard/mouse
  events during the run.
- **Leftover/contaminating background processes.** The very first attempt to
  measure this was confounded by multiple stacked `sng-roguelite-game`
  instances left running from earlier `pkill -x sng-roguelite-game` calls
  that were silent no-ops (the kernel truncates `comm` to 15 bytes, so
  `-x sng-roguelite-game`, which is 19 bytes, never matches). Re-verified
  with a single cleanly-tracked PID (captured at launch, killed by exact
  PID) and the same growth curve reproduced, so it is not purely a
  contamination artifact — though be aware of this footgun when
  reproducing: **use `pkill -f <pattern>` or kill by captured PID, never
  `pkill -x` on a name longer than 15 bytes.**
- **`softbuffer`'s `Surface::resize()` reallocating every frame.** Read
  `softbuffer-0.4.8/src/backends/x11.rs`: `resize()` no-ops when the size is
  unchanged (`if self.size != Some((width, height))`), and our code calls it
  every `present()` with an unchanged size. Not the source.
- **(2026-08-30) Growth being specific to the software/SHM present path.**
  Reproduced the identical growth curve (0.25ms → ~92-98ms over ~12s)
  while `present()` was confirmed running the **GLES** branch the entire
  time, not software: `requested_render_backend()` defaults to
  `DesktopRenderBackendKind::Gles` when `LOADNGO_DESKTOP_BACKEND` is
  unset, and the trace log showed exactly one "Linux GLES backend bound
  to the native window" / "Linux GLES backend rendered the queued frame"
  pair near startup and no later "falling back to software" message
  (`update_backend_detail` only logs on a change, so a single
  "rendered the queued frame" line across 261 presents means GLES stayed
  active — see `present()`'s early `return` on `Ok(())` at
  `host-desktop/src/linux.rs:1368-1378`). GLES never touches
  `softbuffer`/`shm::PutImage`/`finish_wait` at all, so the original
  suspected mechanism (below) cannot be what's actually growing — the two
  backends share nothing in that part of the call path, yet show the same
  curve.
- **(2026-08-30) `Xwayland`'s and `labwc`'s own CPU/memory usage growing.**
  Sampled both with `ps -o %cpu=,rss=` once per second across a 12s
  reproduction: both processes stayed completely flat the entire time
  (`Xwayland` ~0.0-0.1% CPU / 3136KB RSS constant; `labwc` 0.0% CPU /
  33776KB RSS constant) while `present()` duration grew from 0.25ms to
  92.5ms in the same window. Whatever is growing is not showing up as
  either process's own CPU time or resident memory.
- **(2026-08-30) CPU frequency scaling / thermal throttling.** `dolores`
  (Raspberry Pi 5) runs the `ondemand` CPU governor by default; sampled
  `vcgencmd measure_clock arm`/`v3d`, `vcgencmd measure_temp`, and
  `vcgencmd get_throttled` during a reproduction — `get_throttled` stayed
  `0x0` throughout (no under-voltage/throttle event flagged, ever) and
  temperature stayed a safe 48-50°C, but ARM core frequency dropped from
  2.4GHz to ~1.7GHz partway through the run (GPU/v3d frequency stayed
  flat at 960MHz throughout — only the CPU cores, not the GPU). This
  looked like a plausible independent cause (`ondemand` scaling down when
  it perceives less active CPU work, which growing wait-bound `present()`
  calls could produce, in turn making any CPU-bound portion of `present()`
  take proportionally longer — a feedback loop). Pinned all four cores to
  the `performance` governor (confirmed via
  `scaling_governor` reading `performance` on all cores, and `vcgencmd
  measure_clock arm` staying pinned at a constant 2.4GHz for the entire
  run this time) and reproduced again: **the identical growth curve
  reproduced anyway** (0.25ms → 98.6ms). CPU frequency scaling is a real,
  observable phenomenon on this box, but it is not the cause of the
  present-latency growth — most likely a downstream symptom of whatever
  the real cause is (the governor reacting to `present()` spending more
  time blocked/waiting rather than actively computing), not the cause
  itself.

## What's suspected (revised 2026-08-30, still not confirmed)

The original hypothesis — `present()`'s software path blocking on
`softbuffer`'s `finish_wait` X11 `GetInputFocus` round trip
(`softbuffer-0.4.8/src/backends/x11.rs:686-694`) — is very likely **not**
the mechanism, since the 2026-08-30 GLES-backend reproduction shows the
identical growth curve while never executing that code path at all. The
original chain-of-suspicion (Xwayland buffer/import bookkeeping, `labwc`
composition/damage-tracking overhead, or GPU driver state getting slower
over the run) is still plausible in shape, but the evidence now points
away from *userspace CPU-side bookkeeping* in either `Xwayland` or
`labwc` specifically (both measured flat) and toward something that:

- affects both the software/SHM and GLES present paths equally (so
  something downstream of both — the actual buffer hand-off to the
  compositor/DRM/GPU, not anything backend-specific on our side), and
- doesn't show up as growing CPU time or RSS on any userspace process
  sampled so far (so likely a genuinely blocking wait — e.g. a growing
  buffer-release/fence wait, VBlank/frame-scheduling backlog, or GPU
  command-queue depth — rather than accumulating computational work), and
- is not CPU frequency/thermal related (directly disproven by the pinned-
  governor test).

This narrows toward the GPU/DRM/compositor buffer-scheduling layer itself
(something inside `labwc`/wlroots' Wayland buffer commit-and-release
cycle, the `v3d` kernel driver, or DRM/KMS scheduling) rather than
anything in this repo's own code, but is still not confirmed — no
specific mechanism has been identified yet, only ruled out.

## Suggested next steps

1. ~~Reproduce with `LOADNGO_LINUX_TRACE=1` while sampling `Xwayland` and
   `labwc` CPU/memory across the run.~~ **Done 2026-08-30** — both flat;
   see "What was ruled out" above.
2. ~~Try the GLES backend instead of the software/SHM path, to see if the
   growth is specific to that barrier round trip or reproduces there
   too.~~ **Done 2026-08-30** — reproduces identically under GLES; the
   original suspected mechanism is very likely wrong. See above.
3. Try a plain X11 session (no XWayland/labwc in the loop) or a different
   compositor, to isolate whether this is a `labwc`/wlroots-specific
   behavior or general to this Pi's GPU driver stack. **Still open** —
   not attempted this session; would need a different display session set
   up on `dolores`, which wasn't done given the risk of disrupting the
   box's normal desktop session.
4. ~~Check `Xwayland`'s and `labwc`'s own resource usage over a run for
   anything growing there.~~ **Done 2026-08-30** — both flat; see above.
5. If reproducible upstream, this may be a `labwc`/`wlroots`/Mesa `v3d`
   driver issue rather than anything fixable in this repo. **More likely
   now than when originally written**, given (2) and (4) above.
6. **New, 2026-08-30**: localize *where inside* `present()`'s GLES branch
   the growth actually lives, with finer-grained internal timing around
   `backend.sync_image_resources(...)` (CPU-side texture upload prep)
   versus `Renderer::new(...).render(backend, &gles_commands)` (the
   actual GPU draw/submit/swap call) — `host-desktop/src/linux.rs:1336-1378`.
   If the growth is entirely inside the `render()`/swap call and
   `sync_image_resources` stays flat, that further supports a
   GPU/compositor buffer-scheduling cause over anything in our own
   per-frame CPU-side bookkeeping (which the flat `ops_len`/`build_ms`
   numbers already argue against, but this would confirm it directly
   inside the GLES path specifically, which those numbers don't cover).
   Not attempted this session — the next concrete step.
7. **New, 2026-08-30**: check whether `EGL_KHR_swap_buffers_with_damage`
   or a presentation-time extension (`wp_presentation` under Wayland) is
   available and would expose actual compositor-side frame-scheduling
   latency directly, rather than inferring it indirectly from wall-clock
   `present()` duration.

## How to reproduce / instrumentation available

Durable, env-gated diagnostics already exist for this (zero cost when
disabled, following the existing `LOADNGO_LINUX_TRACE` convention):

- `LOADNGO_LINUX_TRACE=1` on the loadngo side logs, among other things:
  - every `advance_frame_clock` call with its trigger source (`resumed`,
    `timer`, `wake_host`, or historically `window_event`), epoch, and dt
  - every `present()` call's wall-clock duration
- `SNG_TICK_TRACE=1` on `sng-roguelite-game` logs per-tick simulation step
  count, event count, and phase timing (`advance_ms`, `build_ms`,
  `render_ops_ms`, `next_frame_wait_ms`).
- `LOADNGO_DESKTOP_BACKEND` selects the render backend explicitly:
  `gles` (the default when unset — **not** `software`, despite this
  document's original title/framing) or `software`. Set it explicitly to
  `software` to reproduce the originally-suspected path, or leave it
  unset/`gles` to reproduce what actually ships by default. As of
  2026-08-30 both reproduce the same growth curve; see "What was ruled
  out" above.

Example repro (adjust window title / binary path as needed):

```bash
env -u WAYLAND_DISPLAY DISPLAY=:0 LOADNGO_LINUX_TRACE=1 SNG_TICK_TRACE=1 \
  WINIT_UNIX_BACKEND=x11 ./target/debug/sng-roguelite-game \
  > /tmp/trace.log 2>&1 &
GAME_PID=$!
sleep 1
WIN=$(DISPLAY=:0 xdotool search --name "SNG" | head -1)
DISPLAY=:0 xdotool windowfocus --sync "$WIN"
sleep 12
kill -9 "$GAME_PID"   # kill by captured PID, not by name -- see footgun above
grep "present duration_ms" /tmp/trace.log
```

To check for CPU-frequency-scaling confounds, cross-reference against
`vcgencmd measure_clock arm`, `vcgencmd measure_clock v3d`, `vcgencmd
measure_temp`, and `vcgencmd get_throttled` sampled once per second
across the same window (Raspberry Pi-specific; `vcgencmd` ships with
Raspberry Pi OS). To rule out the `ondemand` governor entirely, pin all
cores to `performance` first (needs an interactive `sudo`, `vcgencmd`
itself does not):

```bash
ssh -t jay@192.168.1.140 'echo performance | sudo tee /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor /sys/devices/system/cpu/cpu1/cpufreq/scaling_governor /sys/devices/system/cpu/cpu2/cpufreq/scaling_governor /sys/devices/system/cpu/cpu3/cpufreq/scaling_governor'
```
