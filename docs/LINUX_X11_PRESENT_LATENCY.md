# Linux X11 Present Latency Growth (Open Issue)

## Status

Unresolved. Documented 2026-08-26 during an input-lag investigation on
`sng-roguelite` so the finding survives across sessions. Needs a follow-up
investigation session; see "Suggested next steps" below.

## Summary

On the Linux desktop backend (`host-desktop/src/linux.rs`, software/X11
present path), the wall-clock cost of `present()` grows steadily over a
run's lifetime, independent of user input:

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
all of the growth is inside `present()`'s software rasterization path.

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

## What's suspected (not yet confirmed)

`present()`'s software path (`host-desktop/src/linux.rs`, the non-GLES
branch) blocks on `softbuffer`'s `finish_wait`, which sends an X11
`GetInputFocus` request purely as an ordering barrier and blocks on its
reply (`softbuffer-0.4.8/src/backends/x11.rs:686-694`) to know the previous
`shm::PutImage` has been consumed before reusing the shared-memory segment.
That round trip is the one thing in the call path that depends on a process
other than our own.

On this machine the chain is:

```
sng-roguelite-game --(X11 protocol, XWayland)--> Xwayland --(wl_shm import + wl_surface commit)--> labwc (wlroots Wayland compositor) --(DRM/KMS)--> GPU (v3d)
```

(`ps aux` showed `labwc -m`, `Xwayland -rootless ... :0`, and active
`kworker/u16:*-v3d_{bin,render,tfu}` kernel threads for the Broadcom
VideoCore GPU during the run.) The working hypothesis is that something in
that chain — Xwayland's buffer/import bookkeeping, or `labwc`'s own
composition/damage-tracking overhead, or GPU driver state — is what's
actually getting slower over the run, and our `finish_wait` round trip is
just where that latency becomes visible to us. This has **not** been
confirmed; it is the leading hypothesis given everything client-side is
ruled out above, not a diagnosed root cause.

## Suggested next steps

1. Reproduce with `LOADNGO_LINUX_TRACE=1` (present-duration + source
   tracing already instrumented in `linux.rs`, see below) while sampling
   `Xwayland` and `labwc` CPU/memory with `top -H -p <pid>` for both
   processes across the run, not just our own.
2. Try the GLES backend (`requested_render_backend() ==
   DesktopRenderBackendKind::Gles`) instead of the software/SHM path, to see
   if the growth is specific to the `shm::PutImage` + `GetInputFocus`
   barrier round trip or reproduces there too.
3. Try a plain X11 session (no XWayland/labwc in the loop) or a different
   compositor, to isolate whether this is a `labwc`/wlroots-specific
   behavior or general to this Pi's GPU driver stack.
4. Check `Xwayland`'s and `labwc`'s own resource usage (`xrestop` if
   available, or `/proc/<pid>/status` for both) over a run for anything
   growing there.
5. If reproducible upstream, this may be a `labwc`/`wlroots`/Mesa `v3d`
   driver issue rather than anything fixable in this repo.

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
