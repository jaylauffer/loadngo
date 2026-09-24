# Platform-Agnostic Thermal Awareness

## Status

Loadngo contract, partly implemented. This document defines the API and
implementation boundaries; it does not claim that every platform provider exists yet.

Implemented 2026-09-24 (Claude Code, Jay: "we need to remain cool"): the
`loadngo-thermal` crate with the portable types, the governor (immediate escalation;
recovery after three lower samples and a 15 s dwell; unavailable never reported as
nominal), `FakeProvider`, `UnavailableProvider`, and the macOS/iOS `NativeProvider`
over `NSProcessInfo.thermalState` (sequence steps 1 and the macOS part of 4). Linux
sysfs, Android, host ownership (step 3) and the other steps are not built.

First consumer: the Kimi `k3` CLI samples at each token boundary (no timer), prints
transitions, pauses at `Serious` while re-sampling every 2 s through a loadngo proactor
deadline, and stops before the next token at `Critical`. Verified with fake-provider
tests of the pause/stop logic and a live run reporting `nominal` on this Mac mini; no
throttling episode has been observed yet, so the pause path has not met real heat.

The goal is not to keep every device at one arbitrary temperature. Loadngo
must react to the best pressure signal each operating system can provide,
reduce optional work before a device throttles, remain correct when no sensor
is available, and avoid making a warm system hotter through its monitor.

## Lessons Reused From Starlight

Starlight is the proving example for the execution pattern:

- one owned proactor drives I/O and thermal deadlines;
- the thermal check re-arms itself with `ProactorHandle::defer_for` instead of
  a sleep thread or polling loop;
- hot paths admit work before doing expensive conversion or output;
- warning state removes optional work rather than adding diagnostic work;
- critical state stops accepting new nonessential work;
- observations can be published asynchronously without blocking the runtime.

See `starlight/src/runtime.rs` and
`starlight/docs/THERMAL_BEHAVIOUR.md`.

Loadngo should not copy Starlight's Pi-specific details:

- `/sys/class/thermal/thermal_zone0/temp` is a Linux provider detail;
- `82 C` and `85 C` are policy for one Raspberry Pi deployment, not portable
  defaults;
- raw `u8` states are not a public API;
- a missing temperature is not the same as a cool device;
- process exit is a Starlight-specific critical response, not an engine rule;
- a Unix socket is one observer transport, not the core thermal interface.

## Architectural Boundary

Thermal support has three layers:

1. A **provider** obtains native thermal observations. It may receive an OS
   notification or take a coarse sample.
2. A platform-independent **governor** applies normalization, hysteresis, and
   policy to produce a stable work budget.
3. The host and its consumers apply that budget to optional work. The host
   remains responsible for proactor ownership, wakeups, and shutdown.

Applications must not query platform sensors, spawn monitor threads, shell out
to temperature utilities, or invent their own thermal state machine.

The proposed shared implementation belongs in a small `loadngo-thermal` crate.
Its types and governor must have no dependency on a graphics host. Desktop and
mobile hosts supply providers, while headless services such as Starlight can
use the same governor directly.

## Portable Model

