# Proactor-First Engine Adoption

Status: active workspace priority as of 2026-09-01.

## Decision

`loadngo-proactor` is the intended scheduling and asynchronous-I/O core for
`loadngo` hosts and the applications built on them, including:

- `sng-roguelite`
- `sng-zhoenus`
- `sng-rusty`

The important boundary is host ownership, not forcing every game crate to
construct a `Proactor` directly. A game should keep its deterministic
simulation, UI state, and rendering policy. Its `loadngo` host should own one
platform-appropriate proactor and route timer, wake, asynchronous-I/O, and
background-completion scheduling through it.

## Required Host Contract

Every supported host must eventually satisfy these rules:

1. Construct and own one platform proactor for the application's lifetime.
2. Route runtime-future wakeups through the proactor rather than an unrelated
   timer thread or fixed polling loop.
3. Route `FrameDemand::After` through proactor deferred work.
4. Wake an idle `FrameDemand::Idle` runtime on input, invalidation, I/O, or
   posted work without creating an unconditional redraw cadence.
5. Deliver native asynchronous I/O completions through the same runtime where
   the platform backend supports it.
6. Stop the proactor and drain outstanding I/O before host shutdown releases
   buffers or platform resources.

This does not require a static screen to render at 60 Hz. Frame pacing remains
an application policy:

- active animation, simulation, drag, transition, or text reveal may request a
  paced frame;
- a static scene should remain idle until an event or deadline requires work.

## Current Position

The core now supplies queued work, deadline ordering, wakeups, cancellation,
shutdown draining, readiness registration, and real `IoPort` implementations
for kqueue, io_uring, and IOCP.

`host-desktop` uses the kqueue proactor on macOS and the io_uring proactor on
Linux for runtime wakers and deferred frame scheduling; both own their
proactor through the shared `HostProactor` seam
(`host-desktop/src/proactor_driver.rs`, see
[PROACTOR_ARCHITECTURE.md](PROACTOR_ARCHITECTURE.md)). NetBSD desktop paths
also use a kqueue proactor directly. The Rust game executables therefore
already exercise the intended model on macOS and Linux through the shared
host, without each game taking a direct proactor dependency.

The Linux migration (2026-09-01) replaced `host-desktop/src/linux.rs`'s
per-`next_frame()` `thread::spawn` (one OS thread spawned for every pending
`FrameDemand::After` wait) with a deferred timer on the owned
`Proactor<IoUringPort>`, driven by `winit`'s `ControlFlow::WaitUntil` instead
of a background thread. This is a correctness-preserving mechanical swap
(verified: `cargo build`/`cargo test -p loadngo-host-desktop` and a
`sng-roguelite-game` smoke launch on macOS, which shares the same
`proactor_driver` code path) -- it has **not** yet cleared this document's
own evidence gate on real Linux hardware; that requires the `dolores` CI run
and a manual playtest pass (see "Immediate Work" below).

The remaining host gap is platform parity:

| Platform | Core backend | Host status |
| --- | --- | --- |
| macOS | `KqueuePort` | reference integration exists |
| Linux | `IoUringPort` | host scheduler migrated 2026-09-01; pending real-hardware evidence-gate measurements (dolores) |
| iOS | `KqueuePort` | host scheduler migration required |
| Android | none | `ALooper` completion port and host integration required |
| Windows | `IocpPort` | host scheduler migration and real-machine validation required |

## Evidence Gate

The proactor is not considered the default engine core merely because it
compiles or has a high synthetic throughput number. Each platform adoption must
show both correctness and an observable benefit against a same-machine baseline.

### Correctness

- core ordering, wake, shutdown, and cancellation tests pass;
- platform I/O tests cover real file and loopback socket operations where the
  backend supports them;
- concurrency-sensitive paths have a focused model or stress test;
- background/foreground, window close, and in-flight-I/O shutdown complete
  without hangs, leaked work, duplicate frames, or stale input;
- each of the three games completes its existing smoke/playtest flow on the
  migrated host.

### Measurements

Record the same scenes before and after a host migration on the same machine:

- idle CPU use and wakeups per second for a static title/menu;
- active-frame interval and jitter, including p50, p95, and p99;
- input-event-to-present latency for a representative interaction;
- background completion-to-visible-update latency;
- total wakeups and frame submissions over a fixed idle and active interval;
- shutdown duration with pending I/O.

Adoption is accepted only when active-frame and input latency are not regressed,
idle wake/CPU use is materially reduced where the prior host polled, and no
correctness regression appears in the target game flows. Exact thresholds are
set from the first measured baseline rather than invented before hardware data
exists.

