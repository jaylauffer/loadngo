# Proactor Architecture

This crate is the Rust replacement for the original C++ `Machine` + `Timer` split.

## Original C++ shape

The historical code separated responsibilities in two layers:

- `Machine`
  - owned the completion-dispatch loop
  - accepted immediate `Work`
  - dispatched platform completion events (`IOCP` on Windows, `kqueue` on BSD)
- `Timer`
  - owned deferred work ordering by deadline
  - re-injected ready work back into the machine

That separation is the right one. Platform-specific readiness/completion delivery should not own deferred scheduling or runtime policy.

## Rust split

`loadngo-proactor` owns the core pieces:

- completion kinds (`Job`, `Net`, `Io`, `Timer`, `Exit`)
- immediate work posting
- deferred work queue ordered by deadline
- wake semantics when earlier work is scheduled
- run-loop policy (`run_once`, `run_until_stopped`)

Platform backends only need to implement `CompletionPort`:

- `post(...)`
- `poll(timeout)`
- `wake()`

This keeps `IOCP`, `kqueue`, `epoll`, `ALooper`, or `eventfd` details out of the core scheduling model.

## Backend mapping

Backend status:

- BSD/macOS: `kqueue` backend implemented in `loadngo-proactor`
- Windows: `IOCP` backend implemented in `loadngo-proactor`
- Linux: `epoll` + wake fd still to port
- Android: `ALooper` integration still to port onto this core

The current in-memory `ChannelPort` exists only as a test/reference backend. It proves the core semantics without baking in any OS choice.

## What this fixes

The current Rust hosts still poll continuously. That is not the intended `loadngo` architecture.

The proactor core is the first step toward:

- invalidation-driven rendering
- deferred scene/resource work
- host wakeups on input, timers, I/O, and task completion
- removal of fixed-sleep frame loops

## Scheduling policy

The proactor supports two legitimate presentation modes:

- frame-paced mode
  - schedule another frame at the next presentation interval while animation is active
- dirty-driven mode
  - schedule another frame only when state changed or a deferred deadline requires it

For the VN runtime, the right policy is mixed:

- animated scenes, transitions, particles, text reveal, or active drag: frame-paced
- static scenes and menus: dirty-driven

The important architectural point is that both modes should be driven by deferred work and invalidation, not by a fixed host sleep loop.

See `docs/PROACTOR_WINDOWS_AGENT.md` for the Windows validation runbook.