The portable API is semantic because several supported systems expose pressure
without exposing a trustworthy die temperature.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalPressure {
    Unavailable,
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalSource {
    NativePressure,
    ThermalZone,
    External,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThermalRecommendation {
    Baseline,
    ReduceOptional,
    MinimizeOptional,
    PauseOptional,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermalObservation {
    pub pressure: ThermalPressure,
    pub temperature_c: Option<f32>,
    pub throttling: Option<bool>,
    pub source: ThermalSource,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermalSnapshot {
    pub pressure: ThermalPressure,
    pub recommendation: ThermalRecommendation,
    pub temperature_c: Option<f32>,
    pub throttling: Option<bool>,
    pub sequence: u64,
}
```

`Unavailable` is explicit. It must never be silently converted to `Nominal`.
It uses the application's normal bounded-work policy, reports that thermal
feedback is absent, and cannot support a claim that thermal behavior was
device-verified.

The pressure enum deliberately has no derived ordering: `Unavailable` is an
availability condition, not a severity below `Nominal`. The governor uses an
explicit severity function only after handling unavailable input.

`temperature_c` is diagnostic metadata. Portable application behavior must be
driven by `pressure` and `recommendation`, not by a Celsius comparison.
`throttling` records an OS or firmware fact when one exists; it is not inferred
from high utilization.

`sequence` changes only when the published snapshot changes. Consumers can
therefore avoid repainting or reconfiguring work for duplicate samples.

## Governor Contract

The governor is a deterministic state machine with a fake clock/provider for
tests. It accepts observations and publishes snapshots.

Escalation rules:

- native `Serious` and `Critical` observations take effect immediately;
- an explicit throttling signal is at least `Serious`;
- a raw-temperature provider uses platform/configured trip points, never a
  universal engine threshold;
- invalid, stale, or unavailable input becomes `Unavailable`, not `Nominal`.

Recovery rules:

- recovery is slower than escalation;
- require at least three consecutive lower observations and a 15-second
  minimum dwell before lowering a pressure band;
- a temperature-derived provider also requires a recovery margin below the
  entry trip point (3 C is the initial Linux default when firmware supplies no
  separate hysteresis value);
- a provider notification can trigger an immediate sample, but cannot bypass
  recovery hysteresis.

These defaults are part of the governor configuration so hardware evidence can
change them without changing the state model.

## Work Classes And Default Response

Every consumer should classify work rather than treating "thermal throttling"
as an instruction to slow everything equally.

| Work class | Examples | Thermal rule |
| --- | --- | --- |
| Essential | input, save integrity, shutdown, network protocol deadlines | Preserve correctness; never abandon silently |
| Interactive | visible rendering, audio, current gameplay simulation | Preserve responsiveness; lower optional quality/cadence before correctness |
| Background | CAS indexing, prefetch, asset conversion, task execution | Bound concurrency; defer or pause when pressure rises |
| Opportunistic | cache warming, previews, animations with no semantic effect | First to stop |

Initial recommendation mapping:

| Pressure | Recommendation | Default behavior |
| --- | --- | --- |
| `Unavailable` | `Baseline` | Normal bounded policy, mark telemetry unavailable |
| `Nominal` | `Baseline` | Configured concurrency and requested frame cadence |
| `Fair` | `ReduceOptional` | Reduce background concurrency and optional refresh frequency |
| `Serious` | `MinimizeOptional` | One background lane at most; stop prefetch and decorative animation |
| `Critical` | `PauseOptional` | Admit no new optional work; safely finish, checkpoint, or cancel in-flight work |

The governor must not stop the proactor, corrupt an active write, alter a
deterministic simulation rate, or turn an availability problem into data loss.
Consumers define safe checkpoints and cancellation boundaries.

## Proactor And Event-Loop Integration

Native notifications are preferred. When polling is the only safe provider,
the host schedules one self-rearming proactor deadline:

- nominal/unavailable: no faster than once every 5 seconds;
- fair/serious: no faster than once every 2 seconds;
- critical: no faster than once per second;
- never use `thread::sleep`, a spin loop, or an application-owned timer
  thread.

Sampling completion feeds the governor. Only a changed snapshot posts work to
the host and wakes an idle runtime. An unchanged periodic sample must not cause
a frame submission.

Observers use latest-value semantics: a slow observer receives the newest
snapshot rather than an unbounded queue of stale temperature samples. Logging
is transition-based, with an optional low-rate heartbeat, so a fault cannot
become a log or socket storm.

An idle static application remains on `FrameDemand::Idle`. Thermal monitoring
does not create a redraw cadence.

## Host Surface

The shared host should expose the latest snapshot without requiring games to
depend directly on a platform provider:

```rust
pub trait ThermalHostBackend {
    fn thermal_snapshot() -> ThermalSnapshot;
}
```

For frame-driven applications, `HostFrame` should eventually carry the same
snapshot. The host wakes the runtime when its sequence changes, so a game can
apply a new budget immediately and then return to `FrameDemand::Idle`.

Headless consumers use `loadngo-thermal::ThermalGovernor` directly with their
owned proactor. Starlight should migrate its normal/warning/critical policy to
the shared governor after the crate is stable, while retaining its Sense HAT
display and local status publisher as application behavior.

## Platform Providers

| Platform | Preferred input | Temperature | Initial status |
| --- | --- | --- | --- |
| macOS | Native process thermal-pressure state and change notification | Usually unavailable to an unprivileged app | Implement first; no `powermetrics` or privileged helper |
| iOS | Native process thermal-pressure state and change notification | Not required | Same semantic mapping as macOS |
| Android | System thermal status listener; current status for startup | Optional/vendor-specific | Map native status to portable bands; avoid sensor polling from the game |
| Linux | Thermal-zone type, temperature, trip points, and hysteresis from sysfs | Usually available | General provider; select CPU/package zones by type, not fixed index |
| Windows | Supported OS thermal/power notification if available | Commonly unavailable | Return `Unavailable` until a reliable provider is proven; do not poll WMI or vendor tools |
| NetBSD | Native environmental sensor interface when implemented | Hardware-dependent | Begin as `Unavailable`; add behind the same provider contract |

Power/battery-saver state may influence a separate energy policy, but must not
be mislabeled as thermal pressure. CPU utilization is evidence about workload,
not a temperature sensor.

Provider failures are nonfatal. They publish `Unavailable`, retain the bounded
baseline policy, and log one transition rather than retrying tightly.

## External Signals

Starlight demonstrates that thermal state can be useful outside the process.
Loadngo may later expose a versioned status record for runners or task workers,
but external transport is not required for the core API.

If exported, the record should contain semantic state, recommendation,
availability, timestamp, and optional temperature/throttling fields. Exact
hardware identifiers and raw sensor inventories are private by default.

Task workers should refuse new optional offers at `Critical` and advertise
reduced capacity at `Serious`. An already accepted task must follow its own
checkpoint/cancellation contract rather than disappearing.

## Verification Gates

Pure tests:

- every native/provider state maps to the expected portable pressure;
- escalation is immediate and recovery obeys consecutive-sample, dwell, and
  hysteresis rules;
- stale/invalid input becomes `Unavailable`;
- unchanged samples do not advance `sequence`;
- recommendation and work-budget mapping is deterministic;
- a fake provider proves that monitoring never requires a sleep thread.

Host integration tests:

- a changed snapshot wakes an idle runtime exactly once;
- duplicate samples submit no frame and no observer backlog;
- provider failure backs off and does not spin;
- shutdown drains or cancels the outstanding sample safely;
- critical pressure prevents new optional work while essential completions
  still run.

Real-hardware evidence for each provider:

- idle process CPU and wakeups with monitoring off versus on;
- observation-to-policy latency;
- event-storm and sustained-load behavior;
- frame pacing and input latency before and after pressure escalation;
- proof of recovery without band flapping;
- OS thermal/throttling evidence where the platform exposes it.

Compilation is not thermal validation. macOS and real Linux (`dolores`) are
the minimum initial evidence pair; Android, iOS, and Windows require their own
device runs before their providers are marked complete.

## Implementation Sequence

1. Add `loadngo-thermal` with portable types, the pure governor, fake provider,
   work recommendations, and deterministic tests.
2. Extract Starlight's reusable transition/timer lessons into tests for that
   crate; do not move its Pi thresholds into Loadngo defaults.
3. Add host ownership and snapshot-change wakeup without any provider, using
   `Unavailable` plus a fake provider in host tests.
4. Implement macOS/iOS native-pressure providers and the general Linux sysfs
   provider. Validate on the Mac mini and `dolores`.
5. Implement Android native status. Keep Windows and NetBSD explicitly
   `Unavailable` until reliable native sources are demonstrated.
6. Convert one bounded background consumer (the Archive CAS browser's catalog
   refresh/index work) to the budget and completion model before calling the
   API an explorer template.
7. Migrate Starlight to `loadngo-thermal`, preserving its external status
   socket and Sense HAT behavior, then compare behavior on `agnes`.
8. Add thermal capacity to Loadngo Task offers only after local host policy is
   stable and version the exported status separately.

## Definition Of Done

Thermal awareness is established only when:

- applications consume one portable semantic model;
- monitoring is notification-driven or proactor-deferred and measurably idle;
- missing sensors remain explicit and safe;
- optional work responds through bounded concurrency and backpressure;
- state transitions wake an idle host without creating a frame cadence;
- macOS and Linux pass the real-hardware gates;
- Starlight consumes the shared governor instead of carrying a second state
  model;
- no platform requires privilege, a shell command, or vendor-specific logic in
  application code.