## Current Baseline

The 2026-09-01 macOS verification establishes core viability, not a host
comparison:

- `cargo test -p loadngo-proactor`: 13 applicable core/kqueue tests passed;
- loom model: 2 pending-operation/deadlock tests passed;
- real `KqueuePort` stress: 2,617,505 completions in 10 seconds across four
  producer threads, completing cleanly;
- simulated frame work: 4+4 callbacks in about 4.1 microseconds, 16+16 in
  about 14.8 microseconds, and 64+64 in about 55.2 microseconds.

These numbers show ample scheduler headroom inside a 16.6 ms frame budget.
They do not yet measure a game's frame pacing, input latency, idle CPU, or
battery behavior against the previous host loop.

## Rollout Order

### Phase 0: Make the proof repeatable

1. Add host-level timing and wake counters behind an opt-in trace surface.
2. Define a small common capture procedure for static, animated, and
   backgrounded states.
3. Capture the macOS reference baseline with `sng-roguelite`, `sng-zhoenus`,
   and `sng-rusty`.
4. Keep the proactor harness as a regression signal, not as the sole benefit
   claim.

### Phase 1: Establish the portable host-driver seam

1. **Done (2026-09-01).** Extracted the macOS ownership pattern into
   `host-desktop/src/proactor_driver.rs::HostProactor`, a small host
   scheduling contract shared by macOS and Linux.
2. Keep platform window/event pumping native; do not replace it with a generic
   busy loop. (Preserved: macOS still drives `NSApplication`'s event pump,
   Linux still drives `winit`'s event loop -- the proactor only supplies the
   wait deadline and deferred dispatch, per `ControlFlow::WaitUntil`.)
3. Make runtime wake, frame deadline, invalidation, and shutdown flow through
   that contract. (Done for macOS and Linux; iOS/Android/Windows still
   outstanding.)
4. Add deterministic host-level tests for idle wakeups and deferred frames.
   **Not yet done** -- still relying on the proactor crate's own unit/loom
   tests plus manual host smoke passes; no host-level automated regression
   test exists yet for either platform's wake/deferred-frame behavior.

### Phase 2: Migrate the active non-Android game hosts

1. **Code done, hardware evidence pending (2026-09-01).** Moved Linux onto
   `IoUringPort` (`host-desktop/src/linux.rs`), replacing the
   `thread::spawn`-per-frame-wait model. Not yet measured on the three games
   per the evidence gate -- needs a `dolores` run (see "Immediate Work").
2. Move iOS onto `KqueuePort` and repeat device tests, including the existing
   `sng-rusty` touch investigation.
3. Compare each migration with its recorded baseline before treating it as an
   improvement.

### Phase 3: Close mobile and Windows parity

1. Implement an `ALooper`-backed Android completion port and migrate the
   Android host without reintroducing timer-per-frame threads.
2. Move the Windows host to `IocpPort` and validate on a real Windows machine.
3. Exercise lifecycle, cancellation, input, and presentation behavior on those
   real devices rather than relying on cross-compilation.

### Phase 4: Consumer policy

Once a host passes the evidence gate, all `loadngo` applications on that host
inherit proactor ownership automatically. Individual games should only access a
proactor API for genuine background work or I/O; they should not each recreate
their own event loop, timer thread, or scheduler.

## Non-Goals

- Do not move deterministic game simulation into arbitrary background tasks.
- Do not make `sng-*` crates depend directly on the proactor just to schedule
  frames already owned by the host.
- Do not claim an energy, latency, or throughput win without before/after
  measurements on the same platform.
- Do not make qcoin or EAB node adoption the proof that the game framework is
  healthy; they are useful consumers, not substitutes for game-host evidence.
- Do not allow existing Zhoenus/EAB achievement work to introduce an unrelated
  scheduler into a game client.

## Immediate Work

1. Validate the 2026-09-01 Linux migration on `dolores`: `cargo test`/
   `cargo clippy` via CI, then a manual playtest of `sng-roguelite`,
   `sng-zhoenus`, and `sng-rusty` (window close, background/foreground,
   held-key input) to confirm no correctness regression before this counts
   as adopted.
2. Design and add the host-level measurement surface (idle CPU/thread count,
   wakeups/sec, frame interval jitter, input-to-present latency).
3. Capture the macOS and Linux baselines (old thread-per-wait Linux host vs.
   the new `IoUringPort` host makes a real before/after comparison possible
   for the first time) and select explicit pass thresholds.
4. Repeat the same proof on iOS, then build the Android and Windows paths.
