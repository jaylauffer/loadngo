# Windows Proactor Runbook

State of `loadngo-proactor` on Windows, and how to validate it on a real
machine.

## Status (2026-09-15)

- **Backend.** `IocpPort` (`proactor/src/iocp.rs`) implements the whole
  `IoPort` surface -- `read`, `write`, `recv`, `send`, `recv_from`,
  `send_to`, `accept`, `connect`, `cancel_io` -- plus queued work, wakeups
  and shutdown draining.
- **Tests.** `proactor/tests/iocp.rs` exercises that surface against real
  handles and sockets, matching the Unix backends' suites: queued work in
  order, wake, stop, a file round trip including an offset read, UDP
  `recv_from`/`send_to`, TCP `accept` on IPv4 and IPv6 with data through the
  accepted socket, `connect` with a usable connected socket, `cancel_io`, and
  shutdown with an operation still in flight.
- **Host.** `host-desktop/src/windows.rs` owns a `HostProactor<IocpPort>` for
  the process lifetime and follows the `linux.rs`/`ios.rs` shape: frame
  timers are proactor deferred work dispatched from `about_to_wait`, and the
  winit loop blocks on `ControlFlow::WaitUntil(next_deadline)`. `next_frame`
  no longer spawns a thread per call. `wake_host` exists, as on Linux.
- **CI.** `loadngo`'s `ci.yml` `windows` job runs fmt, clippy with warnings as
  errors, and the workspace tests on the `build-windows-x64` runner.

Defects fixed on 2026-09-15, each pinned by a test:

| defect | effect | test |
| --- | --- | --- |
| `read` passed `ReadFile` the zero-capacity placeholder it swapped out, not the caller's buffer | every read completed having read nothing | `iocp_write_then_read_round_trip_an_overlapped_file` |
| `accept`'s pre-created socket was always `AF_INET` | every IPv6 listener failed | `iocp_accept_works_on_an_ipv6_listener` |
| a failed or cancelled accept dropped the pre-created socket | one leaked socket per failed accept, including every accept cancelled at shutdown | covered by the shutdown and accept paths |
| no `SO_UPDATE_CONNECT_CONTEXT` after `ConnectEx` | `getpeername`/`shutdown` failed on connected sockets | `iocp_connect_reaches_a_real_listener_and_leaves_a_usable_socket` |
| error codes re-read with `GetLastError` after the `windows` crate had already captured them, and reported as text | wrong or missing `raw_os_error()` | `iocp_cancel_io_completes_the_op_with_operation_aborted` |

## Contract a caller must meet

- **Files must be opened with `FILE_FLAG_OVERLAPPED`.** IOCP only queues
  completions for overlapped handles. A plain `std::fs::File` completes its
  I/O synchronously and never posts a completion, so the operation's handler
  never runs. Use `std::os::windows::fs::OpenOptionsExt::custom_flags`.
- Sockets created by `std::net` are already overlapped.
- `RawFdCompat` is `RawSocket` (`u64`) on Windows; pass a file's
  `as_raw_handle()` cast through `usize`.
- There is no readiness API (`ReadinessPort`) on Windows. IOCP is
  completion-based. Code that needs readiness on Unix -- such as the camera
  preview's capture pipe -- needs a completion-shaped Windows path instead.

## Validating on a Windows machine

From the `loadngo` root:

```powershell
cargo test -p loadngo-proactor --test iocp -- --nocapture
cargo clippy -p loadngo-proactor -p loadngo-host-desktop -p loadngo-gfx-dx12 --all-targets --all-features -- -D warnings
```

Then run a game on the host and confirm frame pacing, idle behaviour
(`FrameDemand::Idle` should leave the process asleep), window close, and
minimise/restore. Compiling and unit tests do not prove presentation: the
DX12 heap-exhaustion bug passed both.

From macOS or Linux, `scripts/check-windows.sh` type-checks the Windows host
and DX12 backend, and
`cargo clippy -p loadngo-proactor --target x86_64-pc-windows-msvc --all-targets -- -D warnings`
lints the backend and its tests. Neither runs anything.

## Still open

- **No host-level measurement yet.** The Windows host has not been measured
  against its old thread-per-wait loop the way iOS was (49.8 -> 59.6 FPS);
  see `PROACTOR_ENGINE_ADOPTION.md`'s evidence gate.
- **Completions posted from other threads do not wake the winit loop by
  themselves.** The host proactor only carries frame timers today, which the
  loop's own deadline covers. Anything that enqueues work on it from another
  thread must also call `wake_host`. Linux has the same shape.
- **`gui` and `gui-win32` do not compile** against the current `windows`
  crate and are excluded from CI.

## What not to do

- fixed `sleep(...)` host loops, or a thread per pending frame
- unconditional redraw forever while idle
- backend-specific timer threads for normal scheduling
- deferred work queues inside the Windows backend itself -- deadlines belong
  to the core proactor
