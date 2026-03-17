# Windows Proactor Agent Runbook

This runbook is for validating and continuing the `IOCP` backend on a Windows machine.

## Goal

Validate that `loadngo-proactor` on Windows behaves like the original C++ `Machine`:

- immediate work posts through `IOCP`
- wake events interrupt blocking waits
- deferred work remains owned by the Rust core, not by the Windows backend
- host integration can replace polling loops cleanly

## Current code locations

- Core proactor: `proactor/src/lib.rs`
- Deferred queue: `proactor/src/deferred.rs`
- Windows backend: `proactor/src/iocp.rs`
- Shared tests: `proactor/tests/core.rs`
- Windows tests: `proactor/tests/iocp.rs`
- Architecture note: `docs/PROACTOR_ARCHITECTURE.md`

## Required validation steps

Run from the `loadngo` repo root.

1. Build and test the proactor crate on Windows.

```powershell
cargo test -p loadngo-proactor
```

2. Run a Windows-targeted workspace check.

```powershell
cargo check
```

3. If the proactor tests fail, inspect these areas first:
- `IocpPort::new()`
- `IocpPort::post()`
- `IocpPort::poll()`
- `IocpPort::wake()`

4. Confirm the Windows-only tests actually run.
- `iocp_dispatches_enqueued_work`
- `iocp_wake_interrupts_blocking_poll`

## Expected semantics

The Windows backend should only own completion delivery.

It should not own:
- deferred scheduling
- redraw policy
- runtime invalidation policy
- script pacing

Those remain in the core proactor and higher-level runtime/host code.

## Next host integration target on Windows

After the backend tests pass, the next step is to remove polling from the Windows host loop and drive it from the proactor.

The order should be:

1. Replace any fixed sleep/tick loop with proactor wakeups and timer deadlines.
2. Keep native window message pumping as the event source.
3. Use the proactor for:
   - runtime wakeups
   - deferred frame ticks
   - later network / async task completions
4. Only after that, move toward invalidation-driven redraw policy.

## Scheduling policy guidance

Do not conflate these two modes:

- frame-paced mode
  - draw every presentation interval while animation is active
- dirty-driven mode
  - draw only when state changed or a timer/deadline says another frame is required

For a more animated VN, both are needed.

Recommended policy:
- if transitions, live effects, text reveal, particles, or active drag are running: frame-paced
- if the scene is static: dirty-driven

The proactor should support both by scheduling the next frame only when needed.

## What not to do

Do not reintroduce:
- fixed `sleep(...)` host loops
- unconditional redraw forever while idle
- backend-specific timer threads for normal scheduling
- deferred work queues inside the Windows backend itself

## If Windows compile breaks

Capture and report:

```powershell
cargo test -p loadngo-proactor -- --nocapture
cargo check -p loadngo-proactor -v
```

Include:
- exact compiler error
- file and line
- whether failure is API binding, handle lifetime, timeout handling, or wake/completion semantics
