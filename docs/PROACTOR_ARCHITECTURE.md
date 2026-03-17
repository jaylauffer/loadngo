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
- Windows: `IOCP` backend still to port
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
