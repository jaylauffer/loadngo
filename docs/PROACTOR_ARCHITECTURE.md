# Proactor Architecture

This crate is the Rust replacement for the original C++ `Machine` + `Timer` split.

## The platform seam for networking

`loadngo-proactor` is where raw OS address representations stop. Nothing
above it names a `libc` or `windows` address type: `network/` works in
`std::net` and `socket2` types, and `host-desktop` touches none of this at
all. Keeping that boundary is what lets the layers above be genuinely
platform-agnostic while each backend still uses the representation its
own kernel actually speaks.

Inside the crate the same rule applies one level down:

| file | may contain |
| --- | --- |
| `io_port.rs` | the public surface only -- `PeerAddr`, `AcceptTransfer`, `IoBuf`. No `libc`, no `socket2`, no `windows::Win32`. |
| `sockaddr.rs` | raw `sockaddr_storage` decoding, with the primitives that differ by target split per-target |
| `uring.rs` / `kqueue.rs` / `epoll.rs` / `iocp.rs` | whatever their own platform needs |

`proactor/tests/core.rs::io_port_surface_names_no_raw_platform_address_types`
enforces the first row, so the boundary is checked rather than merely
intended.

This is not a style preference. The address types differ in shape, not
just in name -- `sa_family_t` is `u16` on Linux and `u8` on macOS, and
`c_char` is unsigned on aarch64 Linux and signed on darwin. When the
decoding lived in shared code there was no cast form clippy accepted on
every target at once (`as u16` is `unnecessary_cast` where the type is
already `u16`; `u16::from` is `useless_conversion` in the same place), and
the file needed an `#[allow]`. Splitting the differing primitive into
per-target functions removes the ambiguity rather than suppressing it, and
adding a platform means adding a branch in `sockaddr.rs` rather than
touching anything shared.


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

- BSD/macOS/iOS: `KqueuePort` implements completion delivery, readiness, and
  real `IoPort` operations
- Windows: `IocpPort` implements completion delivery and real `IoPort`
  operations; host integration still needs real-machine validation
- Linux: `IoUringPort` implements completion delivery, readiness, and real
  `IoPort` operations
- Android: `EpollPort` implements completion delivery, readiness, and real
  `IoPort` operations. `host-desktop` owns it on a dedicated pump thread;
  Android app processes cannot use `io_uring`, and this is deliberately not an
  `ALooper_addFd` integration.

The current in-memory `ChannelPort` exists only as a test/reference backend. It proves the core semantics without baking in any OS choice.

## Host adoption status

The macOS, Linux, and iOS `host-desktop` paths each own a proactor for the
application's lifetime -- `KqueuePort` on macOS and iOS, `IoUringPort` on
Linux. Runtime wakers route through it, `FrameDemand::After` schedules its
wait as deferred proactor work, and the native event pump (`NSApplication` on
macOS; `winit`'s `ControlFlow::WaitUntil` on Linux and iOS) blocks on the
proactor's next deadline instead of a fixed poll interval. Those hosts share
`host-desktop/src/proactor_driver.rs::HostProactor` rather than each
hand-rolling ownership.

Android also owns `HostProactor<EpollPort>` for the application lifetime, but
uses a dedicated thread blocked in `Proactor::run_until_stopped`: Android's
NativeActivity callback model has no native event-loop hook equivalent to the
macOS/Linux/iOS paths. Windows owns `HostProactor<IocpPort>` too (2026-09-15; CI
and the IOCP tests are green on `acerj`), but has not yet been run with a game,
so its pacing and idle behaviour are unmeasured.

On Linux this replaced a per-call `thread::spawn` in `next_frame()`'s
`FrameDemand::After` path (one new OS thread per pending frame timer,
previously undetected because ephemeral threads don't show up as steady
`/proc/<pid>/status` thread-count growth -- see
`docs/LINUX_X11_PRESENT_LATENCY.md`'s "What was ruled out" for that exact
measurement). iOS, Android and Windows have since adopted the same
host-ownership contract with platform-appropriate pump shapes.

## Portable host-driver seam

`HostProactor<P: CompletionPort>` (`host-desktop/src/proactor_driver.rs`)
holds a host's `Proactor<P>`/`ProactorHandle<P>` pair and provides the three
things every proactor-owning host needs: `drain_ready()` (dispatch
everything currently ready, looping until a poll reports no activity --
matches the drain pattern macOS/NetBSD each used to hand-roll) and
`waker()`/`waker_for()` (a `Waker` that pokes the completion port so a
blocked native event pump re-checks the runtime future). This is the "Phase
1" portable seam called for in
[PROACTOR_ENGINE_ADOPTION.md](PROACTOR_ENGINE_ADOPTION.md). macOS, Linux, and
iOS use its drain/waker shape; Android uses the same ownership type with its
dedicated proactor thread rather than an event-pump hook.

The required contract, evidence gate, and rollout order are defined in
[PROACTOR_ENGINE_ADOPTION.md](PROACTOR_ENGINE_ADOPTION.md).

## What this enables

The proactor core is the first step toward:

- invalidation-driven rendering
- deferred scene/resource work
- host wakeups on input, timers, I/O, and task completion
- removal of fixed-sleep frame loops

The current network refactor now uses this model for active `SneakerNet`
dispatch in two ways:

- generic fallback: nonblocking UDP receive plus deferred proactor work
- Unix fast path: direct socket-fd readiness registration into `epoll`/`kqueue`
- node transport fast path: one logical node can register multiple UDP sockets
  (for example separate IPv4 and IPv6 sockets) against the same proactor

That gives the current codebase a real path toward node runtimes that sleep
until actual network activity instead of keeping a timer-driven pump alive.

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
